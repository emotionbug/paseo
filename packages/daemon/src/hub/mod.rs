mod executions;
mod record;
mod remote;
#[cfg(test)]
mod tests;

use crate::{
    state::AppState,
    transport::{OutboundFrame, RequestContext},
};
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use executions::Executions;
use futures_util::{SinkExt, StreamExt};
use rand::RngCore;
use record::{Credential, Enrollment, Identity, Record, Relationship, Transport};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, Weak},
    time::Duration,
};
use tokio::{
    sync::{Mutex, Notify, OnceCell, mpsc},
    task::JoinHandle,
    time::Instant,
};
use tokio_tungstenite::tungstenite::{Message, protocol::CloseFrame};
use tokio_util::sync::CancellationToken;

pub struct HubService {
    inner: Arc<Inner>,
}
struct Inner {
    app: Weak<AppState>,
    state: Mutex<State>,
    transaction: Mutex<()>,
    enrollment: Mutex<()>,
    remote: OnceCell<remote::Remote>,
    worker: Mutex<Option<JoinHandle<()>>>,
    wake: Notify,
    stop: CancellationToken,
}
struct State {
    record: Option<Record>,
    state: &'static str,
    connected_at: Option<String>,
    last_error: Option<String>,
    generation: u64,
    lifecycle: CancellationToken,
    executions: Option<Arc<Executions>>,
    retry_attempt: u32,
    retry_at: Option<Instant>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            record: None,
            state: "not_connected",
            connected_at: None,
            last_error: None,
            generation: 0,
            lifecycle: CancellationToken::new(),
            executions: None,
            retry_attempt: 0,
            retry_at: None,
        }
    }
}
impl State {
    fn status(&self) -> Value {
        json!({"state":self.state,"daemonId":self.record.as_ref().map(|r| &r.relationship.daemon_id),
            "hubOrigin":self.record.as_ref().map(|r| &r.relationship.hub_origin),
            "permissions":self.record.as_ref().map(|r| r.relationship.permissions.as_slice()).unwrap_or(&[]),
            "connectedAt":self.connected_at,"lastError":self.last_error})
    }
    fn reset_lifecycle(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.lifecycle.cancel();
        self.lifecycle = CancellationToken::new();
        self.retry_attempt = 0;
        self.retry_at = None;
    }
    fn retry(&mut self, error: Option<String>) {
        self.state = "reconnecting";
        if let Some(error) = error {
            self.last_error = Some(error);
        }
        let base = 500u64
            .saturating_mul(
                1u64.checked_shl(self.retry_attempt.min(16))
                    .unwrap_or(u64::MAX),
            )
            .min(30_000);
        let millis = (base as f64 * (0.75 + rand::random::<f64>() * 0.5)).round() as u64;
        self.retry_attempt = self.retry_attempt.saturating_add(1);
        self.retry_at = Some(Instant::now() + Duration::from_millis(millis.max(1)));
    }
}
impl HubService {
    pub fn new(app: Weak<AppState>) -> Self {
        Self {
            inner: Arc::new(Inner {
                app,
                state: Mutex::new(State::default()),
                transaction: Mutex::new(()),
                enrollment: Mutex::new(()),
                remote: OnceCell::new(),
                worker: Mutex::new(None),
                wake: Notify::new(),
                stop: CancellationToken::new(),
            }),
        }
    }
    pub async fn initialize(&self) -> Result<()> {
        self.inner
            .remote
            .set(remote::Remote::new()?)
            .map_err(|_| anyhow::anyhow!("Hub service already initialized"))?;
        let record = record::load(&self.inner.path()?).await?;
        {
            let mut state = self.inner.state.lock().await;
            if let Some(record) = &record {
                state.state = if record.state == "revoked" {
                    "revoked"
                } else {
                    "connecting"
                };
                state.last_error = record.reason.clone();
                if record.state == "active" {
                    state.executions = Some(Executions::new(
                        self.inner.app.clone(),
                        record.relationship.daemon_id.clone(),
                    ));
                }
            }
            state.record = record;
        }
        // Pending credentials are already durable and enrollment is explicitly idempotent.
        if let Err(error) = self.inner.enroll_pending().await {
            if error.downcast_ref::<remote::EnrollmentRejected>().is_none() {
                return Err(error);
            }
        }
        let inner = self.inner.clone();
        *self.inner.worker.lock().await = Some(tokio::spawn(async move {
            if let Err(error) = inner.run().await {
                tracing::error!(%error,"Hub relationship worker stopped");
            }
        }));
        Ok(())
    }
    pub async fn status(&self) -> Value {
        self.inner.state.lock().await.status()
    }
    pub async fn connect(&self, input: &Value) -> Result<Value> {
        let permissions = record::permissions(&input["permissions"])?;
        let origin = record::normalize_url(input["hubUrl"].as_str().context("Missing Hub URL")?)?;
        let token = input["token"]
            .as_str()
            .filter(|s| !s.is_empty())
            .context("Missing Hub enrollment token")?;
        let _transaction = self.inner.transaction.lock().await;
        let current = self.inner.state.lock().await.record.clone();
        let pending = if let Some(mut record) = current.filter(|r| r.state != "revoked") {
            ensure!(
                record.state == "pending",
                "This daemon already has a Hub relationship"
            );
            ensure!(
                record.relationship.hub_origin == origin,
                "A pending Hub enrollment already exists for a different Hub"
            );
            record.enrollment = Some(Enrollment {
                token: token.to_owned(),
            });
            record
        } else {
            let app = self
                .inner
                .app
                .upgrade()
                .context("Daemon is shutting down")?;
            let identity = app.transport.identity();
            drop(app);
            let mut secret = [0u8; 32];
            rand::rngs::OsRng.fill_bytes(&mut secret);
            Record {
                version: 2,
                state: "pending".into(),
                relationship: Relationship {
                    daemon_id: uuid::Uuid::new_v4().to_string(),
                    idempotency_key: Some(uuid::Uuid::new_v4().to_string()),
                    hub_origin: origin,
                    created_at: now(),
                    permissions,
                },
                credential: Some(Credential {
                    secret: URL_SAFE_NO_PAD.encode(secret),
                }),
                enrollment: Some(Enrollment {
                    token: token.to_owned(),
                }),
                identity: Some(Identity {
                    server_id: identity["serverId"]
                        .as_str()
                        .context("Missing daemon identity")?
                        .to_owned(),
                    daemon_public_key: identity["daemonPublicKey"]
                        .as_str()
                        .context("Missing daemon public key")?
                        .to_owned(),
                }),
                transport: None,
                reason: None,
            }
        };
        record::persist(&self.inner.path()?, &pending).await?;
        {
            let mut state = self.inner.state.lock().await;
            state.reset_lifecycle();
            state.record = Some(pending);
            state.state = "connecting";
            state.last_error = None;
        }
        drop(_transaction);
        let result = self.inner.enroll_pending().await;
        self.inner.wake.notify_one();
        result?;
        Ok(self.status().await)
    }
    pub async fn update_permissions(&self, input: &Value) -> Result<Value> {
        let grant = record::permissions(&input["grant"])?;
        let revoke = record::permissions(&input["revoke"])?;
        let _transaction = self.inner.transaction.lock().await;
        let mut record = self
            .inner
            .state
            .lock()
            .await
            .record
            .clone()
            .filter(|r| r.state == "active")
            .context("This daemon is not connected to a Hub")?;
        let mut permissions: Vec<_> = record
            .relationship
            .permissions
            .iter()
            .filter(|p| !revoke.contains(p))
            .cloned()
            .collect();
        for p in grant {
            if !permissions.contains(&p) {
                permissions.push(p);
            }
        }
        self.inner.remote()?.update(&record, &permissions).await?;
        record.relationship.permissions = permissions;
        record::persist(&self.inner.path()?, &record).await?;
        let principal = record.principal_id()?;
        let permissions = record.relationship.permissions.clone();
        self.inner.state.lock().await.record = Some(record);
        let app = self
            .inner
            .app
            .upgrade()
            .context("Daemon is shutting down")?;
        app.transport
            .update_hub_permissions(&principal, &permissions)
            .await?;
        Ok(self.status().await)
    }
    pub async fn disconnect(&self, force: bool) -> Result<Value> {
        let _transaction = self.inner.transaction.lock().await;
        let (prior, generation, executions) = {
            let mut state = self.inner.state.lock().await;
            state.reset_lifecycle();
            let executions = state.executions.take();
            if let Some(executions) = &executions {
                executions.cancel.cancel();
            }
            (state.record.clone(), state.generation, executions)
        };
        let wait_enrollment = prior.as_ref().is_some_and(|r| r.state == "pending");
        let disconnecting = prior.filter(|r| r.state != "revoked").map(|mut r| {
            r.state = "disconnecting".into();
            r.enrollment = None;
            r.identity = None;
            r
        });
        if let Some(record) = &disconnecting {
            record::persist(&self.inner.path()?, record).await?;
            let mut state = self.inner.state.lock().await;
            state.record = Some(record.clone());
            state.state = "disconnecting";
        }
        drop(_transaction);
        self.inner.wake.notify_one();
        if wait_enrollment && !force {
            drop(self.inner.enrollment.lock().await);
        }
        let warning = if let Some(record) = &disconnecting {
            if !force && self.inner.remote()?.revoke(record).await.is_err() {
                Some(
                    "Hub could not be reached; local relationship removed, but server-side revocation may remain pending.",
                )
            } else {
                None
            }
        } else {
            None
        };
        let _transaction = self.inner.transaction.lock().await;
        ensure!(
            self.inner.state.lock().await.generation == generation,
            "Hub relationship changed during disconnect"
        );
        record::remove(&self.inner.path()?).await?;
        {
            let mut state = self.inner.state.lock().await;
            state.record = None;
            state.state = "not_connected";
            state.connected_at = None;
            state.last_error = None;
        }
        drop(_transaction);
        if let Some(executions) = executions {
            executions.invalidate().await;
        }
        let mut result = json!({"status":self.status().await});
        if let Some(warning) = warning {
            result["warning"] = warning.into();
        }
        Ok(result)
    }
    pub async fn handle(&self, context: &RequestContext, message: &Value) -> Result<bool> {
        let kind = message["type"].as_str().unwrap_or("");
        if matches!(
            kind,
            "hub.management.daemon.connect.request"
                | "hub.management.daemon.get_status.request"
                | "hub.management.daemon.disconnect.request"
                | "hub.management.daemon.permissions.update.request"
        ) {
            let result: Result<Value> = async {
                ensure!(
                    context.allows(
                        if kind == "hub.management.daemon.permissions.update.request" {
                            "access.manage"
                        } else {
                            "tunnel.manage"
                        }
                    ),
                    "Hub management operation is not permitted"
                );
                match kind {
                    "hub.management.daemon.connect.request" => {
                        Ok(json!({"status":self.connect(message).await?}))
                    }
                    "hub.management.daemon.permissions.update.request" => {
                        Ok(json!({"status":self.update_permissions(message).await?}))
                    }
                    "hub.management.daemon.disconnect.request" => {
                        self.disconnect(message["force"].as_bool().unwrap_or(false))
                            .await
                    }
                    _ => Ok(json!({"status":self.status().await})),
                }
            }
            .await;
            let response = match result {
                Ok(mut payload) => {
                    payload["requestId"] = message["requestId"].clone();
                    json!({"type":kind.replace(".request",".response"),"payload":payload})
                }
                Err(error) => {
                    json!({"type":"rpc_error","payload":{"requestId":message["requestId"],"requestType":kind,"error":error.to_string(),"code":"handler_error"}})
                }
            };
            context.send(response).await?;
            return Ok(true);
        }
        if !matches!(
            kind,
            "hub.execution.agent.create.request"
                | "hub.execution.agent.validate.request"
                | "hub.execution.control.request"
        ) {
            return Ok(false);
        }
        let executions = self.authority(context).await?;
        let payload = match kind {
            "hub.execution.agent.create.request" => match executions.create(message).await {
                Ok(agent) => {
                    let mut value = json!({"requestId":message["requestId"],"executionId":message["executionId"],"agentId":agent["id"],"agent":agent,"success":true,"error":null});
                    if message.get("toolPolicy").is_some() {
                        value["toolPolicyApplied"] = true.into();
                    }
                    value
                }
                Err(error) => {
                    json!({"requestId":message["requestId"],"executionId":message["executionId"],"agentId":null,"agent":null,"success":false,"error":error})
                }
            },
            "hub.execution.control.request" => {
                let result = executions.control(message).await;
                json!({"requestId":message["requestId"],"executionId":message["executionId"],"action":message["action"],"success":result.is_ok(),"error":result.err()})
            }
            _ => {
                let app = self
                    .inner
                    .app
                    .upgrade()
                    .context("Daemon is shutting down")?;
                match app.agents.hub_validate(message).await {
                    Ok(issues) => {
                        json!({"requestId":message["requestId"],"valid":issues.as_array().is_some_and(Vec::is_empty),"issues":issues,"error":null})
                    }
                    Err(error) => {
                        json!({"requestId":message["requestId"],"valid":false,"issues":[],"error":error.to_string()})
                    }
                }
            }
        };
        if !executions.cancel.is_cancelled() && !context.cancellation().is_cancelled() {
            context
                .send(json!({"type":kind.replace(".request",".response"),"payload":payload}))
                .await?;
        }
        Ok(true)
    }
    async fn authority(&self, context: &RequestContext) -> Result<Arc<Executions>> {
        ensure!(
            context.principal()["kind"] == "hub" && context.allows("hub.execute"),
            "Hub execution authority is required"
        );
        self.relationship_executions(context).await
    }
    async fn relationship_executions(&self, context: &RequestContext) -> Result<Arc<Executions>> {
        ensure!(
            context.principal()["kind"] == "hub",
            "Hub execution authority is required"
        );
        let state = self.inner.state.lock().await;
        let record = state
            .record
            .as_ref()
            .filter(|r| r.state == "active")
            .context("Hub relationship authority is no longer active")?;
        ensure!(
            context.principal()["id"].as_str() == Some(record.principal_id()?.as_str()),
            "Hub execution belongs to another relationship"
        );
        state
            .executions
            .clone()
            .context("Hub execution authority is no longer active")
    }
    pub async fn observe(&self, context: &RequestContext) -> Result<()> {
        self.relationship_executions(context)
            .await?
            .observe(context)
            .await
    }
    pub async fn replay(&self, context: &RequestContext) -> Result<()> {
        if !context.allows("hub.execute") {
            return Ok(());
        }
        self.authority(context).await?.replay(context).await
    }
    pub async fn permissions_for(&self, principal_id: &str) -> Result<Vec<String>> {
        ensure!(!self.inner.stop.is_cancelled(), "Daemon is shutting down");
        let state = self.inner.state.lock().await;
        let record = state
            .record
            .as_ref()
            .filter(|r| r.state == "active")
            .context("Hub relationship authority is no longer active")?;
        ensure!(
            record.principal_id()? == principal_id
                && state
                    .executions
                    .as_ref()
                    .is_some_and(|e| !e.cancel.is_cancelled()),
            "Hub relationship authority is no longer active"
        );
        Ok(record.relationship.permissions.clone())
    }
    pub async fn shutdown(&self) -> Result<()> {
        self.inner.stop.cancel();
        let executions = {
            let mut state = self.inner.state.lock().await;
            state.reset_lifecycle();
            state.executions.take()
        };
        if let Some(executions) = executions {
            executions.invalidate().await;
        }
        if let Some(worker) = self.inner.worker.lock().await.take() {
            worker.await?;
        }
        Ok(())
    }
}
impl Inner {
    fn path(&self) -> Result<PathBuf> {
        Ok(self
            .app
            .upgrade()
            .context("Daemon is shutting down")?
            .home
            .join("hub-relationship.json"))
    }
    fn remote(&self) -> Result<&remote::Remote> {
        self.remote.get().context("Hub service is not initialized")
    }
    async fn enroll_pending(&self) -> Result<()> {
        let _enrollment = self.enrollment.lock().await;
        let (pending, generation) = {
            let state = self.state.lock().await;
            let Some(pending) = state.record.as_ref().filter(|r| r.state == "pending") else {
                return Ok(());
            };
            (pending.clone(), state.generation)
        };
        let result = self.remote()?.enroll(&pending).await;
        let _transaction = self.transaction.lock().await;
        if self.state.lock().await.generation != generation {
            return Ok(());
        }
        match result {
            Ok(url) => {
                let mut active = pending;
                active.state = "active".into();
                active.enrollment = None;
                active.identity = None;
                active.transport = Some(Transport {
                    kind: "direct_websocket".into(),
                    web_socket_url: url,
                });
                record::persist(&self.path()?, &active).await?;
                let mut state = self.state.lock().await;
                state.executions = Some(Executions::new(
                    self.app.clone(),
                    active.relationship.daemon_id.clone(),
                ));
                state.record = Some(active);
                state.retry_attempt = 0;
                state.retry_at = None;
                state.state = "connecting";
                Ok(())
            }
            Err(error) if error.downcast_ref::<remote::EnrollmentRejected>().is_some() => {
                record::remove(&self.path()?).await?;
                let mut state = self.state.lock().await;
                state.reset_lifecycle();
                state.record = None;
                state.state = "not_connected";
                state.connected_at = None;
                state.last_error = None;
                Err(error)
            }
            Err(error) => {
                self.state.lock().await.retry(Some(error.to_string()));
                Ok(())
            }
        }
    }
    async fn run(self: &Arc<Self>) -> Result<()> {
        loop {
            if self.stop.is_cancelled() {
                return Ok(());
            }
            let wake = self.wake.notified();
            let (record, generation, lifecycle, retry_at) = {
                let state = self.state.lock().await;
                (
                    state.record.clone(),
                    state.generation,
                    state.lifecycle.clone(),
                    state.retry_at,
                )
            };
            let Some(record) = record.filter(|r| matches!(r.state.as_str(), "pending" | "active"))
            else {
                tokio::select! { _ = self.stop.cancelled() => return Ok(()), _ = wake => continue }
            };
            if let Some(deadline) = retry_at {
                tokio::select! { _ = self.stop.cancelled() => return Ok(()), _ = lifecycle.cancelled() => continue, _ = wake => continue, _ = tokio::time::sleep_until(deadline) => {} }
            }
            if record.state == "pending" {
                if let Err(error) = self.enroll_pending().await {
                    if error.downcast_ref::<remote::EnrollmentRejected>().is_none() {
                        return Err(error);
                    }
                }
                continue;
            }
            let opened = tokio::select! {
                _ = self.stop.cancelled() => return Ok(()),
                _ = lifecycle.cancelled() => continue,
                result = self.remote()?.open(&record) => result,
            };
            let result = match opened {
                Ok((socket, session_v1)) => {
                    {
                        let mut state = self.state.lock().await;
                        if state.generation != generation {
                            continue;
                        }
                        state.state = "connected";
                        state.connected_at = Some(now());
                        state.last_error = None;
                        state.retry_attempt = 0;
                        state.retry_at = None;
                    }
                    self.socket(socket, session_v1, &record, lifecycle.clone())
                        .await
                }
                Err(error) => Err(error),
            };
            if self.state.lock().await.generation != generation {
                continue;
            }
            match result {
                Ok(Some(4403)) => {
                    self.revoke(generation, "Hub revoked this relationship".into())
                        .await?
                }
                Err(error) if error.downcast_ref::<remote::SocketRejected>().is_some() => {
                    self.revoke(generation, error.to_string()).await?
                }
                Err(error) => self.state.lock().await.retry(Some(error.to_string())),
                Ok(_) => self.state.lock().await.retry(None),
            }
        }
    }
    async fn revoke(&self, generation: u64, reason: String) -> Result<()> {
        let _transaction = self.transaction.lock().await;
        let mut record = {
            let state = self.state.lock().await;
            if state.generation != generation {
                return Ok(());
            }
            state.record.clone().context("Missing Hub relationship")?
        };
        let executions = {
            let mut state = self.state.lock().await;
            state.reset_lifecycle();
            let executions = state.executions.take();
            if let Some(executions) = &executions {
                executions.cancel.cancel();
            }
            executions
        };
        record.state = "revoked".into();
        record.credential = None;
        record.enrollment = None;
        record.identity = None;
        record.relationship.idempotency_key = None;
        record.reason = Some(reason.clone());
        record::persist(&self.path()?, &record).await?;
        {
            let mut state = self.state.lock().await;
            state.record = Some(record);
            state.state = "revoked";
            state.last_error = Some(reason);
        }
        drop(_transaction);
        if let Some(executions) = executions {
            executions.invalidate().await;
        }
        Ok(())
    }
    async fn socket(
        &self,
        mut socket: remote::Socket,
        session_v1: bool,
        record: &Record,
        lifecycle: CancellationToken,
    ) -> Result<Option<u16>> {
        let app = self.app.upgrade().context("Daemon is shutting down")?;
        let transport = app.transport.clone();
        drop(app);
        let permissions = self
            .state
            .lock()
            .await
            .record
            .as_ref()
            .context("Missing Hub relationship")?
            .relationship
            .permissions
            .clone();
        let principal = record.principal_id()?;
        let cancel = lifecycle.child_token();
        let (incoming, receiver) = mpsc::channel(64);
        let (outgoing, mut sender) = mpsc::channel(64);
        let bridge = transport.attach_hub(
            &principal,
            &permissions,
            session_v1,
            receiver,
            outgoing,
            cancel.clone(),
        );
        tokio::pin!(bridge);
        let mut heartbeat = tokio::time::interval_at(
            Instant::now() + Duration::from_secs(10),
            Duration::from_secs(10),
        );
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last_pong = Instant::now();
        let result: Result<Option<u16>> = async {
            loop {
                tokio::select! { biased;
                    _ = self.stop.cancelled() => return Ok(None),
                    _ = cancel.cancelled() => return Ok(None),
                    result = &mut bridge => { result?; return Ok(None); }
                    _ = heartbeat.tick() => {
                        ensure!(last_pong.elapsed() < Duration::from_secs(45),"Hub WebSocket heartbeat timed out");
                        send_socket(&mut socket,Message::Ping(bytes::Bytes::new())).await?;
                    }
                    frame = sender.recv() => {
                        let Some(frame) = frame else { return Ok(None); };
                        let message = match frame {
                            OutboundFrame::Text(text) => Message::Text(text.into()),
                            OutboundFrame::Binary(bytes) => Message::Binary(bytes),
                            OutboundFrame::Ping(bytes) => Message::Ping(bytes),
                            OutboundFrame::Pong(bytes) => Message::Pong(bytes),
                            OutboundFrame::Close { code,reason } => Message::Close(Some(CloseFrame { code:code.into(),reason:reason.into() })),
                            OutboundFrame::Flush(acknowledge) => {
                                let flushed: Result<()> = async {
                                    tokio::time::timeout(Duration::from_secs(45),socket.flush())
                                        .await.context("Hub WebSocket flush timed out")??;
                                    Ok(())
                                }.await;
                                let failure = flushed.as_ref().err().map(ToString::to_string);
                                let _ = acknowledge.send(flushed);
                                if let Some(error) = failure { return Err(anyhow::anyhow!(error)); }
                                continue;
                            }
                        };
                        send_socket(&mut socket,message).await?;
                    }
                    frame = socket.next() => {
                        let Some(frame) = frame else { return Ok(Some(1006)); };
                        let frame = match frame? {
                            Message::Pong(_) => { last_pong = Instant::now(); continue; }
                            Message::Ping(bytes) => { send_socket(&mut socket,Message::Pong(bytes)).await?; continue; }
                            Message::Close(frame) => return Ok(Some(frame.map(|f| u16::from(f.code)).unwrap_or(1005))),
                            Message::Text(text) => OutboundFrame::Text(text.to_string()),
                            Message::Binary(bytes) => OutboundFrame::Binary(bytes),
                            Message::Frame(_) => continue,
                        };
                        // Never block bridge polling behind an overflowing inbound queue.
                        incoming.try_send(frame).map_err(|_| anyhow::anyhow!("Hub incoming frame queue exceeded its bound"))?;
                    }
                }
            }
        }.await;
        cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(1), socket.close(None)).await;
        result
    }
}
fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
async fn send_socket(socket: &mut remote::Socket, message: Message) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(45), socket.send(message))
        .await
        .context("Hub WebSocket write timed out")??;
    Ok(())
}
