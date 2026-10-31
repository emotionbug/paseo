use std::{
    collections::{BTreeMap, HashMap, HashSet},
    convert::Infallible,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
};

use anyhow::{Context, Result, anyhow, bail};
use bytes::Bytes;
use http::{Request, Response, StatusCode};
use http_body_util::{BodyExt, Full, Limited};
use hyper::{body::Incoming, server::conn::http1, service::service_fn};
use hyper_util::rt::TokioIo;
use rand::RngCore;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio_util::sync::CancellationToken;

use super::runtime::Generation;
use crate::providers::api::{LaunchContext, NativeToolExecutor, NativeToolInputError};

const PREFIX: &str = "/_internal/opencode";
const MAX_REQUEST_BYTES: usize = 1024 * 1024;
// These are the source integration's self-contained ESM artifacts. Their Zod
// and Ajv dependencies execute inside OpenCode, never in a Node daemon backend.
const LEGACY_PLUGIN: &[u8] = include_bytes!("assets/legacy-plugin.mjs");
const V2_PLUGIN: &[u8] = include_bytes!("assets/v2-plugin.mjs");

type HttpResponse = Response<Full<Bytes>>;

struct Binding {
    agent_id: String,
    env: BTreeMap<String, String>,
    tools: HashSet<String>,
    tool_order: Vec<String>,
    executor: Option<Arc<dyn NativeToolExecutor>>,
    cancel: CancellationToken,
}

struct State {
    authorization: String,
    manifest: Vec<Value>,
    bindings: RwLock<HashMap<String, Arc<Binding>>>,
    stop: CancellationToken,
}

