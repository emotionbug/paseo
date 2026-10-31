use super::{ProviderOperationError, mapping, schema, session::SessionCore};
use crate::plugins::worker::PluginWorker;
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Semaphore, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

type Reply = oneshot::Sender<Result<Value>>;
struct Request {
    kind: String,
    session: Option<String>,
    reply: Reply,
}
struct Acceptance {
    kind: String,
    session: Option<String>,
    reply: Reply,
}
#[derive(Clone)]
struct Route {
    root: Weak<SessionCore>,
    child: bool,
}
#[derive(Default)]
struct State {
    connected: Option<Reply>,
    acceptances: HashMap<String, Acceptance>,
    requests: HashMap<String, Request>,
    prompts: HashMap<(String, String), Reply>,
    sessions: HashMap<String, Vec<String>>,
    routes: HashMap<String, Route>,
    error: Option<String>,
    closing: bool,
}

pub(super) struct Connection {
    pub id: String,
    pub worker: Arc<PluginWorker>,
    pub capabilities: OnceLock<Vec<String>>,
    pub done: CancellationToken,
    failed: AtomicBool,
    state: Mutex<State>,
    request_slots: Semaphore,
    send_slots: Semaphore,
}

// A dropped caller removes only its own waiter. In particular, cancellation
// never turns a possibly accepted prompt into an automatic retry.
struct Waiter<'a> {
    connection: &'a Connection,
    key: String,
    session: Option<String>,
    table: u8,
    receiver: Option<oneshot::Receiver<Result<Value>>>,
}
impl Drop for Waiter<'_> {
    fn drop(&mut self) {
        let mut state = self.connection.state.lock().unwrap();
        match self.table {
            0 => {
                state.acceptances.remove(&self.key);
            }
            1 => {
                state.requests.remove(&self.key);
            }
            _ => {
                if let Some(session) = &self.session {
                    state.prompts.remove(&(session.clone(), self.key.clone()));
                }
            }
        }
    }
}
impl Waiter<'_> {
    async fn receive(&mut self, cancel: &CancellationToken) -> Result<Value> {
        let receiver = self
            .receiver
            .take()
            .context("Provider reply already consumed")?;
        tokio::select! {
            biased;
            result = receiver => result.context("Provider connection closed")?,
            _ = cancel.cancelled() => bail!("Provider request aborted"),
            _ = self.connection.done.cancelled() => bail!("{}",self.connection.error()),
        }
    }
}

impl Connection {
    pub async fn connect(
        worker: Arc<PluginWorker>,
        provider: &str,
        launch: Option<Value>,
        cancel: CancellationToken,
    ) -> Result<Arc<Self>> {
        if cancel.is_cancelled() {
            bail!("Provider request aborted");
        }
        let id = uuid::Uuid::new_v4().to_string();
        let receiver = worker.register_connection(&id)?;
        let (tx, rx) = oneshot::channel();
        let connection = Arc::new(Self {
            id,
            worker,
            capabilities: OnceLock::new(),
            done: CancellationToken::new(),
            failed: AtomicBool::new(false),
            state: Mutex::new(State {
                connected: Some(tx),
                ..State::default()
            }),
            request_slots: Semaphore::new(256),
            send_slots: Semaphore::new(256),
        });
        let weak = Arc::downgrade(&connection);
        let worker_stop = connection.worker.cancellation();
        let stop = connection.done.clone();
        tokio::spawn(async move {
            Self::receive(weak, receiver, worker_stop, stop).await;
        });
        let mut request = json!({"versions":[1],"capabilities":mapping::CAPABILITIES});
        if let Some(launch) = launch {
            request["launch"] = launch;
        }
        let result = async {
            connection.worker.send(json!({"type":"provider.connect","providerId":provider,"connectionId":connection.id,"request":request})).await?;
            tokio::select! {
                result = tokio::time::timeout(Duration::from_secs(30), rx) => {
                    result.map_err(|_|anyhow!("Plugin provider connection timed out: {}.{provider}",connection.worker.id))?
                        .context("Plugin provider connection closed")??;
                    Ok(())
                }
                _ = cancel.cancelled() => bail!("Provider request aborted"),
            }
        }.await;
        if let Err(error) = result {
            connection.fail(&error.to_string(), true).await;
            let _ = connection
                .worker
                .send(json!({"type":"provider.close","connectionId":connection.id}))
                .await;
            return Err(error);
        }
        Ok(connection)
    }

