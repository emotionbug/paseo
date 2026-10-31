mod bridge;
mod catalog;
mod legacy;
mod runtime;
mod v2;

use std::{path::PathBuf, sync::Arc, time::Duration};

use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use reqwest::Method;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::api::{
    AgentProvider, AgentSession, EventSink, ImportedSession, LaunchContext, ProviderSpec,
};
use crate::process::{self, CommandSpec};
use runtime::{Generation, Runtime, RuntimeManager};

pub struct OpenCodeProvider {
    spec: ProviderSpec,
    runtime: Arc<RuntimeManager>,
    capabilities: Value,
}

impl OpenCodeProvider {
    pub fn new(spec: ProviderSpec) -> Self {
        Self {
            runtime: Arc::new(RuntimeManager::new(spec.clone())),
            spec,
            capabilities: json!({
                "supportsStreaming":true,"supportsSessionPersistence":true,"supportsSessionListing":true,
                "supportsDynamicModes":true,"supportsMcpServers":true,"supportsReasoningStream":true,
                "supportsToolInvocations":true,"supportsNativePaseoTools":true,
                "supportsRewindConversation":false,"supportsRewindFiles":false,"supportsRewindBoth":true
            }),
        }
    }

    fn config(&self, mut config: Value) -> Result<Value> {
        if !config.is_object() {
            bail!("OpenCode session config must be an object");
        }
        config["provider"] = json!(self.spec.id);
        if let Some(defaults) = self
            .spec
            .options
            .as_object()
            .filter(|defaults| !defaults.is_empty())
        {
            let mut merged = defaults.clone();
            if let Some(options) = config
                .get("providerOptions")
                .filter(|value| !value.is_null())
            {
                merged.extend(
                    options
                        .as_object()
                        .ok_or_else(|| anyhow!("OpenCode providerOptions must be an object"))?
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone())),
                );
            }
            config["providerOptions"] = Value::Object(merged);
        }
        catalog::validate_config(&config)?;
        Ok(config)
    }

    fn prepare(&self, config: Value, context: &LaunchContext) -> Result<Value> {
        let mut config = self.config(config)?;
        if context
            .mcp_servers
            .as_object()
            .is_some_and(|servers| !servers.is_empty())
        {
            let mut merged = config["mcpServers"]
                .as_object()
                .cloned()
                .unwrap_or_default();
            merged.extend(
                context
                    .mcp_servers
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone())),
            );
            config["mcpServers"] = Value::Object(merged);
        }
        Ok(config)
    }

    async fn attach(
        &self,
        runtime: Arc<Runtime>,
        info: Value,
        config: Value,
        context: LaunchContext,
        mut options: Value,
        sink: EventSink,
    ) -> Result<Arc<dyn AgentSession>> {
        let id = info["id"]
            .as_str()
            .ok_or_else(|| anyhow!("OpenCode session response contains no id"))?;
        runtime.bind_session(id, &context)?;
        self.runtime.remember_session(id, &runtime);
        if !options.is_object() {
            options = json!({});
        }
        options["requiresPaseoPlugin"] = json!(true);
        match runtime.generation {
            Generation::Legacy => {
                let config = catalog::normalize_config(config)?;
                if let Some(stored) = options.get_mut("persistenceConfig") {
                    *stored = catalog::normalize_config(std::mem::take(stored))?;
                }
                legacy::attach(
                    runtime,
                    self.runtime.clone(),
                    self.spec.clone(),
                    info,
                    config,
                    context,
                    options,
                    sink,
                )
                .await
            }
            Generation::V2 => {
                v2::attach(
                    runtime,
                    self.runtime.clone(),
                    self.spec.clone(),
                    info,
                    config,
                    context,
                    options,
                    sink,
                )
                .await
            }
        }
    }

    async fn get(
        &self,
        runtime: &Runtime,
        id: &str,
        cwd: &str,
        cancel: CancellationToken,
    ) -> Result<Value> {
        let path = session_path(runtime.generation, id);
        let query = directory_query(runtime.generation, cwd);
        let value = runtime
            .request(Method::GET, &path, &query, None, cancel)
            .await?;
        native_info(runtime.generation, value)
    }

    async fn set_archived(&self, persistence: &Value, time: i64) -> Result<()> {
        let config = persistence
            .get("metadata")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let id = native_id(persistence)?;
        let cwd = config["cwd"].as_str().ok_or_else(|| {
            anyhow!("OpenCode native archive update requires the original working directory")
        })?;
        let runtime = match self.runtime.acquire_existing(id) {
            Some(runtime) => runtime,
            None => {
                self.runtime
                    .acquire(None, &config, CancellationToken::new())
                    .await?
            }
        };
        // The original v2 adapter has no native archive operation. Keeping its
        // durable session intact is distinct from removing the session on close.
        if runtime.generation == Generation::V2 {
            return Ok(());
        }
        runtime
            .request(
                Method::PATCH,
                &session_path(runtime.generation, id),
                &directory_query(runtime.generation, cwd),
                Some(json!({"time":{"archived":time}})),
                CancellationToken::new(),
            )
            .await?;
        Ok(())
    }
}

