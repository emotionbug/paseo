use super::{ProviderOperationError, StaleProviderSessionError, mapping, runtime::Connection};
use crate::providers::api::{AgentSession, EventSink, RewindTarget};
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct Child {
    snapshots: HashMap<String, Value>,
    terminal: bool,
    active: HashSet<String>,
}
struct State {
    live: bool,
    closed: bool,
    terminal_published: bool,
    capabilities: Vec<String>,
    config: Value,
    commands: Vec<Value>,
    persistence: Option<Value>,
    history: Vec<Value>,
    permissions: Vec<Value>,
    responses: HashMap<String, Value>,
    snapshots: HashMap<String, Value>,
    revert_tokens: HashMap<String, Value>,
    current_turn: Option<String>,
    active_turns: HashSet<String>,
    children: HashMap<String, Child>,
    pending_children: Vec<Value>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            live: false,
            closed: false,
            terminal_published: false,
            capabilities: Vec::new(),
            config: json!({"models":[],"modes":[],"thinkingOptions":[],"settings":[]}),
            commands: Vec::new(),
            persistence: None,
            history: Vec::new(),
            permissions: Vec::new(),
            responses: HashMap::new(),
            snapshots: HashMap::new(),
            revert_tokens: HashMap::new(),
            current_turn: None,
            active_turns: HashSet::new(),
            children: HashMap::new(),
            pending_children: Vec::new(),
        }
    }
}

pub(super) struct SessionCore {
    pub id: String,
    pub provider: String,
    pub connection: Arc<Connection>,
    cancel: CancellationToken,
    sink: EventSink,
    terminal: AtomicBool,
    state: Mutex<State>,
    delivery: tokio::sync::Mutex<()>,
}

