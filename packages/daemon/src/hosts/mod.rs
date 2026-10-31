pub(crate) mod connector;
mod routes;
pub(crate) mod target;
#[cfg(test)]
mod tests;

use crate::{relay_crypto::SecurityMode, state::AppState, transport::RequestContext};
use anyhow::{Result, anyhow, ensure};
use bytes::Bytes;
pub use routes::HostClient;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock, RwLock, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use target::Target;
use tokio::{
    io::AsyncWriteExt,
    sync::{Mutex as AsyncMutex, Semaphore, mpsc, oneshot, watch},
};
use tokio_util::sync::CancellationToken;

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct HostError {
    pub code: String,
    pub message: String,
}
impl HostError {
    pub fn new(code: &str, message: &str) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}
#[derive(Debug)]
pub enum HostFrame {
    Text(String),
    Binary(Bytes),
    Close { code: u16, reason: String },
    Flush(oneshot::Sender<Result<()>>),
}
impl HostFrame {
    fn len(&self) -> usize {
        match self {
            Self::Text(v) => v.len(),
            Self::Binary(v) => v.len(),
            Self::Close { reason, .. } => reason.len() + 2,
            Self::Flush(_) => 0,
        }
    }
}

#[derive(Clone)]
pub struct HostsService {
    inner: Arc<Inner>,
}
struct Inner {
    app: Weak<AppState>,
    home: OnceLock<PathBuf>,
    identity: OnceLock<Value>,
    rows: RwLock<Vec<Value>>,
    mutation: AsyncMutex<()>,
    live: Mutex<HashMap<String, Arc<Link>>>,
    workers: Mutex<HashMap<String, CancellationToken>>,
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    routes: Mutex<HashMap<String, Arc<routes::Route>>>,
    changes: watch::Sender<u64>,
    budget: Arc<Semaphore>,
    started: AtomicBool,
    closed: AtomicBool,
    cancel: CancellationToken,
}
struct Link {
    sender: connector::Sender,
    cancel: CancellationToken,
    pending: Mutex<HashMap<String, oneshot::Sender<Result<Value>>>>,
    channels: Mutex<HashMap<String, mpsc::Sender<std::result::Result<HostFrame, HostError>>>>,
}
impl Link {
    async fn request(&self, mut message: Value) -> Result<Value> {
        let id = uuid::Uuid::new_v4().to_string();
        message["requestId"] = json!(id);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id.clone(), tx);
        let result=async{self.sender.json(json!({"type":"session","message":message})).await?;
            tokio::select!{_=self.cancel.cancelled()=>Err(anyhow!("Host transport closed")),result=tokio::time::timeout(Duration::from_secs(15),rx)=>result.map_err(|_|anyhow!("Timeout waiting for message"))?.map_err(|_|anyhow!("Host transport closed"))?}
        }.await;
        self.pending.lock().unwrap().remove(&id);
        result
    }
    async fn deliver(&self, frame: HostFrame) -> Result<()> {
        match frame {
            HostFrame::Text(text) => {
                let value: Value = serde_json::from_str(&text)?;
                if value["type"] == "ping" {
                    self.sender.json(json!({"type":"pong"})).await?;
                    return Ok(());
                }
                if value["type"] == "hello.rejected" {
                    return Err(HostError::new(
                        "authentication_failed",
                        "Host rejected the credentials. Replace the connection credentials.",
                    )
                    .into());
                }
                if value["type"] != "session" {
                    return Ok(());
                }
                let message = &value["message"];
                if let Some(id) = message["payload"]["requestId"].as_str() {
                    let tx = self.pending.lock().unwrap().remove(id);
                    if let Some(tx) = tx {
                        let result = if message["type"] == "rpc_error" {
                            Err(remote_error(message).into())
                        } else {
                            Ok(message.clone())
                        };
                        let _ = tx.send(result);
                        return Ok(());
                    }
                }
                if let Some(id) = message["payload"]["subscriptionId"].as_str() {
                    let sender = self.channels.lock().unwrap().get(id).cloned();
                    if let Some(sender) = sender {
                        let frame = if message["type"] == "hosts.route.closed" {
                            Err(HostError::new(
                                message["payload"]["code"]
                                    .as_str()
                                    .unwrap_or("host_offline"),
                                message["payload"]["message"]
                                    .as_str()
                                    .unwrap_or("Host channel closed"),
                            ))
                        } else if message["type"] == "hosts.route.frame" {
                            Ok(HostFrame::Text(
                                message["payload"]["data"]
                                    .as_str()
                                    .ok_or_else(|| anyhow!("Invalid host frame"))?
                                    .into(),
                            ))
                        } else {
                            return Ok(());
                        };
                        tokio::select! {_=self.cancel.cancelled()=>return Ok(()),_=sender.send(frame)=>{}}
                    }
                }
            }
            HostFrame::Binary(bytes) => {
                let (id, payload) = decode_frame(&bytes)?;
                let sender = self.channels.lock().unwrap().get(id).cloned();
                if let Some(sender) = sender {
                    tokio::select! {_=self.cancel.cancelled()=>return Ok(()),_=sender.send(Ok(HostFrame::Binary(bytes.slice(payload..))))=>{}}
                }
            }
            HostFrame::Close { .. } => return Err(anyhow!("Host closed")),
            HostFrame::Flush(ack) => {
                let _ = ack.send(Err(anyhow!(
                    "Flush barrier cannot arrive from a remote host"
                )));
                return Err(anyhow!("Invalid host frame"));
            }
        }
        Ok(())
    }
}
impl HostsService {
    pub fn new(app: Weak<AppState>) -> Self {
        let (changes, _) = watch::channel(0);
        Self {
            inner: Arc::new(Inner {
                app,
                home: OnceLock::new(),
                identity: OnceLock::new(),
                rows: RwLock::new(Vec::new()),
                mutation: AsyncMutex::new(()),
                live: Mutex::new(HashMap::new()),
                workers: Mutex::new(HashMap::new()),
                tasks: Mutex::new(Vec::new()),
                routes: Mutex::new(HashMap::new()),
                changes,
                budget: Arc::new(Semaphore::new(256 * 1024 * 1024)),
                started: AtomicBool::new(false),
                closed: AtomicBool::new(false),
                cancel: CancellationToken::new(),
            }),
        }
    }
    pub async fn prepare(&self) -> Result<()> {
        let _guard = self.inner.mutation.lock().await;
        let app = self
            .inner
            .app
            .upgrade()
            .ok_or_else(|| anyhow!("Daemon is stopping"))?;
        self.inner
            .home
            .set(app.home.clone())
            .map_err(|_| anyhow!("Hosts already initialized"))?;
        self.inner
            .identity
            .set(app.transport.identity())
            .map_err(|_| anyhow!("Hosts already initialized"))?;
        drop(app);
        let rows = match tokio::fs::read(self.home()?.join("hosts.json")).await {
            Ok(bytes) => {
                let value: Value = serde_json::from_slice(&bytes)
                    .map_err(|_| anyhow!("Cannot read saved connected hosts"))?;
                let rows = value
                    .as_array()
                    .ok_or_else(|| anyhow!("Cannot read saved connected hosts"))?;
                for row in rows {
                    validate_saved(row)?;
                }
                rows.clone()
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e.into()),
        };
        let mut loaded = rows;
        for row in &mut loaded {
            if row["enabled"] == true {
                Target::parse(field(row, "connection")?)?;
            }
            row["status"] = json!(if row["enabled"] == true {
                "connecting"
            } else {
                "disabled"
            });
            row["nextRetryAt"] = Value::Null;
        }
        *self.inner.rows.write().unwrap() = loaded;
        Ok(())
    }
    pub async fn initialize(&self) -> Result<()> {
        let _guard = self.inner.mutation.lock().await;
        self.home()?;
        self.identity()?;
        ensure!(
            !self.inner.closed.load(Ordering::Acquire),
            "Hosts are shutting down"
        );
        ensure!(
            !self.inner.started.swap(true, Ordering::AcqRel),
            "Hosts already started"
        );
        let rows = self.inner.rows.read().unwrap().clone();
        for row in rows {
            self.start(row);
        }
        Ok(())
    }
    fn home(&self) -> Result<&PathBuf> {
        self.inner
            .home
            .get()
            .ok_or_else(|| anyhow!("Hosts not initialized"))
    }
    fn identity(&self) -> Result<&Value> {
        self.inner
            .identity
            .get()
            .ok_or_else(|| anyhow!("Hosts not initialized"))
    }
    fn changed(&self) {
        self.inner
            .changes
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }
    pub async fn list(&self) -> Result<Value> {
        let identity = self.identity()?;
        Ok(
            json!({"self":{"serverId":identity["serverId"],"label":identity["label"]},"hosts":self.inner.rows.read().unwrap().iter().map(summary).collect::<Vec<_>>()}),
        )
    }
    async fn persist(&self, rows: &[Value]) -> Result<()> {
        let home = self.home()?;
        tokio::fs::create_dir_all(home).await?;
        let temp = home.join(format!(".hosts-{}.tmp", uuid::Uuid::new_v4()));
        let result = async {
            let mut options = tokio::fs::OpenOptions::new();
            options.create_new(true).write(true);
            #[cfg(unix)]
            options.mode(0o600);
            let mut file = options.open(&temp).await?;
            file.write_all(&serde_json::to_vec(rows)?).await?;
            file.sync_all().await?;
            drop(file);
            tokio::fs::rename(&temp, home.join("hosts.json")).await?;
            Ok::<_, anyhow::Error>(())
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(&temp).await;
        }
        result
    }
    pub async fn handle(&self, context: &RequestContext, message: &Value) -> Result<bool> {
        let kind = message["type"].as_str().unwrap_or("");
        if !matches!(
            kind,
            "hosts.list.request"
                | "hosts.add.request"
                | "hosts.update.request"
                | "hosts.remove.request"
                | "hosts.route.open.request"
                | "hosts.route.frame.request"
                | "hosts.route.close.request"
                | "hosts.link.open.request"
        ) {
            return Ok(false);
        }
        let result = self.execute(context, message).await;
        if let Err(error) = result {
            let failure = error.downcast_ref::<HostError>();
            let code = failure.map(|e| e.code.as_str()).unwrap_or("host_error");
            let text = failure
                .map(|e| e.message.as_str())
                .unwrap_or("Host operation failed. Check the connection and retry.");
            if kind == "hosts.route.frame.request" {
                if let Some(id) = message["routeId"].as_str() {
                    self.close_route(context.source_id(), id, code, text).await;
                }
            } else if let Some(id) = message["requestId"].as_str() {
                context.send(json!({"type":"rpc_error","payload":{"requestId":id,"requestType":kind,"code":code,"error":text}})).await?;
            }
        }
        Ok(true)
    }
    async fn execute(&self, context: &RequestContext, message: &Value) -> Result<()> {
        ensure!(
            !self.inner.closed.load(Ordering::Acquire),
            HostError::new("host_offline", "Daemon is stopping")
        );
        let kind = message["type"].as_str().unwrap_or("");
        if context.is_routed()
            && (kind.starts_with("hosts.route.") || kind == "hosts.link.open.request")
        {
            return Err(HostError::new(
                "unsupported_capability",
                "Connected hosts support one hop. Connect to the managing daemon to select a host.",
            )
            .into());
        }
        match kind {
            "hosts.list.request" => {
                if message["observe"] != true {
                    return context.send(json!({"type":"hosts.list.response","payload":{"requestId":message["requestId"],"directory":self.list().await?}})).await;
                }
                let owner = context.begin_subscription("hosts", None, None)?;
                let mut changed = self.inner.changes.subscribe();
                context.send(json!({"type":"hosts.list.response","payload":{"requestId":message["requestId"],"subscriptionId":owner.response_id(),"directory":self.list().await?}})).await?;
                owner.activate().await?;
                let service = self.clone();
                self.spawn(async move{let cancel=owner.cancellation();loop{tokio::select!{_=cancel.cancelled()=>break,_=service.inner.cancel.cancelled()=>break,result=changed.changed()=>{if result.is_err(){break;}match service.list().await{Ok(directory)=>{if owner.emit(json!({"type":"hosts.changed","payload":{"subscriptionId":owner.response_id(),"directory":directory}})).await.is_err(){break;}},Err(_)=>break}}}}let _=owner.release().await;});
                Ok(())
            }
            "hosts.add.request" => {
                let host = self
                    .add(&message["input"], &context.actor_source_id())
                    .await?;
                context.send(json!({"type":"hosts.add.response","payload":{"requestId":message["requestId"],"host":host}})).await
            }
            "hosts.update.request" => {
                let host = self
                    .update(
                        field(message, "registrationId")?,
                        &message["patch"],
                        &context.actor_source_id(),
                        context.allows("access.manage"),
                    )
                    .await?;
                context.send(json!({"type":"hosts.update.response","payload":{"requestId":message["requestId"],"host":host}})).await
            }
            "hosts.remove.request" => {
                self.remove(
                    field(message, "registrationId")?,
                    &context.actor_source_id(),
                    context.allows("access.manage"),
                )
                .await?;
                context.send(json!({"type":"hosts.remove.response","payload":{"requestId":message["requestId"]}})).await
            }
            _ => self.route_request(context, message).await,
        }
    }
    async fn add(&self, input: &Value, actor: &str) -> Result<Value> {
        validate_input(input, false)?;
        let target = Target::parse(field(input, "connection")?)?;
        let _guard = self.inner.mutation.lock().await;
        let mut rows = self.inner.rows.read().unwrap().clone();
        let key = input["sourceKey"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        if let Some(row) = rows
            .iter()
            .find(|r| r["source"]["id"] == actor && r["source"]["key"] == key)
        {
            if row["connection"] == input["connection"]
                && row["label"] == input["label"]
                && metadata_equal(
                    &row["metadata"],
                    input.get("metadata").unwrap_or(&json!({})),
                )
            {
                return Ok(summary(row));
            }
            return Err(HostError::new(
                "source_conflict",
                "This source key already exists. Update its registration explicitly.",
            )
            .into());
        }
        ensure!(
            rows.len() < 128,
            HostError::new(
                "resource_limit",
                "Remove a host before adding another (limit 128)"
            )
        );
        let row = json!({"registrationId":format!("hst_{}",uuid::Uuid::new_v4()),"serverId":null,"connection":input["connection"],"label":input["label"],"metadata":input.get("metadata").cloned().unwrap_or(json!({})),"source":{"id":actor,"key":key},"enabled":true,"status":"connecting","targetDescription":target.description,"lastOnlineAt":null,"nextRetryAt":null,"error":null});
        rows.push(row.clone());
        self.persist(&rows).await?;
        *self.inner.rows.write().unwrap() = rows;
        self.start(row.clone());
        self.changed();
        Ok(summary(&row))
    }
    async fn update(&self, id: &str, patch: &Value, actor: &str, manage: bool) -> Result<Value> {
        validate_input(patch, true)?;
        let _guard = self.inner.mutation.lock().await;
        let mut rows = self.inner.rows.read().unwrap().clone();
        let index = row_index(&rows, id)?;
        let row = &mut rows[index];
        if (patch.get("connection").is_some()
            || patch.get("metadata").is_some()
            || patch["replaceIdentity"] == true)
            && row["source"]["id"] != actor
            && !manage
        {
            return Err(HostError::new(
                "permission_denied",
                "Only the source or access manager can replace its connection and metadata",
            )
            .into());
        }
        ensure!(
            patch["replaceIdentity"] != true || patch.get("connection").is_some(),
            HostError::new(
                "invalid_connection",
                "Identity replacement requires a replacement connection"
            )
        );
        for name in ["label", "metadata", "enabled"] {
            if let Some(value) = patch.get(name) {
                row[name] = value.clone();
            }
        }
        if let Some(connection) = patch.get("connection") {
            let target = Target::parse(connection.as_str().unwrap_or(""))?;
            row["connection"] = connection.clone();
            row["targetDescription"] = json!(target.description);
        }
        if patch["replaceIdentity"] == true {
            row["serverId"] = Value::Null;
        }
        let restart = patch.get("connection").is_some()
            || patch.get("enabled").is_some()
            || patch["replaceIdentity"] == true;
        if restart {
            row["status"] = json!(if row["enabled"] == true {
                "connecting"
            } else {
                "disabled"
            });
            row["error"] = Value::Null;
            row["nextRetryAt"] = Value::Null;
        }
        let result = row.clone();
        self.persist(&rows).await?;
        if restart {
            self.stop(
                id,
                if result["enabled"] == true {
                    "host_offline"
                } else {
                    "host_disabled"
                },
            );
        }
        *self.inner.rows.write().unwrap() = rows;
        if restart {
            self.start(result.clone());
        }
        self.changed();
        Ok(summary(&result))
    }
    async fn remove(&self, id: &str, actor: &str, manage: bool) -> Result<()> {
        let _guard = self.inner.mutation.lock().await;
        let mut rows = self.inner.rows.read().unwrap().clone();
        let index = row_index(&rows, id)?;
        ensure!(
            rows[index]["source"]["id"] == actor || manage,
            HostError::new(
                "permission_denied",
                "Only the source or access manager can remove this host. Disable it to stop access."
            )
        );
        rows.remove(index);
        self.persist(&rows).await?;
        self.stop(id, "host_unknown");
        *self.inner.rows.write().unwrap() = rows;
        self.changed();
        Ok(())
    }
    fn spawn(&self, future: impl Future<Output = ()> + Send + 'static) {
        let mut tasks = self.inner.tasks.lock().unwrap();
        tasks.retain(|task| !task.is_finished());
        tasks.push(tokio::spawn(future));
    }
    fn stop(&self, id: &str, code: &str) {
        if let Some(cancel) = self.inner.workers.lock().unwrap().remove(id) {
            cancel.cancel();
        }
        if let Some(link) = self.inner.live.lock().unwrap().remove(id) {
            link.cancel.cancel();
        }
        for route in self.inner.routes.lock().unwrap().values() {
            if route.registration == id {
                route.stop(code, "Host access was disabled, replaced or removed");
            }
        }
    }
    fn start(&self, row: Value) {
        if row["enabled"] != true || self.inner.closed.load(Ordering::Acquire) {
            return;
        }
        let id = row["registrationId"].as_str().unwrap().to_owned();
        let cancel = self.inner.cancel.child_token();
        self.inner
            .workers
            .lock()
            .unwrap()
            .insert(id.clone(), cancel.clone());
        let service = self.clone();
        self.spawn(async move {
            service.connect_loop(id, row, cancel).await;
        });
    }
    async fn connect_loop(&self, id: String, row: Value, cancel: CancellationToken) {
        let target = match Target::parse(row["connection"].as_str().unwrap_or("")) {
            Ok(v) => v,
            Err(e) => {
                let _ = self.set_failure(&id, &cancel, &e, None).await;
                return;
            }
        };
        let mut floor = security_floor(&row, &target);
        let mut attempt = 0u32;
        loop {
            if cancel.is_cancelled() {
                break;
            }
            if self.set_connecting(&id, &cancel).await.is_err() {
                break;
            }
            let outcome = self
                .connect_once(&id, &target, &cancel, &mut floor, &mut attempt)
                .await;
            if cancel.is_cancelled() {
                break;
            }
            self.inner.live.lock().unwrap().remove(&id);
            for route in self.inner.routes.lock().unwrap().values() {
                if route.registration == id {
                    route.stop("host_offline", "Host disconnected");
                }
            }
            let error = outcome
                .err()
                .unwrap_or_else(|| anyhow!("Host disconnected"));
            let permanent = error.downcast_ref::<HostError>().is_some_and(|e| {
                matches!(
                    e.code.as_str(),
                    "authentication_failed"
                        | "server_id_mismatch"
                        | "self_host"
                        | "duplicate_host"
                        | "unsupported_capability"
                )
            });
            attempt = attempt.saturating_add(1);
            let delay = Duration::from_millis(
                (1500u64.saturating_mul(2u64.saturating_pow(attempt.saturating_sub(1).min(5))))
                    .min(30000),
            );
            let retry = (!permanent).then(|| {
                chrono::Utc::now() + chrono::Duration::milliseconds(delay.as_millis() as i64)
            });
            if self.set_failure(&id, &cancel, &error, retry).await.is_err() || permanent {
                break;
            }
            tokio::select! {_=cancel.cancelled()=>break,_=tokio::time::sleep(delay)=>{}}
        }
    }
    async fn set_connecting(&self, id: &str, cancel: &CancellationToken) -> Result<()> {
        let _guard = self.inner.mutation.lock().await;
        if cancel.is_cancelled() {
            return Ok(());
        }
        let mut rows = self.inner.rows.read().unwrap().clone();
        let i = row_index(&rows, id)?;
        if rows[i]["status"] == "connecting"
            && rows[i]["nextRetryAt"].is_null()
            && rows[i]["error"].is_null()
        {
            return Ok(());
        }
        rows[i]["status"] = json!("connecting");
        rows[i]["nextRetryAt"] = Value::Null;
        rows[i]["error"] = Value::Null;
        self.persist(&rows).await?;
        *self.inner.rows.write().unwrap() = rows;
        self.changed();
        Ok(())
    }
    async fn set_failure(
        &self,
        id: &str,
        cancel: &CancellationToken,
        error: &anyhow::Error,
        retry: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<()> {
        let _guard = self.inner.mutation.lock().await;
        if cancel.is_cancelled() {
            return Ok(());
        }
        let mut rows = self.inner.rows.read().unwrap().clone();
        let i = row_index(&rows, id)?;
        let failure = error.downcast_ref::<HostError>();
        rows[i]["status"] = json!(if retry.is_some() { "offline" } else { "error" });
        rows[i]["nextRetryAt"] =
            json!(retry.map(|v| v.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)));
        rows[i]["error"] = json!({"code":failure.map(|e|e.code.as_str()).unwrap_or("host_offline"),"message":failure.map(|e|e.message.as_str()).unwrap_or("Cannot reach host. Check the connection; retrying automatically.")});
        self.persist(&rows).await?;
        *self.inner.rows.write().unwrap() = rows;
        self.changed();
        Ok(())
    }
    async fn persist_protected_floor(&self, id: &str, cancel: &CancellationToken) -> Result<()> {
        let _guard = self.inner.mutation.lock().await;
        ensure!(!cancel.is_cancelled(), "Host connection replaced");
        let mut rows = self.inner.rows.read().unwrap().clone();
        let i = row_index(&rows, id)?;
        if rows[i]["minimumSecurityMode"] == "protected" {
            return Ok(());
        }
        rows[i]["minimumSecurityMode"] = json!("protected");
        self.persist(&rows).await?;
        *self.inner.rows.write().unwrap() = rows;
        self.changed();
        Ok(())
    }
    async fn connect_once(
        &self,
        id: &str,
        target: &Target,
        cancel: &CancellationToken,
        floor: &mut SecurityMode,
        attempt: &mut u32,
    ) -> Result<()> {
        let mut connection = tokio::select! {_=cancel.cancelled()=>return Ok(()),result=connector::Connection::connect(target,*floor,self.inner.budget.clone(),cancel.clone())=>result?};
        if connection.security == Some(SecurityMode::Protected) {
            *floor = SecurityMode::Protected;
        }
        let result=async{
            if connection.security==Some(SecurityMode::Protected){self.persist_protected_floor(id,cancel).await?;}
            let mut hello=json!({"type":"hello","clientId":format!("host-link-{}-{id}",self.identity()?["serverId"].as_str().unwrap_or("")),"clientType":"cli","protocolVersion":1,"capabilities":client_capabilities()});
            if let Some(password)=&target.password{hello["auth"]=json!({"kind":"password","password":password});}connection.sender.json(hello).await?;
            let info=tokio::time::timeout(Duration::from_secs(10),async{loop{let frame=connection.incoming.recv().await.ok_or_else(||anyhow!("Host closed before identity"))??;if let HostFrame::Text(text)=frame{let value:Value=serde_json::from_str(&text)?;if value["type"]=="hello.rejected"{return Err(HostError::new("authentication_failed","Host rejected the credentials. Replace the connection credentials.").into());}if value["type"]=="session"&&value["message"]["type"]=="status"&&value["message"]["payload"]["status"]=="server_info"{return Ok::<_,anyhow::Error>(value["message"]["payload"].clone());}}}}).await??;
            let link=Arc::new(Link{sender:connection.sender.clone(),cancel:connection.cancel.clone(),pending:Mutex::new(HashMap::new()),channels:Mutex::new(HashMap::new())});
            self.accept(id,&info,cancel,link.clone()).await?;
            *attempt=0;
            let mut heartbeat=tokio::time::interval(Duration::from_secs(10));let mut last=tokio::time::Instant::now();
            loop{tokio::select!{_=cancel.cancelled()=>break,_=heartbeat.tick()=>{ensure!(last.elapsed()<Duration::from_secs(45),"Host liveness timeout");link.sender.json(json!({"type":"ping"})).await?;},frame=connection.incoming.recv()=>{last=tokio::time::Instant::now();link.deliver(frame.ok_or_else(||anyhow!("Host disconnected"))??).await?;}}}Ok(())
        }.await;
        connection.cancel.cancel();
        let _ = connection.task.await;
        result
    }
    async fn accept(
        &self,
        id: &str,
        info: &Value,
        cancel: &CancellationToken,
        link: Arc<Link>,
    ) -> Result<()> {
        let _guard = self.inner.mutation.lock().await;
        ensure!(!cancel.is_cancelled(), "Host connection replaced");
        let mut rows = self.inner.rows.read().unwrap().clone();
        let i = row_index(&rows, id)?;
        let server = field(info, "serverId")?;
        if let Some(pin) = rows[i]["serverId"].as_str() {
            ensure!(
                pin == server,
                HostError::new(
                    "server_id_mismatch",
                    "Daemon identity changed. Verify the destination and explicitly replace its identity."
                )
            );
        } else {
            rows[i]["serverId"] = json!(server);
            self.persist(&rows).await?;
            *self.inner.rows.write().unwrap() = rows.clone();
        }
        ensure!(
            self.identity()?["serverId"] != server,
            HostError::new(
                "self_host",
                "This connection reaches this daemon. Remove it or replace the connection."
            )
        );
        ensure!(
            rows.iter().position(|r| r["serverId"] == server) == Some(i),
            HostError::new(
                "duplicate_host",
                "Another registration already owns this daemon. Remove the duplicate registration."
            )
        );
        ensure!(
            info["features"]["connectedHosts"] == true
                && info["features"]["hostMultiplexing"] == true,
            HostError::new(
                "unsupported_capability",
                "Update the destination daemon to use connected hosts."
            )
        );
        rows[i]["status"] = json!("online");
        rows[i]["lastOnlineAt"] =
            json!(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
        rows[i]["error"] = Value::Null;
        rows[i]["nextRetryAt"] = Value::Null;
        self.persist(&rows).await?;
        ensure!(!cancel.is_cancelled(), "Host connection replaced");
        *self.inner.rows.write().unwrap() = rows;
        self.inner.live.lock().unwrap().insert(id.into(), link);
        self.changed();
        Ok(())
    }
    pub async fn shutdown(&self) -> Result<()> {
        self.inner.closed.store(true, Ordering::Release);
        self.inner.cancel.cancel();
        let routes: Vec<_> = self
            .inner
            .routes
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect();
        for route in routes {
            route.stop("host_offline", "Daemon is stopping");
        }
        let tasks = std::mem::take(&mut *self.inner.tasks.lock().unwrap());
        for task in tasks {
            let _ = task.await;
        }
        self.inner.live.lock().unwrap().clear();
        Ok(())
    }
}
fn summary(row: &Value) -> Value {
    let mut value = row.clone();
    if let Some(object) = value.as_object_mut() {
        object.remove("connection");
    }
    value
}
fn field<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .ok_or_else(|| HostError::new("invalid_message", "Invalid host request").into())
}
fn row_index(rows: &[Value], id: &str) -> Result<usize> {
    rows.iter()
        .position(|r| r["registrationId"] == id)
        .ok_or_else(|| HostError::new("host_unknown", "Unknown host registration").into())
}
fn metadata_equal(a: &Value, b: &Value) -> bool {
    a.as_object()
        .zip(b.as_object())
        .is_some_and(|(a, b)| a.len() == b.len() && a.iter().all(|(k, v)| b.get(k) == Some(v)))
}
fn validate_input(input: &Value, patch: bool) -> Result<()> {
    ensure!(input.is_object(), "Invalid host input");
    if !patch || input.get("connection").is_some() {
        let connection = field(input, "connection")?;
        ensure!(
            !connection.is_empty() && connection.encode_utf16().count() <= 16384,
            "Invalid connection length"
        );
    }
    if let Some(label) = input.get("label") {
        ensure!(
            (patch && label.is_null())
                || label
                    .as_str()
                    .is_some_and(|s| s.encode_utf16().count() <= 256),
            "Invalid label"
        );
    }
    if let Some(metadata) = input.get("metadata") {
        ensure!(
            metadata
                .as_object()
                .is_some_and(|o| o.iter().all(|(k, v)| k.encode_utf16().count() <= 256
                    && v.as_str().is_some_and(|s| s.encode_utf16().count() <= 4096))),
            "Invalid metadata"
        );
    }
    if let Some(key) = input.get("sourceKey") {
        ensure!(
            key.as_str()
                .is_some_and(|s| !s.is_empty() && s.encode_utf16().count() <= 256),
            "Invalid source key"
        );
    }
    for key in ["enabled", "replaceIdentity"] {
        if let Some(v) = input.get(key) {
            ensure!(v.is_boolean(), "Invalid host flag");
        }
    }
    Ok(())
}
fn validate_saved(row: &Value) -> Result<()> {
    validate_input(row, true)?;
    for key in ["registrationId", "connection", "targetDescription"] {
        field(row, key)?;
    }
    field(&row["source"], "id")?;
    field(&row["source"], "key")?;
    ensure!(
        row["metadata"].is_object() && row["enabled"].is_boolean(),
        "Cannot read saved connected hosts"
    );
    for key in ["serverId", "label", "lastOnlineAt", "nextRetryAt"] {
        ensure!(
            row.get(key).is_some_and(|v| v.is_null() || v.is_string()),
            "Cannot read saved connected hosts"
        );
    }
    ensure!(
        matches!(
            row["status"].as_str(),
            Some("connecting" | "online" | "offline" | "error" | "disabled")
        ),
        "Cannot read saved connected hosts"
    );
    ensure!(
        row.get("error")
            .is_some_and(|e| e.is_null() || (e["code"].is_string() && e["message"].is_string())),
        "Cannot read saved connected hosts"
    );
    ensure!(
        row.get("minimumSecurityMode")
            .is_none_or(|v| v == "protected"),
        "Cannot read saved connected hosts"
    );
    Ok(())
}
fn remote_error(message: &Value) -> HostError {
    HostError::new(
        message["payload"]["code"].as_str().unwrap_or("host_error"),
        message["payload"]["error"]
            .as_str()
            .unwrap_or("Host request failed"),
    )
}
pub fn encode_frame(id: &str, payload: &[u8]) -> Result<Bytes> {
    ensure!(
        !id.is_empty() && id.len() <= 256 && payload.len() <= 100 * 1024 * 1024 - 259,
        "Invalid host route frame"
    );
    let mut bytes = bytes::BytesMut::with_capacity(3 + id.len() + payload.len());
    bytes.extend_from_slice(&[0x30]);
    bytes.extend_from_slice(&(id.len() as u16).to_be_bytes());
    bytes.extend_from_slice(id.as_bytes());
    bytes.extend_from_slice(payload);
    Ok(bytes.freeze())
}
pub fn decode_frame(bytes: &[u8]) -> Result<(&str, usize)> {
    ensure!(
        bytes.len() >= 3 && bytes[0] == 0x30,
        "Invalid host route frame"
    );
    let length = u16::from_be_bytes([bytes[1], bytes[2]]) as usize;
    ensure!(
        (1..=256).contains(&length)
            && bytes.len() >= 3 + length
            && bytes.len() - 3 - length <= 100 * 1024 * 1024 - 259,
        "Invalid host route frame"
    );
    Ok((std::str::from_utf8(&bytes[3..3 + length])?, 3 + length))
}
fn client_capabilities() -> Value {
    let names = [
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
    ];
    Value::Object(
        names
            .into_iter()
            .map(|name| (name.to_owned(), Value::Bool(true)))
            .collect(),
    )
}
fn security_floor(row: &Value, target: &Target) -> SecurityMode {
    let offered = target
        .security
        .as_ref()
        .map(|(_, mode)| *mode)
        .unwrap_or_default();
    if row["minimumSecurityMode"] == "protected" {
        SecurityMode::Protected
    } else {
        offered
    }
}
