use crate::{
    daemon_control::{self, DaemonInstance},
    relay_crypto::SecurityMode,
    transport::hosts::{
        HostFrame,
        connector::{Connection, Sender},
        target::{Endpoint, Target},
    },
};
use anyhow::{Result, anyhow, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};
use tokio::sync::{Semaphore, broadcast, oneshot};
use tokio_util::sync::CancellationToken;

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct ClientError {
    pub code: String,
    pub message: String,
    pub details: Option<Value>,
}
impl ClientError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: None,
        }
    }
    pub fn details(mut self, details: impl Into<Value>) -> Self {
        self.details = Some(details.into());
        self
    }
}

#[derive(Clone)]
pub enum DaemonTarget {
    Instance { home: PathBuf },
    Endpoint { host: String },
}
impl DaemonTarget {
    pub fn select(home: Option<&str>, host: Option<&str>, local_only: bool) -> Result<Self> {
        Self::select_with_env(
            home,
            host,
            local_only,
            std::env::var("PASEO_HOME").ok().as_deref(),
            std::env::var("PASEO_HOST").ok().as_deref(),
        )
    }
    pub fn select_with_env(
        home: Option<&str>,
        host: Option<&str>,
        local_only: bool,
        env_home: Option<&str>,
        env_host: Option<&str>,
    ) -> Result<Self> {
        for (name, value) in [("home", home), ("host", host)] {
            if value.is_some_and(|v| v.trim().is_empty()) {
                return Err(ClientError::new(
                    "TARGET_INVALID",
                    format!("--{name} requires a non-empty value."),
                )
                .into());
            }
        }
        if home.is_some() && host.is_some() {
            return Err(ClientError::new(
                "TARGET_AMBIGUOUS",
                "Choose either --home or --host, not both.",
            )
            .into());
        }
        if local_only && host.is_some() {
            return Err(ClientError::new(
                "LOCAL_OPERATION",
                "This is a local operation; use --home. --host is not supported.",
            )
            .into());
        }
        if let Some(home) = home {
            return Ok(Self::Instance {
                home: resolve_home(Some(home))?,
            });
        }
        if local_only {
            return Ok(Self::Instance {
                home: resolve_home(env_home)?,
            });
        }
        if let Some(host) = host {
            return Ok(Self::Endpoint { host: host.into() });
        }
        if env_home.is_some_and(|s| !s.is_empty()) && env_host.is_some_and(|s| !s.is_empty()) {
            return Err(ClientError::new(
                "TARGET_AMBIGUOUS",
                "PASEO_HOME and PASEO_HOST are both set. Choose --home or --host explicitly.",
            )
            .into());
        }
        if let Some(host) = env_host.filter(|s| !s.is_empty()) {
            return Ok(Self::Endpoint { host: host.into() });
        }
        Ok(Self::Instance {
            home: resolve_home(env_home)?,
        })
    }
    pub fn description(&self) -> String {
        match self {
            Self::Instance { home } => format!("home {}", home.display()),
            Self::Endpoint { host } => {
                Target::parse(host)
                    .map(|t| t.description)
                    .unwrap_or_else(|_| {
                        if is_registration(host) {
                            format!("registered host {host}")
                        } else {
                            "<invalid endpoint>".into()
                        }
                    })
            }
        }
    }
    pub fn home(&self) -> Result<PathBuf> {
        match self {
            Self::Instance { home } => Ok(home.clone()),
            _ => resolve_home(std::env::var("PASEO_HOME").ok().as_deref()),
        }
    }
}
pub fn resolve_home(value: Option<&str>) -> Result<PathBuf> {
    let value = value.unwrap_or("~/.paseo");
    let path = if value == "~" {
        dirs::home_dir().ok_or_else(|| anyhow!("User home unavailable"))?
    } else if let Some(rest) = value.strip_prefix("~/") {
        dirs::home_dir()
            .ok_or_else(|| anyhow!("User home unavailable"))?
            .join(rest)
    } else {
        PathBuf::from(value)
    };
    let absolute = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    Ok(normalized)
}
fn is_registration(host: &str) -> bool {
    host.starts_with("hst_") || host.starts_with("srv_")
}