impl SessionCore {
    pub fn new(
        id: String,
        provider: String,
        connection: Arc<Connection>,
        cancel: CancellationToken,
        sink: EventSink,
    ) -> Self {
        Self {
            id,
            provider,
            connection,
            cancel,
            sink,
            terminal: AtomicBool::new(false),
            state: Mutex::new(State::default()),
            delivery: tokio::sync::Mutex::new(()),
        }
    }
    pub fn mark_terminal(&self) {
        self.terminal.store(true, Ordering::Release);
    }
    fn stale(&self) -> bool {
        self.terminal.load(Ordering::Acquire) || !self.connection.is_live()
    }
    fn stale_error(&self) -> anyhow::Error {
        StaleProviderSessionError {
            session_id: self.id.clone(),
        }
        .into()
    }
    pub fn capabilities(&self) -> Value {
        mapping::agent_capabilities(&self.state.lock().unwrap().capabilities)
    }
    fn persistence(&self) -> Option<Value> {
        self.state
            .lock()
            .unwrap()
            .persistence
            .as_ref()
            .map(|value| mapping::persistence_handle(&self.provider, value))
    }
    fn native_id(&self) -> String {
        self.persistence()
            .and_then(|value| value["sessionId"].as_str().map(str::to_owned))
            .unwrap_or_else(|| self.id.clone())
    }
    pub fn history(&self) -> Vec<Value> {
        self.state.lock().unwrap().history.clone()
    }
    pub fn initial_timeline(&self) -> Vec<Value> {
        self.state
            .lock()
            .unwrap()
            .history
            .iter()
            .filter(|event| event["type"] == "timeline" && event["item"]["type"] == "notification")
            .map(timeline_entry)
            .collect()
    }
    pub async fn activate(&self) -> Result<()> {
        let _delivery = self.delivery.lock().await;
        let output = {
            let mut state = self.state.lock().unwrap();
            state.live = true;
            let mut output = Vec::new();
            for event in std::mem::take(&mut state.pending_children) {
                output.extend(self.translate_child(&mut state, event)?);
            }
            state.history.extend(output.iter().cloned());
            output
        };
        self.emit(output).await
    }
    async fn emit(&self, output: Vec<Value>) -> Result<()> {
        for event in output {
            tokio::select! {
                _ = self.cancel.cancelled() => return Ok(()),
                result = self.sink.emit(event) => result?,
            }
        }
        Ok(())
    }
    pub async fn accept(&self, event: Value, child: bool) -> Result<()> {
        let _delivery = self.delivery.lock().await;
        let output = {
            let mut state = self.state.lock().unwrap();
            if state.closed {
                return Ok(());
            }
            if child && !state.live {
                if state.pending_children.len() >= 1024 {
                    bail!("Provider pending descendant event limit exceeded");
                }
                state.pending_children.push(event);
                return Ok(());
            }
            let output = if child {
                self.translate_child(&mut state, event)?
            } else {
                self.translate(&mut state, event)
            };
            state.history.extend(output.iter().cloned());
            if state.live { output } else { Vec::new() }
        };
        self.emit(output).await
    }
    pub async fn connection_closed(&self, error: &str, runtime_failure: bool) -> Result<()> {
        let _delivery = self.delivery.lock().await;
        self.mark_terminal();
        let output = {
            let mut state = self.state.lock().unwrap();
            if state.closed {
                return Ok(());
            }
            let mut output = Vec::new();
            if !state.terminal_published && (runtime_failure || !state.active_turns.is_empty()) {
                output.push(self.failure(&state, &json!({"message":error})));
            }
            state.terminal_published = true;
            for (id, child) in &mut state.children {
                if !child.terminal && (runtime_failure || !child.active.is_empty()) {
                    child.terminal = true;
                    output.push(json!({"type":"provider_subagent","provider":self.provider,"event":{"type":"upsert","id":id,"status":"failed"}}));
                }
            }
            state.pending_children.clear();
            state.history.extend(output.iter().cloned());
            if state.live { output } else { Vec::new() }
        };
        self.emit(output).await
    }
    fn runtime_info(&self, state: &State) -> Value {
        let id = state
            .persistence
            .as_ref()
            .map(mapping::persistence_id)
            .unwrap_or_else(|| self.id.clone());
        json!({"provider":self.provider,"sessionId":id,"model":state.config["model"],"modeId":state.config["mode"],"thinkingOptionId":state.config["thinkingOption"]})
    }
    fn with_turn(&self, state: &State, mut event: Value) -> Value {
        if let Some(turn) = &state.current_turn {
            event["turnId"] = json!(turn);
        }
        event
    }
    fn failure(&self, state: &State, error: &Value) -> Value {
        let mut event = self.with_turn(
            state,
            json!({"type":"turn_failed","provider":self.provider,"error":error["message"]}),
        );
        mapping::copy(error, "code", &mut event, "code");
        mapping::copy(error, "diagnostic", &mut event, "diagnostic");
        event
    }
    fn translate(&self, state: &mut State, event: Value) -> Vec<Value> {
        match event["type"].as_str().unwrap_or("") {
            "session.opened" => {
                state.capabilities = event["capabilities"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|value| value.as_str().unwrap().to_owned())
                    .collect();
                state.persistence = event.get("persistence").cloned();
                Vec::new()
            }
            "session.persistence" => {
                state.persistence = Some(event["persistence"].clone());
                Vec::new()
            }
            "session.commands" => {
                state.commands = event["commands"].as_array().unwrap().clone();
                Vec::new()
            }
            "session.prompt_result" => {
                if event["result"]["type"] == "turn" {
                    state
                        .active_turns
                        .insert(event["result"]["turnId"].as_str().unwrap().to_owned());
                }
                Vec::new()
            }
            "session.turn" => {
                let id = event["turnId"].as_str().unwrap();
                let mut translated = json!({"provider":self.provider,"turnId":id});
                match event["state"].as_str().unwrap() {
                    "started" => {
                        state.current_turn = Some(id.to_owned());
                        state.active_turns.insert(id.to_owned());
                        translated["type"] = json!("turn_started");
                    }
                    status => {
                        if state.current_turn.as_deref() == Some(id) {
                            state.current_turn = None;
                        }
                        state.active_turns.remove(id);
                        match status {
                            "completed" => translated["type"] = json!("turn_completed"),
                            "canceled" => {
                                translated["type"] = json!("turn_canceled");
                                translated["reason"] = json!(
                                    event["error"]["message"]
                                        .as_str()
                                        .unwrap_or("Canceled by provider")
                                );
                            }
                            _ => {
                                translated["type"] = json!("turn_failed");
                                translated["error"] = json!(
                                    event["error"]["message"]
                                        .as_str()
                                        .unwrap_or("Provider turn failed")
                                );
                                mapping::copy(&event["error"], "code", &mut translated, "code");
                                mapping::copy(
                                    &event["error"],
                                    "diagnostic",
                                    &mut translated,
                                    "diagnostic",
                                );
                            }
                        }
                    }
                }
                vec![translated]
            }
            "session.usage" => {
                let mut translated =
                    json!({"type":"usage_updated","provider":self.provider,"usage":event["usage"]});
                mapping::copy(&event, "turnId", &mut translated, "turnId");
                vec![translated]
            }
            "timeline.item" => {
                let item = mapping::timeline_item(
                    event["item"].clone(),
                    &mut state.snapshots,
                    Some(&mut state.revert_tokens),
                );
                item.map(|item| {
                    let mut translated = self.with_turn(
                        state,
                        json!({"type":"timeline","provider":self.provider,"item":item}),
                    );
                    mapping::copy(&event, "timestamp", &mut translated, "timestamp");
                    translated
                })
                .into_iter()
                .collect()
            }
            "session.permission" => {
                let mut request = event["request"].clone();
                request["provider"] = json!(self.provider);
                if let Some(pending) = state
                    .permissions
                    .iter_mut()
                    .find(|pending| pending["id"] == request["id"])
                {
                    *pending = request.clone();
                } else {
                    state.permissions.push(request.clone());
                }
                vec![self.with_turn(state,json!({"type":"permission_requested","provider":self.provider,"request":request}))]
            }
            "session.permission_resolved" => {
                let id = event["permissionId"].as_str().unwrap();
                state.permissions.retain(|request| request["id"] != id);
                let resolution = state
                    .responses
                    .remove(id)
                    .unwrap_or_else(|| json!({"behavior":"deny","message":"Resolved by provider"}));
                vec![self.with_turn(state,json!({"type":"permission_resolved","provider":self.provider,"requestId":id,"resolution":resolution}))]
            }
            "session.config" => {
                state.config = event["config"].clone();
                vec![
                    json!({"type":"mode_changed","provider":self.provider,"currentModeId":state.config["mode"],"availableModes":state.config["modes"]}),
                    json!({"type":"model_changed","provider":self.provider,"runtimeInfo":self.runtime_info(state)}),
                    json!({"type":"thinking_option_changed","provider":self.provider,"thinkingOptionId":state.config["thinkingOption"]}),
                ]
            }
            "session.notice" => {
                if event["notice"]["dismissed"] == true {
                    return Vec::new();
                }
                let message = ["title", "description"]
                    .iter()
                    .filter_map(|key| event["notice"][*key].as_str())
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n");
                vec![self.with_turn(state,json!({"type":"timeline","provider":self.provider,"timestamp":chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis,true),"item":{"type":"notification","level":event["notice"]["severity"],"message":message}}))]
            }
            "session.runtime_failed" | "session.closed" => {
                if self.terminal.swap(true, Ordering::AcqRel) {
                    return Vec::new();
                }
                state.terminal_published = true;
                event
                    .get("error")
                    .map(|error| self.failure(state, error))
                    .into_iter()
                    .collect()
            }
            _ => Vec::new(),
        }
    }
    fn translate_child(&self, state: &mut State, event: Value) -> Result<Vec<Value>> {
        let id = event["sessionId"]
            .as_str()
            .context("Missing child session ID")?;
        let mapped = match event["type"].as_str().unwrap_or("") {
            "session.opened" => {
                if state.children.contains_key(id) {
                    return Ok(Vec::new());
                }
                let parent = event["parentSessionId"]
                    .as_str()
                    .context("Missing plugin child parent")?;
                let parent_id = if parent == self.id {
                    None
                } else {
                    if !state.children.contains_key(parent) {
                        bail!("Missing plugin child parent {parent}");
                    }
                    Some(parent)
                };
                state.children.insert(id.to_owned(), Child::default());
                Some(
                    json!({"type":"upsert","id":id,"parentSubagentId":parent_id,"toolCallId":event["toolCallId"],"title":event["title"],"description":event["description"],"status":"running","cwd":event["cwd"]}),
                )
            }
            "timeline.item" => {
                let Some(child) = state.children.get_mut(id) else {
                    return Ok(Vec::new());
                };
                mapping::timeline_item(event["item"].clone(), &mut child.snapshots, None).map(
                    |item| {
                        let mut result = json!({"type":"timeline","id":id,"item":item});
                        mapping::copy(&event, "timestamp", &mut result, "timestamp");
                        result
                    },
                )
            }
            "session.prompt_result" => {
                if event["result"]["type"] == "turn" {
                    if let Some(child) = state.children.get_mut(id) {
                        child
                            .active
                            .insert(event["result"]["turnId"].as_str().unwrap().to_owned());
                    }
                }
                None
            }
            "session.turn" => {
                if let Some(child) = state.children.get_mut(id) {
                    let turn = event["turnId"].as_str().unwrap();
                    if event["state"] == "started" {
                        child.active.insert(turn.to_owned());
                    } else {
                        child.active.remove(turn);
                    }
                }
                (event["state"] != "started")
                    .then(|| json!({"type":"upsert","id":id,"status":event["state"]}))
            }
            "session.runtime_failed" | "session.closed" => {
                let Some(child) = state.children.get_mut(id) else {
                    return Ok(Vec::new());
                };
                if child.terminal {
                    return Ok(Vec::new());
                }
                child.terminal = true;
                Some(
                    json!({"type":"upsert","id":id,"status":if event["type"] == "session.runtime_failed" || event.get("error").is_some() {"failed"} else {"completed"}}),
                )
            }
            _ => None,
        };
        Ok(mapped
            .map(|event| json!({"type":"provider_subagent","provider":self.provider,"event":event}))
            .into_iter()
            .collect())
    }
    async fn publish(&self, events: Vec<Value>) -> Result<()> {
        let _delivery = self.delivery.lock().await;
        self.state
            .lock()
            .unwrap()
            .history
            .extend(events.iter().cloned());
        self.emit(events).await
    }
    async fn configure(&self, changes: Value) -> Result<()> {
        self.connection
            .complete(
                json!({"type":"session.configure","sessionId":self.id,"changes":changes}),
                &self.cancel,
            )
            .await?;
        Ok(())
    }
    async fn prompt(&self, prompt: Value, options: &Value, steer: bool) -> Result<(String, Value)> {
        let (commands, capabilities) = {
            let state = self.state.lock().unwrap();
            (state.commands.clone(), state.capabilities.clone())
        };
        let id = options["clientMessageId"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let mut prompt = json!({"clientMessageId":id,"delivery":if steer {"steer"} else {"auto"},"input":mapping::prompt_input(prompt,&commands,steer)?});
        mapping::copy(options, "outputSchema", &mut prompt, "outputSchema");
        if steer {
            mapping::copy(
                options,
                "clearPendingPermissions",
                &mut prompt,
                "clearPendingPermissions",
            );
        }
        mapping::require_capabilities(
            &capabilities,
            &json!({"type":"session.prompt","prompt":prompt}),
        )?;
        if self.stale() {
            return Err(self.stale_error());
        }
        match self.connection.prompt(&self.id, prompt, &self.cancel).await {
            Ok(result) => Ok((id, result)),
            Err(_) if self.stale() => Err(self.stale_error()),
            Err(error) => Err(error),
        }
    }
}