pub(super) struct Bridge {
    state: Arc<State>,
    base_url: String,
    plugin_url: String,
    server: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Bridge {
    pub async fn start(home: &Path, generation: Generation, catalog: Value) -> Result<Self> {
        let plugin_url = materialize_plugin(home, generation).await?;
        let manifest = manifest(&catalog)?;
        let mut secret = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut secret);
        let state = Arc::new(State {
            authorization: format!("Bearer {}", hex::encode(secret)),
            manifest,
            bindings: RwLock::new(HashMap::new()),
            stop: CancellationToken::new(),
        });
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let base_url = format!("http://127.0.0.1:{}", listener.local_addr()?.port());
        let server = state.clone();
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    biased;
                    _ = server.stop.cancelled() => break,
                    _ = connections.join_next(), if !connections.is_empty() => {},
                    accepted = listener.accept() => {
                        let (socket, _) = match accepted {
                            Ok(connection) => connection,
                            Err(error) => { tracing::warn!(%error, "OpenCode bridge accept failed"); break; }
                        };
                        let state = server.clone();
                        connections.spawn(async move {
                            let disconnected = state.stop.child_token();
                            let _guard = disconnected.clone().drop_guard();
                            let requests = disconnected.clone();
                            let service = service_fn(move |request| {
                                route(state.clone(), request, requests.clone())
                            });
                            tokio::select! {
                                biased;
                                _ = disconnected.cancelled() => {},
                                result = http1::Builder::new().serve_connection(TokioIo::new(socket), service) => {
                                    if let Err(error) = result { tracing::debug!(%error, "OpenCode bridge connection ended"); }
                                }
                            }
                        });
                    }
                }
            }
            server.stop.cancel();
            while connections.join_next().await.is_some() {}
        });
        Ok(Self {
            state,
            base_url,
            plugin_url,
            server: Mutex::new(Some(task)),
        })
    }

    pub fn decorate_env(
        &self,
        environment: &mut BTreeMap<String, String>,
        generation: Generation,
    ) -> Result<()> {
        let mut config = match environment
            .get("OPENCODE_CONFIG_CONTENT")
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
        {
            Some(text) => serde_json::from_str::<Value>(text)?,
            None => json!({}),
        };
        let config = config
            .as_object_mut()
            .ok_or_else(|| anyhow!("OPENCODE_CONFIG_CONTENT must contain a JSON object"))?;
        let options = json!({ "baseUrl": self.base_url, "token": self.state.authorization.strip_prefix("Bearer ").expect("bridge bearer prefix") });
        match generation {
            Generation::Legacy => {
                let mut plugins = match config.remove("plugin") {
                    None | Some(Value::Null) => Vec::new(),
                    Some(Value::Array(plugins)) => plugins,
                    Some(_) => bail!("OpenCode plugin must be an array"),
                };
                plugins.retain(|entry| {
                    let specifier = entry
                        .as_str()
                        .or_else(|| entry.get(0).and_then(Value::as_str));
                    !specifier.is_some_and(|specifier| {
                        specifier.contains("/paseo-") && specifier.ends_with(".mjs")
                    })
                });
                plugins.push(json!([self.plugin_url, options]));
                config.insert("plugin".into(), Value::Array(plugins));
            }
            Generation::V2 => {
                let mut plugins = match config.remove("plugins") {
                    None => Vec::new(),
                    Some(Value::Array(plugins)) => plugins,
                    Some(_) => bail!("OpenCode v2 plugins must be an array"),
                };
                plugins.retain(|entry| {
                    entry.as_str() != Some(self.plugin_url.as_str())
                        && entry.get("package").and_then(Value::as_str)
                            != Some(self.plugin_url.as_str())
                });
                plugins.push(json!({ "package": self.plugin_url, "options": options }));
                config.insert("plugins".into(), Value::Array(plugins));
            }
        }
        environment.insert(
            "OPENCODE_CONFIG_CONTENT".into(),
            serde_json::to_string(config)?,
        );
        Ok(())
    }

    pub fn bind_session(&self, id: &str, context: &LaunchContext) -> Result<()> {
        if self.state.stop.is_cancelled() {
            bail!("OpenCode bridge is not started");
        }
        let tool_order: Vec<String> = manifest(&context.tool_catalog)?
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str).map(str::to_owned))
            .collect();
        let tools: HashSet<String> = tool_order.iter().cloned().collect();
        if !tools.is_empty() && context.tool_executor.is_none() {
            bail!("OpenCode tool catalog has no native executor");
        }
        let binding = Arc::new(Binding {
            agent_id: context.agent_id.clone(),
            env: context.env.clone(),
            tools,
            tool_order,
            executor: context.tool_executor.clone(),
            cancel: context.cancel.child_token(),
        });
        self.state
            .bindings
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .insert(id.to_owned(), binding);
        Ok(())
    }

    pub fn unbind_session(&self, id: &str, agent_id: &str) {
        let mut bindings = self
            .state
            .bindings
            .write()
            .unwrap_or_else(|p| p.into_inner());
        // A child can be adopted by another agent while its original parent
        // remains open. The parent's later cleanup must not remove the adopter.
        if bindings
            .get(id)
            .is_some_and(|binding| binding.agent_id == agent_id)
        {
            if let Some(binding) = bindings.remove(id) {
                binding.cancel.cancel();
            }
        }
    }

    pub fn close(&self) {
        self.state.stop.cancel();
        let mut bindings = self
            .state
            .bindings
            .write()
            .unwrap_or_else(|p| p.into_inner());
        for binding in bindings.values() {
            binding.cancel.cancel();
        }
        bindings.clear();
    }

    pub async fn shutdown(&self) -> Result<()> {
        self.close();
        let task = self.server.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(task) = task {
            task.await.context("OpenCode bridge server failed")?;
        }
        Ok(())
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.close();
    }
}

async fn route(
    state: Arc<State>,
    request: Request<Incoming>,
    disconnected: CancellationToken,
) -> std::result::Result<HttpResponse, Infallible> {
    let supplied = request
        .headers()
        .get(http::header::AUTHORIZATION)
        .map(|value| value.as_bytes())
        .unwrap_or_default();
    if !bool::from(supplied.ct_eq(state.authorization.as_bytes())) {
        return Ok(reply(
            StatusCode::UNAUTHORIZED,
            json!({"error":"Unauthorized"}),
        ));
    }
    let result = route_authorized(&state, request, disconnected).await;
    Ok(match result {
        Ok(response) => response,
        Err(error) => {
            if let Some(error) = error.downcast_ref::<BridgeClientError>() {
                reply(error.status, json!({"error": error.message}))
            } else if let Some(error) = error.downcast_ref::<NativeToolInputError>() {
                let body = match &error.issues {
                    Some(issues) => json!({"error":"Invalid tool input", "issues": issues}),
                    None => json!({"error":error.to_string()}),
                };
                reply(
                    StatusCode::from_u16(error.status).unwrap_or(StatusCode::BAD_REQUEST),
                    body,
                )
            } else {
                reply(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    json!({"error":error.to_string()}),
                )
            }
        }
    })
}