#[async_trait]
impl AgentProvider for OpenCodeProvider {
    fn id(&self) -> &str {
        &self.spec.id
    }
    fn capabilities(&self) -> &Value {
        &self.capabilities
    }
    fn resolve_create_config(&self, input: &Value) -> Option<Result<Value>> {
        catalog::resolve_create_config(input)
    }
    fn is_create_config_unattended(&self, input: &Value) -> Option<bool> {
        Some(
            crate::providers::create_config::is_default_unattended(input)
                || input.pointer("/config/featureValues/auto_accept") == Some(&Value::Bool(true))
                || input["features"].as_array().is_some_and(|features| {
                    features.iter().any(|feature| {
                        feature["id"] == "auto_accept"
                            && (feature["value"] == true || feature["value"] == "true")
                    })
                }),
        )
    }

    async fn create_session(
        &self,
        config: Value,
        context: LaunchContext,
        mut options: Value,
        sink: EventSink,
    ) -> Result<Arc<dyn AgentSession>> {
        let stored = self.config(config)?;
        if !options.is_object() {
            options = json!({});
        }
        options["persistenceConfig"] = stored.clone();
        let config = self.prepare(stored, &context)?;
        let runtime = self
            .runtime
            .acquire(Some(&context), &config, context.cancel.clone())
            .await?;
        let cwd = config["cwd"].as_str().unwrap();
        let permission = catalog::permission_rules(&config, runtime.generation)?;
        let (path, query, body) = if runtime.generation == Generation::V2 {
            let mut body = json!({"location":{"directory":cwd},"agent":config["modeId"].as_str().unwrap_or("build"),"permissions":permission});
            if let Some(title) = config.get("title") {
                body["title"] = title.clone();
            }
            if let Some(model) = config["model"].as_str() {
                body["model"] = catalog::model_ref(model, config["thinkingOptionId"].as_str())?;
            }
            ("/api/session", vec![], body)
        } else {
            let mut body = json!({});
            if !permission.is_empty() {
                body["permission"] = json!(permission);
            }
            ("/session", vec![("directory".into(), cwd.into())], body)
        };
        let response = runtime
            .request(
                Method::POST,
                path,
                &query,
                Some(body),
                context.cancel.clone(),
            )
            .await?;
        let info = native_info(runtime.generation, response)?;
        self.attach(runtime, info, config, context, options, sink)
            .await
    }