pub struct ConnectOptions {
    pub target: DaemonTarget,
    pub timeout: Duration,
    pub instance: Option<DaemonInstance>,
    pub client_type: String,
}
impl ConnectOptions {
    pub fn new(target: DaemonTarget) -> Self {
        Self {
            target,
            timeout: Duration::from_secs(15),
            instance: None,
            client_type: "cli".into(),
        }
    }
}
struct Pending {
    response: String,
    reply: oneshot::Sender<Result<Value>>,
}
struct Shared {
    pending: Mutex<HashMap<String, Pending>>,
    events: broadcast::Sender<Value>,
    binary: broadcast::Sender<bytes::Bytes>,
    info: RwLock<Value>,
    cancel: CancellationToken,
}
#[derive(Clone)]
enum ClientSender {
    Direct(Sender),
    Routed(tokio::sync::mpsc::Sender<HostFrame>, CancellationToken),
}
impl ClientSender {
    async fn frame(&self, frame: HostFrame) -> Result<()> {
        match self {
            Self::Direct(sender) => sender.send(frame).await,
            Self::Routed(sender, cancel) => {
                tokio::select! {_=cancel.cancelled()=>bail!("Host route closed"),result=sender.send(frame)=>result.map_err(|_|anyhow!("Host route closed"))}
            }
        }
    }
    async fn json(&self, value: Value) -> Result<()> {
        self.frame(HostFrame::Text(serde_json::to_string(&value)?))
            .await
    }
    async fn flush(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.frame(HostFrame::Flush(tx)).await?;
        tokio::time::timeout(Duration::from_secs(15), rx).await???;
        Ok(())
    }
}
pub struct DaemonClient {
    sender: ClientSender,
    shared: Arc<Shared>,
    reader: Mutex<Option<tokio::task::JoinHandle<()>>>,
    transport: Mutex<Option<tokio::task::JoinHandle<()>>>,
}
pub struct ClientEvents<T: Clone> {
    receiver: broadcast::Receiver<T>,
    cancel: CancellationToken,
}
impl<T: Clone> ClientEvents<T> {
    pub async fn recv(&mut self) -> std::result::Result<T, broadcast::error::RecvError> {
        tokio::select! {biased;event=self.receiver.recv()=>event,_=self.cancel.cancelled()=>Err(broadcast::error::RecvError::Closed)}
    }
    pub fn try_recv(&mut self) -> std::result::Result<T, broadcast::error::TryRecvError> {
        match self.receiver.try_recv() {
            Err(broadcast::error::TryRecvError::Empty) if self.cancel.is_cancelled() => {
                Err(broadcast::error::TryRecvError::Closed)
            }
            result => result,
        }
    }
}
impl DaemonClient {
    pub async fn connect(options: ConnectOptions, cancel: CancellationToken) -> Result<Self> {
        if let DaemonTarget::Endpoint { host } = &options.target {
            if is_registration(host) {
                return Self::connect_registration(options, cancel).await;
            }
        }
        Self::connect_direct(options, cancel).await
    }
    async fn connect_direct(options: ConnectOptions, cancel: CancellationToken) -> Result<Self> {
        let home = options.target.home()?;
        let host = match &options.target {
            DaemonTarget::Endpoint { host } => host.clone(),
            DaemonTarget::Instance { home } => {
                let instance = daemon_control::wait_for_daemon_ready(
                    home,
                    options.timeout,
                    options.instance.as_ref(),
                    &cancel,
                )
                .await?;
                instance.listen.ok_or_else(|| {
                    ClientError::new(
                        "DAEMON_NOT_READY",
                        "Daemon has not published a listening address",
                    )
                })?
            }
        };
        let mut target = Target::parse(&host)?;
        if target.password.is_none() {
            target.password = std::env::var("PASEO_PASSWORD")
                .ok()
                .filter(|v| !v.is_empty());
        }
        let auth = if let Some(password) = target.password.as_ref() {
            Some(json!({"kind":"password","password":password}))
        } else {
            read_local_credential_for_target(&home, &host)
                .await?
                .map(|token| json!({"kind":"localCredential","token":token}))
        };
        let identity = client_id(&home).await?;
        let connect = Connection::connect(
            &target,
            SecurityMode::Legacy,
            Arc::new(Semaphore::new(256 * 1024 * 1024)),
            cancel.clone(),
        );
        let connection = tokio::select! {_=cancel.cancelled()=>bail!("Daemon connection cancelled"),result=tokio::time::timeout(options.timeout,connect)=>result.map_err(|_|ClientError::new("DAEMON_UNREACHABLE","Daemon connection timed out"))??};
        Self::from_parts(
            ClientSender::Direct(connection.sender),
            connection.incoming,
            connection.cancel,
            connection.task,
            hello(&identity, &options.client_type, auth),
            options.timeout,
        )
        .await
    }
    async fn from_parts(
        sender: ClientSender,
        mut incoming: tokio::sync::mpsc::Receiver<Result<HostFrame>>,
        cancel: CancellationToken,
        transport: tokio::task::JoinHandle<()>,
        hello: Value,
        timeout: Duration,
    ) -> Result<Self> {
        let (events, _) = broadcast::channel(1024);
        let (binary, _) = broadcast::channel(64);
        let shared = Arc::new(Shared {
            pending: Mutex::new(HashMap::new()),
            events,
            binary,
            info: RwLock::new(Value::Null),
            cancel,
        });
        let (ready_tx, ready_rx) = oneshot::channel();
        let state = shared.clone();
        let reader = tokio::spawn(async move {
            let mut ready = Some(ready_tx);
            let outcome:Result<()>=async {
                loop { let frame=tokio::select!{biased;frame=incoming.recv()=>frame.ok_or_else(||ClientError::new("DAEMON_CONNECTION_LOST","Daemon connection closed"))??,_=state.cancel.cancelled()=>bail!("Daemon connection closed")};
                    let text=match frame {HostFrame::Text(text)=>text,HostFrame::Binary(bytes)=>{let _=state.binary.send(bytes);continue;},HostFrame::Close{reason,..}=>bail!("Daemon connection closed: {reason}"),HostFrame::Flush(reply)=>{let _=reply.send(Err(anyhow!("Invalid inbound flush barrier")));continue;}};
                    let wire:Value=serde_json::from_str(&text)?;
                    if wire["type"]=="hello.rejected" {let (code,message)=match wire["reason"].as_str(){Some("password_required")=>("AUTH_REQUIRED","Password required"),Some("incorrect_password")=>("AUTH_FAILED","Incorrect password"),_=>("INCOMPATIBLE_PROTOCOL","Incompatible protocol version")};return Err(ClientError::new(code,message).into());}
                    if wire["type"]=="pong" {continue;}
                    ensure!(wire["type"]=="session","Invalid daemon message envelope");
                    let message=wire.get("message").cloned().ok_or_else(||anyhow!("Missing daemon session message"))?;
                    if message["type"]=="status" && message["payload"]["status"]=="server_info" {
                        ensure!(message["payload"]["protocolVersion"].as_u64().is_none_or(|v|v==1),"Incompatible protocol version");
                        *state.info.write().unwrap()=message["payload"].clone();if let Some(ready)=ready.take(){let _=ready.send(Ok(()));}
                    }
                    dispatch(&state,message);
                }
            }.await;
            let failure = outcome
                .err()
                .unwrap_or_else(|| anyhow!("Daemon connection closed"));
            if let Some(ready) = ready.take() {
                let _ = ready.send(Err(failure));
            }
            let pending = std::mem::take(&mut *state.pending.lock().unwrap());
            for (_, pending) in pending {
                let _=pending.reply.send(Err(ClientError::new("DAEMON_CONNECTION_LOST","Daemon connection closed before the response; the operation may already have completed").into()));
            }
            state.cancel.cancel();
        });
        let client = Self {
            sender,
            shared,
            reader: Mutex::new(Some(reader)),
            transport: Mutex::new(Some(transport)),
        };
        let result = async {
            client.sender.json(hello).await?;
            tokio::time::timeout(timeout, ready_rx)
                .await
                .map_err(|_| {
                    ClientError::new(
                        "DAEMON_UNREACHABLE",
                        "Timed out waiting for daemon greeting",
                    )
                })??
        }
        .await;
        if let Err(error) = result {
            client.close().await;
            return Err(error);
        }
        Ok(client)
    }
    async fn connect_registration(
        options: ConnectOptions,
        cancel: CancellationToken,
    ) -> Result<Self> {
        let DaemonTarget::Endpoint { host } = options.target else {
            unreachable!()
        };
        let local = Self::connect_direct(
            ConnectOptions {
                target: DaemonTarget::Instance {
                    home: resolve_home(std::env::var("PASEO_HOME").ok().as_deref())?,
                },
                timeout: options.timeout,
                instance: None,
                client_type: options.client_type.clone(),
            },
            cancel.clone(),
        )
        .await?;
        let result=async {
            local.require_feature("hostMultiplexing","Update the host to use connected hosts.")?;
            let hosts=local.request(json!({"type":"hosts.list.request"}),"hosts.list.response",options.timeout).await?;
            let row=hosts["hosts"].as_array().and_then(|rows|rows.iter().find(|r|r["registrationId"]==host || r["serverId"]==host)).ok_or_else(||ClientError::new("host_unknown",format!("Unknown registration {host}")))?;
            ensure!(row["enabled"]==true,"Host registration is disabled");
            let server_id=row["serverId"].as_str().ok_or_else(||anyhow!("Host has not established its identity"))?;
            let mut events=local.subscribe();
            let mut binary=local.subscribe_binary();
            let opened=local.request(json!({"type":"hosts.route.open.request","serverId":server_id,"scopeId":uuid::Uuid::new_v4().to_string()}),"hosts.route.open.response",options.timeout).await?;
            let route=opened["routeId"].as_str().ok_or_else(||anyhow!("Missing host route ID"))?.to_owned();
            let (incoming_tx,incoming)=tokio::sync::mpsc::channel(64);
            let (outgoing_tx,mut outgoing)=tokio::sync::mpsc::channel(64);
            let route_cancel=cancel.child_token();let task_cancel=route_cancel.clone();let sender=local.sender.clone();let routing_id=route.clone();
            let task=tokio::spawn(async move {
                let result:Result<()>=async{loop{tokio::select!{
                    _=task_cancel.cancelled()=>break,
                    frame=outgoing.recv()=>{let Some(frame)=frame else{break};match frame {HostFrame::Text(data)=>sender.json(json!({"type":"session","message":{"type":"hosts.route.frame.request","routeId":routing_id,"data":data,"binary":false}})).await?,HostFrame::Binary(bytes)=>sender.frame(HostFrame::Binary(crate::transport::hosts::encode_frame(&routing_id,&bytes)?)).await?,HostFrame::Flush(reply)=>{let result=sender.flush().await;let failed=result.is_err();let _=reply.send(result);if failed{bail!("Host output flush failed");}},HostFrame::Close{..}=>break}},
                    event=events.recv()=>{let event=event?;if event["payload"]["subscriptionId"]!=routing_id{continue;}if event["type"]=="hosts.route.closed"{bail!("{}",event["payload"]["message"].as_str().unwrap_or("Connected host closed"));}if event["type"]=="hosts.route.frame"{let data=event["payload"]["data"].as_str().ok_or_else(||anyhow!("Invalid routed host frame"))?;incoming_tx.send(Ok(HostFrame::Text(data.into()))).await.map_err(|_|anyhow!("Routed host client closed"))?;}},
                    frame=binary.recv()=>{let frame=frame?;let(id,offset)=crate::transport::hosts::decode_frame(&frame)?;if id==routing_id{incoming_tx.send(Ok(HostFrame::Binary(frame.slice(offset..)))).await.map_err(|_|anyhow!("Routed host client closed"))?;}}
                }}Ok(())}.await;
                if let Err(error)=result{let _=incoming_tx.try_send(Err(error));}
                let _=sender.json(json!({"type":"session","message":{"type":"hosts.route.close.request","routeId":routing_id}})).await;
                task_cancel.cancel();
            });
            Ok((outgoing_tx,incoming,route_cancel,task))
        }.await;
        let (outgoing, incoming, route_cancel, task) = match result {
            Ok(v) => v,
            Err(error) => {
                local.close().await;
                return Err(error);
            }
        };
        let client = Self::from_parts(
            ClientSender::Routed(outgoing, route_cancel.clone()),
            incoming,
            route_cancel,
            task,
            hello(
                &format!("cid_{}", uuid::Uuid::new_v4().simple()),
                &options.client_type,
                None,
            ),
            options.timeout,
        )
        .await;
        match client {
            Ok(client) => {
                let stop = client.shared.cancel.clone();
                tokio::spawn(async move {
                    stop.cancelled().await;
                    local.close().await;
                });
                Ok(client)
            }
            Err(error) => {
                local.close().await;
                Err(error)
            }
        }
    }
    pub fn server_info(&self) -> Value {
        self.shared.info.read().unwrap().clone()
    }
    pub fn subscribe(&self) -> ClientEvents<Value> {
        ClientEvents {
            receiver: self.shared.events.subscribe(),
            cancel: self.shared.cancel.clone(),
        }
    }
    pub fn subscribe_binary(&self) -> ClientEvents<bytes::Bytes> {
        ClientEvents {
            receiver: self.shared.binary.subscribe(),
            cancel: self.shared.cancel.clone(),
        }
    }
    pub fn cancellation(&self) -> CancellationToken {
        self.shared.cancel.clone()
    }
    pub fn require_feature(&self, key: &str, message: &str) -> Result<()> {
        if self.shared.info.read().unwrap()["features"][key] != true {
            bail!("{message}");
        }
        Ok(())
    }
    pub async fn send(&self, message: Value) -> Result<()> {
        self.sender
            .json(json!({"type":"session","message":message}))
            .await
    }
    pub async fn send_binary(&self, bytes: bytes::Bytes) -> Result<()> {
        self.sender.frame(HostFrame::Binary(bytes)).await
    }
    pub async fn flush(&self) -> Result<()> {
        self.sender.flush().await
    }
    pub async fn request(
        &self,
        message: Value,
        response_type: &str,
        timeout: Duration,
    ) -> Result<Value> {
        self.request_cancellable(message, response_type, timeout, CancellationToken::new())
            .await
    }
    pub async fn request_cancellable(
        &self,
        mut message: Value,
        response_type: &str,
        timeout: Duration,
        cancel: CancellationToken,
    ) -> Result<Value> {
        if cancel.is_cancelled() {
            return Err(ClientError::new(
                "DAEMON_REQUEST_CANCELLED",
                "Daemon request cancelled before submission",
            )
            .into());
        }
        if self.shared.cancel.is_cancelled() {
            return Err(ClientError::new(
                "DAEMON_CONNECTION_LOST",
                "Daemon connection closed before submission",
            )
            .into());
        }
        let id = message["requestId"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        message["requestId"] = Value::String(id.clone());
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.shared.pending.lock().unwrap();
            ensure!(!pending.contains_key(&id), "Duplicate live request ID");
            pending.insert(
                id.clone(),
                Pending {
                    response: response_type.into(),
                    reply: tx,
                },
            );
        }
        let guard = PendingGuard {
            state: self.shared.clone(),
            id,
        };
        tokio::select! {biased;
            _=cancel.cancelled()=>return Err(ClientError::new("DAEMON_REQUEST_CANCELLED","Daemon request cancelled before submission").into()),
            result=self.send(message)=>result?,
        }
        let response = async {
            rx.await.map_err(|_| {
                ClientError::new("DAEMON_CONNECTION_LOST", "Daemon request consumer closed")
            })?
        };
        let deadline = async {
            if timeout.is_zero() {
                std::future::pending::<()>().await;
            } else {
                tokio::time::sleep(timeout).await;
            }
        };
        let result = tokio::select! {biased;
            result=response=>result,
            _=cancel.cancelled()=>Err(ClientError::new("DAEMON_REQUEST_CANCELLED","Daemon request cancelled; a submitted mutation may still complete").into()),
            _=self.shared.cancel.cancelled()=>Err(ClientError::new("DAEMON_CONNECTION_LOST","Daemon connection closed; a submitted mutation may still complete").into()),
            _=deadline=>Err(ClientError::new("DAEMON_REQUEST_TIMEOUT","Daemon request timed out; a submitted mutation may still complete").into()),
        };
        drop(guard);
        result
    }
    pub async fn release_subscription(&self, id: &str) -> Result<()> {
        self.request(
            json!({"type":"subscription.release.request","subscriptionId":id}),
            "subscription.release.response",
            Duration::from_secs(15),
        )
        .await?;
        Ok(())
    }
    pub async fn daemon_status(&self, timeout: Duration) -> Result<Value> {
        self.require_feature("daemonStatusRpc", "Update the host to read daemon status.")?;
        self.request(
            json!({"type":"daemon.get_status.request"}),
            "daemon.get_status.response",
            timeout,
        )
        .await
    }
    pub async fn restart(&self, reason: &str, timeout: Duration) -> Result<Value> {
        self.request(
            json!({"type":"restart_server_request","reason":reason}),
            "status",
            timeout,
        )
        .await
    }
    pub async fn shutdown(&self, timeout: Duration) -> Result<Value> {
        self.request(json!({"type":"shutdown_server_request"}), "status", timeout)
            .await
    }
    pub async fn close(&self) {
        self.shared.cancel.cancel();
        let reader = self.reader.lock().unwrap().take();
        let transport = self.transport.lock().unwrap().take();
        if let Some(task) = reader {
            let _ = task.await;
        }
        if let Some(task) = transport {
            let _ = task.await;
        }
    }
}
impl Drop for DaemonClient {
    fn drop(&mut self) {
        self.shared.cancel.cancel();
    }
}
struct PendingGuard {
    state: Arc<Shared>,
    id: String,
}
impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.state.pending.lock().unwrap().remove(&self.id);
    }
}
fn dispatch(state: &Shared, message: Value) {
    let id = message["payload"]["requestId"]
        .as_str()
        .or_else(|| message["requestId"].as_str());
    if let Some(id) = id {
        let pending = {
            let mut pending = state.pending.lock().unwrap();
            let matches = pending
                .get(id)
                .is_some_and(|p| message["type"] == p.response || message["type"] == "rpc_error");
            if matches { pending.remove(id) } else { None }
        };
        if let Some(pending) = pending {
            let payload = message.get("payload").cloned().unwrap_or(Value::Null);
            let result = if let Some(error) = payload["error"].as_str() {
                Err(ClientError::new(
                    payload["code"].as_str().unwrap_or("DAEMON_RPC_ERROR"),
                    error,
                )
                .into())
            } else {
                Ok(payload)
            };
            let _ = pending.reply.send(result);
        }
    }
    let _ = state.events.send(message);
}
fn hello(identity: &str, client_type: &str, auth: Option<Value>) -> Value {
    let capabilities = [
        "hello_rejection",
        "owned_subscriptions",
        "explicit_event_subscriptions",
        "all_providers",
        "selective_agent_timeline",
        "reasoning_merge_enum",
        "custom_mode_icons",
        "terminal_reflowable_snapshot",
        "provider_subagents",
        "projected_subagent_timeline",
        "project_updates",
        "compact_provider_snapshots",
        "provider_snapshot_references",
        "timeline_replacement_invalidation",
        "timeline_notifications",
        "plugin_timeline_items",
        "workspace_setup_observation",
        "workspace_setup_blocked",
    ]
    .into_iter()
    .map(|key| (key.to_owned(), Value::Bool(true)))
    .collect::<serde_json::Map<_, _>>();
    let mut value = json!({"type":"hello","clientId":identity,"clientType":client_type,"protocolVersion":1,"capabilities":capabilities,"appVersion":env!("CARGO_PKG_VERSION")});
    if let Some(auth) = auth {
        value["auth"] = auth;
    }
    value
}
async fn client_id(home: &Path) -> Result<String> {
    let path = home.join("cli-client-id");
    match tokio::fs::read_to_string(&path).await {
        Ok(id) if !id.trim().is_empty() => return Ok(id.trim().into()),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    tokio::fs::create_dir_all(home).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = tokio::fs::set_permissions(home, std::fs::Permissions::from_mode(0o700)).await;
    }
    let id = format!("cid_{}", uuid::Uuid::new_v4().simple());
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&path).await?;
    use tokio::io::AsyncWriteExt;
    file.write_all(id.as_bytes()).await?;
    file.sync_all().await?;
    Ok(id)
}
pub async fn read_local_credential_for_target(home: &Path, host: &str) -> Result<Option<String>> {
    let lock = match tokio::fs::read(home.join("paseo.pid")).await {
        Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(lock) => lock,
            Err(_) => return Ok(None),
        },
        Err(_) => return Ok(None),
    };
    let Some(listen) = lock["listen"].as_str() else {
        return Ok(None);
    };
    let Some(selected) = normalized_local_target(host) else {
        return Ok(None);
    };
    if normalized_local_target(listen).as_deref() != Some(selected.as_str()) {
        return Ok(None);
    }
    let token = match tokio::fs::read_to_string(home.join("local-credential")).await {
        Ok(token) => token,
        Err(_) => return Ok(None),
    };
    let token = token.trim();
    Ok((token.len() == 43
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'))
    .then(|| token.to_owned()))
}
fn normalized_local_target(host: &str) -> Option<String> {
    let host = host.trim();
    if host.starts_with("unix://") || host.starts_with("pipe://") {
        return Some(host.into());
    }
    if host.starts_with('/') {
        return Some(format!("unix://{host}"));
    }
    if host.starts_with(r"\\.\pipe\") {
        return Some(format!("pipe://{host}"));
    }
    if host.contains("://") && !host.starts_with("tcp://") {
        return None;
    }
    let target = Target::parse(host).ok()?;
    if target.security.is_some() {
        return None;
    }
    match target.endpoint {
        Endpoint::Socket { url, ipc: None } => {
            let url = url::Url::parse(&url).ok()?;
            let host = url.host_str()?;
            let host = if matches!(
                host,
                "127.0.0.1" | "0.0.0.0" | "[::1]" | "::1" | "[::]" | "::" | "localhost"
            ) {
                "localhost"
            } else {
                host
            };
            Some(format!("{host}:{}", url.port_or_known_default()?))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selectors_reject_ambiguous_authority() {
        assert!(
            DaemonTarget::select_with_env(None, None, false, Some("/a"), Some("host:6767"))
                .is_err()
        );
        assert!(
            DaemonTarget::select_with_env(Some("/a"), Some("host:6767"), false, None, None)
                .is_err()
        );
        assert!(
            matches!(DaemonTarget::select_with_env(Some("/a"),None,false,Some("/b"),Some("else:1")).unwrap(),DaemonTarget::Instance{home} if home==Path::new("/a"))
        );
    }
    #[test]
    fn credentials_are_never_selected_for_ssh_or_relay() {
        assert!(normalized_local_target("ssh://localhost?daemonPort=6767").is_none());
        assert_eq!(
            normalized_local_target("tcp://127.0.0.1:6767?password=private"),
            normalized_local_target("localhost:6767")
        );
    }
    #[tokio::test]
    async fn request_correlation_ignores_other_response_type() {
        let (events, _) = broadcast::channel(4);
        let (binary, _) = broadcast::channel(1);
        let state = Shared {
            pending: Mutex::new(HashMap::new()),
            events,
            binary,
            info: RwLock::new(Value::Null),
            cancel: CancellationToken::new(),
        };
        let (tx, mut rx) = oneshot::channel();
        state.pending.lock().unwrap().insert(
            "one".into(),
            Pending {
                response: "expected".into(),
                reply: tx,
            },
        );
        dispatch(
            &state,
            json!({"type":"wrong","payload":{"requestId":"one","result":false}}),
        );
        assert!(matches!(
            rx.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        dispatch(
            &state,
            json!({"type":"expected","payload":{"requestId":"one","result":true}}),
        );
        assert_eq!(rx.await.unwrap().unwrap()["result"], true);
    }
    #[tokio::test]
    async fn credentials_require_exact_published_authority() {
        let home = tempfile::tempdir().unwrap();
        let token = "a".repeat(43);
        tokio::fs::write(home.path().join("local-credential"), &token)
            .await
            .unwrap();
        tokio::fs::write(
            home.path().join("paseo.pid"),
            br#"{"listen":"0.0.0.0:6767"}"#,
        )
        .await
        .unwrap();
        assert_eq!(
            read_local_credential_for_target(home.path(), "127.0.0.1:6767")
                .await
                .unwrap(),
            Some(token)
        );
        for target in [
            "localhost:6768",
            "remote.example:6767",
            "ws://localhost:6767/other",
            "ssh://localhost?daemonPort=6767",
        ] {
            assert_eq!(
                read_local_credential_for_target(home.path(), target)
                    .await
                    .unwrap(),
                None
            );
        }
        tokio::fs::write(home.path().join("paseo.pid"), b"{partial")
            .await
            .unwrap();
        assert_eq!(
            read_local_credential_for_target(home.path(), "localhost:6767")
                .await
                .unwrap(),
            None
        );
    }
    #[tokio::test]
    async fn empty_identity_is_replaced_and_reused() {
        let home = tempfile::tempdir().unwrap();
        tokio::fs::write(home.path().join("cli-client-id"), " \n")
            .await
            .unwrap();
        let first = client_id(home.path()).await.unwrap();
        assert!(first.starts_with("cid_"));
        assert_eq!(first.len(), 36);
        assert_eq!(client_id(home.path()).await.unwrap(), first);
    }
    #[tokio::test]
    async fn cancellation_prevents_submission_and_cleans_pending_ownership() {
        let (events, _) = broadcast::channel(4);
        let (binary, _) = broadcast::channel(1);
        let cancel = CancellationToken::new();
        let shared = Arc::new(Shared {
            pending: Mutex::new(HashMap::new()),
            events,
            binary,
            info: RwLock::new(Value::Null),
            cancel: cancel.clone(),
        });
        let (tx, mut incoming) = tokio::sync::mpsc::channel(1);
        let client = DaemonClient {
            sender: ClientSender::Routed(tx, cancel),
            shared: shared.clone(),
            reader: Mutex::new(None),
            transport: Mutex::new(None),
        };
        let stop = CancellationToken::new();
        stop.cancel();
        assert!(
            client
                .request_cancellable(json!({"type":"mutation"}), "done", Duration::ZERO, stop)
                .await
                .is_err()
        );
        assert!(incoming.try_recv().is_err());
        assert!(shared.pending.lock().unwrap().is_empty());
        let stop = CancellationToken::new();
        let request = client.request_cancellable(
            json!({"type":"mutation","requestId":"owned"}),
            "done",
            Duration::ZERO,
            stop.clone(),
        );
        tokio::pin!(request);
        tokio::select! {result=&mut request=>panic!("Request finished before cancellation: {result:?}"),frame=incoming.recv()=>assert!(matches!(frame,Some(HostFrame::Text(_))))}
        assert!(shared.pending.lock().unwrap().contains_key("owned"));
        stop.cancel();
        assert!(request.await.is_err());
        assert!(shared.pending.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn disconnection_drains_buffered_events_then_closes_subscription() {
        let (sender, receiver) = broadcast::channel(2);
        let cancel = CancellationToken::new();
        let mut events = ClientEvents {
            receiver,
            cancel: cancel.clone(),
        };
        sender.send(json!({"finished":true})).unwrap();
        cancel.cancel();
        assert_eq!(events.recv().await.unwrap()["finished"], true);
        assert!(matches!(
            events.recv().await,
            Err(broadcast::error::RecvError::Closed)
        ));
    }
}