async fn route_authorized(
    state: &State,
    request: Request<Incoming>,
    disconnected: CancellationToken,
) -> Result<HttpResponse> {
    let path = request.uri().path();
    if request.method() == http::Method::GET && path == format!("{PREFIX}/tools") {
        return Ok(reply(StatusCode::OK, json!({"tools":state.manifest})));
    }
    let Some(path) = path.strip_prefix("/_internal/opencode/sessions/") else {
        return Ok(not_found());
    };
    let pieces: Vec<_> = path.split('/').collect();
    if pieces.first().is_none_or(|id| id.is_empty()) {
        return Ok(not_found());
    }
    let session_id = percent_encoding::percent_decode_str(pieces[0])
        .decode_utf8()
        .context("Malformed OpenCode session ID")?;
    let is_context =
        request.method() == http::Method::GET && pieces.len() == 2 && pieces[1] == "context";
    let is_tool = request.method() == http::Method::POST
        && pieces.len() == 3
        && pieces[1] == "tools"
        && !pieces[2].is_empty();
    if !is_context && !is_tool {
        return Ok(not_found());
    }
    let binding = state
        .bindings
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .get(session_id.as_ref())
        .cloned();
    let Some(binding) = binding else {
        return Ok(reply(
            StatusCode::NOT_FOUND,
            json!({"error":"OpenCode session is not bound to a Paseo agent"}),
        ));
    };
    if is_context {
        let mut context = json!({"env": binding.env});
        if request.uri().query().is_some_and(|query| {
            url::form_urlencoded::parse(query.as_bytes()).any(|(key, _)| key == "tools")
        }) {
            context["tools"] = json!(binding.tool_order);
        }
        return Ok(reply(StatusCode::OK, context));
    }
    let Some(executor) = &binding.executor else {
        return Ok(reply(
            StatusCode::FORBIDDEN,
            json!({"error":"Paseo tools are disabled for this session"}),
        ));
    };
    let tool_name = percent_encoding::percent_decode_str(pieces[2])
        .decode_utf8()
        .context("Malformed OpenCode tool name")?
        .into_owned();
    if !binding.tools.contains(&tool_name) {
        return Err(BridgeClientError {
            status: StatusCode::NOT_FOUND,
            message: format!("Paseo tool not found: {tool_name}"),
        }
        .into());
    }
    let body = Limited::new(request.into_body(), MAX_REQUEST_BYTES)
        .collect()
        .await
        .map_err(|error| {
            if error
                .downcast_ref::<http_body_util::LengthLimitError>()
                .is_some()
            {
                anyhow!(BridgeClientError {
                    status: StatusCode::PAYLOAD_TOO_LARGE,
                    message: "OpenCode bridge request body is too large".into()
                })
            } else {
                anyhow::Error::from_boxed(error)
            }
        })?
        .to_bytes();
    let input = if body.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&body).map_err(|_| BridgeClientError {
            status: StatusCode::BAD_REQUEST,
            message: "Malformed JSON request body".into(),
        })?
    };
    let cancel = disconnected.child_token();
    let _guard = cancel.clone().drop_guard();
    let result = tokio::select! {
        biased;
        _ = binding.cancel.cancelled() => { cancel.cancel(); bail!("OpenCode tool request was aborted"); }
        _ = cancel.cancelled() => bail!("OpenCode tool request was aborted"),
        result = executor.execute(&tool_name, input, cancel.clone()) => result?,
    };
    Ok(reply(
        StatusCode::OK,
        add_model_visible_structured_content(result)?,
    ))
}

#[derive(Debug)]
struct BridgeClientError {
    status: StatusCode,
    message: String,
}
impl std::fmt::Display for BridgeClientError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}
impl std::error::Error for BridgeClientError {}