    async fn resume_session(
        &self,
        persistence: Value,
        config: Value,
        context: LaunchContext,
        mut options: Value,
        sink: EventSink,
    ) -> Result<Arc<dyn AgentSession>> {
        let overrides = config.clone();
        let stored = self.config(merge_config(&persistence, config))?;
        if !options.is_object() {
            options = json!({});
        }
        options["persistenceConfig"] = stored.clone();
        let config = self.prepare(stored, &context)?;
        let id = native_id(&persistence)?;
        let adopted = self.runtime.acquire_existing(id);
        let existing = adopted.is_some();
        let runtime = match adopted {
            Some(runtime) => runtime,
            None => {
                self.runtime
                    .acquire(Some(&context), &config, context.cancel.clone())
                    .await?
            }
        };
        let cwd = config["cwd"].as_str().unwrap();
        let mut info = self.get(&runtime, id, cwd, context.cancel.clone()).await?;
        options["externallyDriven"] = json!(existing && info["parentID"].is_string());
        if runtime.generation == Generation::Legacy {
            let rules = catalog::permission_rules(&config, runtime.generation)?;
            if !rules.is_empty() {
                runtime
                    .request(
                        Method::PATCH,
                        &session_path(runtime.generation, id),
                        &directory_query(runtime.generation, cwd),
                        Some(json!({"permission":rules})),
                        context.cancel.clone(),
                    )
                    .await?;
            }
        } else {
            if let Some(mode) = overrides["modeId"].as_str().filter(|mode| !mode.is_empty()) {
                runtime
                    .request(
                        Method::POST,
                        &format!("{}/agent", session_path(runtime.generation, id)),
                        &[],
                        Some(json!({"agent":mode})),
                        context.cancel.clone(),
                    )
                    .await?;
                info["agent"] = json!(mode);
            }
            if overrides["model"].as_str().is_some() || overrides.get("thinkingOptionId").is_some()
            {
                let variant = overrides["thinkingOptionId"]
                    .as_str()
                    .or_else(|| info.pointer("/model/variant").and_then(Value::as_str));
                let mut model = if let Some(model) = overrides["model"].as_str() {
                    catalog::model_ref(model, variant)?
                } else {
                    info["model"].clone()
                };
                if !model.is_object() {
                    bail!("Select an OpenCode model before changing its variant");
                }
                if overrides.get("model").is_none() && overrides.get("thinkingOptionId").is_some() {
                    if let Some(variant) = overrides["thinkingOptionId"].as_str() {
                        model["variant"] = json!(variant);
                    } else {
                        model.as_object_mut().unwrap().remove("variant");
                    }
                }
                runtime
                    .request(
                        Method::POST,
                        &format!("{}/model", session_path(runtime.generation, id)),
                        &[],
                        Some(json!({"model":model})),
                        context.cancel.clone(),
                    )
                    .await?;
                info["model"] = model;
            }
        }
        self.attach(runtime, info, config, context, options, sink)
            .await
    }

    async fn fetch_catalog(&self, options: Value, cancel: CancellationToken) -> Result<Value> {
        let cwd = if options["scope"] == "workspace" {
            options["cwd"]
                .as_str()
                .ok_or_else(|| anyhow!("Workspace OpenCode catalog requires cwd"))?
                .to_owned()
        } else {
            let directory = paseo_home(&self.spec).join("opencode-home");
            tokio::select! {
                biased;
                _=cancel.cancelled()=>bail!("OpenCode server acquisition canceled"),
                result=tokio::fs::create_dir_all(&directory)=>result?,
            }
            directory.to_string_lossy().into_owned()
        };
        let runtime = self
            .runtime
            .acquire_catalog(options["force"] == true, cancel.clone())
            .await?;
        let result = catalog::fetch(&runtime, &cwd, &self.spec.id, cancel).await;
        let close = if let Some(runtime) = Arc::into_inner(runtime) {
            runtime.stop().await
        } else {
            Ok(())
        };
        match result {
            Ok(value) => {
                close?;
                Ok(value)
            }
            Err(error) => Err(error),
        }
    }

    async fn list_features(&self, config: &Value) -> Result<Vec<Value>> {
        let config = self.config(config.clone())?;
        Ok(
            match self.runtime.generation(CancellationToken::new()).await? {
                Generation::Legacy => catalog::features(&catalog::normalize_config(config)?),
                Generation::V2 => catalog::features_v2(&config),
            },
        )
    }

    async fn list_commands(&self, config: &Value) -> Result<Vec<Value>> {
        let config = self.config(config.clone())?;
        let runtime = self
            .runtime
            .acquire(None, &config, CancellationToken::new())
            .await?;
        catalog::commands(
            &runtime,
            config["cwd"].as_str().unwrap(),
            CancellationToken::new(),
        )
        .await
    }

