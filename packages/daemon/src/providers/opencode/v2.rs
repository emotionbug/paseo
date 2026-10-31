mod notices;
mod permissions;
mod session;
mod timeline;

use super::{
    catalog,
    runtime::{Runtime, RuntimeManager},
};
use crate::providers::api::{AgentSession, EventSink, LaunchContext, ProviderSpec, RewindTarget};
use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use serde_json::{Value, json};
use session::Session;
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

pub(super) fn s<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}
pub(super) fn array(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or(&[])
}
pub(super) fn component(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}
pub(super) fn session_path(id: &str, suffix: &str) -> String {
    format!("/api/session/{}{suffix}", component(id))
}
pub(super) fn usage(info: &Value) -> Value {
    json!({"inputTokens":info["tokens"]["input"],"outputTokens":info["tokens"]["output"],"cachedInputTokens":info["tokens"]["cache"]["read"],"totalCostUsd":info["cost"]})
}
#[derive(Clone)]
struct Snapshot {
    info: Value,
    config: Value,
    pending: Vec<Value>,
    modes: Vec<Value>,
    features: Arc<[Value]>,
}
struct Call {
    operation: Operation,
    reply: oneshot::Sender<Result<Value>>,
}
enum Operation {
    Start(Value, Value),
    Steer(Value, Value),
    History(EventSink),
    RuntimeInfo,
    Mode(String),
    Permission(String, Value),
    Interrupt,
    Commands,
    Model(Option<String>),
    Thinking(Option<String>),
    Feature(String, Value),
    Rewind(String),
}
struct V2Session {
    provider: String,
    id: String,
    capabilities: Value,
    initial: Vec<Value>,
    snapshot: Arc<Mutex<Snapshot>>,
    sender: mpsc::Sender<Call>,
    cancel: CancellationToken,
    closed: CancellationToken,
    close_error: Arc<Mutex<Option<String>>>,
}
impl V2Session {
    async fn call(&self, operation: Operation) -> Result<Value> {
        let (reply, receiver) = oneshot::channel();
        tokio::select! { _=self.cancel.cancelled()=>bail!("OpenCode session is closed"), result=self.sender.send(Call{operation,reply})=>result.map_err(|_|anyhow!("OpenCode session is closed"))? }
        receiver
            .await
            .map_err(|_| anyhow!("OpenCode session owner closed"))?
    }
}
impl Drop for V2Session {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
pub async fn attach(
    runtime: Arc<Runtime>,
    manager: Arc<RuntimeManager>,
    spec: ProviderSpec,
    info: Value,
    mut config: Value,
    context: LaunchContext,
    options: Value,
    sink: EventSink,
) -> Result<Arc<dyn AgentSession>> {
    let id = s(&info, "id").to_owned();
    if id.is_empty() {
        bail!("OpenCode session response omitted its id");
    }
    let initial = notices::initialize(&mut config)?;
    let snapshot = Arc::new(Mutex::new(Snapshot {
        info: info.clone(),
        config: config.clone(),
        pending: Vec::new(),
        modes: Vec::new(),
        features: catalog::features_v2(&config).into(),
    }));
    let (sender, receiver) = mpsc::channel(64);
    let cancel = context.cancel.child_token();
    let closed = CancellationToken::new();
    let close_error = Arc::new(Mutex::new(None));
    let facade = Arc::new(V2Session {
        provider: spec.id.clone(),
        id: id.clone(),
        capabilities: json!({"supportsStreaming":true,"supportsSessionPersistence":true,"supportsSessionListing":true,"supportsDynamicModes":true,"supportsMcpServers":true,"supportsReasoningStream":true,"supportsToolInvocations":true,"supportsRewindBoth":true,"supportsRewindConversation":false,"supportsRewindFiles":false}),
        initial,
        snapshot: snapshot.clone(),
        sender,
        cancel: cancel.clone(),
        closed: closed.clone(),
        close_error: close_error.clone(),
    });
    let actor = Session::new(
        runtime,
        manager,
        id,
        info,
        config,
        context,
        options,
        sink,
        snapshot,
        cancel.clone(),
        closed,
        close_error,
    );
    let (ready, initialized) = oneshot::channel();
    tokio::spawn(actor.run(receiver, ready));
    match initialized.await {
        Ok(Ok(())) => Ok(facade),
        Ok(Err(error)) => {
            cancel.cancel();
            Err(error)
        }
        Err(_) => {
            cancel.cancel();
            Err(anyhow!("OpenCode session initialization failed"))
        }
    }
}
#[async_trait]
impl AgentSession for V2Session {
    fn provider(&self) -> &str {
        &self.provider
    }
    fn native_id(&self) -> Option<String> {
        Some(self.id.clone())
    }
    fn capabilities(&self) -> &Value {
        &self.capabilities
    }
    fn features(&self) -> Arc<[Value]> {
        self.snapshot.lock().unwrap().features.clone()
    }
    fn initial_timeline(&self) -> &[Value] {
        &self.initial
    }
    fn describe_persistence(&self) -> Option<Value> {
        Some(
            json!({"provider":self.provider,"sessionId":self.id,"nativeHandle":self.id,"metadata":self.snapshot.lock().unwrap().config}),
        )
    }
    fn pending_permissions(&self) -> Vec<Value> {
        self.snapshot.lock().unwrap().pending.clone()
    }
    fn usage_session(&self) -> Option<Value> {
        None
    }
    async fn start_turn(&self, prompt: Value, options: Value) -> Result<Value> {
        self.call(Operation::Start(prompt, options)).await
    }
    async fn steer_active_turn(&self, prompt: Value, options: Value) -> Result<Value> {
        self.call(Operation::Steer(prompt, options)).await
    }
    async fn stream_history(&self, sink: EventSink) -> Result<()> {
        self.call(Operation::History(sink)).await?;
        Ok(())
    }
    async fn runtime_info(&self) -> Result<Value> {
        self.call(Operation::RuntimeInfo).await
    }
    async fn available_modes(&self) -> Result<Vec<Value>> {
        Ok(self.snapshot.lock().unwrap().modes.clone())
    }
    async fn current_mode(&self) -> Result<Option<String>> {
        Ok(self.snapshot.lock().unwrap().info["agent"]
            .as_str()
            .map(str::to_owned))
    }
    async fn set_mode(&self, mode: &str) -> Result<Option<Value>> {
        self.call(Operation::Mode(mode.to_owned())).await?;
        Ok(None)
    }
    async fn respond_to_permission(&self, id: &str, response: Value) -> Result<Option<Value>> {
        self.call(Operation::Permission(id.to_owned(), response))
            .await?;
        Ok(None)
    }
    async fn interrupt(&self) -> Result<()> {
        self.call(Operation::Interrupt).await?;
        Ok(())
    }
    async fn close(&self) -> Result<()> {
        self.cancel.cancel();
        self.closed.cancelled().await;
        if let Some(error) = self.close_error.lock().unwrap().as_ref() {
            bail!("{error}");
        }
        Ok(())
    }
    async fn list_commands(&self) -> Result<Vec<Value>> {
        Ok(array(&self.call(Operation::Commands).await?).to_vec())
    }
    async fn set_model(&self, model: Option<&str>) -> Result<()> {
        self.call(Operation::Model(model.map(str::to_owned)))
            .await?;
        Ok(())
    }
    async fn set_thinking_option(&self, option: Option<&str>) -> Result<Option<Value>> {
        self.call(Operation::Thinking(option.map(str::to_owned)))
            .await?;
        Ok(None)
    }
    async fn set_feature(&self, feature: &str, value: Value) -> Result<()> {
        self.call(Operation::Feature(feature.to_owned(), value))
            .await?;
        Ok(())
    }
    async fn rewind(&self, target: RewindTarget, message_id: &str) -> Result<()> {
        if target != RewindTarget::Both {
            bail!("OpenCode only supports rewinding conversation and files together");
        }
        self.call(Operation::Rewind(message_id.to_owned())).await?;
        Ok(())
    }
    async fn try_out_of_band(&self, _prompt: &Value) -> Result<bool> {
        Ok(false)
    }
}
