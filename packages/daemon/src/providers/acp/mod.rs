mod mapping;
pub(crate) mod rpc;
mod terminal;
#[cfg(test)]
mod tests;

use crate::providers::registry::catalog::refresh_activity;
use crate::{
    process::CommandSpec,
    providers::api::{
        AgentProvider, AgentSession, EventSink, ImportedSession, LaunchContext, ProviderSpec,
        RewindTarget,
    },
};
use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use mapping::*;
use rpc::{Rpc, RpcError};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use terminal::Terminal;
use tokio::{
    io::AsyncReadExt,
    sync::{Notify, Semaphore},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

fn id() -> String {
    Uuid::new_v4().to_string()
}
fn capabilities() -> Value {
    json!({"supportsStreaming":true,"supportsSessionPersistence":true,"supportsSessionListing":true,"supportsDynamicModes":true,"supportsMcpServers":true,"supportsReasoningStream":true,"supportsToolInvocations":true,"supportsRewindConversation":false,"supportsRewindFiles":false,"supportsRewindBoth":false})
}
fn merge(target: &mut Value, source: &Value) {
    if let (Some(target), Some(source)) = (target.as_object_mut(), source.as_object()) {
        for (key, value) in source {
            target.insert(key.clone(), value.clone());
        }
    }
}

pub struct AcpProvider {
    spec: ProviderSpec,
    capabilities: Value,
    sessions: Mutex<Vec<Weak<Inner>>>,
}
pub struct AcpSession {
    inner: Arc<Inner>,
    capabilities: Value,
    initial: Vec<Value>,
}
struct Permission {
    rpc_id: Value,
    options: Value,
    request: Value,
    turn: Option<String>,
}
struct State {
    id: Option<String>,
    config: Value,
    native: Value,
    persistence: Value,
    mode: Option<String>,
    model: Option<String>,
    thinking: Option<String>,
    title: Value,
    updated: Value,
    turn: Option<String>,
    assistant: Option<String>,
    pending_user: Option<Value>,
    submitted: bool,
    history: Vec<Value>,
    replay: bool,
    early: Vec<Value>,
    tools: HashMap<String, Value>,
    permissions: HashMap<String, Permission>,
    terminals: HashMap<String, Arc<Terminal>>,
    commands: Vec<Value>,
    commands_ready: bool,
    usage: Value,
    closed: bool,
    released: bool,
    features: Arc<[Value]>,
    opening: bool,
    provisional_id: Option<String>,
}
struct Inner {
    spec: ProviderSpec,
    rpc: Arc<Rpc>,
    sink: EventSink,
    state: Mutex<State>,
    agent_capabilities: Mutex<Value>,
    commands_changed: Notify,
    env: BTreeMap<String, String>,
    probe: bool,
    reverse_limit: Arc<Semaphore>,
    mutation: tokio::sync::Mutex<()>,
    usage_key: String,
    finishing: tokio::sync::Mutex<()>,
    configured_models: Mutex<HashSet<String>>,
}
impl Inner {
    fn session_id(&self) -> Result<String> {
        self.state
            .lock()
            .unwrap()
            .id
            .clone()
            .ok_or_else(|| anyhow!("ACP session is not initialized"))
    }
    fn runtime(&self) -> Value {
        let s = self.state.lock().unwrap();
        json!({"provider":self.spec.id,"sessionId":s.id,"model":s.model,"thinkingOptionId":s.thinking,"modeId":s.mode,"extra":{"title":s.title,"updatedAt":s.updated}})
    }
    fn state_response(&self, response: &Value) {
        let mut s = self.state.lock().unwrap();
        let features_changed = response
            .get("configOptions")
            .is_some_and(|options| options != &s.native["configOptions"]);
        for key in ["modes", "models", "configOptions"] {
            if let Some(v) = response.get(key) {
                s.native[key] = v.clone();
            }
        }
        let (_, mode) = modes(&self.spec.family, &s.native);
        if mode.is_some() {
            s.mode = mode;
        }
        s.model = s.native["models"]["currentModelId"]
            .as_str()
            .map(str::to_owned)
            .or_else(|| {
                option(&s.native["configOptions"], "model")
                    .and_then(|o| o["currentValue"].as_str())
                    .map(str::to_owned)
            })
            .or_else(|| s.model.clone());
        s.thinking = option(&s.native["configOptions"], "thought_level")
            .and_then(|o| o["currentValue"].as_str())
            .map(str::to_owned)
            .or_else(|| s.thinking.clone());
        if features_changed {
            s.features = features(&self.spec.family, &s.native, &s.config).into();
        }
    }
    async fn emit(&self, mut event: Value) -> Result<()> {
        event["provider"] = json!(self.spec.id);
        let (replay, turn) = {
            let s = self.state.lock().unwrap();
            (s.replay, s.turn.clone())
        };
        if matches!(event["type"].as_str(), Some("timeline" | "usage_updated"))
            && event.get("turnId").is_none()
        {
            if let Some(turn) = turn {
                event["turnId"] = json!(turn);
            }
        }
        if replay {
            if event["type"] == "timeline" {
                self.state.lock().unwrap().history.push(event);
            }
            return Ok(());
        }
        self.sink.emit(event).await
    }
    async fn timeline(&self, item: Value) -> Result<()> {
        self.emit(json!({"type":"timeline","item":item})).await
    }
    async fn flush_user(&self) -> Result<()> {
        let pending = self.state.lock().unwrap().pending_user.take();
        if let Some(item) = pending {
            self.timeline(item).await?;
        }
        Ok(())
    }
    async fn cancel_permissions(&self) -> Result<()> {
        let pending: Vec<Permission> = self
            .state
            .lock()
            .unwrap()
            .permissions
            .drain()
            .map(|(_, v)| v)
            .collect();
        let mut failure = None;
        for p in pending {
            if let Err(error) = self
                .rpc
                .reply(p.rpc_id, Ok(json!({"outcome":{"outcome":"cancelled"}})))
                .await
            {
                failure = Some(error);
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
        Ok(())
    }
    async fn finish(&self, turn: &str, result: Result<Value>) -> Result<()> {
        let _finishing = self.finishing.lock().await;
        if self.state.lock().unwrap().turn.as_deref() != Some(turn) {
            return Ok(());
        }
        self.flush_user().await?;
        let (mut event, canceled) = match result {
            Ok(response) => {
                if let Some(usage) = response.get("usage") {
                    self.state.lock().unwrap().usage = mapping::usage(usage);
                }
                if response["stopReason"] == "cancelled" {
                    (
                        json!({"type":"turn_canceled","reason":"Interrupted","turnId":turn}),
                        true,
                    )
                } else {
                    (json!({"type":"turn_completed","turnId":turn}), false)
                }
            }
            Err(error) => {
                let mut e = json!({"type":"turn_failed","error":error.to_string(),"turnId":turn,"diagnostic":self.rpc.process.stderr_tail()});
                if let Some(error) = error.downcast_ref::<RpcError>() {
                    e["code"] = json!(error.code.to_string());
                    e["diagnostic"] = json!(format!(
                        "{} | code={} | data={}",
                        error.message, error.code, error.data
                    ));
                }
                (e, true)
            }
        };
        if canceled {
            let tools: Vec<Value> = self
                .state
                .lock()
                .unwrap()
                .tools
                .values()
                .filter(|s| s["status"] != "completed" && s["status"] != "failed")
                .cloned()
                .collect();
            for tool in tools {
                let mut item = tool_item(&tool, self.terminal_snapshot(&tool));
                item["status"] = json!("canceled");
                item["error"] = Value::Null;
                self.timeline(item).await?;
            }
            let _ = self.cancel_permissions().await;
        } else {
            let usage = self.state.lock().unwrap().usage.clone();
            if !usage.as_object().is_none_or(|o| o.is_empty()) {
                event["usage"] = usage;
            }
        }
        {
            let mut s = self.state.lock().unwrap();
            s.turn = None;
            s.assistant = None;
            s.submitted = false;
        }
        self.emit(event).await
    }
    fn terminal_snapshot(&self, snapshot: &Value) -> Option<Value> {
        let terminal = array(&snapshot["content"])
            .iter()
            .find(|c| c["type"] == "terminal")?;
        self.state
            .lock()
            .unwrap()
            .terminals
            .get(text(&terminal["terminalId"]))
            .map(|t| t.output())
    }
    async fn notification(&self, method: &str, params: Value) -> Result<()> {
        if method != "session/update" {
            if self.spec.family == "kiro" && method == "_kiro.dev/commands/available" {
                let mut s = self.state.lock().unwrap();
                if params["sessionId"]
                    .as_str()
                    .is_some_and(|id| s.id.as_deref().is_some_and(|ours| ours != id))
                {
                    return Ok(());
                }
                let commands = mapping::commands(&params, true);
                if !commands.is_empty() {
                    s.commands = commands;
                }
                s.commands_ready = true;
                drop(s);
                self.commands_changed.notify_waiters();
            }
            return Ok(());
        }
        {
            let mut s = self.state.lock().unwrap();
            if s.id.is_none() {
                s.early.push(params);
                return Ok(());
            }
            if params["sessionId"].as_str() != s.id.as_deref() {
                return Ok(());
            }
        }
        let update = &params["update"];
        let kind = text(&update["sessionUpdate"]);
        if kind == "user_message_chunk" {
            let mut flush = None;
            {
                let mut s = self.state.lock().unwrap();
                s.assistant = None;
                if s.turn.is_some() && s.submitted {
                    return Ok(());
                }
                let text = content_text(&update["content"]);
                if text.is_empty() {
                    return Ok(());
                }
                if s.pending_user.as_ref().is_some_and(|p| {
                    p["messageId"].as_str().is_some_and(|id| !id.is_empty())
                        && update["messageId"]
                            .as_str()
                            .is_some_and(|id| !id.is_empty())
                        && p["messageId"] != update["messageId"]
                }) {
                    flush = s.pending_user.take();
                }
                if let Some(pending) = s.pending_user.as_mut() {
                    if pending.get("messageId").is_none() {
                        if let Some(id) = update["messageId"].as_str().filter(|id| !id.is_empty()) {
                            pending["messageId"] = json!(id);
                        }
                    }
                    let Value::String(combined) = &mut pending["text"] else {
                        unreachable!("ACP user message text invariant")
                    };
                    combined.push_str(&text);
                } else {
                    let mut item = json!({"type":"user_message","text":text});
                    if let Some(id) = update["messageId"].as_str().filter(|id| !id.is_empty()) {
                        item["messageId"] = json!(id);
                    }
                    s.pending_user = Some(item);
                }
            }
            if let Some(item) = flush {
                self.timeline(item).await?;
            }
            return Ok(());
        }
        self.flush_user().await?;
        match kind {
            "agent_message_chunk" | "agent_thought_chunk" => {
                let content = content_text(&update["content"]);
                if content.is_empty() {
                    return Ok(());
                }
                let mut item = json!({"type":if kind=="agent_message_chunk"{"assistant_message"}else{"reasoning"},"text":content});
                {
                    let mut s = self.state.lock().unwrap();
                    if kind == "agent_message_chunk" {
                        let message = if let Some(id) =
                            update["messageId"].as_str().filter(|id| !id.is_empty())
                        {
                            s.assistant = None;
                            id.to_owned()
                        } else {
                            s.assistant.get_or_insert_with(id).clone()
                        };
                        item["messageId"] = json!(message);
                    } else {
                        s.assistant = None;
                    }
                }
                let mut event = json!({"type":"timeline","item":item});
                if let Some(timestamp) = update.get("timestamp") {
                    event["timestamp"] = timestamp.clone();
                }
                self.emit(event).await?;
            }
            "tool_call" | "tool_call_update" => {
                let snapshot = {
                    let mut s = self.state.lock().unwrap();
                    if kind == "tool_call" {
                        s.assistant = None;
                    }
                    let key = text(&update["toolCallId"]).to_owned();
                    let tool = s.tools.entry(key).or_insert_with(
                        || json!({"title":"Tool","status":"pending","content":[],"locations":[]}),
                    );
                    merge(tool, update);
                    tool.clone()
                };
                self.timeline(tool_item(&snapshot, self.terminal_snapshot(&snapshot)))
                    .await?;
            }
            "plan" => {
                self.state.lock().unwrap().assistant = None;
                self.timeline(json!({"type":"todo","items":array(&update["entries"]).iter().map(|e|json!({"text":e["content"],"completed":e["status"]=="completed"})).collect::<Vec<_>>()})).await?;
            }
            "current_mode_update" => {
                let mode = text(&update["currentModeId"]);
                self.state.lock().unwrap().mode = Some(
                    if self.spec.family == "copilot" && mode == AUTOPILOT {
                        AGENT
                    } else {
                        mode
                    }
                    .to_owned(),
                );
                self.emit_mode().await?;
            }
            "config_option_update" => {
                {
                    let mut s = self.state.lock().unwrap();
                    s.native.as_object_mut().unwrap().remove("modes");
                    if let Some(model) = option(&update["configOptions"], "model")
                        .and_then(|o| o.get("currentValue"))
                    {
                        if !s.native["models"].is_object() {
                            s.native["models"] = json!({});
                        }
                        s.native["models"]["currentModelId"] = model.clone();
                    }
                }
                self.state_response(update);
                let (mode, model, thinking) = {
                    let s = self.state.lock().unwrap();
                    (
                        option(&s.native["configOptions"], "mode").is_some(),
                        option(&s.native["configOptions"], "model").is_some(),
                        option(&s.native["configOptions"], "thought_level").is_some(),
                    )
                };
                if mode {
                    self.emit_mode().await?;
                }
                if model {
                    self.emit(json!({"type":"model_changed","runtimeInfo":self.runtime()}))
                        .await?;
                }
                if thinking {
                    let value = self.state.lock().unwrap().thinking.clone();
                    self.emit(json!({"type":"thinking_option_changed","thinkingOptionId":value}))
                        .await?;
                }
            }
            "session_info_update" => {
                let mut s = self.state.lock().unwrap();
                if let Some(title) = update.get("title") {
                    s.title = title.clone();
                }
                if let Some(updated) = update.get("updatedAt") {
                    s.updated = updated.clone();
                }
            }
            "usage_update" => {
                if update["size"]
                    .as_f64()
                    .is_some_and(|v| v.is_finite() && v > 0.0)
                    && update["used"]
                        .as_f64()
                        .is_some_and(|v| v.is_finite() && v >= 0.0)
                {
                    let mut usage = self.state.lock().unwrap().usage.clone();
                    usage["contextWindowMaxTokens"] = update["size"].clone();
                    usage["contextWindowUsedTokens"] = update["used"].clone();
                    self.emit(json!({"type":"usage_updated","usage":usage}))
                        .await?;
                }
            }
            "available_commands_update" => {
                let mut s = self.state.lock().unwrap();
                s.commands = mapping::commands(update, false);
                s.commands_ready = true;
                drop(s);
                self.commands_changed.notify_waiters();
            }
            _ => {}
        }
        Ok(())
    }
    async fn emit_mode(&self) -> Result<()> {
        let (mode, available) = {
            let s = self.state.lock().unwrap();
            (s.mode.clone(), modes(&self.spec.family, &s.native).0)
        };
        self.emit(json!({"type":"mode_changed","currentModeId":mode,"availableModes":available}))
            .await
    }
    async fn permission(&self, rpc_id: Value, params: Value) -> Result<()> {
        if self.probe {
            return self
                .rpc
                .reply(rpc_id, Ok(json!({"outcome":{"outcome":"cancelled"}})))
                .await;
        }
        self.validate_owner(&params)?;
        let auto = self.state.lock().unwrap().config["featureValues"]["auto_accept"] == true;
        if auto && !chooser(&params["options"]) {
            if let Some(option) =
                permission_option(&params["options"], &json!({"behavior":"allow"}))?
            {
                return self
                    .rpc
                    .reply(
                        rpc_id,
                        Ok(json!({"outcome":{"outcome":"selected","optionId":option["optionId"]}})),
                    )
                    .await;
            }
        }
        let request_id = id();
        let snapshot = {
            let s = self.state.lock().unwrap();
            s.tools
                .get(text(&params["toolCall"]["toolCallId"]))
                .cloned()
                .unwrap_or_else(|| params["toolCall"].clone())
        };
        let request = mapping::permission(&self.spec.id, &request_id, &params, &snapshot);
        let turn = self.state.lock().unwrap().turn.clone();
        self.state.lock().unwrap().permissions.insert(
            request_id,
            Permission {
                rpc_id,
                options: params["options"].clone(),
                request: request.clone(),
                turn: turn.clone(),
            },
        );
        let mut event = json!({"type":"permission_requested","request":request});
        if let Some(turn) = turn {
            event["turnId"] = json!(turn);
        }
        self.emit(event).await
    }
    fn validate_owner(&self, params: &Value) -> Result<()> {
        let mut s = self.state.lock().unwrap();
        if s.closed || s.released {
            bail!("ACP session owner is closed");
        }
        let incoming = params["sessionId"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| anyhow!("Missing ACP request sessionId"))?;
        if let Some(session) = &s.id {
            if session != incoming {
                bail!("ACP reverse request belongs to another session");
            }
        } else if s.opening {
            if s.provisional_id.as_deref().is_some_and(|id| id != incoming) {
                bail!("ACP reverse request belongs to another provisional session");
            }
            s.provisional_id = Some(incoming.to_owned());
        } else {
            bail!("ACP reverse request has no initialized session owner");
        }
        Ok(())
    }
    async fn reverse(&self, method: &str, params: Value) -> Result<Value> {
        self.validate_owner(&params)?;
        match method {
            "fs/read_text_file" => {
                let path = params["path"]
                    .as_str()
                    .ok_or_else(|| anyhow!("Missing ACP file path"))?;
                let file = tokio::fs::File::open(path).await?;
                let mut bytes = Vec::new();
                file.take(32 * 1024 * 1024 + 1)
                    .read_to_end(&mut bytes)
                    .await?;
                if bytes.len() > 32 * 1024 * 1024 {
                    bail!("ACP file read exceeds 32 MiB output bound");
                }
                let raw = String::from_utf8_lossy(&bytes);
                let content = if params.get("line").is_none() && params.get("limit").is_none() {
                    raw.into_owned()
                } else {
                    let start = params["line"]
                        .as_u64()
                        .unwrap_or(1)
                        .saturating_sub(1)
                        .min(usize::MAX as u64) as usize;
                    let limit = params["limit"]
                        .as_u64()
                        .filter(|v| *v > 0)
                        .unwrap_or(usize::MAX as u64)
                        .min(usize::MAX as u64) as usize;
                    raw.split('\n')
                        .skip(start)
                        .take(limit)
                        .map(|s| s.strip_suffix('\r').unwrap_or(s))
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                Ok(json!({"content":content}))
            }
            "fs/write_text_file" => {
                let path = PathBuf::from(
                    params["path"]
                        .as_str()
                        .ok_or_else(|| anyhow!("Missing ACP file path"))?,
                );
                let content = params["content"]
                    .as_str()
                    .ok_or_else(|| anyhow!("Missing ACP file content"))?;
                if let Some(parent) = path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                tokio::fs::write(path, content).await?;
                Ok(json!({}))
            }
            "terminal/create" => {
                if self.probe {
                    bail!("ACP model probe does not support terminal execution");
                }
                let cwd = text(&self.state.lock().unwrap().config["cwd"]).to_owned();
                let terminal = Terminal::spawn(&params, &cwd, self.env.clone()).await?;
                if let Err(error) = self.validate_owner(&params) {
                    let _ = terminal.kill().await;
                    return Err(error);
                }
                let terminal_id = id();
                self.state
                    .lock()
                    .unwrap()
                    .terminals
                    .insert(terminal_id.clone(), terminal);
                Ok(json!({"terminalId":terminal_id}))
            }
            "terminal/output" | "terminal/wait_for_exit" | "terminal/kill" | "terminal/release" => {
                let terminal_id = text(&params["terminalId"]);
                let terminal = self
                    .state
                    .lock()
                    .unwrap()
                    .terminals
                    .get(terminal_id)
                    .cloned()
                    .ok_or_else(|| anyhow!("Unknown ACP terminal '{terminal_id}'"))?;
                match method {
                    "terminal/output" => Ok(terminal.output()),
                    "terminal/wait_for_exit" => terminal.wait().await,
                    "terminal/kill" => {
                        terminal.kill().await?;
                        Ok(json!({}))
                    }
                    _ => {
                        terminal.kill().await?;
                        self.state.lock().unwrap().terminals.remove(terminal_id);
                        Ok(json!({}))
                    }
                }
            }
            _ => Err(RpcError {
                code: -32601,
                message: format!("Unsupported ACP client method {method}"),
                data: Value::Null,
            }
            .into()),
        }
    }
    async fn close(&self) -> Result<()> {
        let (session, active, released) = {
            let mut s = self.state.lock().unwrap();
            if s.closed {
                return Ok(());
            }
            s.closed = true;
            (s.id.clone(), s.turn.is_some(), s.released)
        };
        self.commands_changed.notify_waiters();
        if tokio::time::timeout(Duration::from_secs(2), self.cancel_permissions())
            .await
            .is_err()
        {
            self.rpc.cancel.cancel();
        }
        if !released && !self.rpc.cancel.is_cancelled() {
            if let Some(session) = session {
                if active {
                    let _ = tokio::time::timeout(
                        Duration::from_secs(2),
                        self.rpc
                            .notify("session/cancel", json!({"sessionId":session})),
                    )
                    .await;
                }
                let supported = self.agent_capabilities.lock().unwrap()["sessionCapabilities"]
                    .get("close")
                    .is_some_and(|v| !v.is_null() && v != false);
                if supported {
                    let _ = self
                        .rpc
                        .request(
                            "session/close",
                            json!({"sessionId":session}),
                            Some(Duration::from_secs(2)),
                        )
                        .await;
                }
            }
        }
        let terminals: Vec<_> = self
            .state
            .lock()
            .unwrap()
            .terminals
            .drain()
            .map(|(_, v)| v)
            .collect();
        let mut error = None;
        for terminal in terminals {
            if let Err(e) = terminal.kill().await {
                error = Some(e);
            }
        }
        if let Err(e) = self.rpc.close().await {
            error = Some(e);
        }
        if let Some(error) = error {
            return Err(error);
        }
        Ok(())
    }
    async fn write_config(
        &self,
        config_id: &str,
        value: &str,
        category: Option<&str>,
    ) -> Result<String> {
        let response = self
            .rpc
            .call(
                "session/set_config_option",
                json!({"sessionId":self.session_id()?,"configId":config_id,"value":value}),
            )
            .await?;
        self.state_response(&response);
        let s = self.state.lock().unwrap();
        Ok(option_id(&s.native["configOptions"], config_id)
            .or_else(|| category.and_then(|c| option(&s.native["configOptions"], c)))
            .and_then(|o| o["currentValue"].as_str())
            .unwrap_or(value)
            .to_owned())
    }
}
impl Drop for AcpSession {
    fn drop(&mut self) {
        self.inner.rpc.cancel.cancel();
    }
}

impl AcpProvider {
    pub fn new(spec: ProviderSpec) -> Self {
        Self {
            spec,
            capabilities: capabilities(),
            sessions: Mutex::new(Vec::new()),
        }
    }
    fn probe_context(&self, cancel: CancellationToken) -> LaunchContext {
        LaunchContext {
            agent_id: id(),
            home: dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")),
            executable: std::env::current_exe().unwrap_or_default(),
            env: BTreeMap::from([("NO_BROWSER".into(), "true".into())]),
            mcp_servers: json!({}),
            tool_catalog: json!([]),
            tool_manifest: json!([]),
            tool_executor: None,
            cancel,
        }
    }
    async fn spawn(
        &self,
        config: Value,
        context: &LaunchContext,
        persistence: Value,
        history: bool,
        probe: bool,
        sink: EventSink,
    ) -> Result<Arc<Inner>> {
        let catalog = context
            .tool_catalog
            .as_array()
            .or_else(|| context.tool_catalog["tools"].as_array());
        if catalog.is_some_and(|tools| !tools.is_empty()) && context.tool_executor.is_none() {
            bail!("ACP native tool catalog requires an authority-bound executor");
        }
        let (program, args) = self
            .spec
            .command
            .split_first()
            .ok_or_else(|| anyhow!("Empty ACP executable prefix"))?;
        let mut args = args.to_vec();
        if self.spec.family == "copilot" {
            args.push("--acp".into());
        }
        let mut env = self.spec.env.clone();
        env.extend(context.env.clone());
        let cwd = config["cwd"]
            .as_str()
            .ok_or_else(|| anyhow!("ACP requires a working directory"))?;
        let process_cwd = if history && !tokio::fs::metadata(cwd).await.is_ok_and(|m| m.is_dir()) {
            context.home.clone()
        } else {
            PathBuf::from(cwd)
        };
        let (rpc, mut messages) = Rpc::spawn(
            CommandSpec {
                program: program.clone(),
                args,
                cwd: Some(process_cwd),
                env: env.clone(),
                clear_env: false,
            },
            context.cancel.clone(),
        )
        .await?;
        let inner = Arc::new(Inner {
            spec: self.spec.clone(),
            rpc: rpc.clone(),
            sink,
            state: Mutex::new(State {
                id: persistence["sessionId"].as_str().map(str::to_owned),
                mode: config["modeId"].as_str().map(str::to_owned),
                model: config["model"].as_str().map(str::to_owned),
                thinking: config["thinkingOptionId"].as_str().map(str::to_owned),
                title: config["title"].clone(),
                updated: Value::Null,
                features: features(&self.spec.family, &json!({}), &config).into(),
                opening: false,
                provisional_id: None,
                config,
                native: json!({}),
                persistence,
                turn: None,
                assistant: None,
                pending_user: None,
                submitted: false,
                history: Vec::new(),
                replay: history,
                early: Vec::new(),
                tools: HashMap::new(),
                permissions: HashMap::new(),
                terminals: HashMap::new(),
                commands: Vec::new(),
                commands_ready: false,
                usage: json!({}),
                closed: false,
                released: false,
            }),
            agent_capabilities: Mutex::new(json!({})),
            commands_changed: Notify::new(),
            env,
            probe,
            reverse_limit: Arc::new(Semaphore::new(64)),
            mutation: tokio::sync::Mutex::new(()),
            usage_key: id(),
            finishing: tokio::sync::Mutex::new(()),
            configured_models: Mutex::new(HashSet::new()),
        });
        {
            let mut sessions = self.sessions.lock().unwrap();
            sessions.retain(|session| session.strong_count() > 0);
            sessions.push(Arc::downgrade(&inner));
        }
        let receiver = inner.clone();
        tokio::spawn(async move {
            loop {
                let incoming = tokio::select! {_ = rpc.cancel.cancelled()=>None, message=messages.recv()=>message};
                let Some((sequence, message)) = incoming else {
                    break;
                };
                let method = text(&message["method"]).to_owned();
                let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
                let result = if let Some(request_id) = message.get("id") {
                    if method == "session/request_permission" {
                        match receiver.permission(request_id.clone(), params).await {
                            Ok(()) => Ok(()),
                            Err(error) => rpc.reply(request_id.clone(), Err(error)).await,
                        }
                    } else {
                        match receiver.reverse_limit.clone().try_acquire_owned() {
                            Ok(permit) => {
                                let handler = receiver.clone();
                                let request_id = request_id.clone();
                                tokio::spawn(async move {
                                    let _permit = permit;
                                    let result = tokio::select! {
                                        _=handler.rpc.cancel.cancelled()=>Err(anyhow!("ACP reverse request owner closed")),
                                        result=handler.reverse(&method,params)=>result,
                                    };
                                    if handler.rpc.reply(request_id, result).await.is_err() {
                                        handler.rpc.cancel.cancel();
                                    }
                                });
                                Ok(())
                            }
                            Err(_) => {
                                rpc.reply(
                                    request_id.clone(),
                                    Err(anyhow!("ACP reverse request concurrency limit exceeded")),
                                )
                                .await
                            }
                        }
                    }
                } else {
                    receiver.notification(&method, params).await
                };
                rpc.acknowledge(sequence);
                if let Err(error) = result {
                    let turn = receiver.state.lock().unwrap().turn.clone();
                    if let Some(turn) = turn {
                        let _ = receiver.finish(&turn, Err(error)).await;
                    }
                    rpc.cancel.cancel();
                    break;
                }
            }
            let turn = {
                let s = receiver.state.lock().unwrap();
                if s.closed || s.released {
                    None
                } else {
                    s.turn.clone()
                }
            };
            if let Some(turn) = turn {
                let _ = receiver
                    .finish(
                        &turn,
                        Err(anyhow!(
                            "ACP provider connection closed: {}",
                            rpc.process.stderr_tail()
                        )),
                    )
                    .await;
            }
            let terminals: Vec<_> = receiver
                .state
                .lock()
                .unwrap()
                .terminals
                .drain()
                .map(|(_, terminal)| terminal)
                .collect();
            for terminal in terminals {
                let _ = terminal.kill().await;
            }
            receiver.commands_changed.notify_waiters();
        });
        let mut client = json!({"fs":{"readTextFile":false,"writeTextFile":false},"terminal":true});
        let configured = {
            let s = inner.state.lock().unwrap();
            s.config["providerOptions"]["clientCapabilities"].clone()
        };
        let configured = if configured.is_object() {
            configured
        } else {
            self.spec.options["clientCapabilities"].clone()
        };
        let fs = configured["fs"].clone();
        merge(&mut client, &configured);
        if fs.is_object() {
            client["fs"] = json!({"readTextFile":false,"writeTextFile":false});
            merge(&mut client["fs"], &fs);
        }
        if self.spec.family == "cursor" {
            if !client["_meta"].is_object() {
                client["_meta"] = json!({});
            }
            client["_meta"]["parameterizedModelPicker"] = json!(true);
        }
        match inner.rpc.call("initialize",json!({"protocolVersion":1,"clientCapabilities":client,"clientInfo":{"name":"Paseo","version":env!("CARGO_PKG_VERSION")}})).await {
            Ok(response)=>{*inner.agent_capabilities.lock().unwrap()=response.get("agentCapabilities").cloned().unwrap_or_else(||json!({}));Ok(inner)},
            Err(error)=>{let _=inner.close().await;Err(error)},
        }
    }
    async fn open(
        &self,
        config: Value,
        context: LaunchContext,
        persistence: Value,
        options: Value,
        sink: EventSink,
        probe: bool,
    ) -> Result<Arc<AcpSession>> {
        let history = options["purpose"] == "history";
        let resumed = persistence["sessionId"].is_string();
        let initialize = self.spawn(config.clone(), &context, persistence, history, probe, sink);
        let inner = if probe {
            refresh_activity("initialize", initialize).await?
        } else {
            initialize.await?
        };
        inner.configured_models.lock().unwrap().extend(
            array(&options["configuredModelIds"])
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned),
        );
        let result:Result<Arc<AcpSession>>=async{
            let supports_mcp=config["providerOptions"]["supportsMcpServers"].as_bool().or_else(||self.spec.options["supportsMcpServers"].as_bool()).unwrap_or(true);
            let mut servers=config.get("mcpServers").cloned().unwrap_or_else(||json!({}));merge(&mut servers,&context.mcp_servers);
            let mut params=json!({"cwd":config["cwd"],"mcpServers":if supports_mcp{mcp(&servers)}else{Vec::new()}});
            let method=if resumed {
                params["sessionId"]=json!(inner.session_id()?);
                let capabilities=inner.agent_capabilities.lock().unwrap();
                if capabilities["loadSession"]==true{"session/load"}else if capabilities["sessionCapabilities"].get("resume").is_some_and(|v|!v.is_null()&&v!=false){"session/resume"}else{bail!("{} does not support ACP session resume",self.spec.id);}
            }else{"session/new"};
            {let mut s=inner.state.lock().unwrap();s.opening=!resumed;s.replay=method=="session/load";}
            let request=inner.rpc.call(method,params);
            let response=if probe&&!resumed{refresh_activity("session/new",request).await?}else{request.await?};
            if !resumed {
                let session=response["sessionId"].as_str().filter(|id|!id.is_empty()).ok_or_else(||anyhow!("ACP session/new omitted sessionId"))?;
                let mut s=inner.state.lock().unwrap();
                if s.provisional_id.as_deref().is_some_and(|id|id!=session){bail!("ACP session/new changed the reverse request session owner");}
                s.id=Some(session.into());s.opening=false;s.provisional_id=None;
            }
            inner.state_response(&response);
            let early=std::mem::take(&mut inner.state.lock().unwrap().early);
            for update in early{inner.notification("session/update",update).await?;}
            inner.flush_user().await?;
            inner.state.lock().unwrap().replay=false;
            let mut caps=self.capabilities.clone();caps["supportsMcpServers"]=json!(supports_mcp);
            let session=AcpSession{inner:inner.clone(),capabilities:caps,initial:Vec::new()};
            if !probe&&!history{
                if let Some(mode)=config["modeId"].as_str(){if inner.state.lock().unwrap().mode.as_deref()!=Some(mode){session.set_mode(mode).await?;}}
                let mut switched=false;
                if let Some(model)=config["model"].as_str(){
                    if inner.state.lock().unwrap().model.as_deref()!=Some(model){
                        match session.set_model(Some(model)).await{
                            Ok(())=>switched=true,
                            Err(error) if error.to_string()==format!("{} does not expose ACP model selection",self.spec.id)=>tracing::warn!(provider=%self.spec.id,"ACP model selection unavailable; retaining provider default"),
                            Err(error)=>return Err(error),
                        }
                    }
                }
                if let Some(thinking)=config["thinkingOptionId"].as_str(){if inner.state.lock().unwrap().thinking.as_deref()!=Some(thinking){session.set_thinking_option(Some(thinking)).await?;}}
                if let Some(values)=config["featureValues"].as_object(){for(key,value)in values{
                    if key=="auto_accept"||self.spec.family=="cursor"&&key=="fast"||self.spec.family=="copilot"&&key=="agent"{
                        if let Err(error)=session.set_feature(key,value.clone()).await {
                            if error.to_string().contains("does not expose ACP feature")||switched&&error.downcast_ref::<RpcError>().is_some_and(|e|e.code==-32602){tracing::warn!(provider=%self.spec.id,feature=%key,error=%error,"ACP stale feature preference not applicable to selected model");}else{return Err(error);}
                        }
                    }
                }}
            }
            if history{
                inner.state.lock().unwrap().released=true;
                inner.rpc.close().await?;
            }else if !probe{inner.emit(json!({"type":"thread_started","sessionId":inner.session_id()?})).await?;}
            Ok(Arc::new(session))
        }.await;
        if result.is_err() {
            let _ = inner.close().await;
        }
        result
    }
    async fn probe(&self, options: Value, cancel: CancellationToken) -> Result<Arc<AcpSession>> {
        let context = self.probe_context(cancel.clone());
        let cwd = if options["scope"] == "global" {
            context.home.to_string_lossy().into_owned()
        } else {
            options["cwd"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| context.home.to_string_lossy().into_owned())
        };
        let (sink, mut rx) = EventSink::channel(64, cancel);
        tokio::spawn(async move { while rx.recv().await.is_some() {} });
        self.open(
            json!({"provider":self.spec.id,"cwd":cwd,"providerOptions":options["providerOptions"]}),
            context,
            Value::Null,
            json!({}),
            sink,
            true,
        )
        .await
    }
}

#[async_trait]
impl AgentSession for AcpSession {
    fn provider(&self) -> &str {
        &self.inner.spec.id
    }
    fn native_id(&self) -> Option<String> {
        self.inner.state.lock().unwrap().id.clone()
    }
    fn capabilities(&self) -> &Value {
        &self.capabilities
    }
    fn features(&self) -> Arc<[Value]> {
        self.inner.state.lock().unwrap().features.clone()
    }
    fn initial_timeline(&self) -> &[Value] {
        &self.initial
    }
    fn describe_persistence(&self) -> Option<Value> {
        let s = self.inner.state.lock().unwrap();
        let session = s.id.as_ref()?;
        let mut handle = if s.persistence.is_object() {
            s.persistence.clone()
        } else {
            json!({})
        };
        handle["provider"] = json!(self.provider());
        handle["sessionId"] = json!(session);
        if handle.get("nativeHandle").is_none() {
            handle["nativeHandle"] = json!(session);
        }
        let mut metadata = handle
            .get("metadata")
            .filter(|v| v.is_object())
            .cloned()
            .unwrap_or_else(|| json!({}));
        let mut config = s.config.clone();
        if let Some(object) = config.as_object_mut() {
            object.remove("providerOptions");
            object.remove("mcpServers");
        }
        merge(&mut metadata, &config);
        metadata["title"] = s.title.clone();
        handle["metadata"] = metadata;
        Some(handle)
    }
    fn pending_permissions(&self) -> Vec<Value> {
        self.inner
            .state
            .lock()
            .unwrap()
            .permissions
            .values()
            .map(|p| p.request.clone())
            .collect()
    }
    fn usage_session(&self) -> Option<Value> {
        let s = self.inner.state.lock().unwrap();
        if s.closed || s.released || self.inner.spec.family != "cursor" {
            return None;
        }
        Some(
            json!({"provider":"cursor","model":s.model,"env":self.inner.env,"sessionKey":self.inner.usage_key}),
        )
    }
    async fn start_turn(&self, prompt: Value, options: Value) -> Result<Value> {
        let _mutation = self.inner.mutation.lock().await;
        let _foreground = self.inner.finishing.lock().await;
        let blocks = crate::providers::prompt::to_acp_blocks(&prompt)?;
        let submitted = crate::providers::prompt::text(&prompt)?;
        self.inner.flush_user().await?;
        let turn = id();
        let message_id = options["clientMessageId"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(id);
        let session = {
            let mut s = self.inner.state.lock().unwrap();
            if s.closed || s.released || self.inner.rpc.cancel.is_cancelled() {
                bail!("{} session is closed", self.provider());
            }
            if s.turn.is_some() {
                bail!("A foreground turn is already active");
            }
            let session =
                s.id.clone()
                    .ok_or_else(|| anyhow!("ACP session is not initialized"))?;
            s.turn = Some(turn.clone());
            s.assistant = None;
            s.submitted = !submitted.trim().is_empty();
            session
        };
        if let Err(error) = self
            .inner
            .emit(json!({"type":"turn_started","turnId":turn}))
            .await
        {
            self.inner.state.lock().unwrap().turn = None;
            return Err(error);
        }
        if !submitted.trim().is_empty() {
            let mut item = json!({"type":"user_message","text":submitted,"messageId":message_id});
            if let Some(client) = options.get("clientMessageId") {
                item["clientMessageId"] = client.clone();
            }
            if let Err(error) = self.inner.timeline(item).await {
                self.inner.state.lock().unwrap().turn = None;
                return Err(error);
            }
        }
        let inner = self.inner.clone();
        let active_turn = turn.clone();
        tokio::spawn(async move {
            let response = inner
                .rpc
                .request(
                    "session/prompt",
                    json!({"sessionId":session,"messageId":message_id,"prompt":blocks}),
                    None,
                )
                .await;
            if inner.finish(&active_turn, response).await.is_err() {
                inner.rpc.cancel.cancel();
            }
        });
        Ok(json!({"turnId":turn}))
    }
    async fn steer_active_turn(&self, _prompt: Value, _options: Value) -> Result<Value> {
        bail!(
            "{} ACP adapter does not support steering an active turn",
            self.provider()
        )
    }
    async fn stream_history(&self, sink: EventSink) -> Result<()> {
        let history = std::mem::take(&mut self.inner.state.lock().unwrap().history);
        for event in history {
            sink.emit(event).await?;
        }
        Ok(())
    }
    async fn runtime_info(&self) -> Result<Value> {
        Ok(self.inner.runtime())
    }
    async fn available_modes(&self) -> Result<Vec<Value>> {
        Ok(modes(
            &self.inner.spec.family,
            &self.inner.state.lock().unwrap().native,
        )
        .0)
    }
    async fn current_mode(&self) -> Result<Option<String>> {
        Ok(self.inner.state.lock().unwrap().mode.clone())
    }
    async fn set_mode(&self, mode: &str) -> Result<Option<Value>> {
        let _mutation = self.inner.mutation.lock().await;
        let session = self.inner.session_id()?;
        let copilot = self.inner.spec.family == "copilot";
        if copilot && (mode == "allow-all" || mode == AUTOPILOT) {
            self.inner
                .rpc
                .call(
                    "session/set_mode",
                    json!({"sessionId":session,"modeId":AGENT}),
                )
                .await?;
            self.inner.state.lock().unwrap().mode = Some(AGENT.into());
            self.inner.write_config("allow_all", "on", None).await?;
            self.inner.state.lock().unwrap().mode = Some("allow-all".into());
            self.inner.emit_mode().await?;
            return Ok(None);
        }
        let (available, config, current) = {
            let s = self.inner.state.lock().unwrap();
            (
                modes(&self.inner.spec.family, &s.native).0,
                option(&s.native["configOptions"], "mode").cloned(),
                s.mode.clone(),
            )
        };
        if !available.is_empty() && !available.iter().any(|m| m["id"] == mode) {
            return Ok(Some(
                json!({"warning":format!("'{mode}' is not a valid {} mode",self.provider())}),
            ));
        }
        if copilot && current.as_deref() == Some("allow-all") {
            self.inner.write_config("allow_all", "off", None).await?;
        }
        let selected = if !available.is_empty() {
            self.inner
                .rpc
                .call(
                    "session/set_mode",
                    json!({"sessionId":session,"modeId":mode}),
                )
                .await?;
            mode.to_owned()
        } else {
            let option = config
                .ok_or_else(|| anyhow!("{} does not expose ACP mode switching", self.provider()))?;
            self.inner
                .write_config(text(&option["id"]), mode, Some("mode"))
                .await?
        };
        self.inner.state.lock().unwrap().mode = Some(selected);
        self.inner.emit_mode().await?;
        Ok(None)
    }
    async fn respond_to_permission(
        &self,
        request_id: &str,
        response: Value,
    ) -> Result<Option<Value>> {
        let (selected, rpc_id, turn) = {
            let mut s = self.inner.state.lock().unwrap();
            let permission = s
                .permissions
                .get(request_id)
                .ok_or_else(|| anyhow!("No pending permission request with id '{request_id}'"))?;
            let selected = permission_option(&permission.options, &response)?;
            let permission = s.permissions.remove(request_id).unwrap();
            (selected, permission.rpc_id, permission.turn)
        };
        let outcome = selected
            .map(|o| json!({"outcome":{"outcome":"selected","optionId":o["optionId"]}}))
            .unwrap_or_else(|| json!({"outcome":{"outcome":"cancelled"}}));
        self.inner.rpc.reply(rpc_id, Ok(outcome)).await?;
        let mut event =
            json!({"type":"permission_resolved","requestId":request_id,"resolution":response});
        if let Some(turn) = turn {
            event["turnId"] = json!(turn);
        }
        self.inner.emit(event).await?;
        if response["behavior"] == "deny" && response["interrupt"] == true {
            self.inner
                .rpc
                .notify(
                    "session/cancel",
                    json!({"sessionId":self.inner.session_id()?}),
                )
                .await?;
        }
        Ok(None)
    }
    async fn interrupt(&self) -> Result<()> {
        self.inner.cancel_permissions().await?;
        let active = self.inner.state.lock().unwrap().turn.is_some();
        if active {
            self.inner
                .rpc
                .notify(
                    "session/cancel",
                    json!({"sessionId":self.inner.session_id()?}),
                )
                .await?;
        }
        Ok(())
    }
    async fn close(&self) -> Result<()> {
        self.inner.close().await
    }
    async fn list_commands(&self) -> Result<Vec<Value>> {
        if self.inner.spec.family != "copilot" {
            let duration = if matches!(self.inner.spec.family.as_str(), "cursor" | "kiro") {
                Duration::from_secs(10)
            } else {
                Duration::from_millis(1500)
            };
            let _ = tokio::time::timeout(duration, async {
                loop {
                    let notified = self.inner.commands_changed.notified();
                    {
                        let s = self.inner.state.lock().unwrap();
                        if s.commands_ready
                            || s.closed
                            || s.released
                            || self.inner.rpc.cancel.is_cancelled()
                        {
                            break;
                        }
                    }
                    tokio::select! {_ = notified=>{},_ = self.inner.rpc.cancel.cancelled()=>break}
                }
            })
            .await;
            self.inner.state.lock().unwrap().commands_ready = true;
            self.inner.commands_changed.notify_waiters();
        }
        Ok(self.inner.state.lock().unwrap().commands.clone())
    }
    async fn set_model(&self, model: Option<&str>) -> Result<()> {
        let _mutation = self.inner.mutation.lock().await;
        let Some(model) = model else {
            self.inner.state.lock().unwrap().model = None;
            return Ok(());
        };
        let (available, option) = {
            let s = self.inner.state.lock().unwrap();
            (
                array(&s.native["models"]["availableModels"]).to_vec(),
                option(&s.native["configOptions"], "model").cloned(),
            )
        };
        let configured = self
            .inner
            .spec
            .configured_models
            .iter()
            .any(|m| m["id"] == model || m.as_str() == Some(model))
            || self.inner.configured_models.lock().unwrap().contains(model);
        let selected = available.iter().any(|m| m["modelId"] == model) || configured;
        if !available.is_empty() && !selected {
            tracing::warn!(provider=%self.provider(),model,"Invalid ACP model selection ignored");
            return Ok(());
        }
        if selected {
            match self
                .inner
                .rpc
                .call(
                    "session/set_model",
                    json!({"sessionId":self.inner.session_id()?,"modelId":model}),
                )
                .await
            {
                Ok(_) => {
                    self.inner.state.lock().unwrap().model = Some(model.into());
                    self.inner
                        .emit(json!({"type":"model_changed","runtimeInfo":self.inner.runtime()}))
                        .await?;
                    return Ok(());
                }
                // Method/parameter rejection proves non-acceptance; never replay on transport/internal errors.
                Err(error)
                    if error
                        .downcast_ref::<RpcError>()
                        .is_some_and(|e| matches!(e.code, -32601 | -32602)) => {}
                Err(error) => return Err(error),
            }
        }
        let option = option
            .ok_or_else(|| anyhow!("{} does not expose ACP model selection", self.provider()))?;
        if !choices(&option).any(|c| c["value"] == model) {
            tracing::warn!(provider=%self.provider(),model,"Invalid ACP model config choice ignored");
            return Ok(());
        }
        let selected = self
            .inner
            .write_config(text(&option["id"]), model, Some("model"))
            .await?;
        self.inner.state.lock().unwrap().model = Some(selected);
        self.inner
            .emit(json!({"type":"model_changed","runtimeInfo":self.inner.runtime()}))
            .await?;
        Ok(())
    }
    async fn set_thinking_option(&self, thinking: Option<&str>) -> Result<Option<Value>> {
        let _mutation = self.inner.mutation.lock().await;
        let Some(thinking) = thinking else {
            self.inner.state.lock().unwrap().thinking = None;
            return Ok(None);
        };
        let option = {
            let s = self.inner.state.lock().unwrap();
            option(&s.native["configOptions"], "thought_level").cloned()
        }
        .ok_or_else(|| {
            anyhow!(
                "{} does not expose ACP thought-level selection",
                self.provider()
            )
        })?;
        let selected = self
            .inner
            .write_config(text(&option["id"]), thinking, Some("thought_level"))
            .await?;
        self.inner.state.lock().unwrap().thinking = Some(selected.clone());
        self.inner
            .emit(json!({"type":"thinking_option_changed","thinkingOptionId":selected}))
            .await?;
        Ok(None)
    }
    async fn set_feature(&self, feature: &str, value: Value) -> Result<()> {
        let _mutation = self.inner.mutation.lock().await;
        if feature == "auto_accept" {
            let mut s = self.inner.state.lock().unwrap();
            let enabled = value == true;
            if (s.config["featureValues"][feature] == true) != enabled {
                if !s.config["featureValues"].is_object() {
                    s.config["featureValues"] = json!({});
                }
                s.config["featureValues"][feature] = json!(enabled);
                s.features = features(&self.inner.spec.family, &s.native, &s.config).into();
            }
            return Ok(());
        }
        if !(self.inner.spec.family == "cursor" && feature == "fast"
            || self.inner.spec.family == "copilot" && feature == "agent")
        {
            bail!("Unknown {} feature: {feature}", self.provider());
        }
        let option = {
            let s = self.inner.state.lock().unwrap();
            option_id(&s.native["configOptions"], feature)
                .or_else(|| {
                    if feature == "agent" {
                        option(&s.native["configOptions"], "_agent")
                    } else {
                        None
                    }
                })
                .cloned()
        }
        .ok_or_else(|| {
            anyhow!(
                "{} does not expose ACP feature '{feature}'",
                self.provider()
            )
        })?;
        let value = if value.is_null() {
            String::new()
        } else if let Some(s) = value.as_str() {
            s.to_owned()
        } else {
            bail!("ACP feature value must be a string")
        };
        if !choices(&option).any(|c| c["value"] == value) {
            bail!(
                "{} feature '{feature}' does not include option '{value}'",
                self.provider()
            );
        }
        let selected = self
            .inner
            .write_config(text(&option["id"]), &value, option["category"].as_str())
            .await?;
        let mut s = self.inner.state.lock().unwrap();
        if !s.config["featureValues"].is_object() {
            s.config["featureValues"] = json!({});
        }
        s.config["featureValues"][feature] = json!(selected);
        Ok(())
    }
    async fn rewind(&self, _target: RewindTarget, _message_id: &str) -> Result<()> {
        bail!("{} ACP adapter does not support rewind", self.provider())
    }
    async fn try_out_of_band(&self, _prompt: &Value) -> Result<bool> {
        Ok(false)
    }
}

#[async_trait]
impl AgentProvider for AcpProvider {
    fn id(&self) -> &str {
        &self.spec.id
    }
    fn capabilities(&self) -> &Value {
        &self.capabilities
    }
    fn resolve_create_config(&self, input: &Value) -> Option<Result<Value>> {
        Some(mapping::create_config(input, &self.spec.id))
    }
    fn is_create_config_unattended(&self, input: &Value) -> Option<bool> {
        Some(
            crate::providers::create_config::is_default_unattended(input)
                || input["config"]["featureValues"]["auto_accept"] == true
                || array(&input["features"]).iter().any(|feature| {
                    feature["id"] == "auto_accept"
                        && feature["type"] == "toggle"
                        && feature["value"] == true
                }),
        )
    }
    async fn create_session(
        &self,
        mut config: Value,
        context: LaunchContext,
        options: Value,
        sink: EventSink,
    ) -> Result<Arc<dyn AgentSession>> {
        config["provider"] = json!(self.spec.id);
        Ok(self
            .open(config, context, Value::Null, options, sink, false)
            .await?)
    }
    async fn resume_session(
        &self,
        persistence: Value,
        config: Value,
        context: LaunchContext,
        options: Value,
        sink: EventSink,
    ) -> Result<Arc<dyn AgentSession>> {
        if persistence["provider"]
            .as_str()
            .is_some_and(|p| p != self.spec.id)
        {
            bail!(
                "Cannot resume another provider's persistence handle with {}",
                self.spec.id
            );
        }
        if !persistence["sessionId"].is_string() {
            bail!("ACP resume requires a native sessionId");
        }
        let mut merged = persistence
            .get("metadata")
            .filter(|v| v.is_object())
            .cloned()
            .unwrap_or_else(|| json!({}));
        merge(&mut merged, &config);
        merged["provider"] = json!(self.spec.id);
        Ok(self
            .open(merged, context, persistence, options, sink, false)
            .await?)
    }
    async fn fetch_catalog(&self, options: Value, cancel: CancellationToken) -> Result<Value> {
        let session = self.probe(options, cancel).await?;
        let result:Result<Value>=async{
            let native=session.inner.state.lock().unwrap().native.clone();
            let mut models=mapping::models(&self.spec.id,&native);
            if self.spec.family=="cursor"{
                models=refresh_activity("catalog.resolve",async {
                let response=match session.inner.rpc.call("cursor/list_available_models",json!({})).await {
                    Ok(response)=>response,
                    Err(error) if error.downcast_ref::<RpcError>().is_some_and(|e|e.code==-32601)=>bail!("Update Cursor CLI: this version does not support cursor/list_available_models."),
                    Err(error)=>return Err(error),
                };
                let catalog=response["models"].as_array().ok_or_else(||anyhow!("Cursor model catalog omitted models"))?;
                let current=models.iter().find(|m|m["isDefault"]==true).map(|m|m["id"].clone());
                catalog.iter().map(|m|{
                    let value=m["value"].as_str().filter(|v|!v.is_empty()).ok_or_else(||anyhow!("Cursor model catalog has invalid model value"))?;
                    let name=m["name"].as_str().ok_or_else(||anyhow!("Cursor model catalog has invalid model name"))?;
                    if !m["configOptions"].is_array(){bail!("Cursor model catalog has invalid configOptions");}
                    let mut model=json!({"provider":self.spec.id,"id":value,"label":name,"isDefault":current.as_ref()==Some(&m["value"])});
                    apply_thinking(&mut model,&selectors(&m["configOptions"],"thought_level"));Ok(model)
                }).collect::<Result<Vec<_>>>()
                }).await?;
            }else if self.spec.family=="kimi"&&models.len()>1 {
                refresh_activity("catalog.resolve",async {
                if let Some(model_option)=option(&native["configOptions"],"model"){
                    for model in &mut models{
                        let response=session.inner.rpc.call("session/set_config_option",json!({"sessionId":session.inner.session_id()?,"configId":model_option["id"],"value":model["id"]})).await;
                        match response{
                            Ok(response)=>apply_thinking(model,&selectors(&response["configOptions"],"thought_level")),
                            Err(error)=>{
                                if session.inner.rpc.cancel.is_cancelled(){return Err(error);}
                                tracing::warn!(provider=%self.spec.id,model=%model["id"],error=%error,"ACP model thought-level discovery failed");
                                if model["isDefault"]!=true{apply_thinking(model,&[]);}
                            },
                        }
                    }
                }
                Ok(())
                }).await?;
            }
            Ok(json!({"models":models,"modes":modes(&self.spec.family,&native).0}))
        }.await;
        let close = session.close().await;
        match result {
            Err(error) => Err(error),
            Ok(value) => {
                close?;
                Ok(value)
            }
        }
    }
    async fn list_features(&self, config: &Value) -> Result<Vec<Value>> {
        if !matches!(self.spec.family.as_str(), "cursor" | "copilot") {
            return Ok(features(&self.spec.family, &json!({}), config));
        }
        let session = self.probe(config.clone(), CancellationToken::new()).await?;
        let result = features(
            &self.spec.family,
            &session.inner.state.lock().unwrap().native,
            config,
        );
        session.close().await?;
        Ok(result)
    }
    async fn list_commands(&self, config: &Value) -> Result<Vec<Value>> {
        let session = self.probe(config.clone(), CancellationToken::new()).await?;
        let result = session.list_commands().await;
        let close = session.close().await;
        let result = result?;
        close?;
        Ok(result)
    }
    async fn list_importable_sessions(&self, options: Value) -> Result<Vec<Value>> {
        let context = self.probe_context(CancellationToken::new());
        let (sink, mut rx) = EventSink::channel(64, context.cancel.clone());
        tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let inner = self
            .spawn(
                json!({"cwd":context.home,"provider":self.spec.id}),
                &context,
                Value::Null,
                true,
                true,
                sink,
            )
            .await?;
        let result:Result<Vec<Value>>=async{
            let capabilities=inner.agent_capabilities.lock().unwrap().clone();
            if !capabilities["sessionCapabilities"].get("list").is_some_and(|v|!v.is_null()&&v!=false){return Ok(Vec::new());}
            let scan_limit=options["scanLimit"].as_u64().unwrap_or(500).min(500) as usize;
            let limit=options["limit"].as_u64().unwrap_or(usize::MAX as u64).min(usize::MAX as u64) as usize;
            let deadline=tokio::time::Instant::now()+Duration::from_secs(60);
            let mut sessions=Vec::new();let mut scanned=0;let mut cursor=None;let mut seen=HashSet::new();
            loop{
                if scanned>=scan_limit||sessions.len()>=limit{break;}
                let mut params=json!({});if let Some(cursor)=&cursor{params["cursor"]=json!(cursor);}if let Some(cwd)=options.get("cwd"){params["cwd"]=cwd.clone();}
                let page=inner.rpc.call("session/list",params).await?;
                for session in array(&page["sessions"]){
                    if scanned>=scan_limit||sessions.len()>=limit{break;}scanned+=1;
                    let native_id=session["sessionId"].as_str().ok_or_else(||anyhow!("ACP session/list omitted sessionId"))?;
                    let mut first=Value::Null;let mut last=Value::Null;let mut empty=false;
                    if capabilities["loadSession"]==true&&tokio::time::Instant::now()<deadline{
                        {let mut s=inner.state.lock().unwrap();s.id=Some(native_id.into());s.history.clear();s.pending_user=None;s.replay=true;s.assistant=None;s.tools.clear();}
                        let timeout=(deadline-tokio::time::Instant::now()).min(Duration::from_secs(30));
                        match inner.rpc.request("session/load",json!({"sessionId":native_id,"cwd":session["cwd"],"mcpServers":[]}),Some(timeout)).await{
                            Ok(_)=>{
                                inner.flush_user().await?;
                                let history=std::mem::take(&mut inner.state.lock().unwrap().history);empty=history.is_empty();
                                let prompts:Vec<Value>=history.iter().filter(|e|e["item"]["type"]=="user_message").filter_map(|e|{
                                    let normalized=text(&e["item"]["text"]).split_whitespace().collect::<Vec<_>>().join(" ");
                                    if normalized.is_empty(){None}else{Some(json!(String::from_utf16_lossy(&normalized.encode_utf16().take(160).collect::<Vec<_>>())))}
                                }).collect();first=prompts.first().cloned().unwrap_or(Value::Null);last=prompts.last().cloned().unwrap_or(Value::Null);
                            },
                            Err(error)=>{if inner.rpc.cancel.is_cancelled(){return Err(error);}tracing::debug!(provider=%self.spec.id,session_id=native_id,error=%error,"ACP import history could not be read");},
                        }
                        if capabilities["sessionCapabilities"].get("close").is_some_and(|v|!v.is_null()&&v!=false)&&tokio::time::Instant::now()<deadline{
                            let timeout=(deadline-tokio::time::Instant::now()).min(Duration::from_secs(2));
                            let _=inner.rpc.request("session/close",json!({"sessionId":native_id}),Some(timeout)).await;
                        }
                    }
                    if !empty{sessions.push(json!({"providerHandleId":native_id,"cwd":session["cwd"],"title":session["title"],"firstPromptPreview":first,"lastPromptPreview":last,"lastActivityAt":session.get("updatedAt").filter(|v|!v.is_null()).cloned().unwrap_or_else(||json!("1970-01-01T00:00:00.000Z"))}));}
                }
                cursor=page["nextCursor"].as_str().filter(|v|!v.is_empty()).map(str::to_owned);
                let Some(next)=&cursor else{break};if !seen.insert(next.clone()){bail!("ACP session/list returned a repeated pagination cursor");}
            }
            Ok(sessions)
        }.await;
        let close = inner.close().await;
        match result {
            Err(error) => Err(error),
            Ok(value) => {
                close?;
                Ok(value)
            }
        }
    }
    async fn import_session(
        &self,
        input: Value,
        mut config: Value,
        mut stored_config: Value,
        context: LaunchContext,
        sink: EventSink,
    ) -> Result<ImportedSession> {
        let native_id = input["providerHandleId"]
            .as_str()
            .ok_or_else(|| anyhow!("ACP import requires providerHandleId"))?;
        config["cwd"] = input["cwd"].clone();
        config["provider"] = json!(self.spec.id);
        stored_config["cwd"] = input["cwd"].clone();
        stored_config["provider"] = json!(self.spec.id);
        let persistence = json!({"provider":self.spec.id,"sessionId":native_id,"nativeHandle":native_id,"metadata":stored_config});
        let session = self
            .open(config, context, persistence.clone(), json!({}), sink, false)
            .await?;
        let history = std::mem::take(&mut session.inner.state.lock().unwrap().history);
        let timeline = history
            .into_iter()
            .map(|event| {
                let mut entry = json!({"item":event["item"]});
                if let Some(timestamp) = event.get("timestamp") {
                    entry["timestamp"] = timestamp.clone();
                }
                entry
            })
            .collect();
        Ok(ImportedSession {
            session,
            config: stored_config,
            persistence,
            timeline,
            provider_subagent_events: Vec::new(),
        })
    }
    async fn is_available(&self, _options: Value, cancel: CancellationToken) -> Result<bool> {
        if cancel.is_cancelled() {
            bail!("ACP availability request cancelled");
        }
        let Some(command) = self.spec.command.first() else {
            return Ok(false);
        };
        let path = self
            .spec
            .env
            .get("PATH")
            .map(std::ffi::OsString::from)
            .or_else(|| std::env::var_os("PATH"));
        Ok(which::which_in(command, path, std::env::current_dir()?).is_ok())
    }
    async fn diagnostic(&self) -> Result<String> {
        if !self
            .is_available(json!({}), CancellationToken::new())
            .await?
        {
            return Ok(format!(
                "{}: executable '{}' not found",
                self.spec.id,
                self.spec.command.first().map(String::as_str).unwrap_or("")
            ));
        }
        if self.spec.family == "copilot" {
            return Ok(format!(
                "{}: executable '{}' available",
                self.spec.id, self.spec.command[0]
            ));
        }
        match self.probe(json!({}), CancellationToken::new()).await {
            Ok(session) => {
                let native = session.inner.state.lock().unwrap().native.clone();
                let result = format!(
                    "{}: ACP initialize/session/new succeeded; models={}, modes={}",
                    self.spec.id,
                    models(&self.spec.id, &native).len(),
                    modes(&self.spec.family, &native).0.len()
                );
                session.close().await?;
                Ok(result)
            }
            Err(error) => Ok(format!("{}: ACP probe failed: {error}", self.spec.id)),
        }
    }
    async fn archive_native_session(&self, _persistence: &Value) -> Result<()> {
        bail!(
            "{} ACP adapter has no native archive operation",
            self.spec.id
        )
    }
    async fn unarchive_native_session(&self, _persistence: &Value) -> Result<()> {
        bail!(
            "{} ACP adapter has no native unarchive operation",
            self.spec.id
        )
    }
    async fn shutdown(&self) -> Result<()> {
        let sessions: Vec<_> = self
            .sessions
            .lock()
            .unwrap()
            .drain(..)
            .filter_map(|s| s.upgrade())
            .collect();
        let mut failure = None;
        for session in sessions {
            if let Err(error) = session.close().await {
                failure = Some(error);
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
        Ok(())
    }
}