    async fn list_importable_sessions(&self, options: Value) -> Result<Vec<Value>> {
        let runtime = self
            .runtime
            .acquire(None, &json!({}), CancellationToken::new())
            .await?;
        let scan = options["scanLimit"]
            .as_u64()
            .unwrap_or(if runtime.generation == Generation::V2 {
                100
            } else {
                options["limit"].as_u64().unwrap_or(200)
            })
            .min(500) as usize;
        let limit = options["limit"]
            .as_u64()
            .unwrap_or(if runtime.generation == Generation::V2 {
                50
            } else {
                200
            }) as usize;
        if scan == 0 || limit == 0 {
            return Ok(vec![]);
        }
        let mut sessions = Vec::new();
        if runtime.generation == Generation::V2 {
            let mut cursor = None::<String>;
            loop {
                let mut query = vec![("limit".into(), (scan - sessions.len()).min(50).to_string())];
                if let Some(cursor) = &cursor {
                    query.push(("cursor".into(), cursor.clone()));
                } else {
                    if let Some(cwd) = options["cwd"].as_str() {
                        query.push(("directory".into(), cwd.into()));
                    }
                    if let Some(search) = options["query"].as_str() {
                        query.push(("search".into(), search.into()));
                    }
                }
                let response = runtime
                    .request(
                        Method::GET,
                        "/api/session",
                        &query,
                        None,
                        CancellationToken::new(),
                    )
                    .await?;
                let page = response["data"]
                    .as_array()
                    .ok_or_else(|| anyhow!("OpenCode session inventory is malformed"))?;
                sessions.extend(page.iter().cloned());
                let next = response
                    .pointer("/cursor/next")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                if sessions.len() >= scan || next.is_none() {
                    break;
                }
                if next == cursor {
                    bail!("OpenCode returned a repeated session cursor");
                }
                cursor = next;
            }
            sessions.retain(|info| {
                info["parentID"].is_null()
                    && info
                        .pointer("/time/archived")
                        .and_then(Value::as_i64)
                        .unwrap_or(0)
                        == 0
            });
        } else {
            let count = if options["cwd"].is_string() {
                scan.max(200)
            } else {
                scan
            }
            .min(500);
            let mut query = vec![
                ("archived".into(), "true".into()),
                ("roots".into(), "true".into()),
                ("limit".into(), count.to_string()),
            ];
            if let Some(search) = options["query"].as_str() {
                query.push(("search".into(), search.into()));
            }
            let response = runtime
                .request(
                    Method::GET,
                    "/experimental/session",
                    &query,
                    None,
                    CancellationToken::new(),
                )
                .await?;
            sessions = response
                .as_array()
                .ok_or_else(|| anyhow!("OpenCode session inventory is malformed"))?
                .clone();
            if let Some(cwd) = options["cwd"].as_str() {
                let cwd = canonical_or_original(cwd);
                sessions.retain(|info| {
                    info["directory"]
                        .as_str()
                        .is_some_and(|directory| canonical_or_original(directory) == cwd)
                });
            }
            sessions.sort_by_key(|info| {
                std::cmp::Reverse(
                    info.pointer("/time/updated")
                        .or_else(|| info.pointer("/time/created"))
                        .and_then(Value::as_i64)
                        .unwrap_or(0),
                )
            });
        }
        sessions.into_iter().take(limit).map(|info|{
            let id=info["id"].as_str().ok_or_else(||anyhow!("OpenCode session has no id"))?;
            let cwd=if runtime.generation==Generation::V2{&info["location"]["directory"]}else{&info["directory"]};
            let title=info["title"].as_str().map(str::trim).filter(|title|!title.is_empty());
            let timestamp=info.pointer("/time/updated").or_else(||info.pointer("/time/created")).and_then(Value::as_i64).unwrap_or(0);
            let timestamp=chrono::DateTime::from_timestamp_millis(timestamp).ok_or_else(||anyhow!("OpenCode session timestamp is invalid"))?.to_rfc3339_opts(chrono::SecondsFormat::Millis,true);
            Ok(json!({"providerHandleId":id,"cwd":cwd,"title":title,"firstPromptPreview":null,"lastPromptPreview":null,"lastActivityAt":timestamp}))
        }).collect()
    }

