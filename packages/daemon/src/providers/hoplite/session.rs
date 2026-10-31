use super::compat;
use crate::{
    process::CommandSpec,
    providers::{
        acp::rpc::{Rpc, RpcError},
        api::{AgentSession, EventSink, LaunchContext, ProviderSpec, RewindTarget},
    },
};
use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

struct Permission {
    rpc_id: Value,
    options: Value,
    request: Value,
}
struct State {
    id: Option<String>,
    native: Value,
    config: Value,
    features: Arc<[Value]>,
    projection: compat::Projection,
    commands: Vec<Value>,
    permissions: HashMap<String, Permission>,
    turn: Option<String>,
    history: Vec<Value>,
    replay: bool,
    ready: bool,
    closed: bool,
    config_transaction: bool,
    opening: bool,
    provisional_id: Option<String>,
}
pub(super) struct Inner {
    provider: String,
    pub(super) rpc: Arc<Rpc>,
    agent_capabilities: Mutex<Value>,
    state: Mutex<State>,
    sink: Option<EventSink>,
    mutation: tokio::sync::Mutex<()>,
    admission: tokio::sync::Mutex<()>,
    closing: tokio::sync::Mutex<()>,
    finishing: tokio::sync::Mutex<()>,
    turn_changed: Notify,
}
pub(super) struct HopliteSession {
    inner: Arc<Inner>,
    capabilities: Value,
    initial: Vec<Value>,
}
impl Inner {
    pub(super) async fn spawn(
        spec: &ProviderSpec,
        context: Option<&LaunchContext>,
        cancel: CancellationToken,
        sink: Option<EventSink>,
        config: Value,
    ) -> Result<Arc<Self>> {
        let (program, args) = spec
            .command
            .split_first()
            .ok_or_else(|| anyhow!("Hoplite executable prefix is empty"))?;
        let mut env = spec.env.clone();
        if let Some(context) = context {
            env.extend(context.env.clone());
        }
        // The plugin's native child inherits daemon cwd; workspace cwd travels
        // in session/new/load rather than changing executable startup behavior.
        let (rpc, mut messages) = Rpc::spawn(
            CommandSpec {
                program: program.clone(),
                args: args.to_vec(),
                cwd: None,
                env,
                clear_env: false,
            },
            cancel,
        )
        .await?;
        let inner = Arc::new(Self {
            provider: spec.id.clone(),
            rpc: rpc.clone(),
            agent_capabilities: Mutex::new(json!({})),
            state: Mutex::new(State {
                id: None,
                native: json!({}),
                config,
                features: Arc::from([]),
                projection: compat::Projection::default(),
                commands: Vec::new(),
                permissions: HashMap::new(),
                turn: None,
                history: Vec::new(),
                replay: false,
                ready: false,
                closed: false,
                config_transaction: false,
                opening: false,
                provisional_id: None,
            }),
            sink,
            mutation: tokio::sync::Mutex::new(()),
            admission: tokio::sync::Mutex::new(()),
            closing: tokio::sync::Mutex::new(()),
            finishing: tokio::sync::Mutex::new(()),
            turn_changed: Notify::new(),
        });
        if let Some(sink) = &inner.sink {
            let owner = sink.cancellation();
            let transport = rpc.cancel.clone();
            tokio::spawn(async move {
                tokio::select! {_ = owner.cancelled()=>transport.cancel(),_ = transport.cancelled()=>{}}
            });
        }
        let weak = Arc::downgrade(&inner);
        tokio::spawn(async move {
            while let Some((sequence, frame)) = messages.recv().await {
                let Some(inner) = weak.upgrade() else {
                    rpc.cancel.cancel();
                    break;
                };
                let method = frame["method"].as_str().unwrap_or("");
                let result = if frame.get("id").is_some() {
                    if method == "session/request_permission" {
                        inner
                            .permission(frame["id"].clone(), frame["params"].clone())
                            .await
                    } else {
                        rpc.reply(
                            frame["id"].clone(),
                            Err(RpcError {
                                code: -32601,
                                message: format!("Method not found: {method}"),
                                data: Value::Null,
                            }
                            .into()),
                        )
                        .await
                    }
                } else {
                    inner.notification(method, &frame["params"]).await
                };
                rpc.acknowledge(sequence);
                if let Err(error) = result {
                    let _ = inner
                        .emit(json!({"type":"turn_failed","error":error.to_string()}))
                        .await;
                    rpc.cancel.cancel();
                    break;
                }
            }
            if let Some(inner) = weak.upgrade() {
                let idle = {
                    let state = inner.state.lock().unwrap();
                    !state.closed && state.ready && state.turn.is_none()
                };
                if idle {
                    let _=inner.emit(json!({"type":"turn_failed","error":format!("ACP transport closed unexpectedly: {}",rpc.process.stderr_tail())})).await;
                }
                inner.turn_changed.notify_waiters();
            }
        });
        let initialized=inner.rpc.request("initialize",json!({"protocolVersion":1,"clientCapabilities":compat::client_capabilities(),"clientInfo":{"name":"paseo","version":"1"}}),Some(Duration::from_secs(10))).await;
        match initialized {
            Ok(response) => {
                *inner.agent_capabilities.lock().unwrap() = response
                    .get("agentCapabilities")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                Ok(inner)
            }
            Err(error) => {
                let _ = inner.close().await;
                Err(error)
            }
        }
    }
    pub(super) fn negotiated_capabilities(&self) -> Value {
        super::capabilities(Some(&self.agent_capabilities.lock().unwrap()), true)
    }
    pub(super) fn supports_listing(&self) -> bool {
        self.agent_capabilities.lock().unwrap()["sessionCapabilities"]
            .get("list")
            .is_some_and(|v| !v.is_null() && v != false)
    }
    fn id(&self) -> Result<String> {
        self.state
            .lock()
            .unwrap()
            .id
            .clone()
            .ok_or_else(|| anyhow!("ACP session is not initialized"))
    }
    pub(super) fn accept_open(&self, response: &Value) -> Result<()> {
        let id = response["sessionId"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| anyhow!("ACP session/new omitted sessionId"))?;
        let mut state = self.state.lock().unwrap();
        if state
            .provisional_id
            .as_deref()
            .is_some_and(|provisional| provisional != id)
        {
            bail!("ACP session/new changed the reverse request session owner");
        }
        state.id = Some(id.into());
        state.opening = false;
        state.provisional_id = None;
        state.native = json!({"modes":response["modes"],"configOptions":response.get("configOptions").cloned().unwrap_or_else(||json!([]))});
        state.features = compat::generic_features(&state.native, &state.config).into();
        Ok(())
    }
    pub(super) async fn open(
        self: &Arc<Self>,
        config: &Value,
        context: &LaunchContext,
        persistence: Option<Value>,
    ) -> Result<Arc<HopliteSession>> {
        let mut servers = config
            .get("mcpServers")
            .filter(|v| v.is_object())
            .cloned()
            .unwrap_or_else(|| json!({}));
        if let Some(runtime) = context.mcp_servers.as_object() {
            for (key, value) in runtime {
                servers[key] = value.clone();
            }
        }
        let mut params = json!({"cwd":config["cwd"],"mcpServers":mcp_servers(&servers),"_meta":compat::session_meta(config)});
        let native = persistence
            .as_ref()
            .and_then(|value| value["data"]["sessionId"].as_str())
            .map(str::to_owned);
        if persistence.is_some() && self.agent_capabilities.lock().unwrap()["loadSession"] != true {
            bail!(
                "Provider '{}' does not support session.persistence",
                self.provider
            );
        }
        if let Some(id) = native {
            {
                let mut state = self.state.lock().unwrap();
                state.id = Some(id.clone());
                state.replay = true;
            }
            params["sessionId"] = json!(id);
            let response = self.rpc.request("session/load", params, None).await;
            self.state.lock().unwrap().replay = false;
            let response = response?;
            let mut state = self.state.lock().unwrap();
            state.native = json!({"modes":response["modes"],"configOptions":response.get("configOptions").cloned().unwrap_or_else(||json!([]))});
            state.features = compat::generic_features(&state.native, &state.config).into();
        } else {
            self.state.lock().unwrap().opening = true;
            let response = self.rpc.request("session/new", params, None).await?;
            self.accept_open(&response)?;
        }
        for (key, target) in [
            ("model", "model"),
            ("modeId", "mode"),
            ("thinkingOptionId", "thinking"),
        ] {
            if let Some(value) = config
                .get(key)
                .filter(|value| value.as_str().is_some_and(|s| !s.is_empty()))
            {
                self.apply_config(target, None, value.clone()).await?;
            }
        }
        if let Some(settings) = config["featureValues"].as_object() {
            for (id, value) in settings {
                self.apply_config("setting", Some(id), value.clone())
                    .await?;
            }
        }
        self.emit_config().await?;
        let native = self.agent_capabilities.lock().unwrap().clone();
        let configure = {
            let state = self.state.lock().unwrap();
            !state.native["modes"].is_null()
                || state.native["configOptions"]
                    .as_array()
                    .is_some_and(|options| !options.is_empty())
        };
        self.state.lock().unwrap().ready = true;
        Ok(Arc::new(HopliteSession {
            inner: self.clone(),
            capabilities: super::capabilities(Some(&native), configure),
            initial: Vec::new(),
        }))
    }
    async fn emit(&self, mut event: Value) -> Result<()> {
        event["provider"] = json!(self.provider);
        let deliver = {
            let mut state = self.state.lock().unwrap();
            if state.closed {
                return Ok(());
            }
            if event.get("turnId").is_none()
                && matches!(
                    event["type"].as_str(),
                    Some(
                        "timeline" | "permission_requested" | "permission_resolved" | "turn_failed"
                    )
                )
            {
                if let Some(turn) = &state.turn {
                    event["turnId"] = json!(turn);
                }
            }
            let deliver = (state.ready || event["type"] == "permission_requested") && !state.replay;
            state.history.push(event.clone());
            deliver
        };
        if deliver {
            if let Some(sink) = &self.sink {
                sink.emit(event).await?;
            }
        }
        Ok(())
    }
    fn runtime(&self) -> Value {
        let state = self.state.lock().unwrap();
        json!({"provider":self.provider,"sessionId":state.id.as_deref().map(super::encode_id),"model":compat::selected(&state.native,"model"),"modeId":state.native["modes"]["currentModeId"],"thinkingOptionId":compat::selected(&state.native,"thought_level")})
    }
    async fn emit_config(&self) -> Result<()> {
        let (mode, modes, thinking) = {
            let state = self.state.lock().unwrap();
            if state.config_transaction {
                return Ok(());
            }
            (
                state.native["modes"]["currentModeId"].clone(),
                compat::modes(&state.native),
                compat::selected(&state.native, "thought_level"),
            )
        };
        self.emit(json!({"type":"mode_changed","currentModeId":mode,"availableModes":modes}))
            .await?;
        self.emit(json!({"type":"model_changed","runtimeInfo":self.runtime()}))
            .await?;
        self.emit(json!({"type":"thinking_option_changed","thinkingOptionId":thinking}))
            .await
    }
    async fn notification(&self, method: &str, params: &Value) -> Result<()> {
        let events = {
            let mut state = self.state.lock().unwrap();
            if method == "session/update" && params["sessionId"].as_str() != state.id.as_deref() {
                return Ok(());
            }
            let replay = state.replay;
            state.projection.project(method, params, replay)
        };
        if let Some(events) = events {
            for event in events {
                self.emit(event).await?;
            }
            return Ok(());
        }
        if method != "session/update" {
            return Ok(());
        }
        let update = &params["update"];
        match update["sessionUpdate"].as_str().unwrap_or("") {
            "available_commands_update" => {
                self.state.lock().unwrap().commands=update["availableCommands"].as_array().into_iter().flatten().map(|command|json!({"name":command["name"],"description":command["description"],"argumentHint":command["input"]["hint"].as_str().unwrap_or(""),"kind":"command"})).collect();
            }
            "current_mode_update" => {
                {
                    let mut state = self.state.lock().unwrap();
                    if !state.native["modes"].is_null() {
                        state.native["modes"]["currentModeId"] = update["currentModeId"].clone();
                    }
                }
                self.emit_config().await?;
            }
            "config_option_update" => {
                {
                    let mut state = self.state.lock().unwrap();
                    state.native["configOptions"] = update["configOptions"].clone();
                    state.features = compat::generic_features(&state.native, &state.config).into();
                }
                self.emit_config().await?;
            }
            _ => {}
        }
        Ok(())
    }
    async fn permission(&self, rpc_id: Value, params: Value) -> Result<()> {
        if self.sink.is_none() {
            return self
                .rpc
                .reply(rpc_id, Ok(json!({"outcome":{"outcome":"cancelled"}})))
                .await;
        }
        let valid_owner = {
            let mut state = self.state.lock().unwrap();
            match params["sessionId"].as_str().filter(|id| !id.is_empty()) {
                Some(incoming) if state.id.as_deref() == Some(incoming) => true,
                Some(incoming)
                    if state.id.is_none()
                        && state.opening
                        && state
                            .provisional_id
                            .as_deref()
                            .is_none_or(|id| id == incoming) =>
                {
                    state.provisional_id = Some(incoming.to_owned());
                    true
                }
                _ => false,
            }
        };
        if !valid_owner {
            return self
                .rpc
                .reply(
                    rpc_id,
                    Err(anyhow!("ACP permission belongs to another session")),
                )
                .await;
        }
        let request = compat::permission_request(&self.provider, &params);
        let id = request["id"]
            .as_str()
            .ok_or_else(|| anyhow!("ACP permission omitted toolCallId"))?
            .to_owned();
        let replaced = self.state.lock().unwrap().permissions.insert(
            id,
            Permission {
                rpc_id,
                options: params["options"].clone(),
                request: request.clone(),
            },
        );
        if let Some(old) = replaced {
            self.rpc
                .reply(old.rpc_id, Ok(json!({"outcome":{"outcome":"cancelled"}})))
                .await?;
        }
        self.emit(json!({"type":"permission_requested","request":request}))
            .await
    }
    async fn apply_config(&self, target: &str, setting: Option<&str>, value: Value) -> Result<()> {
        let (method, params) = {
            let state = self.state.lock().unwrap();
            compat::config_request(
                &state.native,
                state
                    .id
                    .as_deref()
                    .ok_or_else(|| anyhow!("ACP session is not initialized"))?,
                target,
                setting,
                value.clone(),
            )?
        };
        let response = self.rpc.request(method, params, None).await?;
        let mut state = self.state.lock().unwrap();
        if target == "mode" {
            state.native["modes"]["currentModeId"] = value;
        } else {
            state.native["configOptions"] = response
                .get("configOptions")
                .cloned()
                .ok_or_else(|| anyhow!("ACP configuration response omitted configOptions"))?;
        }
        state.features = compat::generic_features(&state.native, &state.config).into();
        Ok(())
    }
    async fn configure(&self, target: &str, setting: Option<&str>, value: Value) -> Result<()> {
        let _mutation = self.mutation.lock().await;
        let previous = {
            let mut state = self.state.lock().unwrap();
            if state.closed {
                bail!("ACP session is closed");
            }
            state.config_transaction = true;
            state.native.clone()
        };
        let result = self.apply_config(target, setting, value).await;
        if let Err(error) = result {
            let rollback = self.restore_config(&previous).await;
            {
                let mut state = self.state.lock().unwrap();
                state.config_transaction = false;
                if rollback.is_ok() {
                    state.native = previous;
                    state.features = compat::generic_features(&state.native, &state.config).into();
                }
            }
            if let Err(rollback_error) = rollback {
                let failure = anyhow!(
                    "ACP configuration failed and rollback could not restore the previous state: {error}; {rollback_error}"
                );
                let _ = self
                    .emit(json!({"type":"turn_failed","error":failure.to_string()}))
                    .await;
                let _ = self.close().await;
                return Err(failure);
            }
            return Err(error);
        }
        self.state.lock().unwrap().config_transaction = false;
        self.emit_config().await
    }
    async fn restore_config(&self, previous: &Value) -> Result<()> {
        let current = self.state.lock().unwrap().native.clone();
        if previous["modes"]["currentModeId"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
            && previous["modes"]["currentModeId"] != current["modes"]["currentModeId"]
        {
            self.apply_config("mode", None, previous["modes"]["currentModeId"].clone())
                .await?;
        }
        for option in previous["configOptions"].as_array().into_iter().flatten() {
            let current_value = current["configOptions"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|candidate| candidate["id"] == option["id"])
                .map(|candidate| &candidate["currentValue"]);
            if current_value == Some(&option["currentValue"]) {
                continue;
            }
            self.apply_config(
                "setting",
                option["id"].as_str(),
                option["currentValue"].clone(),
            )
            .await?;
        }
        Ok(())
    }
    async fn finish(&self, turn: &str, response: Result<Value>) -> Result<()> {
        let _finishing = self.finishing.lock().await;
        if self.state.lock().unwrap().turn.as_deref() != Some(turn) {
            return Ok(());
        }
        let (status, event) = match response {
            Ok(response) if response["stopReason"] == "cancelled" => (
                "canceled",
                json!({"type":"turn_canceled","turnId":turn,"reason":"Canceled by provider"}),
            ),
            Ok(_) => ("completed", json!({"type":"turn_completed","turnId":turn})),
            Err(error) => (
                "failed",
                json!({"type":"turn_failed","turnId":turn,"error":error.to_string()}),
            ),
        };
        let events = self.state.lock().unwrap().projection.finish(status);
        let result = async {
            for event in events {
                self.emit(event).await?;
            }
            self.emit(event).await
        }
        .await;
        self.state.lock().unwrap().turn = None;
        self.turn_changed.notify_waiters();
        result
    }
    pub(super) async fn close(&self) -> Result<()> {
        let _closing = self.closing.lock().await;
        let (id, permissions) = {
            let mut state = self.state.lock().unwrap();
            if state.closed {
                return Ok(());
            }
            state.closed = true;
            (
                state.id.clone(),
                state
                    .permissions
                    .drain()
                    .map(|(_, permission)| permission)
                    .collect::<Vec<_>>(),
            )
        };
        for permission in permissions {
            let _ = self
                .rpc
                .reply(
                    permission.rpc_id,
                    Ok(json!({"outcome":{"outcome":"cancelled"}})),
                )
                .await;
        }
        if let Some(id) = id {
            let _ = self
                .rpc
                .request(
                    "session/close",
                    json!({"sessionId":id}),
                    Some(Duration::from_secs(1)),
                )
                .await;
        }
        let result = self.rpc.close().await;
        self.turn_changed.notify_waiters();
        result
    }
}

impl HopliteSession {
    pub(super) fn history(&self) -> Vec<Value> {
        self.inner.state.lock().unwrap().history.clone()
    }
}
impl Drop for HopliteSession {
    fn drop(&mut self) {
        self.inner.rpc.cancel.cancel();
    }
}
#[async_trait]
impl AgentSession for HopliteSession {
    fn provider(&self) -> &str {
        &self.inner.provider
    }
    fn native_id(&self) -> Option<String> {
        self.inner
            .state
            .lock()
            .unwrap()
            .id
            .as_deref()
            .map(super::encode_id)
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
        self.inner
            .state
            .lock()
            .unwrap()
            .id
            .as_deref()
            .map(|id| super::persistence(self.provider(), id))
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
        None
    }
    async fn start_turn(&self, prompt: Value, options: Value) -> Result<Value> {
        let _admission = self.inner.admission.lock().await;
        let blocks = prompt_blocks(&prompt)?;
        if blocks.iter().any(|block| block["type"] == "image")
            && self.inner.agent_capabilities.lock().unwrap()["promptCapabilities"]["image"] != true
        {
            bail!(
                "Provider '{}' does not support prompt.image",
                self.provider()
            );
        }
        let previous = self.inner.state.lock().unwrap().turn.clone();
        if let Some(previous) = previous {
            self.interrupt().await?;
            loop {
                let changed = self.inner.turn_changed.notified();
                if self.inner.state.lock().unwrap().turn.as_deref() != Some(previous.as_str()) {
                    break;
                }
                tokio::select! {_ = changed=>{},_ = self.inner.rpc.cancel.cancelled()=>bail!("ACP transport closed unexpectedly")}
            }
        }
        let message = options["clientMessageId"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let turn = format!("acp:{message}");
        let session = {
            let mut state = self.inner.state.lock().unwrap();
            if state.closed || self.inner.rpc.cancel.is_cancelled() {
                bail!("ACP transport is closed");
            }
            state.projection.reset_chunks();
            state
                .id
                .clone()
                .ok_or_else(|| anyhow!("ACP session is not initialized"))?
        };
        let text = blocks
            .iter()
            .filter(|block| block["type"] == "text")
            .filter_map(|block| block["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        self.inner.emit(json!({"type":"timeline","item":{"type":"user_message","messageId":message,"clientMessageId":message,"text":text}})).await?;
        self.inner.state.lock().unwrap().turn = Some(turn.clone());
        if let Err(error) = self
            .inner
            .emit(json!({"type":"turn_started","turnId":turn}))
            .await
        {
            self.inner.state.lock().unwrap().turn = None;
            return Err(error);
        }
        let inner = self.inner.clone();
        let active = turn.clone();
        tokio::spawn(async move {
            let response = inner
                .rpc
                .request(
                    "session/prompt",
                    json!({"sessionId":session,"prompt":blocks}),
                    None,
                )
                .await;
            if inner.finish(&active, response).await.is_err() {
                inner.rpc.cancel.cancel();
            }
        });
        Ok(json!({"turnId":turn}))
    }
    async fn steer_active_turn(&self, _prompt: Value, _options: Value) -> Result<Value> {
        Ok(json!({"status":"unavailable"}))
    }
    async fn stream_history(&self, sink: EventSink) -> Result<()> {
        for event in self.history() {
            sink.emit(event).await?;
        }
        Ok(())
    }
    async fn runtime_info(&self) -> Result<Value> {
        Ok(self.inner.runtime())
    }
    async fn available_modes(&self) -> Result<Vec<Value>> {
        Ok(compat::modes(&self.inner.state.lock().unwrap().native))
    }
    async fn current_mode(&self) -> Result<Option<String>> {
        Ok(
            self.inner.state.lock().unwrap().native["modes"]["currentModeId"]
                .as_str()
                .map(str::to_owned),
        )
    }
    async fn set_mode(&self, mode: &str) -> Result<Option<Value>> {
        self.inner.configure("mode", None, json!(mode)).await?;
        Ok(None)
    }
    async fn respond_to_permission(
        &self,
        request_id: &str,
        response: Value,
    ) -> Result<Option<Value>> {
        let (permission, selected) = {
            let mut state = self.inner.state.lock().unwrap();
            let permission = state
                .permissions
                .get(request_id)
                .ok_or_else(|| anyhow!("Unknown ACP permission: {request_id}"))?;
            let selected = compat::permission_option(&permission.options, &response)?;
            (
                state
                    .permissions
                    .remove(request_id)
                    .expect("permission was read under the same lock"),
                selected,
            )
        };
        let outcome = selected
            .map(|option| json!({"outcome":"selected","optionId":option["optionId"]}))
            .unwrap_or_else(|| json!({"outcome":"cancelled"}));
        self.inner
            .rpc
            .reply(permission.rpc_id, Ok(json!({"outcome":outcome})))
            .await?;
        self.inner
            .emit(
                json!({"type":"permission_resolved","requestId":request_id,"resolution":response}),
            )
            .await?;
        Ok(None)
    }
    async fn interrupt(&self) -> Result<()> {
        self.inner
            .rpc
            .notify("session/cancel", json!({"sessionId":self.inner.id()?}))
            .await
    }
    async fn close(&self) -> Result<()> {
        self.inner.close().await
    }
    async fn list_commands(&self) -> Result<Vec<Value>> {
        Ok(self.inner.state.lock().unwrap().commands.clone())
    }
    async fn set_model(&self, model: Option<&str>) -> Result<()> {
        self.inner.configure("model", None, json!(model)).await
    }
    async fn set_thinking_option(&self, option: Option<&str>) -> Result<Option<Value>> {
        self.inner
            .configure("thinking", None, json!(option))
            .await?;
        Ok(None)
    }
    async fn set_feature(&self, feature: &str, value: Value) -> Result<()> {
        self.inner.configure("setting", Some(feature), value).await
    }
    async fn rewind(&self, _target: RewindTarget, _message_id: &str) -> Result<()> {
        bail!("session.revert is not supported by this ACP provider")
    }
    async fn try_out_of_band(&self, _prompt: &Value) -> Result<bool> {
        Ok(false)
    }
}
fn mcp_servers(servers: &Value) -> Vec<Value> {
    servers.as_object().into_iter().flatten().map(|(name,server)| {
        let pairs=|value:&Value|value.as_object().into_iter().flatten().map(|(name,value)|json!({"name":name,"value":value})).collect::<Vec<_>>();
        if server["type"]=="stdio" {json!({"name":name,"command":server["command"],"args":server.get("args").cloned().unwrap_or_else(||json!([])),"env":pairs(&server["env"])})}
        else {json!({"type":server["type"],"name":name,"url":server["url"],"headers":pairs(&server["headers"])})}
    }).collect()
}
fn prompt_blocks(prompt: &Value) -> Result<Vec<Value>> {
    if let Some(text) = prompt.as_str() {
        return Ok(vec![json!({"type":"text","text":text})]);
    }
    let blocks = prompt
        .as_array()
        .ok_or_else(|| anyhow!("Prompt must be text or content blocks"))?;
    let mut result = Vec::with_capacity(blocks.len());
    for block in blocks {
        if block["type"] == "text" {
            result.push(block.clone());
        } else if block["type"] == "image" {
            result.extend(crate::providers::prompt::to_acp_blocks(&json!([block]))?);
        } else {
            result.push(json!({"type":"text","text":serde_json::to_string(block)?}));
        }
    }
    Ok(result)
}
