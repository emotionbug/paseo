use super::transport::Transport;
use crate::{
    mcp_client::McpConnection,
    providers::api::{LaunchContext, NativeToolExecutor},
};
use anyhow::{Result, anyhow, bail};
use futures_util::StreamExt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

struct BridgedTool {
    client: Arc<McpConnection>,
    server: String,
    name: String,
}
pub struct Router {
    runtime: Arc<Transport>,
    executor: Option<Arc<dyn NativeToolExecutor>>,
    tools: HashMap<String, BridgedTool>,
    clients: Vec<Arc<McpConnection>>,
    pending: Mutex<HashMap<String, CancellationToken>>,
    closed: AtomicBool,
    cancel: CancellationToken,
}
impl Router {
    pub async fn register(
        runtime: Arc<Transport>,
        context: &LaunchContext,
        cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> Result<Arc<Self>> {
        let native = context
            .tool_catalog
            .as_array()
            .or_else(|| context.tool_catalog["tools"].as_array())
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        if !native.is_empty() && context.tool_executor.is_none() {
            bail!("OMP native tool catalog has no executable tool dispatcher");
        }
        let mut definitions = Vec::new();
        for tool in native {
            let name = tool["name"]
                .as_str()
                .ok_or_else(|| anyhow!("Native tool definition requires a name"))?;
            let mut definition = json!({"name":name,"description":tool["description"].as_str().unwrap_or(""),"loadMode":"essential","parameters":tool.get("inputSchema").or_else(||tool.get("parameters")).cloned().unwrap_or_else(||json!({"type":"object","properties":{}}))});
            if let Some(title) = tool["title"].as_str() {
                definition["label"] = json!(title);
            }
            definitions.push(definition);
        }
        let cancel = context.cancel.child_token();
        let servers = crate::mcp::strip_internal_servers(context.mcp_servers.clone());
        let mut connections = futures_util::stream::FuturesUnordered::new();
        for (server, config) in servers.as_object().into_iter().flatten() {
            let server = server.clone();
            let config = config.clone();
            let cancel = cancel.child_token();
            connections.push(async move {
                let opened = tokio::time::timeout(
                    Duration::from_secs(10),
                    McpConnection::connect(&server, &config, cwd, env, cancel.clone()),
                )
                .await;
                let client = match opened {
                    Ok(Ok(client)) => client,
                    Ok(Err(error)) => {
                        tracing::warn!(%server,%error,"OMP MCP server unavailable");
                        return None;
                    }
                    Err(_) => {
                        cancel.cancel();
                        tracing::warn!(%server,"OMP MCP server connection timed out");
                        return None;
                    }
                };
                let listed = tokio::time::timeout(
                    Duration::from_secs(10),
                    client.list_tools(cancel.clone()),
                )
                .await;
                match listed {
                    Ok(Ok(tools)) => Some((server, client, tools)),
                    _ => {
                        let _ = client.close().await;
                        tracing::warn!(%server,"OMP MCP server tool listing failed");
                        None
                    }
                }
            });
        }
        let mut tools = HashMap::new();
        let mut clients = Vec::new();
        while let Some(connection) = connections.next().await {
            let Some((server, client, listed)) = connection else {
                continue;
            };
            for tool in listed {
                let Some(name) = tool["name"].as_str() else {
                    continue;
                };
                let mapped = bridge_name(&server, name);
                definitions.push(json!({"name":mapped,"label":format!("{server} / {name}"),"description":tool["description"].as_str().filter(|s|!s.is_empty()).map(str::to_owned).unwrap_or_else(||format!("{name} from {server}")),"loadMode":"essential","parameters":tool["inputSchema"]}));
                tools.insert(
                    mapped,
                    BridgedTool {
                        client: client.clone(),
                        server: server.clone(),
                        name: name.into(),
                    },
                );
            }
            clients.push(client);
        }
        let router = Arc::new(Self {
            runtime,
            executor: context.tool_executor.clone(),
            tools,
            clients,
            pending: Mutex::new(HashMap::new()),
            closed: AtomicBool::new(false),
            cancel,
        });
        if !native.is_empty() || servers.as_object().is_some_and(|s| !s.is_empty()) {
            if let Err(error) = router
                .runtime
                .request(json!({"type":"set_host_tools","tools":definitions}))
                .await
            {
                router.close().await;
                return Err(error);
            }
        }
        Ok(router)
    }
    pub fn identities(&self) -> Value {
        json!(
            self.tools
                .iter()
                .map(|(name, tool)| json!([name,{"server":tool.server,"tool":tool.name}]))
                .collect::<Vec<_>>()
        )
    }
    pub fn handle(self: &Arc<Self>, frame: &Value) -> bool {
        match frame["type"].as_str() {
            Some("host_tool_call") => {
                let (Some(id), Some(name)) = (frame["id"].as_str(), frame["toolName"].as_str())
                else {
                    return true;
                };
                if !frame["toolCallId"].is_string() || !frame["arguments"].is_object() {
                    tracing::debug!("Dropped malformed OMP host tool call");
                    return true;
                }
                let (id, name, args) = (id.to_owned(), name.to_owned(), frame["arguments"].clone());
                if self.closed.load(Ordering::Acquire) {
                    return true;
                }
                let cancel = self.cancel.child_token();
                self.pending
                    .lock()
                    .unwrap()
                    .insert(id.clone(), cancel.clone());
                let router = self.clone();
                tokio::spawn(async move {
                    router.call(id, name, args, cancel).await;
                });
                true
            }
            Some("host_tool_cancel") => {
                if frame["id"].is_string() {
                    if let Some(id) = frame["targetId"].as_str() {
                        if let Some(cancel) = self.pending.lock().unwrap().get(id) {
                            cancel.cancel();
                        }
                    }
                }
                true
            }
            Some("host_tool_update") => true,
            _ => false,
        }
    }
    async fn call(
        self: Arc<Self>,
        id: String,
        name: String,
        args: Value,
        cancel: CancellationToken,
    ) {
        let (updates, mut receive) = mpsc::channel(32);
        let update_runtime = self.runtime.clone();
        let update_id = id.clone();
        let update_cancel = cancel.clone();
        let update_task = tokio::spawn(async move {
            loop {
                let update = tokio::select! {_ = update_cancel.cancelled()=>break,value=receive.recv()=>value};
                let Some(update) = update else {
                    break;
                };
                match map_result(update){Ok(result)=>{if update_runtime.send(&json!({"type":"host_tool_update","id":update_id,"partialResult":result})).await.is_err(){break;}},Err(error)=>tracing::warn!(%error,"Invalid native tool update")}
            }
        });
        let result = if let Some(tool) = self.tools.get(&name) {
            drop(updates);
            tool.client
                .call_tool(&tool.name, args, cancel.clone())
                .await
        } else if let Some(executor) = &self.executor {
            executor
                .execute_streaming(&name, args, cancel.clone(), Some(updates))
                .await
        } else {
            drop(updates);
            Ok(
                json!({"content":[{"type":"text","text":format!("Unknown host tool: {name}")}],"isError":true}),
            )
        };
        let _ = update_task.await;
        if !cancel.is_cancelled() && !self.closed.load(Ordering::Acquire) {
            let result=result.and_then(map_result).unwrap_or_else(|error|json!({"content":[{"type":"text","text":error.to_string()}],"details":{},"isError":true}));
            let mut frame = json!({"type":"host_tool_result","id":id,"result":result});
            if let Some(is_error) = frame["result"].get("isError").cloned() {
                frame["isError"] = is_error;
            }
            if let Err(error) = self.runtime.send(&frame).await {
                tracing::warn!(%error,"OMP host tool result delivery failed");
            }
        }
        self.pending.lock().unwrap().remove(&id);
    }
    pub async fn close(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        self.cancel.cancel();
        for (_, cancel) in self.pending.lock().unwrap().drain() {
            cancel.cancel();
        }
        for client in &self.clients {
            if let Err(error) = client.close().await {
                tracing::debug!(%error,"OMP MCP cleanup failed");
            }
        }
    }
}
fn map_result(result: Value) -> Result<Value> {
    let mut content = result["content"]
        .as_array()
        .cloned()
        .ok_or_else(|| anyhow!("Native tool result requires content"))?;
    if content.is_empty() {
        if let Some(structured) = result.get("structuredContent") {
            let mut summary = Vec::new();
            if let Some(object) = structured.as_object() {
                for (key, value) in object {
                    if let Some(values) = value.as_array() {
                        summary.push(format!("{key}_count={}", values.len()));
                        let ids = values
                            .iter()
                            .filter_map(|item| item["id"].as_str().filter(|s| !s.is_empty()))
                            .collect::<Vec<_>>();
                        if !ids.is_empty() && ids.len() == values.len() {
                            summary.push(format!("{key}_ids={}", ids.join(",")));
                        }
                    }
                }
            }
            let text = serde_json::to_string_pretty(structured)?;
            let text = if summary.is_empty() {
                text
            } else {
                format!("{}\n\n{text}", summary.join("\n"))
            };
            content.push(json!({"type":"text","text":text}));
        }
    }
    let mut mapped = json!({"content":content});
    if let Some(details) = result.get("structuredContent") {
        mapped["details"] = details.clone();
    }
    if let Some(error) = result.get("isError") {
        mapped["isError"] = error.clone();
    }
    Ok(mapped)
}
fn bridge_name(server: &str, tool: &str) -> String {
    let hash = hex::encode(Sha256::digest(format!("{server}\0{tool}")));
    let clean = |name: &str, length: usize| {
        name.encode_utf16()
            .take(length)
            .map(|c| {
                if (c < 128 && (c as u8).is_ascii_alphanumeric())
                    || c == b'_' as u16
                    || c == b'-' as u16
                {
                    char::from_u32(c as u32).unwrap()
                } else {
                    '_'
                }
            })
            .collect::<String>()
    };
    format!(
        "mcp_{}__{}_{}",
        clean(server, 20),
        clean(tool, 25),
        &hash[..12]
    )
}