    async fn import_session(
        &self,
        input: Value,
        mut config: Value,
        mut stored_config: Value,
        context: LaunchContext,
        sink: EventSink,
    ) -> Result<ImportedSession> {
        let id = input["providerHandleId"]
            .as_str()
            .ok_or_else(|| anyhow!("OpenCode import requires providerHandleId"))?;
        let cwd = input["cwd"]
            .as_str()
            .ok_or_else(|| anyhow!("OpenCode import requires cwd"))?;
        if !config.is_object() {
            config = json!({});
        }
        config["provider"] = json!(self.spec.id);
        config["cwd"] = json!(cwd);
        stored_config["provider"] = json!(self.spec.id);
        stored_config["cwd"] = json!(cwd);
        let runtime = self
            .runtime
            .acquire(Some(&context), &config, context.cancel.clone())
            .await?;
        let info = self.get(&runtime, id, cwd, context.cancel.clone()).await?;
        if let Some(title) = info["title"]
            .as_str()
            .map(str::trim)
            .filter(|title| !title.is_empty())
        {
            config["title"] = json!(title);
            stored_config["title"] = json!(title);
        }
        if let Some(agent) = info["agent"].as_str() {
            config["modeId"] = json!(agent);
            stored_config["modeId"] = json!(agent);
        }
        if let (Some(provider), Some(model)) = (
            info["model"]["providerID"].as_str(),
            info["model"]["id"].as_str(),
        ) {
            config["model"] = json!(format!("{provider}/{model}"));
            stored_config["model"] = config["model"].clone();
        }
        if runtime.generation == Generation::Legacy {
            let response = runtime
                .request(
                    Method::GET,
                    &format!("{}/message", session_path(runtime.generation, id)),
                    &directory_query(runtime.generation, cwd),
                    None,
                    context.cancel.clone(),
                )
                .await?;
            let mut messages = response
                .as_array()
                .ok_or_else(|| anyhow!("OpenCode session messages are malformed"))?
                .as_slice();
            if info["revert"]["partID"].is_null() {
                if let Some(revert) = info["revert"]["messageID"].as_str() {
                    if let Some(index) = messages
                        .iter()
                        .position(|message| message["info"]["id"] == revert)
                    {
                        messages = &messages[..index];
                    }
                }
            }
            if !config["modeId"].is_string() {
                if let Some(agent) = messages.iter().find_map(|message| {
                    message["info"]["agent"]
                        .as_str()
                        .filter(|s| !s.trim().is_empty())
                }) {
                    config["modeId"] = json!(agent);
                    stored_config["modeId"] = json!(agent);
                }
            }
            if !config["model"].is_string() {
                for message in messages {
                    let info = &message["info"];
                    let model = if info["role"] == "user" {
                        &info["model"]
                    } else {
                        info
                    };
                    if let (Some(provider), Some(model)) =
                        (model["providerID"].as_str(), model["modelID"].as_str())
                    {
                        config["model"] = json!(format!("{provider}/{model}"));
                        stored_config["model"] = config["model"].clone();
                        break;
                    }
                }
            }
        }
        self.runtime.remember_session(id, &runtime);
        let persistence = json!({"provider":self.spec.id,"sessionId":id,"nativeHandle":id,"metadata":stored_config});
        let session = self
            .resume_session(
                persistence.clone(),
                config,
                context.clone(),
                json!({}),
                sink,
            )
            .await?;
        let (history_sink, mut receiver) = EventSink::channel(64, context.cancel.child_token());
        let history = async {
            let mut timeline = Vec::new();
            let mut subagents = Vec::new();
            while let Some(event) = receiver.recv().await {
                match event["type"].as_str() {
                    Some("timeline") => {
                        let mut entry = json!({"item":event["item"]});
                        if let Some(timestamp) = event.get("timestamp") {
                            entry["timestamp"] = timestamp.clone();
                        }
                        timeline.push(entry);
                    }
                    Some("provider_subagent") => subagents.push(event),
                    _ => {}
                }
            }
            Ok::<_, anyhow::Error>((timeline, subagents))
        };
        let replay = tokio::try_join!(session.stream_history(history_sink), history);
        let (_, (timeline, provider_subagent_events)) = match replay {
            Ok(result) => result,
            Err(error) => {
                let _ = session.close().await;
                return Err(error);
            }
        };
        Ok(ImportedSession {
            session,
            config: stored_config,
            persistence,
            timeline,
            provider_subagent_events,
        })
    }

