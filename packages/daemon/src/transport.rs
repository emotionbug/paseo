mod context;
mod features;
#[path = "hosts/mod.rs"]
pub mod hosts;
mod http;
#[path = "hub/mod.rs"]
pub mod hub;
mod identity;
pub mod permissions;
mod relay;
pub mod storage;

pub use context::{
    MAX_BUFFERED_BYTES, MAX_FRAME_BYTES, OutboundFrame, OwnedOperation, OwnedSubscription,
    RequestContext, has_capability, message_type, request_id,
};
pub use identity::{generate_local_pairing_offer, hash_daemon_password, offline_pairing_offer};

use crate::{relay_crypto::KeyPair, state::AppState};
use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use rand::RngCore;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, RwLock, Weak},
    time::Duration,
};
use subtle::ConstantTimeEq;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{Mutex as AsyncMutex, mpsc},
    task::{JoinHandle, JoinSet},
    time::Instant,
};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{Message, protocol::CloseFrame},
};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct Admission {
    pub principal: Value,
    pub permissions: Vec<String>,
}
impl Admission {
    pub fn owner() -> Self {
        Self {
            principal: json!({"id":"owner","kind":"owner"}),
            permissions: permissions::OWNER_PERMISSIONS
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
        }
    }
}

struct Session {
    id: String,
    permissions: Arc<RwLock<HashSet<String>>>,
    sources: HashMap<String, Weak<context::Source>>,
    retained_until: Option<Instant>,
    ephemeral: bool,
    permission_ceiling: Option<HashSet<String>>,
}
#[derive(Default)]
struct Sessions {
    sessions: HashMap<String, Session>,
    principal_grants: HashMap<String, Vec<String>>,
}

pub struct TransportService {
    app: Weak<AppState>,
    pub hosts: Arc<hosts::HostsService>,
    pub hub: Arc<hub::HubService>,
    relay: Arc<relay::RelayService>,
    identity: RwLock<Value>,
    keypair: RwLock<Option<Arc<KeyPair>>>,
    local_credential: RwLock<Option<String>>,
    config: RwLock<Value>,
    listen: RwLock<Value>,
    endpoint: RwLock<Option<String>>,
    listener: AsyncMutex<Option<http::HttpListener>>,
    lifecycle: AsyncMutex<()>,
    sessions: Mutex<Sessions>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    cancel: CancellationToken,
}

struct SourceGuard<'a> {
    transport: &'a TransportService,
    context: &'a RequestContext,
    key: &'a str,
    completed: bool,
}
impl Drop for SourceGuard<'_> {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        self.transport.detach_registration(self.context, self.key);
        let weak = self.transport.app.clone();
        let source = self.context.source_id().to_owned();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let Some(app) = weak.upgrade() else {
                    return;
                };
                let hosts = app.transport.hosts.clone();
                let speech = app.speech.clone();
                drop(app);
                hosts.detach_source(&source).await;
                let _ = speech.detach_source(&source).await;
            });
        }
    }
}

