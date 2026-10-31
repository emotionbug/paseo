//! Native port of the explicitly development-only mock providers; never a production fallback.
mod scenario;
#[cfg(test)]
mod tests;
use super::api::*;
use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{Arc, LazyLock, Mutex},
};
use tokio::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

static FIXTURES: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!("development/fixtures.json")).expect("mock fixture JSON")
});
const DEFAULT_MODEL: &str = "five-minute-stream";
fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
fn capabilities(slow: bool) -> Value {
    let mut value = json!({"supportsStreaming":!slow,"supportsSessionPersistence":!slow,"supportsDynamicModes":false,"supportsMcpServers":false,"supportsReasoningStream":!slow,"supportsToolInvocations":!slow,"supportsRewindConversation":!slow,"supportsRewindFiles":!slow,"supportsRewindBoth":!slow});
    if !slow {
        value["supportsSessionListing"] = json!(true);
    }
    value
}
fn modes() -> Vec<Value> {
    super::manifest::development()
        .iter()
        .find(|v| v["id"] == "mock")
        .and_then(|v| v["modes"].as_array())
        .cloned()
        .unwrap_or_default()
}
fn commands() -> Vec<Value> {
    vec![
        json!({"name":"mock","description":"Reply without starting a turn","argumentHint":"handled-command","kind":"command"}),
    ]
}
fn text(prompt: &Value) -> String {
    prompt.as_str().map(str::to_owned).unwrap_or_else(|| {
        prompt
            .as_array()
            .map(|blocks| {
                blocks
                    .iter()
                    .filter(|b| b["type"] == "text")
                    .filter_map(|b| b["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
                    .trim()
                    .to_owned()
            })
            .unwrap_or_default()
    })
}
fn positive(v: &Value) -> u64 {
    v.as_u64()
        .filter(|v| *v > 0 && *v <= 9_007_199_254_740_991)
        .unwrap_or(0)
}

pub struct DevelopmentProvider {
    family: String,
    capabilities: Value,
}
impl DevelopmentProvider {
    /// Registry must restrict these families to config.isDev (PASEO_NODE_ENV=development).
    pub fn new(family: &str) -> Result<Self> {
        if !matches!(family, "mock" | "mock-slow") {
            bail!("Unknown development provider {family}");
        }
        Ok(Self {
            family: family.into(),
            capabilities: capabilities(family == "mock-slow"),
        })
    }
    fn session(
        &self,
        config: Value,
        session_id: String,
        sink: EventSink,
    ) -> Result<Arc<dyn AgentSession>> {
        if self.family == "mock-slow" {
            bail!("Mock slow provider is dev-only; sessions are not supported.");
        }
        let f = &config["featureValues"];
        let responses = f["mockAssistantResponses"]
            .as_array()
            .filter(|a| a.iter().all(Value::is_string))
            .map(|a| a.iter().map(|s| s.as_str().unwrap().to_owned()).collect())
            .unwrap_or_default();
        Ok(Arc::new(Session {
            inner: Arc::new(Inner {
                id: session_id,
                usage_key: id(),
                capabilities: self.capabilities.clone(),
                sink,
                gate: tokio::sync::Mutex::new(()),
                state: Mutex::new(State {
                    model: config["model"].as_str().unwrap_or(DEFAULT_MODEL).into(),
                    mode: config["modeId"].as_str().unwrap_or("load-test").into(),
                    history: vec![],
                    permissions: vec![],
                    active: None,
                    responses,
                    rejections: positive(&f["mockPromptRejections"]),
                    steer_failures: positive(&f["mockSteerAmbiguousFailures"]),
                    closed: false,
                }),
                features: f.clone(),
            }),
        }))
    }
}
#[async_trait]
impl AgentProvider for DevelopmentProvider {
    fn id(&self) -> &str {
        &self.family
    }
    fn capabilities(&self) -> &Value {
        &self.capabilities
    }
    async fn create_session(
        &self,
        config: Value,
        _: LaunchContext,
        _: Value,
        sink: EventSink,
    ) -> Result<Arc<dyn AgentSession>> {
        self.session(config, id(), sink)
    }
    async fn resume_session(
        &self,
        p: Value,
        config: Value,
        _: LaunchContext,
        _: Value,
        sink: EventSink,
    ) -> Result<Arc<dyn AgentSession>> {
        let mut merged = p["metadata"].as_object().cloned().unwrap_or_default();
        if let Some(c) = config.as_object() {
            merged.extend(c.clone());
        }
        self.session(
            Value::Object(merged),
            p["sessionId"]
                .as_str()
                .ok_or_else(|| anyhow!("Missing sessionId"))?
                .into(),
            sink,
        )
    }
    async fn fetch_catalog(&self, _: Value, cancel: CancellationToken) -> Result<Value> {
        if self.family == "mock-slow" {
            cancel.cancelled().await;
            bail!("mock.catalog canceled");
        }
        Ok(json!({"models":FIXTURES["models"],"modes":modes()}))
    }
    async fn list_features(&self, _: &Value) -> Result<Vec<Value>> {
        Ok(vec![])
    }
    async fn list_commands(&self, _: &Value) -> Result<Vec<Value>> {
        Ok(if self.family == "mock" {
            commands()
        } else {
            vec![]
        })
    }
    async fn list_importable_sessions(&self, _: Value) -> Result<Vec<Value>> {
        Ok(vec![])
    }
    async fn import_session(
        &self,
        input: Value,
        mut config: Value,
        mut stored_config: Value,
        context: LaunchContext,
        sink: EventSink,
    ) -> Result<ImportedSession> {
        config["provider"] = json!(self.family);
        config["cwd"] = input["cwd"].clone();
        stored_config["provider"] = json!(self.family);
        stored_config["cwd"] = input["cwd"].clone();
        let persistence = json!({"provider":self.family,"sessionId":input["providerHandleId"],"nativeHandle":input["providerHandleId"],"metadata":stored_config});
        let session = self
            .resume_session(persistence.clone(), config, context, Value::Null, sink)
            .await?;
        Ok(ImportedSession {
            session,
            config: stored_config,
            persistence,
            timeline: vec![],
            provider_subagent_events: vec![],
        })
    }
    async fn is_available(&self, _: Value, _: CancellationToken) -> Result<bool> {
        Ok(self.family == "mock"
            || std::env::var("PASEO_ENABLE_MOCK_SLOW").as_deref() == Ok("true"))
    }
    async fn diagnostic(&self) -> Result<String> {
        Ok(if self.family=="mock" {"Mock load-test provider is available in development builds."} else {"Mock slow provider: dev-only. fetchCatalog() never resolves so the snapshot manager will time out."}.into())
    }
    async fn archive_native_session(&self, _: &Value) -> Result<()> {
        bail!("{} does not support native session archival", self.family)
    }
    async fn unarchive_native_session(&self, _: &Value) -> Result<()> {
        bail!("{} does not support native session archival", self.family)
    }
    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}
struct State {
    model: String,
    mode: String,
    history: Vec<Value>,
    permissions: Vec<Value>,
    active: Option<Arc<Turn>>,
    responses: VecDeque<String>,
    rejections: u64,
    steer_failures: u64,
    closed: bool,
}
struct Inner {
    id: String,
    usage_key: String,
    capabilities: Value,
    sink: EventSink,
    gate: tokio::sync::Mutex<()>,
    state: Mutex<State>,
    features: Value,
}
#[derive(Clone)]
struct Session {
    inner: Arc<Inner>,
}
struct Turn {
    id: String,
    message_id: String,
    cancel: CancellationToken,
    started: Instant,
    profile: Value,
}
impl Session {
    async fn emit_locked(&self, event: Value) -> Result<()> {
        let closed = {
            let mut s = self.inner.state.lock().unwrap();
            s.history.push(event.clone());
            s.closed
        };
        if !closed {
            self.inner.sink.emit(event).await?;
        }
        Ok(())
    }
    async fn event(&self, t: &Turn, mut event: Value) -> Result<()> {
        let _gate = self.inner.gate.lock().await;
        if !self.active(t) {
            bail!("Mock turn canceled");
        }
        event["provider"] = json!("mock");
        event["turnId"] = json!(t.id);
        self.emit_locked(event).await
    }
    fn active(&self, t: &Turn) -> bool {
        self.inner
            .state
            .lock()
            .unwrap()
            .active
            .as_ref()
            .is_some_and(|a| a.id == t.id)
    }
    async fn timeline(&self, t: &Turn, item: Value) -> Result<()> {
        self.event(t, json!({"type":"timeline","item":item})).await
    }
    async fn assistant(&self, t: &Turn, text: &str) -> Result<()> {
        self.timeline(
            t,
            json!({"type":"assistant_message","text":text,"messageId":t.message_id}),
        )
        .await
    }
    async fn finish(&self, t: &Turn, usage: Option<Value>, failed: bool) -> Result<()> {
        let _gate = self.inner.gate.lock().await;
        if !self.active(t) {
            return Ok(());
        }
        self.inner.state.lock().unwrap().active = None;
        let mut e = json!({"type":if failed {"turn_failed"}else{"turn_completed"},"provider":"mock","turnId":t.id});
        if failed {
            e["error"] = json!("Requested mock provider failure");
        }
        if let Some(u) = usage {
            e["usage"] = u;
        }
        self.emit_locked(e).await
    }
    async fn user(&self, t: &Turn, prompt: &Value, options: &Value) -> Result<()> {
        let mut item = json!({"type":"user_message","text":text(prompt),"messageId":id()});
        if let Some(client) = options["clientMessageId"]
            .as_str()
            .filter(|s| !s.is_empty())
        {
            item["clientMessageId"] = json!(client);
        }
        self.timeline(t, item).await
    }
}
#[async_trait]
impl AgentSession for Session {
    fn provider(&self) -> &str {
        "mock"
    }
    fn native_id(&self) -> Option<String> {
        Some(self.inner.id.clone())
    }
    fn capabilities(&self) -> &Value {
        &self.inner.capabilities
    }
    fn features(&self) -> Arc<[Value]> {
        Arc::from([])
    }
    fn initial_timeline(&self) -> &[Value] {
        &[]
    }
    fn describe_persistence(&self) -> Option<Value> {
        let s = self.inner.state.lock().unwrap();
        Some(
            json!({"provider":"mock","sessionId":self.inner.id,"metadata":{"model":s.model,"modeId":s.mode}}),
        )
    }
    fn pending_permissions(&self) -> Vec<Value> {
        self.inner.state.lock().unwrap().permissions.clone()
    }
    fn usage_session(&self) -> Option<Value> {
        Some(
            json!({"provider":"mock","model":self.inner.state.lock().unwrap().model,"env":{},"sessionKey":self.inner.usage_key}),
        )
    }
    async fn start_turn(&self, prompt: Value, options: Value) -> Result<Value> {
        let gate = self.inner.gate.lock().await;
        let turn = {
            let mut s = self.inner.state.lock().unwrap();
            if s.active.is_some() {
                bail!("Mock load-test provider already has an active turn");
            }
            if s.rejections > 0 {
                s.rejections -= 1;
                bail!("Requested mock prompt rejection");
            }
            let models = FIXTURES["models"].as_array().unwrap();
            let profile = models
                .iter()
                .find(|m| m["id"] == s.model)
                .unwrap_or(&models[0])["metadata"]
                .clone();
            let t = Arc::new(Turn {
                id: id(),
                message_id: id(),
                cancel: CancellationToken::new(),
                started: Instant::now(),
                profile,
            });
            s.active = Some(t.clone());
            t
        };
        drop(gate);
        let early = scenario::matches(
            "emit synthetic user message before accepting turn",
            &text(&prompt),
        );
        if early {
            self.event(&turn, json!({"type":"turn_started"})).await?;
            self.user(&turn, &prompt, &options).await?;
        }
        let this = self.clone();
        let t = turn.clone();
        tokio::spawn(async move {
            tokio::select! {
                _=t.cancel.cancelled()=>{},
                _=this.inner.sink.cancellation().cancelled_owned()=>{},
                result=this.run(&t,prompt,options,early)=> {if result.is_err() && this.active(&t) {let _=this.finish(&t,None,true).await;}}
            }
        });
        Ok(json!({"turnId":turn.id}))
    }
    async fn steer_active_turn(&self, _: Value, options: Value) -> Result<Value> {
        let mut s = self.inner.state.lock().unwrap();
        if !s
            .active
            .as_ref()
            .is_some_and(|t| options["expectedTurnId"] == t.id)
        {
            return Ok(json!({"status":"unavailable"}));
        }
        if s.steer_failures > 0 {
            s.steer_failures -= 1;
            bail!("Requested mock steer transport failure");
        }
        Ok(json!({"status":"accepted"}))
    }
    async fn stream_history(&self, sink: EventSink) -> Result<()> {
        let history = self.inner.state.lock().unwrap().history.clone();
        for e in history {
            sink.emit(e).await?;
        }
        Ok(())
    }
    async fn runtime_info(&self) -> Result<Value> {
        let s = self.inner.state.lock().unwrap();
        Ok(json!({"provider":"mock","sessionId":self.inner.id,"model":s.model,"modeId":s.mode}))
    }
    async fn available_modes(&self) -> Result<Vec<Value>> {
        Ok(modes())
    }
    async fn current_mode(&self) -> Result<Option<String>> {
        Ok(Some(self.inner.state.lock().unwrap().mode.clone()))
    }
    async fn set_mode(&self, mode: &str) -> Result<Option<Value>> {
        self.inner.state.lock().unwrap().mode = mode.into();
        Ok(None)
    }
    async fn respond_to_permission(
        &self,
        request_id: &str,
        response: Value,
    ) -> Result<Option<Value>> {
        let gate = self.inner.gate.lock().await;
        let turn = {
            let mut s = self.inner.state.lock().unwrap();
            let Some(index) = s.permissions.iter().position(|r| r["id"] == request_id) else {
                return Ok(None);
            };
            s.permissions.remove(index);
            s.active.clone()
        };
        let mut e = json!({"type":"permission_resolved","provider":"mock","requestId":request_id,"resolution":response});
        if let Some(t) = &turn {
            e["turnId"] = json!(t.id);
        }
        self.emit_locked(e).await?;
        drop(gate);
        if let Some(t) = turn {
            self.finish(&t, Some(scenario::usage(0, 0)), false).await?;
        }
        Ok(None)
    }
    async fn interrupt(&self) -> Result<()> {
        let _gate = self.inner.gate.lock().await;
        let turn = self.inner.state.lock().unwrap().active.take();
        if let Some(t) = turn {
            t.cancel.cancel();
            self.emit_locked(json!({"type":"turn_canceled","provider":"mock","reason":"Interrupted","turnId":t.id})).await?;
        }
        Ok(())
    }
    async fn close(&self) -> Result<()> {
        self.interrupt().await?;
        self.inner.state.lock().unwrap().closed = true;
        Ok(())
    }
    async fn list_commands(&self) -> Result<Vec<Value>> {
        Ok(commands())
    }
    async fn set_model(&self, model: Option<&str>) -> Result<()> {
        self.inner.state.lock().unwrap().model = model.unwrap_or(DEFAULT_MODEL).into();
        Ok(())
    }
    async fn set_thinking_option(&self, _: Option<&str>) -> Result<Option<Value>> {
        bail!("mock does not support changing thinking options")
    }
    async fn set_feature(&self, _: &str, _: Value) -> Result<()> {
        bail!("mock does not support changing features")
    }
    async fn rewind(&self, target: RewindTarget, message_id: &str) -> Result<()> {
        if let Some(e) = self.inner.features["mockRewindError"]
            .as_str()
            .filter(|s| !s.is_empty())
        {
            bail!("{e}");
        }
        let mut s = self.inner.state.lock().unwrap();
        if !s.history.iter().any(|e| {
            e["type"] == "timeline"
                && e["item"]["type"] == "user_message"
                && e["item"]["messageId"] == message_id
        }) {
            bail!("Mock rewind target {message_id} was not found in session history");
        }
        if target != RewindTarget::Files {
            let first = s
                .history
                .iter()
                .find(|e| e["type"] == "timeline" && e["item"]["type"] == "user_message")
                .cloned();
            s.history = first.into_iter().collect();
        }
        Ok(())
    }
    async fn try_out_of_band(&self, prompt: &Value) -> Result<bool> {
        if prompt.as_str() != Some("/mock handled-command") {
            return Ok(false);
        }
        let _gate = self.inner.gate.lock().await;
        self.emit_locked(json!({"type":"timeline","provider":"mock","item":{"type":"assistant_message","text":"Mock command handled"}})).await?;
        Ok(true)
    }
}