pub(super) struct PluginSession {
    pub core: Arc<SessionCore>,
    capabilities: Value,
    initial: Vec<Value>,
}
impl PluginSession {
    pub fn new(core: Arc<SessionCore>) -> Self {
        Self {
            capabilities: core.capabilities(),
            initial: core.initial_timeline(),
            core,
        }
    }
    pub fn history(&self) -> Vec<Value> {
        self.core.history()
    }
}
pub(super) fn timeline_entry(event: &Value) -> Value {
    let mut entry = json!({"item":event["item"]});
    mapping::copy(event, "timestamp", &mut entry, "timestamp");
    entry
}

#[async_trait]
impl AgentSession for PluginSession {
    fn provider(&self) -> &str {
        &self.core.provider
    }
    fn native_id(&self) -> Option<String> {
        Some(self.core.native_id())
    }
    fn capabilities(&self) -> &Value {
        &self.capabilities
    }
    fn features(&self) -> Arc<[Value]> {
        let state = self.core.state.lock().unwrap();
        state.config["settings"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|setting| {
                let mut feature = setting.clone();
                if feature["type"] == "select" {
                    for option in feature["options"].as_array_mut().unwrap() {
                        option["id"] = option["value"].clone();
                    }
                }
                feature
            })
            .collect::<Vec<_>>()
            .into()
    }
    fn initial_timeline(&self) -> &[Value] {
        &self.initial
    }
    fn describe_persistence(&self) -> Option<Value> {
        self.core.persistence()
    }
    fn pending_permissions(&self) -> Vec<Value> {
        self.core.state.lock().unwrap().permissions.clone()
    }
    fn usage_session(&self) -> Option<Value> {
        None
    }
    async fn start_turn(&self, prompt: Value, options: Value) -> Result<Value> {
        let (id, result) = self.core.prompt(prompt, &options, false).await?;
        match result["type"].as_str() {
            Some("failed") => Err(ProviderOperationError::from_value(&result["error"]).into()),
            Some("turn" | "steer") => Ok(json!({"turnId":result["turnId"]})),
            Some("completed") => {
                let turn = format!("command-{id}");
                self.core
                    .publish(vec![
                        json!({"type":"turn_started","provider":self.provider(),"turnId":turn}),
                        json!({"type":"turn_completed","provider":self.provider(),"turnId":turn}),
                    ])
                    .await?;
                Ok(json!({"turnId":turn}))
            }
            _ => bail!("Invalid provider prompt result"),
        }
    }
    async fn steer_active_turn(&self, prompt: Value, options: Value) -> Result<Value> {
        if !self
            .core
            .state
            .lock()
            .unwrap()
            .capabilities
            .iter()
            .any(|value| value == "prompt.steer")
        {
            return Ok(json!({"status":"unavailable"}));
        }
        let (_, result) = self.core.prompt(prompt, &options, true).await?;
        Ok(
            json!({"status":if result["type"] == "steer" && result["turnId"] == options["expectedTurnId"] {"accepted"} else {"unavailable"}}),
        )
    }
    async fn stream_history(&self, sink: EventSink) -> Result<()> {
        for event in self.history() {
            sink.emit(event).await?;
        }
        Ok(())
    }
    async fn runtime_info(&self) -> Result<Value> {
        Ok(self.core.runtime_info(&self.core.state.lock().unwrap()))
    }
    async fn available_modes(&self) -> Result<Vec<Value>> {
        Ok(self.core.state.lock().unwrap().config["modes"]
            .as_array()
            .unwrap()
            .clone())
    }
    async fn current_mode(&self) -> Result<Option<String>> {
        Ok(self.core.state.lock().unwrap().config["mode"]
            .as_str()
            .map(str::to_owned))
    }
    async fn set_mode(&self, mode: &str) -> Result<Option<Value>> {
        self.core.configure(json!({"mode":mode})).await?;
        Ok(None)
    }
    async fn respond_to_permission(&self, id: &str, mut response: Value) -> Result<Option<Value>> {
        super::schema::permission_response(&mut response)?;
        self.core
            .state
            .lock()
            .unwrap()
            .responses
            .insert(id.to_owned(), response.clone());
        let result = self.core.connection.send(json!({"type":"session.permission","sessionId":self.core.id,"permissionId":id,"response":response}),&self.core.cancel).await;
        if result.is_err() {
            self.core.state.lock().unwrap().responses.remove(id);
        }
        result?;
        Ok(None)
    }
    async fn interrupt(&self) -> Result<()> {
        self.core
            .connection
            .complete(
                json!({"type":"session.interrupt","sessionId":self.core.id}),
                &self.core.cancel,
            )
            .await?;
        Ok(())
    }
    async fn close(&self) -> Result<()> {
        {
            let mut state = self.core.state.lock().unwrap();
            if state.closed {
                return Ok(());
            }
            state.closed = true;
            state.pending_children.clear();
        }
        if !self.core.connection.is_live() {
            self.core.connection.remove_session(&self.core.id);
            self.core.mark_terminal();
            return Ok(());
        }
        let result = self
            .core
            .connection
            .complete(
                json!({"type":"session.close","sessionId":self.core.id}),
                &CancellationToken::new(),
            )
            .await;
        self.core.connection.remove_session(&self.core.id);
        self.core.mark_terminal();
        if self.core.connection.is_live() {
            result?;
        }
        Ok(())
    }
    async fn list_commands(&self) -> Result<Vec<Value>> {
        Ok(self
            .core
            .state
            .lock()
            .unwrap()
            .commands
            .iter()
            .map(|command| {
                let mut value = command.clone();
                value["kind"] = json!("command");
                if value.get("argumentHint").is_none() {
                    value["argumentHint"] = json!("");
                }
                value
            })
            .collect())
    }
    async fn set_model(&self, model: Option<&str>) -> Result<()> {
        self.core.configure(json!({"model":model})).await
    }
    async fn set_thinking_option(&self, option: Option<&str>) -> Result<Option<Value>> {
        self.core
            .configure(json!({"thinkingOption":option}))
            .await?;
        Ok(None)
    }
    async fn set_feature(&self, feature: &str, value: Value) -> Result<()> {
        let mut settings = json!({});
        settings[feature] = value;
        self.core.configure(json!({"settings":settings})).await
    }
    async fn rewind(&self, target: RewindTarget, message_id: &str) -> Result<()> {
        let token = self
            .core
            .state
            .lock()
            .unwrap()
            .revert_tokens
            .get(message_id)
            .cloned()
            .with_context(|| format!("No provider revert token for message {message_id}"))?;
        let scope = match target {
            RewindTarget::Conversation => "conversation",
            RewindTarget::Files => "files",
            RewindTarget::Both => "both",
        };
        self.core.connection.complete(json!({"type":"session.revert","sessionId":self.core.id,"token":token,"scope":scope}),&self.core.cancel).await?;
        Ok(())
    }
    async fn try_out_of_band(&self, _prompt: &Value) -> Result<bool> {
        Ok(false)
    }
}
impl Drop for PluginSession {
    fn drop(&mut self) {
        self.core.connection.remove_session(&self.core.id);
        self.core.mark_terminal();
    }
}