    async fn is_available(&self, _options: Value, cancel: CancellationToken) -> Result<bool> {
        if cancel.is_cancelled() {
            bail!("OpenCode availability check canceled");
        }
        let Some(program) = self.spec.command.first() else {
            return Ok(false);
        };
        let path = self
            .spec
            .env
            .get("PATH")
            .map(std::ffi::OsString::from)
            .or_else(|| std::env::var_os("PATH"));
        Ok(which::which_in(program, path, std::env::current_dir()?).is_ok())
    }

    async fn diagnostic(&self) -> Result<String> {
        let available = self
            .is_available(json!({}), CancellationToken::new())
            .await?;
        let Some((program, prefix)) = self.spec.command.split_first() else {
            return Ok(
                "OpenCode\n  Command: not configured\n  Available: false\n  Auth: Not checked"
                    .into(),
            );
        };
        let mut auth = "Not checked".to_owned();
        if available {
            let mut args = prefix.to_vec();
            args.extend(["auth".into(), "list".into()]);
            let mut env = std::env::vars().collect::<std::collections::BTreeMap<_, _>>();
            env.extend(self.spec.env.clone());
            match process::run(
                CommandSpec {
                    program: program.clone(),
                    args,
                    cwd: None,
                    env,
                    clear_env: true,
                },
                1024 * 1024,
                Duration::from_secs(5),
                CancellationToken::new(),
            )
            .await
            {
                Ok(output) => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    auth = if stdout.trim().is_empty() {
                        output.stderr.trim().to_owned()
                    } else {
                        stdout.trim().into()
                    };
                    if auth.is_empty() {
                        auth = "(empty)".into();
                    }
                }
                Err(error) => auth = format!("Error - {error}"),
            }
        }
        Ok(format!(
            "OpenCode\n  Command: {program}\n  Available: {available}\n  Auth: {}",
            auth.replace('\n', "\n    ")
        ))
    }

    async fn archive_native_session(&self, persistence: &Value) -> Result<()> {
        self.set_archived(persistence, chrono::Utc::now().timestamp_millis())
            .await
    }
    async fn unarchive_native_session(&self, persistence: &Value) -> Result<()> {
        self.set_archived(persistence, 0).await
    }
    async fn shutdown(&self) -> Result<()> {
        self.runtime.shutdown().await
    }
}

fn native_id(persistence: &Value) -> Result<&str> {
    persistence["nativeHandle"]
        .as_str()
        .or_else(|| persistence["sessionId"].as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("OpenCode persistence requires a native session id"))
}
fn native_info(generation: Generation, value: Value) -> Result<Value> {
    let value = if generation == Generation::V2 {
        value
            .get("data")
            .cloned()
            .ok_or_else(|| anyhow!("OpenCode session response contains no data"))?
    } else {
        value
    };
    if !value["id"].is_string() {
        bail!("OpenCode session response contains no id");
    }
    Ok(value)
}
fn session_path(generation: Generation, id: &str) -> String {
    let id = percent_encoding::utf8_percent_encode(id, percent_encoding::NON_ALPHANUMERIC);
    format!(
        "{}/session/{id}",
        if generation == Generation::V2 {
            "/api"
        } else {
            ""
        }
    )
}
fn directory_query(generation: Generation, cwd: &str) -> Vec<(String, String)> {
    if generation == Generation::Legacy {
        vec![("directory".into(), cwd.into())]
    } else {
        vec![]
    }
}
fn merge_config(persistence: &Value, overrides: Value) -> Value {
    let mut config = persistence["metadata"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    if let Some(overrides) = overrides.as_object() {
        config.extend(
            overrides
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
    }
    Value::Object(config)
}
fn paseo_home(spec: &ProviderSpec) -> PathBuf {
    let raw = spec
        .env
        .get("PASEO_HOME")
        .cloned()
        .or_else(|| std::env::var("PASEO_HOME").ok());
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    match raw.as_deref() {
        Some("~") => home,
        Some(path) if path.starts_with("~/") => home.join(&path[2..]),
        Some(path) => PathBuf::from(path),
        None => home.join(".paseo"),
    }
}
fn canonical_or_original(path: &str) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path))
}