impl TransportService {
    pub fn new(app: Weak<AppState>) -> Self {
        Self {
            hosts: Arc::new(hosts::HostsService::new(app.clone())),
            hub: Arc::new(hub::HubService::new(app.clone())),
            relay: Arc::new(relay::RelayService::new(app.clone())),
            app,
            identity: RwLock::new(Value::Null),
            keypair: RwLock::new(None),
            local_credential: RwLock::new(None),
            config: RwLock::new(Value::Null),
            listen: RwLock::new(Value::Null),
            endpoint: RwLock::new(None),
            listener: AsyncMutex::new(None),
            lifecycle: AsyncMutex::new(()),
            sessions: Mutex::new(Sessions::default()),
            tasks: Mutex::new(Vec::new()),
            cancel: CancellationToken::new(),
        }
    }
    pub fn identity(&self) -> Value {
        self.identity.read().unwrap().clone()
    }
    pub fn daemon_keypair(&self) -> Result<Arc<KeyPair>> {
        self.keypair
            .read()
            .unwrap()
            .clone()
            .ok_or_else(|| anyhow!("Transport identity is not initialized"))
    }
    pub fn config_snapshot(&self) -> Value {
        self.config.read().unwrap().clone()
    }
    pub fn local_http_endpoint(&self) -> Option<String> {
        self.endpoint.read().unwrap().clone()
    }
    pub fn listen(&self) -> Value {
        self.listen.read().unwrap().clone()
    }
    pub fn service_proxy_config(&self) -> Value {
        let listen = self.listen();
        let config = self.config_snapshot();
        json!({"daemonPort":listen.get("port"),"daemonListenHost":listen.get("host"),"publicBaseUrl":config["serviceProxy"].get("publicBaseUrl")})
    }
    /// Establish identity and reserve listeners without admitting clients during domain initialization.
    pub async fn prepare(&self) -> Result<()> {
        let _guard = self.lifecycle.lock().await;
        if !self.identity.read().unwrap().is_null() {
            bail!("Transport is already prepared");
        }
        let app = self
            .app
            .upgrade()
            .ok_or_else(|| anyhow!("Daemon is shutting down"))?;
        let config = app.config.snapshot();
        let env = app.config.environment();
        let home = app.home.clone();
        let transport = app.transport.clone();
        drop(app);
        tokio::fs::create_dir_all(&home).await?;
        let (identity, keypair) = identity::load(&home, &env).await?;
        *self.identity.write().unwrap() = identity;
        *self.keypair.write().unwrap() = Some(keypair);
        *self.config.write().unwrap() = config;
        let mut credential_bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut credential_bytes);
        let credential = URL_SAFE_NO_PAD.encode(credential_bytes);
        storage::write_private(
            &home.join("local-credential"),
            format!("{credential}\n").as_bytes(),
        )
        .await?;
        *self.local_credential.write().unwrap() = Some(credential);
        let listener = http::bind(self.app.clone(), transport, self.cancel.child_token()).await?;
        *self.listen.write().unwrap() = listener.listen();
        *self.endpoint.write().unwrap() = listener.local_http_endpoint();
        *self.listener.lock().await = Some(listener);
        self.hosts.prepare().await?;
        Ok(())
    }
    pub async fn start(&self) -> Result<()> {
        let _guard = self.lifecycle.lock().await;
        if self.identity.read().unwrap().is_null() {
            bail!("Transport prepare must precede start");
        }
        self.hosts.initialize().await?;
        self.hub.initialize().await?;
        self.relay.initialize().await?;
        let listener = self
            .listener
            .lock()
            .await
            .take()
            .ok_or_else(|| anyhow!("Transport listener already started"))?;
        let cancel = self.cancel.clone();
        let weak = self.app.clone();
        self.tasks.lock().unwrap().push(tokio::spawn(async move {
            if let Err(error) = listener.start().await {
                tracing::error!(%error,"HTTP transport stopped");
                cancel.cancel();
                if let Some(app) = weak.upgrade() {
                    app.request_failure(error);
                }
            }
        }));
        let weak = self.app.clone();
        let cancel = self.cancel.clone();
        self.tasks.lock().unwrap().push(tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(10));
            loop { tokio::select! { _ = cancel.cancelled() => break, _ = interval.tick() => {
                let Some(app) = weak.upgrade() else { break; }; let transport = app.transport.clone(); let app_cancel = app.cancel.clone(); drop(app);
                if app_cancel.is_cancelled() { cancel.cancel(); break; }
                transport.sessions.lock().unwrap().sessions.retain(|_, s| s.retained_until.is_none_or(|until| until > Instant::now()));
            } } }
        }));
        if let Some(app) = self.app.upgrade() {
            let mut readiness = app.speech.subscribe_readiness();
            let weak = self.app.clone();
            let cancel = self.cancel.clone();
            drop(app);
            self.tasks.lock().unwrap().push(tokio::spawn(async move {
                loop { tokio::select! { _ = cancel.cancelled() => break, changed = readiness.changed() => {
                    if changed.is_err() { break; }
                    let Some(app) = weak.upgrade() else { break; }; let transport = app.transport.clone(); drop(app);
                    for source in transport.live_sources() {
                        if source.wants_event("status.server_info") {
                            if let Ok(info) = transport.server_info(&source).await { let _ = source.emit_event(info).await; }
                        }
                    }
                } } }
            }));
        }
        Ok(())
    }
    pub async fn initialize(&self) -> Result<()> {
        self.prepare().await?;
        self.start().await
    }
    pub async fn apply_config(&self, next: &Value, previous: &Value) -> Result<()> {
        self.relay.apply_config(next, previous).await?;
        let mut config = self.config.write().unwrap();
        for key in [
            "hostnames",
            "corsAllowedOrigins",
            "trustedProxies",
            "relayEnabled",
            "relayRequireProtectedSecurity",
        ] {
            if let Some(value) = next.get(key) {
                config[key] = value.clone();
            } else if let Some(map) = config.as_object_mut() {
                map.remove(key);
            }
        }
        Ok(())
    }
    pub async fn publish_daemon_config_changed(&self, config: Value) -> Result<()> {
        self.publish_event(
            json!({"type":"status","payload":{"status":"daemon_config_changed","config":config}}),
        )
        .await
    }
    pub async fn authenticate_bearer(&self, authorization: Option<&str>) -> bool {
        let config = self.config_snapshot();
        let Some(hash) = config["auth"]["password"]
            .as_str()
            .filter(|hash| !hash.is_empty())
        else {
            return true;
        };
        let Some(token) = authorization.and_then(extract_http_bearer_token) else {
            return false;
        };
        let hash = hash.to_owned();
        let token = token.to_owned();
        tokio::task::spawn_blocking(move || bcrypt::verify(token, &hash).unwrap_or(false))
            .await
            .unwrap_or(false)
    }
    pub async fn authenticate_status(&self, authorization: Option<&str>) -> bool {
        if let Some(token) = authorization.and_then(extract_http_bearer_token) {
            if self.matches_local_credential(token) {
                return true;
            }
        }
        self.authenticate_bearer(authorization).await
    }
    fn matches_local_credential(&self, candidate: &str) -> bool {
        self.local_credential
            .read()
            .unwrap()
            .as_ref()
            .is_some_and(|expected| {
                expected.len() == candidate.len()
                    && bool::from(expected.as_bytes().ct_eq(candidate.as_bytes()))
            })
    }
    async fn admit(&self, hello: &Value) -> std::result::Result<Admission, &'static str> {
        let config = self.config_snapshot();
        let Some(hash) = config["auth"]["password"]
            .as_str()
            .filter(|hash| !hash.is_empty())
        else {
            return Ok(Admission::owner());
        };
        let Some(auth) = hello.get("auth") else {
            return Err("password_required");
        };
        let accepted = match auth["kind"].as_str() {
            Some("localCredential") => auth["token"]
                .as_str()
                .is_some_and(|token| self.matches_local_credential(token)),
            Some("password") => {
                let password = auth["password"].as_str().unwrap_or("").to_owned();
                let hash = hash.to_owned();
                tokio::task::spawn_blocking(move || {
                    bcrypt::verify(password, &hash).unwrap_or(false)
                })
                .await
                .unwrap_or(false)
            }
            _ => false,
        };
        if accepted {
            Ok(Admission::owner())
        } else {
            Err("incorrect_password")
        }
    }
    pub async fn run_socket<S>(
        &self,
        socket: WebSocketStream<S>,
        admission: Option<Admission>,
        cancel: CancellationToken,
    ) -> Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let (mut sink, mut stream) = socket.split();
        let cancel = cancel.child_token();
        let (incoming, inbound) = mpsc::channel(1);
        let (outbound, mut outgoing) = mpsc::channel::<OutboundFrame>(1);
        let (control, mut controls) = mpsc::channel(1);
        let reading = async {
            let result = async {
                loop {
                    let frame = tokio::select! { biased; _ = cancel.cancelled() => break, frame = stream.next() => frame };
                    let Some(frame) = frame else { break; };
                    let frame = match frame? {
                        Message::Text(v) => OutboundFrame::Text(v.to_string()), Message::Binary(v) => OutboundFrame::Binary(v),
                        Message::Ping(v) => { tokio::select! { _ = cancel.cancelled() => break, result = control.send(OutboundFrame::Pong(v)) => if result.is_err() { break; } } continue; },
                        Message::Pong(_) | Message::Frame(_) => continue, Message::Close(_) => break,
                    };
                    if frame.byte_len() > MAX_FRAME_BYTES { let _ = control.send(OutboundFrame::Close {code:1009,reason:"Message too large".into()}).await; break; }
                    tokio::select! { _ = cancel.cancelled() => break, result = incoming.send(frame) => if result.is_err() { break; } }
                }
                Ok::<(),anyhow::Error>(())
            }.await;
            drop(incoming);
            drop(control);
            result
        };
        let writing = async {
            let result = async {
                let mut control_open = true;
                loop {
                    let frame = tokio::select! { biased;
                        _ = cancel.cancelled() => break,
                        frame = controls.recv(), if control_open => { match frame { Some(frame) => Some(frame), None => {control_open=false;continue;} } },
                        frame = outgoing.recv() => frame,
                    };
                    let Some(frame) = frame else { break; }; let close = matches!(frame,OutboundFrame::Close {..});
                    let message = match frame {
                        OutboundFrame::Text(v) => Message::Text(v.into()), OutboundFrame::Binary(v) => Message::Binary(v), OutboundFrame::Ping(v) => Message::Ping(v), OutboundFrame::Pong(v) => Message::Pong(v),
                        OutboundFrame::Close {code,reason} => Message::Close(Some(CloseFrame {code:code.into(),reason:reason.into()})),
                        OutboundFrame::Flush(acknowledge) => {
                            let flushed = tokio::select! { _ = cancel.cancelled() => Err(anyhow!("Physical source closed before flush")), result = sink.flush() => result.map_err(Into::into) };
                            let failed = flushed.is_err(); let _ = acknowledge.send(flushed); if failed { break; } continue;
                        }
                    };
                    tokio::select! { _ = cancel.cancelled() => break, result = sink.send(message) => result? };
                    if close { break; }
                }
                Ok::<(),anyhow::Error>(())
            }.await;
            cancel.cancel();
            result
        };
        let (session, read, write) = tokio::join!(
            self.run_channel(inbound, outbound, cancel.clone(), admission),
            reading,
            writing
        );
        read?;
        write?;
        session
    }
    pub fn run_channel(
        &self,
        mut inbound: mpsc::Receiver<OutboundFrame>,
        outbound: mpsc::Sender<OutboundFrame>,
        cancel: CancellationToken,
        admission: Option<Admission>,
    ) -> futures_util::future::BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            let first = tokio::select! { _ = cancel.cancelled() => return Ok(()), _ = self.cancel.cancelled() => return Ok(()),
            result = tokio::time::timeout(Duration::from_secs(15), inbound.recv()) => result };
            let frame = match first {
                Ok(Some(OutboundFrame::Text(text))) => text,
                Ok(None) => return Ok(()),
                Err(_) => {
                    send_close(&outbound, 4001, "Hello timeout").await;
                    return Ok(());
                }
                _ => {
                    send_close(&outbound, 4002, "Invalid hello").await;
                    return Ok(());
                }
            };
            if frame.len() > MAX_FRAME_BYTES {
                send_close(&outbound, 1009, "Message too large").await;
                return Ok(());
            }
            let hello: Value = match serde_json::from_str(&frame) {
                Ok(v) => v,
                Err(_) => {
                    send_close(&outbound, 4002, "Invalid hello").await;
                    return Ok(());
                }
            };
            // Schema first, then protocol, then credentials, then identity:
            // the original rejects a bad credential before an empty or reserved clientId.
            if let Err(error) = validate_hello(&hello) {
                send_close(&outbound, 4002, error).await;
                return Ok(());
            }
            if hello["protocolVersion"].as_f64().is_some_and(|v| v < 1.0) {
                reject_hello(&outbound, &hello, "incompatible_protocol").await;
                return Ok(());
            }
            let mut admission = match admission {
                Some(a) => a,
                None => match self.admit(&hello).await {
                    Ok(a) => a,
                    Err(reason) => {
                        reject_hello(&outbound, &hello, reason).await;
                        return Ok(());
                    }
                },
            };
            if let Err(error) = validate_hello_identity(
                &hello,
                admission.principal["pluginId"].as_str(),
            ) {
                send_close(&outbound, 4002, error).await;
                return Ok(());
            }
            if let Some(ceiling) = hello["sessionPermissions"].as_array() {
                admission
                    .permissions
                    .retain(|p| ceiling.iter().any(|v| v == p));
            }
            self.run_admitted(inbound, outbound, cancel, admission, hello, false)
                .await
        })
    }
    async fn run_admitted(
        &self,
        mut inbound: mpsc::Receiver<OutboundFrame>,
        outbound: mpsc::Sender<OutboundFrame>,
        cancel: CancellationToken,
        mut admission: Admission,
        hello: Value,
        legacy_hub: bool,
    ) -> Result<()> {
        let client_id = hello["clientId"].as_str().unwrap().trim().to_owned();
        let routed = hello["routed"].as_bool() == Some(true);
        let principal_id = admission.principal["id"]
            .as_str()
            .ok_or_else(|| anyhow!("Admission principal lacks identity"))?
            .to_owned();
        let plugin_id = admission.principal["pluginId"].as_str().map(str::to_owned);
        let ephemeral = routed || legacy_hub || plugin_id.is_some();
        let session_client_key = if plugin_id.is_some() {
            uuid::Uuid::new_v4().to_string()
        } else {
            client_id.clone()
        };
        let key = serde_json::to_string(&(&principal_id, &session_client_key))?;
        admission.principal["sourceId"] = if let Some(plugin_id) = &plugin_id {
            Value::String(format!("plugin:{plugin_id}"))
        } else if routed && hello["producer"].is_object() {
            Value::String(format!(
                "route:{}:{}:{}",
                encode_component(&principal_id),
                encode_component(hello["producer"]["serverId"].as_str().unwrap()),
                encode_component(hello["producer"]["sourceId"].as_str().unwrap())
            ))
        } else {
            Value::String(principal_id.clone())
        };
        let old_sources: Vec<_> = self
            .sessions
            .lock()
            .unwrap()
            .sessions
            .get(&key)
            .map(|s| s.sources.keys().cloned().collect())
            .unwrap_or_default();
        for source in old_sources {
            self.hosts.revoke_source(&source).await;
        }
        let source_cancel = cancel.child_token();
        let (context, mut output) = {
            let mut registry = self.sessions.lock().unwrap();
            registry
                .sessions
                .retain(|_, s| s.retained_until.is_none_or(|until| until > Instant::now()));
            if let Some(current) = registry.principal_grants.get(&principal_id) {
                admission.permissions.retain(|p| current.contains(p));
            }
            let session = registry
                .sessions
                .entry(key.clone())
                .or_insert_with(|| Session {
                    id: uuid::Uuid::new_v4().to_string(),
                    permissions: Arc::new(RwLock::new(HashSet::new())),
                    sources: HashMap::new(),
                    retained_until: None,
                    ephemeral,
                    permission_ceiling: None,
                });
            session.permission_ceiling = hello["sessionPermissions"].as_array().map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            });
            *session.permissions.write().unwrap() = admission.permissions.into_iter().collect();
            session.retained_until = None;
            session.ephemeral = ephemeral;
            // A later direct hello must not turn an existing routed physical source into a second-hop producer.
            for source in session.sources.values().filter_map(Weak::upgrade) {
                source
                    .routed
                    .fetch_or(routed, std::sync::atomic::Ordering::AcqRel);
            }
            let (context, output) = RequestContext::attach(
                uuid::Uuid::new_v4().to_string(),
                session.id.clone(),
                client_id,
                hello.get("capabilities").cloned().unwrap_or(Value::Null),
                admission.principal,
                session.permissions.clone(),
                routed,
                hello["appVersion"].as_str().map(str::to_owned),
                source_cancel,
            );
            session.sources.insert(
                context.source_id().to_owned(),
                Arc::downgrade(&context.source),
            );
            (context, output)
        };
        let mut source_guard = SourceGuard {
            transport: self,
            context: &context,
            key: &key,
            completed: false,
        };
        let writer_cancel = context.cancellation();
        let output_sender = outbound.clone();
        let writer = tokio::spawn(async move {
            loop {
                tokio::select! { biased; _ = writer_cancel.cancelled() => break, queued = output.recv() => {
                    let Some(mut queued) = queued else { break; };
                    let mut frame = std::mem::replace(&mut queued.frame, OutboundFrame::Pong(Bytes::new()));
                    let drain = !matches!(frame,OutboundFrame::Flush(_) | OutboundFrame::Close {..});
                    if legacy_hub { if let OutboundFrame::Text(text) = &mut frame { if let Ok(mut envelope) = serde_json::from_str::<Value>(text) { if envelope["type"] == "session" { *text = envelope["message"].take().to_string(); } } } }
                    tokio::select! { _ = writer_cancel.cancelled() => break, result = output_sender.send(frame) => if result.is_err() { writer_cancel.cancel(); break; } }
                    if drain {
                        let (acknowledge, flushed) = tokio::sync::oneshot::channel();
                        tokio::select! { _ = writer_cancel.cancelled() => break, result = output_sender.send(OutboundFrame::Flush(acknowledge)) => if result.is_err() { writer_cancel.cancel(); break; } }
                        let result = tokio::select! { _ = writer_cancel.cancelled() => break, result = flushed => result };
                        if !matches!(result,Ok(Ok(()))) { writer_cancel.cancel(); break; }
                    }
                } }
            }
        });
        let result = async {
            context.send(self.server_info(&context).await?).await?;
            if context.principal()["kind"] == "hub" { self.hub.observe(&context).await?; }
            let mut lease = None; let mut check = tokio::time::interval(Duration::from_secs(10)); let mut requests = JoinSet::new();
            loop { tokio::select! { biased;
                _ = self.cancel.cancelled() => { cancel.cancel(); break; },
                _ = context.source.cancel.cancelled() => { cancel.cancel(); break; },
                _ = cancel.cancelled() => break,
                _ = check.tick() => if lease.is_some_and(|deadline| Instant::now() >= deadline) { cancel.cancel(); context.detach(); break; },
                completed = requests.join_next(), if !requests.is_empty() => { if let Some(Err(error)) = completed { tracing::warn!(%error,"Session handler task failed"); } },
                frame = inbound.recv() => {
                    let Some(frame) = frame else { break; };
                    if lease.is_some() { lease = Some(Instant::now()+Duration::from_secs(45)); }
                    match frame {
                        OutboundFrame::Text(text) => {
                            if text.len() > MAX_FRAME_BYTES { send_close(&outbound,1009,"Message too large").await; break; }
                            let envelope: Value = match serde_json::from_str(&text) { Ok(v) => v, Err(_) => { context.protocol_failure(&Value::Null,"invalid_message","Invalid JSON").await?; continue; } };
                            let message = if legacy_hub { envelope } else { match envelope["type"].as_str() {
                                Some("ping") => { if plugin_id.is_none() { lease = Some(Instant::now()+Duration::from_secs(45)); } context.queue(OutboundFrame::Text("{\"type\":\"pong\"}".to_owned())).await?; continue; },
                                Some("recording_state") if envelope["isRecording"].is_boolean() => continue,
                                Some("session") => envelope["message"].clone(),
                                Some("hello") => { send_close(&outbound,4002,"Unexpected hello").await; break; },
                                _ => { context.protocol_failure(&envelope,"unknown_schema","Unknown WebSocket envelope").await?; continue; }
                            } };
                            let kind = match message_type(&message) { Ok(kind) => kind, Err(_) => { context.protocol_failure(&message,"invalid_message","Message type is required").await?; continue; } };
                            if permissions::inbound(kind).is_none() { context.protocol_failure(&message,"unknown_schema","Unknown session message").await?; continue; }
                            if let Err(error) = validate_control_message(&message) { context.protocol_failure(&message,"invalid_message",&error.to_string()).await?; continue; }
                            if !permissions::allows_inbound(&message, |p| context.allows(p)) { context.protocol_failure(&message,if kind.starts_with("hosts.") {"permission_denied"} else {"access_denied"},&format!("Session is not authorized for {kind}")).await?; continue; }
                            if requests.len() >= 128 { context.protocol_failure(&message,"handler_error","Too many active requests").await?; continue; }
                            let request = context.for_request(message); let weak = self.app.clone();
                            requests.spawn(async move {
                                let result = if !request.is_live() || !permissions::allows_inbound(request.request().unwrap(), |p| request.allows(p)) { Err(anyhow!("Request source authority was revoked")) }
                                    else if let Some(app) = weak.upgrade() { app.dispatch(&request,request.request().unwrap()).await } else { Err(anyhow!("Daemon is shutting down")) };
                                if let Err(error) = result { let _ = request.protocol_failure(request.request().unwrap(),"handler_error",&error.to_string()).await; }
                                request.finish_request();
                            });
                        }
                        OutboundFrame::Binary(bytes) => {
                            if let Err(error) = context.validate_binary(&bytes,true) { send_close(&outbound,1008,&error.to_string()).await; break; }
                            let app = self.app.upgrade().ok_or_else(|| anyhow!("Daemon is shutting down"))?;
                            let handled = match bytes[0] { 0x01..=0x05 => app.terminals.handle_binary(&context,&bytes).await, 0x10..=0x12 => app.files.handle_binary(&context,bytes).await, 0x30 => self.hosts.handle_binary(&context,bytes).await, _ => Ok(false) };
                            match handled {
                                Ok(true) => {},
                                Ok(false) => { send_close(&outbound,1008,"Unowned binary frame").await; break; },
                                Err(error) => { let code = if error.downcast_ref::<hosts::HostError>().is_some_and(|e| matches!(e.code.as_str(),"permission_denied" | "host_unknown" | "invalid_message")) { 1008 } else { 1011 }; send_close(&outbound,code,&error.to_string()).await; break; }
                            }
                        }
                        OutboundFrame::Ping(bytes) => { context.queue(OutboundFrame::Pong(bytes)).await?; }
                        OutboundFrame::Pong(_) => {}
                        OutboundFrame::Close {..} => break,
                        OutboundFrame::Flush(acknowledge) => { let _ = acknowledge.send(Err(anyhow!("Flush is not an inbound frame"))); send_close(&outbound,1008,"Invalid inbound transport frame").await; break; }
                    }
                }
            } }
            context.detach(); requests.abort_all(); while requests.join_next().await.is_some() {} Ok::<(),anyhow::Error>(())
        }.await;
        context.detach();
        self.hosts.detach_source(context.source_id()).await;
        if let Some(app) = self.app.upgrade() {
            if let Err(error) = app.speech.detach_source(context.source_id()).await {
                tracing::debug!(%error,"Speech source teardown failed");
            }
        }
        self.detach_registration(&context, &key);
        let _ = writer.await;
        source_guard.completed = true;
        result
    }
    pub async fn admit_host_channel(
        &self,
        parent: &RequestContext,
        ceiling: Vec<String>,
        inbound: mpsc::Receiver<OutboundFrame>,
        outbound: mpsc::Sender<OutboundFrame>,
        cancel: CancellationToken,
    ) -> Result<()> {
        if parent.is_routed() || !parent.is_live() {
            bail!("Nested host routes are not allowed");
        }
        let admission = Admission {
            principal: parent.principal().clone(),
            permissions: ceiling.into_iter().filter(|p| parent.allows(p)).collect(),
        };
        let app = self
            .app
            .upgrade()
            .ok_or_else(|| anyhow!("Daemon is shutting down"))?;
        let transport = app.transport.clone();
        drop(app);
        let mut tasks = self.tasks.lock().unwrap();
        tasks.retain(|task| !task.is_finished());
        tasks.push(tokio::spawn(async move {
            if let Err(error) = transport
                .run_channel(inbound, outbound, cancel, Some(admission))
                .await
            {
                tracing::debug!(%error,"Host channel ended");
            }
        }));
        Ok(())
    }
    pub async fn open_plugin_session(
        &self,
        plugin_id: &str,
        inbound: mpsc::Receiver<OutboundFrame>,
        outbound: mpsc::Sender<OutboundFrame>,
        cancel: CancellationToken,
    ) -> Result<()> {
        if plugin_id.is_empty() {
            bail!("Plugin identity is required");
        }
        let mut admission = Admission::owner();
        admission.principal["kind"] = json!("plugin");
        admission.principal["pluginId"] = json!(plugin_id);
        self.run_channel(inbound, outbound, cancel, Some(admission))
            .await
    }
    pub async fn attach_hub(
        &self,
        principal_id: &str,
        grants: &[String],
        session_v1: bool,
        inbound: mpsc::Receiver<OutboundFrame>,
        outbound: mpsc::Sender<OutboundFrame>,
        cancel: CancellationToken,
    ) -> Result<()> {
        let current = self.hub.permissions_for(principal_id).await?;
        let permissions = grants
            .iter()
            .filter(|p| current.contains(p))
            .cloned()
            .collect();
        let admission = Admission {
            principal: json!({"id":principal_id,"kind":"hub"}),
            permissions,
        };
        if session_v1 {
            self.run_channel(inbound, outbound, cancel, Some(admission))
                .await
        } else {
            self.run_admitted(
                inbound,
                outbound,
                cancel,
                admission,
                json!({"clientId":principal_id,"clientType":"hub","protocolVersion":1}),
                true,
            )
            .await
        }
    }
    pub async fn update_hub_permissions(
        &self,
        principal_id: &str,
        grants: &[String],
    ) -> Result<()> {
        if grants.iter().any(|p| !permissions::known_permission(p)) {
            bail!("Invalid daemon permission");
        }
        let (sources, replay) = {
            let mut registry = self.sessions.lock().unwrap();
            registry
                .principal_grants
                .insert(principal_id.to_owned(), grants.to_vec());
            let mut sources = Vec::new();
            let mut replay = Vec::new();
            for (key, session) in &mut registry.sessions {
                let pair: (String, String) = serde_json::from_str(key)?;
                if pair.0 != principal_id {
                    continue;
                }
                let previous_execute = session.permissions.read().unwrap().contains("hub.execute");
                let effective: HashSet<_> = grants
                    .iter()
                    .filter(|p| {
                        session
                            .permission_ceiling
                            .as_ref()
                            .is_none_or(|ceiling| ceiling.contains(*p))
                    })
                    .cloned()
                    .collect();
                let now_execute = effective.contains("hub.execute");
                *session.permissions.write().unwrap() = effective;
                sources.extend(session.sources.keys().cloned());
                if !previous_execute && now_execute {
                    replay.extend(
                        session
                            .sources
                            .values()
                            .filter_map(Weak::upgrade)
                            .map(RequestContext::from_source),
                    );
                }
            }
            (sources, replay)
        };
        for source in sources {
            self.hosts.revoke_source(&source).await;
        }
        for source in replay {
            if source.wants_event("hub.execution.agent.update")
                || source.wants_event("hub.execution.agent.stream")
            {
                self.hub.replay(&source).await?;
            }
        }
        Ok(())
    }
    pub async fn handle(&self, context: &RequestContext, message: &Value) -> Result<bool> {
        validate_control_message(message)?;
        match message_type(message)? {
            "subscription.release.request" => {
                let id = message["subscriptionId"]
                    .as_str()
                    .ok_or_else(|| anyhow!("subscriptionId is required"))?;
                context.release_subscription(id).await?;
                context.send(json!({"type":"subscription.release.response","payload":{"requestId":request_id(message),"subscriptionId":id}})).await?;
            }
            "session.events.set_subscription.request" => {
                let events = message["events"]
                    .as_array()
                    .ok_or_else(|| anyhow!("events must be an array"))?
                    .iter()
                    .map(|v| {
                        v.as_str()
                            .filter(|s| context::SESSION_EVENTS.contains(s))
                            .map(str::to_owned)
                            .ok_or_else(|| anyhow!("Unknown session event"))
                    })
                    .collect::<Result<HashSet<_>>>()?;
                let previous_hub_demand = context.wants_event("hub.execution.agent.update")
                    || context.wants_event("hub.execution.agent.stream");
                let subscription = context.begin_subscription("events", None, None)?;
                context.register_events(
                    &subscription,
                    events,
                    message["notifications"].as_bool() == Some(true),
                );
                context.send(json!({"type":"session.events.set_subscription.response","payload":{"requestId":request_id(message),"subscriptionId":subscription.response_id()}})).await?;
                subscription.activate().await?;
                if context.principal()["kind"] == "hub"
                    && !previous_hub_demand
                    && (context.wants_event("hub.execution.agent.update")
                        || context.wants_event("hub.execution.agent.stream"))
                {
                    self.hub
                        .replay(&RequestContext::from_source(context.source.clone()))
                        .await?;
                }
            }
            "ping" => {
                let now = chrono::Utc::now().timestamp_millis();
                let mut payload = json!({"requestId":request_id(message),"serverReceivedAt":now,"serverSentAt":now});
                if let Some(sent) = message.get("clientSentAt") {
                    payload["clientSentAt"] = sent.clone();
                }
                context
                    .send(json!({"type":"pong","payload":payload}))
                    .await?;
            }
            "client_heartbeat" => {
                let mut activity = message.clone();
                activity.as_object_mut().unwrap().remove("type");
                activity["focusedTerminalId"] = message["focusedTerminalId"]
                    .as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(Value::from)
                    .unwrap_or(Value::Null);
                if activity.get("appVisibilityChangedAt").is_none() {
                    activity["appVisibilityChangedAt"] = activity["lastActivityAt"].clone();
                }
                context.set_activity(activity);
                return Ok(false);
            }
            "daemon.get_status.request" => {
                context.send(json!({"type":"daemon.get_status.response","payload":self.status_payload(message).await?})).await?;
            }
            "daemon.get_pairing_offer.request" => {
                context.send(json!({"type":"daemon.get_pairing_offer.response","payload":self.pairing_payload(message)?})).await?;
            }
            _ => {
                if self.hosts.handle(context, message).await?
                    || self.hub.handle(context, message).await?
                {
                    return Ok(true);
                }
                return Ok(false);
            }
        }
        Ok(true)
    }
    pub async fn publish_event(&self, message: Value) -> Result<()> {
        let sources = self.live_sources();
        for source in sources {
            if !source.is_live() {
                continue;
            }
            let result = if context::event_category(&message).is_some() {
                source.emit_event(message.clone()).await
            } else if !source.is_modern() {
                if let Some(projected) = crate::agent::project_legacy_event(&source, &message) {
                    source.send(projected).await
                } else {
                    Ok(())
                }
            } else {
                Ok(())
            };
            if let Err(error) = result {
                tracing::debug!(%error,source_id=source.source_id(),"Event delivery ended");
            }
        }
        Ok(())
    }
    pub async fn attention_sources(
        &self,
        event: &str,
        snapshot: &Value,
    ) -> Result<Vec<RequestContext>> {
        if !matches!(
            event,
            "agent_attention_required" | "terminal_attention_required"
        ) {
            bail!("Unknown attention event");
        }
        let app = self
            .app
            .upgrade()
            .ok_or_else(|| anyhow!("Daemon is shutting down"))?;
        let agents = app.agents.clone();
        let terminals = app.terminals.clone();
        drop(app);
        let mut selected = Vec::new();
        for source in self.live_sources() {
            if !source.is_live()
                || !source.wants_event(event)
                || !permissions::allows_outbound(&json!({"type":event}), |p| source.allows(p))
            {
                continue;
            }
            let eligible = if source.is_modern() {
                true
            } else if event == "agent_attention_required" {
                crate::agent::legacy_attention_eligible(&agents, &source, snapshot).await?
            } else {
                terminals
                    .legacy_attention_eligible(&source, snapshot)
                    .await?
            };
            if eligible {
                selected.push(source);
            }
        }
        Ok(selected)
    }
    pub async fn publish_attention(
        &self,
        message: Value,
        snapshot: &Value,
        notify_source: Option<&str>,
    ) -> Result<()> {
        let kind = message_type(&message)?;
        for source in self.attention_sources(kind, snapshot).await? {
            let mut projected = message.clone();
            projected["payload"]["shouldNotify"] = Value::Bool(
                notify_source == Some(source.source_id()) && source.wants_notification(kind),
            );
            if let Err(error) = source.emit_event(projected).await {
                tracing::debug!(%error,source_id=source.source_id(),"Attention source detached");
            }
        }
        Ok(())
    }
    fn live_sources(&self) -> Vec<RequestContext> {
        self.sessions
            .lock()
            .unwrap()
            .sessions
            .values()
            .flat_map(|s| s.sources.values().filter_map(Weak::upgrade))
            .map(RequestContext::from_source)
            .collect()
    }
    pub(crate) fn event_sources(&self, event: &str) -> Vec<RequestContext> {
        if !context::SESSION_EVENTS.contains(&event) {
            return Vec::new();
        }
        let message = if let Some(status) = event.strip_prefix("status.") {
            json!({"type":"status","payload":{"status":status}})
        } else {
            json!({"type":event})
        };
        let mut sources = self.live_sources();
        sources.retain(|source| {
            source.is_live()
                && source.wants_event(event)
                && permissions::allows_outbound(&message, |permission| source.allows(permission))
        });
        sources
    }
    pub fn has_event_demand(&self, event: &str) -> bool {
        !self.event_sources(event).is_empty()
    }
    pub fn runtime_metrics(&self) -> Value {
        let registry = self.sessions.lock().unwrap();
        json!({"sessionCount":registry.sessions.len(),"socketCount":registry.sessions.values().map(|s|s.sources.len()).sum::<usize>(),"retainedSessionCount":registry.sessions.values().filter(|s|s.retained_until.is_some()).count()})
    }
    fn detach_registration(&self, context: &RequestContext, key: &str) {
        context.detach();
        let Ok(mut registry) = self.sessions.lock() else {
            return;
        };
        let Some(session) = registry.sessions.get_mut(key) else {
            return;
        };
        if session.id != context.session_id()
            || session.sources.remove(context.source_id()).is_none()
        {
            return;
        }
        if session.sources.is_empty() {
            if session.ephemeral {
                registry.sessions.remove(key);
            } else {
                session.retained_until = Some(Instant::now() + Duration::from_secs(90));
            }
        }
    }
    pub async fn shutdown(&self) -> Result<()> {
        self.cancel.cancel();
        for source in self.live_sources() {
            source.detach();
        }
        self.listener.lock().await.take();
        let relay = self.relay.shutdown().await;
        let hosts = self.hosts.shutdown().await;
        let hub = self.hub.shutdown().await;
        let tasks = std::mem::take(&mut *self.tasks.lock().unwrap());
        for task in tasks {
            let _ = task.await;
        }
        if let Some(app) = self.app.upgrade() {
            let path = app.home.join("local-credential");
            let owned = self.local_credential.write().unwrap().take();
            drop(app);
            if let Some(owned) = owned {
                if let Some(bytes) = storage::read_private(&path).await? {
                    if String::from_utf8(bytes)?.trim() == owned {
                        tokio::fs::remove_file(path).await?;
                    }
                }
            }
        }
        relay?;
        hosts?;
        hub?;
        Ok(())
    }
    async fn server_info(&self, context: &RequestContext) -> Result<Value> {
        let identity = self.identity();
        let config = self.config_snapshot();
        let app = self
            .app
            .upgrade()
            .ok_or_else(|| anyhow!("Daemon is shutting down"))?;
        let hoplite = app.providers.definition("hoplite").is_some();
        let speech = app.speech.clone();
        drop(app);
        let readiness = speech.readiness().await;
        Ok(
            json!({"type":"status","payload":{"status":"server_info","protocolVersion":1,"serverId":identity["serverId"],"hostname":identity["label"],"version":env!("CARGO_PKG_VERSION"),"sourceId":context.actor_source_id(),"permissions":context.permissions(),"desktopManaged":config["desktopManaged"].as_bool().unwrap_or(false),"features":features::features(&config,hoplite),"capabilities":features::speech_capabilities(&readiness)}}),
        )
    }
    async fn status_payload(&self, request: &Value) -> Result<Value> {
        let app = self
            .app
            .upgrade()
            .ok_or_else(|| anyhow!("Daemon is shutting down"))?;
        let providers = futures_util::future::join_all(app.providers.ids().into_iter().map(|id| {
            let registry = &app.providers;
            let cancel = self.cancel.child_token();
            async move {
                let available = match registry.get(&id) {
                    Ok(provider) => provider.is_available(json!({}), cancel).await,
                    Err(error) => Err(error),
                };
                match available {
                    Ok(available) => json!({"provider":id,"available":available,"error":null}),
                    Err(error) => {
                        json!({"provider":id,"available":false,"error":error.to_string()})
                    }
                }
            }
        }))
        .await;
        let pid = match tokio::fs::read(app.home.join("paseo.pid")).await {
            Ok(bytes) => serde_json::from_slice::<Value>(&bytes).ok(),
            Err(_) => None,
        };
        let config = self.config_snapshot();
        let relay = relay_config(&config);
        Ok(
            json!({"requestId":request_id(request),"serverId":self.identity()["serverId"],"version":env!("CARGO_PKG_VERSION"),"pid":std::process::id(),"nodePath":app.executable,"startedAt":pid.and_then(|p|p.get("startedAt").cloned()),"listen":self.listen_string(),"relay":relay,"providers":providers}),
        )
    }
    pub fn listen_string(&self) -> Option<String> {
        let listen = self.listen();
        match listen["type"].as_str() {
            Some("tcp") => {
                let host = listen["host"].as_str()?;
                Some(format!(
                    "{}:{}",
                    if host.contains(':') {
                        format!("[{host}]")
                    } else {
                        host.to_owned()
                    },
                    listen["port"].as_u64()?
                ))
            }
            Some("unix") | Some("pipe") => listen["path"].as_str().map(str::to_owned),
            _ => None,
        }
    }
    fn pairing_payload(&self, request: &Value) -> Result<Value> {
        let mut payload = identity::pairing_offer(&self.identity(), &self.config_snapshot(), true)?;
        payload["requestId"] = request_id(request).map(Value::from).unwrap_or(Value::Null);
        if payload["url"].is_null() {
            payload["url"] = json!("");
        }
        Ok(payload)
    }
}