fn reply(status: StatusCode, body: Value) -> HttpResponse {
    let mut response = Response::new(Full::new(Bytes::from(
        serde_json::to_vec(&body).expect("JSON value serialization"),
    )));
    *response.status_mut() = status;
    response.headers_mut().insert(
        http::header::CONTENT_TYPE,
        http::HeaderValue::from_static("application/json"),
    );
    response
}
fn not_found() -> HttpResponse {
    reply(StatusCode::NOT_FOUND, json!({"error":"Not found"}))
}

fn manifest(catalog: &Value) -> Result<Vec<Value>> {
    let tools = catalog.get("tools").unwrap_or(catalog);
    let entries: Vec<&Value> = match tools {
        Value::Null => Vec::new(),
        Value::Array(tools) => tools.iter().collect(),
        Value::Object(tools) => tools.values().collect(),
        _ => bail!("OpenCode tool catalog must contain tool definitions"),
    };
    entries
        .into_iter()
        .map(|tool| {
            let name = tool
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("OpenCode tool catalog entry has no name"))?;
            let description = tool
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("");
            let schema = tool
                .get("inputSchema")
                .cloned()
                .unwrap_or_else(|| json!({"type":"object","properties":{}}));
            let mut definition =
                json!({"name":name, "description":description, "inputSchema":schema});
            if let Some(title) = tool
                .get("title")
                .and_then(Value::as_str)
                .filter(|title| !title.is_empty())
            {
                definition["title"] = json!(title);
            }
            Ok(definition)
        })
        .collect()
}

fn add_model_visible_structured_content(mut result: Value) -> Result<Value> {
    let object = result
        .as_object_mut()
        .ok_or_else(|| anyhow!("Native Paseo tool returned a non-object result"))?;
    let content = object
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("Native Paseo tool returned no content array"))?;
    if !content.is_empty() {
        return Ok(result);
    }
    let Some(structured) = object.get("structuredContent") else {
        return Ok(result);
    };
    let mut summaries = Vec::new();
    if let Some(object) = structured.as_object() {
        for (key, value) in object {
            let Some(items) = value.as_array() else {
                continue;
            };
            summaries.push(format!("{key}_count={}", items.len()));
            let ids: Vec<_> = items
                .iter()
                .filter_map(|item| {
                    item.get("id")
                        .and_then(Value::as_str)
                        .filter(|id| !id.is_empty())
                })
                .collect();
            if !ids.is_empty() && ids.len() == items.len() {
                summaries.push(format!("{key}_ids={}", ids.join(",")));
            }
        }
    }
    let json = serde_json::to_string_pretty(structured)?;
    let text = if summaries.is_empty() {
        json
    } else {
        format!("{}\n\n{json}", summaries.join("\n"))
    };
    object.insert("content".into(), json!([{"type":"text","text":text}]));
    Ok(result)
}

async fn materialize_plugin(home: &Path, generation: Generation) -> Result<String> {
    let artifact = if generation == Generation::Legacy {
        LEGACY_PLUGIN
    } else {
        V2_PLUGIN
    };
    let digest = hex::encode(Sha256::digest(artifact));
    let root = home.join("runtime").join("opencode");
    let destination = if generation == Generation::Legacy {
        let destination = root.join(format!("paseo-{digest}.mjs"));
        atomic_write(destination.clone(), artifact).await?;
        destination
    } else {
        let directory = root.join(format!("paseo-v2-{digest}"));
        atomic_write(directory.join("server.js"), artifact).await?;
        atomic_write(directory.join("package.json"), br#"{"name":"paseo-opencode-bridge","private":true,"type":"module","exports":{"./server":"./server.js"}}"#).await?;
        directory
    };
    url::Url::from_file_path(&destination)
        .map(|url| url.to_string())
        .map_err(|_| {
            anyhow!(
                "OpenCode plugin path is not absolute: {}",
                destination.display()
            )
        })
}

async fn atomic_write(path: PathBuf, bytes: &'static [u8]) -> Result<()> {
    tokio::task::spawn_blocking(move || -> Result<()> {
        use std::io::Write;
        let parent = path
            .parent()
            .ok_or_else(|| anyhow!("OpenCode asset has no parent directory"))?;
        std::fs::create_dir_all(parent)?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary.write_all(bytes)?;
        temporary.as_file().sync_all()?;
        temporary.persist(&path).map_err(|error| error.error)?;
        Ok(())
    })
    .await
    .context("OpenCode asset writer failed")?
}