    async fn receive(
        weak: Weak<Self>,
        mut receiver: mpsc::Receiver<Value>,
        worker_stop: CancellationToken,
        stop: CancellationToken,
    ) {
        loop {
            let message = tokio::select! {
                biased;
                _ = stop.cancelled() => break,
                _ = worker_stop.cancelled() => None,
                message = receiver.recv() => message,
            };
            let Some(connection) = weak.upgrade() else {
                break;
            };
            let Some(message) = message else {
                connection
                    .fail("Plugin provider connection closed", true)
                    .await;
                break;
            };
            if let Err(error) = connection.accept_envelope(message).await {
                connection.fail(&format!("{error:#}"), true).await;
                let _ = connection
                    .worker
                    .send(json!({"type":"provider.close","connectionId":connection.id}))
                    .await;
                break;
            }
        }
    }

    pub fn is_live(&self) -> bool {
        !self.failed.load(Ordering::Acquire) && !self.done.is_cancelled() && self.worker.is_live()
    }
    pub fn error(&self) -> String {
        self.state
            .lock()
            .unwrap()
            .error
            .clone()
            .unwrap_or_else(|| "Provider connection closed".into())
    }
    pub fn supports(&self, capability: &str) -> bool {
        self.capabilities
            .get()
            .is_some_and(|values| values.iter().any(|value| value == capability))
    }
    pub fn register_session(&self, id: &str, core: &Arc<SessionCore>) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        if !self.is_live() || state.closing {
            bail!("Provider connection is closed");
        }
        if state.routes.contains_key(id) {
            bail!("Provider session already exists: {id}");
        }
        state.routes.insert(
            id.to_owned(),
            Route {
                root: Arc::downgrade(core),
                child: false,
            },
        );
        Ok(())
    }
    pub fn remove_session(&self, id: &str) {
        let mut state = self.state.lock().unwrap();
        let root = state.routes.remove(id).map(|route| route.root);
        state.sessions.remove(id);
        if let Some(root) = root {
            let children: Vec<String> = state
                .routes
                .iter()
                .filter(|(_, route)| route.root.ptr_eq(&root))
                .map(|(id, _)| id.clone())
                .collect();
            for child in children {
                state.routes.remove(&child);
                state.sessions.remove(&child);
            }
        }
    }

    pub async fn send(&self, mut input: Value, cancel: &CancellationToken) -> Result<()> {
        schema::input(&mut input)?;
        let _permit = tokio::select! {
            permit = self.send_slots.acquire() => permit?,
            _ = cancel.cancelled() => bail!("Provider request aborted"),
            _ = self.done.cancelled() => bail!("{}",self.error()),
        };
        let id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        {
            let mut state = self.state.lock().unwrap();
            if !self.is_live() || state.closing {
                bail!("Provider connection is closed");
            }
            let session = input["sessionId"].as_str();
            let kind = input["type"].as_str().unwrap();
            let capabilities = if kind == "session.open" || session.is_none() {
                self.capabilities
                    .get()
                    .context("Provider connection is not ready")?
            } else {
                state
                    .sessions
                    .get(session.unwrap())
                    .with_context(|| format!("Unknown provider session: {}", session.unwrap()))?
            };
            mapping::require_capabilities(capabilities, &input)?;
            state.acceptances.insert(
                id.clone(),
                Acceptance {
                    kind: kind.to_owned(),
                    session: session.map(str::to_owned),
                    reply: tx,
                },
            );
        }
        let mut waiter = Waiter {
            connection: self,
            key: id.clone(),
            session: None,
            table: 0,
            receiver: Some(rx),
        };
        self.worker.send(json!({"type":"provider.send","connectionId":self.id,"acceptanceId":id,"input":input})).await?;
        waiter.receive(cancel).await?;
        Ok(())
    }

    pub async fn complete(&self, mut input: Value, cancel: &CancellationToken) -> Result<Value> {
        let _permit = tokio::select! {
            permit = self.request_slots.acquire() => permit?,
            _ = cancel.cancelled() => bail!("Provider request aborted"),
            _ = self.done.cancelled() => bail!("{}",self.error()),
        };
        let id = uuid::Uuid::new_v4().to_string();
        input["requestId"] = json!(id);
        let (tx, rx) = oneshot::channel();
        {
            let mut state = self.state.lock().unwrap();
            state.requests.insert(
                id.clone(),
                Request {
                    kind: input["type"]
                        .as_str()
                        .context("Missing provider request type")?
                        .to_owned(),
                    session: input["sessionId"].as_str().map(str::to_owned),
                    reply: tx,
                },
            );
        }
        let mut waiter = Waiter {
            connection: self,
            key: id,
            session: None,
            table: 1,
            receiver: Some(rx),
        };
        self.send(input, cancel).await?;
        waiter.receive(cancel).await
    }

    pub async fn prompt(
        &self,
        session: &str,
        prompt: Value,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        let _permit = tokio::select! {
            permit = self.request_slots.acquire() => permit?,
            _ = cancel.cancelled() => bail!("Provider request aborted"),
            _ = self.done.cancelled() => bail!("{}",self.error()),
        };
        let id = prompt["clientMessageId"]
            .as_str()
            .context("Missing client message ID")?
            .to_owned();
        let key = (session.to_owned(), id.clone());
        let (tx, rx) = oneshot::channel();
        {
            let mut state = self.state.lock().unwrap();
            if state.prompts.contains_key(&key) {
                bail!("Provider prompt is already pending: {id}");
            }
            state.prompts.insert(key, tx);
        }
        let mut waiter = Waiter {
            connection: self,
            key: id,
            session: Some(session.to_owned()),
            table: 2,
            receiver: Some(rx),
        };
        self.send(
            json!({"type":"session.prompt","sessionId":session,"prompt":prompt}),
            cancel,
        )
        .await?;
        waiter.receive(cancel).await
    }

    async fn accept_envelope(&self, mut message: Value) -> Result<()> {
        schema::envelope(&mut message)?;
        match message["type"]
            .as_str()
            .context("Missing provider envelope type")?
        {
            "provider.connected" => {
                if message["version"].as_f64() != Some(1.0) {
                    bail!(
                        "Plugin provider selected unsupported version {}",
                        message["version"]
                    );
                }
                let offered: Vec<String> = mapping::CAPABILITIES
                    .iter()
                    .map(|value| (*value).to_owned())
                    .collect();
                let capabilities = mapping::negotiate(&message["capabilities"], &offered)?;
                self.capabilities
                    .set(capabilities)
                    .map_err(|_| anyhow!("Provider connected twice"))?;
                if let Some(tx) = self.state.lock().unwrap().connected.take() {
                    let _ = tx.send(Ok(Value::Null));
                }
            }
            "provider.connect_failed" => bail!(
                "{}",
                message["error"]
                    .as_str()
                    .context("Missing provider connection error")?
            ),
            "provider.accepted" | "provider.rejected" => {
                let id = message["acceptanceId"]
                    .as_str()
                    .context("Missing provider acceptance ID")?;
                let mut state = self.state.lock().unwrap();
                if let Some(pending) = state.acceptances.remove(id) {
                    let result = if message["type"] == "provider.accepted" {
                        if let Some(session) = pending.session {
                            if pending.kind == "session.open" {
                                state.sessions.entry(session).or_default();
                            } else if pending.kind == "session.close" {
                                state.sessions.remove(&session);
                            }
                        }
                        Ok(Value::Null)
                    } else {
                        Err(anyhow!(
                            message["error"]
                                .as_str()
                                .context("Missing provider rejection error")?
                                .to_owned()
                        ))
                    };
                    let _ = pending.reply.send(result);
                }
            }
            "provider.event" => {
                let event = message.as_object_mut().unwrap().remove("event").unwrap();
                self.accept_event(event).await?;
            }
            "provider.closed" => {
                let error = message
                    .get("error")
                    .map(|value| value.as_str().context("Invalid provider close error"))
                    .transpose()?;
                self.fail(
                    error.unwrap_or("Provider connection closed"),
                    error.is_some(),
                )
                .await;
            }
            _ => bail!("Unknown provider envelope type"),
        }
        Ok(())
    }

    async fn accept_event(&self, mut event: Value) -> Result<()> {
        if event["type"] == "timeline.item"
            && event["item"]["type"] == "plugin"
            && event["item"]["pluginId"] != self.worker.id
        {
            bail!(
                "Provider plugin {} cannot emit timeline items for {}",
                self.worker.id,
                event["item"]["pluginId"]
            );
        }
        let kind = event["type"].as_str().unwrap().to_owned();
        let session = event["sessionId"].as_str().map(str::to_owned);
        let route = {
            let mut state = self.state.lock().unwrap();
            if kind == "session.opened" {
                let capabilities = mapping::negotiate(
                    &event["capabilities"],
                    self.capabilities
                        .get()
                        .context("Provider event before connection negotiation")?,
                )?;
                event["capabilities"] = json!(capabilities);
                let id = session.as_ref().unwrap();
                if let Some(parent) = event["parentSessionId"].as_str() {
                    if !state
                        .sessions
                        .get(parent)
                        .is_some_and(|caps| caps.iter().any(|cap| cap == "session.subsession"))
                    {
                        bail!("Provider parent session did not negotiate session.subsession");
                    }
                    if !state.routes.contains_key(id) {
                        if let Some(parent_route) = state.routes.get(parent).cloned() {
                            state.routes.insert(
                                id.clone(),
                                Route {
                                    root: parent_route.root,
                                    child: true,
                                },
                            );
                        }
                    }
                }
                state.sessions.insert(id.clone(), capabilities);
            }
            if matches!(
                kind.as_str(),
                "request.failed" | "request.completed" | "catalog" | "sessions" | "session.ready"
            ) {
                if let Some(id) = event["requestId"].as_str() {
                    let ready = state.requests.get(id).is_some_and(|pending| {
                        kind == "request.failed"
                            || pending.kind != "session.open"
                            || kind == "session.ready"
                    });
                    if ready {
                        if let Some(pending) = state.requests.remove(id) {
                            let result = if kind == "request.failed" {
                                Err(ProviderOperationError::from_value(&event["error"]).into())
                            } else {
                                Ok(event.clone())
                            };
                            let _ = pending.reply.send(result);
                        }
                    }
                }
                if kind != "session.ready" {
                    return Ok(());
                }
            }
            session
                .as_ref()
                .and_then(|id| state.routes.get(id))
                .cloned()
        };
        // Update session state before settling prompts or terminal requests. A
        // caller waking from a reply observes the state established by that event.
        if let Some(route) = route {
            if let Some(root) = route.root.upgrade() {
                root.accept(event.clone(), route.child).await?;
            }
        }
        if let Some(session) = session {
            let mut state = self.state.lock().unwrap();
            if kind == "session.prompt_result" {
                if let Some(tx) = state.prompts.remove(&(
                    session.clone(),
                    event["clientMessageId"].as_str().unwrap().to_owned(),
                )) {
                    let _ = tx.send(Ok(event["result"].clone()));
                }
            }
            if matches!(kind.as_str(), "session.runtime_failed" | "session.closed") {
                let error = event["error"]["message"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("Provider session {session} closed"));
                let ids: Vec<String> = state
                    .requests
                    .iter()
                    .filter(|(_, pending)| pending.session.as_deref() == Some(session.as_str()))
                    .map(|(id, _)| id.clone())
                    .collect();
                for id in ids {
                    if let Some(pending) = state.requests.remove(&id) {
                        let result = if kind == "session.closed" && pending.kind == "session.close"
                        {
                            Ok(event.clone())
                        } else if kind == "session.runtime_failed" {
                            Err(ProviderOperationError::from_value(&event["error"]).into())
                        } else {
                            Err(anyhow!(error.clone()))
                        };
                        let _ = pending.reply.send(result);
                    }
                }
                let keys: Vec<_> = state
                    .prompts
                    .keys()
                    .filter(|(id, _)| id == &session)
                    .cloned()
                    .collect();
                for key in keys {
                    if let Some(tx) = state.prompts.remove(&key) {
                        let _ = tx.send(Err(anyhow!(error.clone())));
                    }
                }
                if kind == "session.closed" {
                    state.sessions.remove(&session);
                    state.routes.remove(&session);
                }
            }
        }
        Ok(())
    }

    async fn fail(&self, error: &str, runtime_failure: bool) {
        let (roots, connected, acceptances, requests, prompts) = {
            let mut state = self.state.lock().unwrap();
            if self.failed.swap(true, Ordering::AcqRel) {
                return;
            }
            state.error = Some(error.to_owned());
            let roots: Vec<_> = state
                .routes
                .values()
                .filter(|route| !route.child)
                .filter_map(|route| route.root.upgrade())
                .collect();
            state.routes.clear();
            state.sessions.clear();
            for root in &roots {
                root.mark_terminal();
            }
            (
                roots,
                state.connected.take(),
                std::mem::take(&mut state.acceptances),
                std::mem::take(&mut state.requests),
                std::mem::take(&mut state.prompts),
            )
        };
        let _completion = self.done.clone().drop_guard();
        self.worker.remove_connection(&self.id);
        // Publish terminal events before releasing requests: otherwise a caller
        // reopening a stale session could close its old sink first and lose the
        // interrupted turn's failure. No state lock is held during publication.
        for root in roots {
            let _ = root.connection_closed(error, runtime_failure).await;
        }
        if let Some(tx) = connected {
            let _ = tx.send(Err(anyhow!(error.to_owned())));
        }
        for (_, pending) in acceptances {
            let _ = pending.reply.send(Err(anyhow!(error.to_owned())));
        }
        for (_, pending) in requests {
            let _ = pending.reply.send(Err(anyhow!(error.to_owned())));
        }
        for (_, tx) in prompts {
            let _ = tx.send(Err(anyhow!(error.to_owned())));
        }
    }

    pub async fn close(&self) -> Result<()> {
        if self.failed.load(Ordering::Acquire) {
            self.done.cancelled().await;
            return Ok(());
        }
        if !self.worker.is_live() {
            self.fail("Provider connection closed", false).await;
            self.done.cancelled().await;
            return Ok(());
        }
        let send = {
            let mut state = self.state.lock().unwrap();
            if self.done.is_cancelled() {
                return Ok(());
            }
            let send = !state.closing;
            state.closing = true;
            send
        };
        if send {
            if let Err(error) = self
                .worker
                .send(json!({"type":"provider.close","connectionId":self.id}))
                .await
            {
                self.fail(&error.to_string(), false).await;
                return Err(error);
            }
        }
        self.done.cancelled().await;
        let error = self.error();
        if error != "Provider connection closed" {
            bail!("{error}");
        }
        Ok(())
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        self.done.cancel();
        self.worker.remove_connection(&self.id);
    }
}