pub fn extract_http_bearer_token(value: &str) -> Option<&str> {
    let mut parts = value
        .split(|c: char| {
            matches!(
                c,
                '\t' | '\n'
                    | '\u{000b}'
                    | '\u{000c}'
                    | '\r'
                    | ' '
                    | '\u{00a0}'
                    | '\u{1680}'
                    | '\u{2000}'
                    ..='\u{200a}'
                        | '\u{2028}'
                        | '\u{2029}'
                        | '\u{202f}'
                        | '\u{205f}'
                        | '\u{3000}'
                        | '\u{feff}'
            )
        })
        .filter(|part| !part.is_empty());
    if parts.next() != Some("Bearer") {
        return None;
    }
    let token = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    Some(token)
}

fn relay_config(config: &Value) -> Value {
    json!({"enabled":config["relayEnabled"],"endpoint":config["relayEndpoint"],"publicEndpoint":config["relayPublicEndpoint"],"useTls":config["relayUseTls"],"publicUseTls":config["relayPublicUseTls"],"requireProtectedSecurity":config["relayRequireProtectedSecurity"]})
}
async fn send_close(outbound: &mpsc::Sender<OutboundFrame>, code: u16, reason: &str) {
    let mut reason = reason.to_owned();
    while reason.len() > 123 {
        reason.pop();
    }
    let _ = outbound.send(OutboundFrame::Close { code, reason }).await;
}
async fn reject_hello(outbound: &mpsc::Sender<OutboundFrame>, hello: &Value, reason: &str) {
    if hello.get("auth").is_some() || has_capability(&hello["capabilities"], "hello_rejection") {
        let _ = outbound
            .send(OutboundFrame::Text(
                json!({"type":"hello.rejected","reason":reason,"accepts":["password"]}).to_string(),
            ))
            .await;
    }
    let (code, description) = match reason {
        "incompatible_protocol" => (4003, "Incompatible protocol version"),
        "password_required" => (4401, "Password required"),
        _ => (4401, "Incorrect password"),
    };
    send_close(outbound, code, description).await;
}
fn validate_control_message(message: &Value) -> Result<()> {
    match message_type(message)? {
        "subscription.release.request"
        | "session.events.set_subscription.request"
        | "ping"
        | "daemon.get_status.request"
        | "daemon.get_pairing_offer.request" => {
            if request_id(message).is_none() {
                bail!("requestId must be a string");
            }
        }
        "client_heartbeat" => {
            if !matches!(message["deviceType"].as_str(), Some("web" | "mobile"))
                || !message["lastActivityAt"].is_string()
                || !message["appVisible"].is_boolean()
            {
                bail!("Invalid client heartbeat");
            }
            if message
                .get("focusedAgentId")
                .is_none_or(|v| !v.is_null() && !v.is_string())
                || message
                    .get("focusedTerminalId")
                    .is_some_and(|v| !v.is_null() && !v.is_string())
                || message
                    .get("appVisibilityChangedAt")
                    .is_some_and(|v| !v.is_string())
            {
                bail!("Invalid heartbeat focus or visibility");
            }
        }
        _ => return Ok(()),
    }
    match message_type(message)? {
        "subscription.release.request" if !message["subscriptionId"].is_string() => {
            bail!("subscriptionId must be a string")
        }
        "session.events.set_subscription.request" => {
            if message["events"].as_array().is_none_or(|events| {
                events.iter().any(|v| {
                    v.as_str()
                        .is_none_or(|s| !context::SESSION_EVENTS.contains(&s))
                })
            }) {
                bail!("Invalid session events");
            }
            if message
                .get("notifications")
                .is_some_and(|v| !v.is_boolean())
            {
                bail!("notifications must be a boolean");
            }
        }
        "ping"
            if message.get("clientSentAt").is_some_and(|v| {
                v.as_f64()
                    .is_none_or(|n| !n.is_finite() || n.fract() != 0.0)
            }) =>
        {
            bail!("clientSentAt must be an integer")
        }
        _ => {}
    }
    Ok(())
}
/// Original `WSHelloMessageSchema`: shape only. Identity rules run after admission.
fn validate_hello(hello: &Value) -> std::result::Result<(), &'static str> {
    if hello["type"] != "hello"
        || hello["clientId"].as_str().is_none_or(str::is_empty)
        || hello["protocolVersion"]
            .as_f64()
            .is_none_or(|v| !v.is_finite() || v.fract() != 0.0)
        || !matches!(
            hello["clientType"].as_str(),
            Some("mobile" | "browser" | "cli" | "mcp" | "hub")
        )
    {
        return Err("Invalid hello");
    }
    if hello.get("routed").is_some_and(|v| !v.is_boolean())
        || hello.get("capabilities").is_some_and(|v| !v.is_object())
        || hello.get("appVersion").is_some_and(|v| !v.is_string())
    {
        return Err("Invalid hello");
    }
    for capability in [
        "voice",
        "hello_rejection",
        "pushNotifications",
        "explicit_event_subscriptions",
        "all_providers",
        "reasoning_merge_enum",
        "selective_agent_timeline",
        "custom_mode_icons",
        "terminal_reflowable_snapshot",
        "provider_subagents",
        "project_updates",
        "compact_provider_snapshots",
        "provider_snapshot_references",
        "timeline_replacement_invalidation",
        "timeline_notifications",
    ] {
        if hello["capabilities"]
            .get(capability)
            .is_some_and(|v| !v.is_boolean())
        {
            return Err("Invalid hello capability");
        }
    }
    if let Some(values) = hello.get("sessionPermissions") {
        if values.as_array().is_none_or(|a| {
            a.iter()
                .any(|v| v.as_str().is_none_or(|p| !permissions::known_permission(p)))
        }) {
            return Err("Invalid session permissions");
        }
    }
    if let Some(producer) = hello.get("producer") {
        for (key, max) in [("serverId", 256), ("sourceId", 1024)] {
            if producer[key]
                .as_str()
                .is_none_or(|s| s.is_empty() || s.encode_utf16().count() > max)
            {
                return Err("Invalid routed producer");
            }
        }
    }
    if let Some(auth) = hello.get("auth") {
        if match auth["kind"].as_str() {
            Some("password") => !auth["password"].is_string(),
            Some("localCredential") => !auth["token"].is_string(),
            _ => true,
        } {
            return Err("Invalid hello credential");
        }
    }
    Ok(())
}
/// Original `handleHello` after admission: a blank trimmed clientId, or a reserved
/// plugin identity on the wrong socket, closes with 4002.
fn validate_hello_identity(
    hello: &Value,
    plugin_id: Option<&str>,
) -> std::result::Result<(), &'static str> {
    let client_id = hello["clientId"].as_str().unwrap_or_default().trim();
    if client_id.is_empty() {
        return Err("Invalid hello");
    }
    if plugin_id.map_or_else(
        || client_id.starts_with("plugin:"),
        |id| client_id != format!("plugin:{id}"),
    ) {
        return Err("Invalid plugin clientId");
    }
    Ok(())
}
fn encode_component(value: &str) -> String {
    const SET: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'_')
        .remove(b'.')
        .remove(b'!')
        .remove(b'~')
        .remove(b'*')
        .remove(b'\'')
        .remove(b'(')
        .remove(b')');
    percent_encoding::utf8_percent_encode(value, SET).to_string()
}
pub fn decode_host_frame(bytes: &[u8]) -> Result<(&str, &[u8])> {
    if bytes.len() < 4 || bytes[0] != 0x30 {
        bail!("Invalid host route frame");
    }
    let n = u16::from_be_bytes([bytes[1], bytes[2]]) as usize;
    if !(1..=256).contains(&n) || bytes.len() < 3 + n || bytes.len() > MAX_FRAME_BYTES {
        bail!("Invalid host route frame length");
    }
    Ok((
        std::str::from_utf8(&bytes[3..3 + n]).context("Invalid route ID UTF-8")?,
        &bytes[3 + n..],
    ))
}
/// Test-only inverse of `decode_host_frame`.
#[cfg(test)]
pub fn encode_host_frame(route: &str, payload: &[u8]) -> Result<Bytes> {
    if !(1..=256).contains(&route.len()) || payload.len() > MAX_FRAME_BYTES - 259 {
        bail!("Invalid host route frame length");
    }
    let mut bytes = Vec::with_capacity(3 + route.len() + payload.len());
    bytes.push(0x30);
    bytes.extend_from_slice(&(route.len() as u16).to_be_bytes());
    bytes.extend_from_slice(route.as_bytes());
    bytes.extend_from_slice(payload);
    Ok(Bytes::from(bytes))
}
fn render_qr(url: &str) -> Result<String> {
    let code = qrcode::QrCode::with_error_correction_level(url.as_bytes(), qrcode::EcLevel::M)?;
    let width = code.width();
    let margin = 4;
    let total = width + margin * 2;
    let mut output = String::new();
    let dark = |x: usize, y: usize| {
        x >= margin
            && y >= margin
            && x < width + margin
            && y < width + margin
            && code[(x - margin, y - margin)] == qrcode::Color::Dark
    };
    for y in (0..total).step_by(2) {
        output.push_str("\u{1b}[47m\u{1b}[30m");
        for x in 0..total {
            output.push(match (dark(x, y), dark(x, y + 1)) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            });
        }
        output.push_str("\u{1b}[0m");
        if y + 2 < total {
            output.push('\n');
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests;
