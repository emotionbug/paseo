mod projection;
mod prompt;
mod tools;

use super::{
    catalog,
    runtime::{Generation, Runtime, RuntimeManager, read_sse},
};
use crate::providers::api::{AgentSession, EventSink, LaunchContext, ProviderSpec, RewindTarget};
use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use projection::{
    Projection, diagnostic, event, filter_reverted, history_message, model, s, session_id,
    subagent, timeline,
};
pub(super) use prompt::{prompt_parts, prompt_text};
use rand::RngCore;
use reqwest::Method;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{Notify, mpsc, watch};
use tokio_util::sync::CancellationToken;
pub(super) use tools::map_tool;

fn query(cwd: &str) -> Vec<(String, String)> {
    vec![("directory".into(), cwd.into())]
}
fn segment(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
}
fn model_ref(value: &Value) -> Option<Value> {
    let value = value.as_str().filter(|s| !s.is_empty())?;
    let (provider, id) = value.split_once('/').unwrap_or(("opencode", value));
    Some(json!({"providerID":provider,"modelID":id}))
}
fn normalize(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "default")
        .map(str::to_owned)
}
fn message_id() -> String {
    static LAST: AtomicU64 = AtomicU64::new(0);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
        * 0x1000;
    let mut prev = LAST.load(Ordering::Relaxed);
    let value = loop {
        let next = now.max(prev + 1);
        match LAST.compare_exchange_weak(prev, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break next,
            Err(current) => prev = current,
        }
    };
    const ALPHABET: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let mut bytes = [0u8; 14];
    rand::thread_rng().fill_bytes(&mut bytes);
    let suffix: String = bytes
        .iter()
        .map(|b| ALPHABET[(b % 62) as usize] as char)
        .collect();
    format!("msg_{:012x}{suffix}", value & 0xffffffffffff)
}
fn tagged(mut e: Value, provider: &str, turn: Option<&str>) -> Value {
    e["provider"] = json!(provider);
    if let Some(request) = e.get_mut("request") {
        request["provider"] = json!(provider);
    }
    if let Some(turn) = turn {
        e["turnId"] = json!(turn);
    }
    e
}

struct Pending {
    request: Value,
    directory: String,
}
struct State {
    config: Value,
    projection: Projection,
    turn: Option<String>,
    stopping: bool,
    cancellation_turn: Option<String>,
    next_turn: u64,
    dispatch: Option<String>,
    pending: BTreeMap<String, Pending>,
    running: HashMap<String, Value>,
    abort_inflight: usize,
    abort_epoch: u64,
    abort_error: Option<String>,
    mcp_ready: bool,
    modes: Option<Vec<Value>>,
    unrelated: HashSet<String>,
    features: Arc<[Value]>,
    auto_accept: bool,
}
impl State {
    fn start(&mut self) -> String {
        let id = format!("opencode-turn-{}", self.next_turn);
        self.next_turn += 1;
        self.turn = Some(id.clone());
        self.running.clear();
        id
    }
    fn interrupted_tools(&mut self) -> Vec<Value> {
        self.running
            .drain()
            .map(|(_, mut item)| {
                item["status"] = json!("failed");
                item["error"] = json!({"message":"Tool execution aborted"});
                if item["detail"]["type"] == "sub_agent" {
                    let old = s(&item["detail"]["log"]);
                    item["detail"]["log"] = json!(if old.trim().is_empty() {
                        "Tool execution aborted".to_owned()
                    } else {
                        format!("{old}\nTool execution aborted")
                    });
                }
                timeline(item)
            })
            .collect()
    }
    fn acknowledge_stop(&mut self) -> Option<(String, Value)> {
        self.cancellation_turn.take().map(|turn| {
            (
                turn,
                json!({"type":"turn_canceled","provider":"opencode","reason":"interrupted"}),
            )
        })
    }
    fn finish_stop(&mut self) -> Option<(String, Value)> {
        let e = self.acknowledge_stop();
        self.stopping = false;
        self.projection.root.reset_turn();
        e
    }
}
struct Inner {
    id: String,
    cwd: String,
    spec: ProviderSpec,
    manager: Arc<RuntimeManager>,
    context: LaunchContext,
    sink: EventSink,
    state: Mutex<State>,
    runtime: Mutex<Option<Arc<Runtime>>>,
    closed: AtomicBool,
    persist: bool,
    cancel: CancellationToken,
    stream_cancel: Mutex<CancellationToken>,
    ready: watch::Sender<bool>,
    changed: Notify,
    reconnect: tokio::sync::Mutex<()>,
    start_gate: tokio::sync::Mutex<()>,
    usage_key: String,
}
struct LegacySession {
    inner: Arc<Inner>,
    capabilities: Value,
}
impl Drop for LegacySession {
    fn drop(&mut self) {
        self.inner.cancel.cancel();
    }
}

pub async fn attach(
    runtime: Arc<Runtime>,
    manager: Arc<RuntimeManager>,
    spec: ProviderSpec,
    info: Value,
    config: Value,
    context: LaunchContext,
    options: Value,
    sink: EventSink,
) -> Result<Arc<dyn AgentSession>> {
    let id = info["id"]
        .as_str()
        .ok_or_else(|| anyhow!("OpenCode returned a session without an id"))?
        .to_owned();
    let cwd = config["cwd"]
        .as_str()
        .ok_or_else(|| anyhow!("OpenCode session requires cwd"))?
        .to_owned();
    runtime.bind_session(&id, &context)?;
    manager.remember_session(&id, &runtime);
    let cancel = context.cancel.child_token();
    let stream_cancel = cancel.child_token();
    let (ready, _) = watch::channel(false);
    let mut projection = Projection::new(id.clone(), cwd.clone());
    let mut messages = runtime
        .request(
            Method::GET,
            &format!("/session/{}/message", segment(&id)),
            &query(&cwd),
            None,
            cancel.clone(),
        )
        .await
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default();
    filter_reverted(&mut messages, &info);
    projection.root.hydrate(&messages);
    if let Ok(providers) = runtime
        .request(Method::GET, "/provider", &query(&cwd), None, cancel.clone())
        .await
    {
        let connected: HashSet<_> = providers["connected"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        for provider in providers["all"].as_array().into_iter().flatten() {
            if provider["source"] != "api" && !connected.contains(s(&provider["id"])) {
                continue;
            }
            if let Some(models) = provider["models"].as_object() {
                for (key, model) in models {
                    if let Some(n) = model["limit"]["context"].as_f64().filter(|n| *n > 0.0) {
                        projection
                            .windows
                            .insert(format!("{}/{key}", s(&provider["id"])), n);
                    }
                }
            }
        }
    }
    if let Some(n) = config["model"]
        .as_str()
        .and_then(|m| projection.windows.get(m))
    {
        projection.root.usage = json!({"contextWindowMaxTokens":n});
    }
    let features: Arc<[Value]> = catalog::features(&config).into();
    let auto_accept =
        config["featureValues"]["auto_accept"] == true && config["toolPolicy"].is_null();
    let inner = Arc::new(Inner {
        id,
        cwd,
        spec,
        manager,
        context,
        sink,
        state: Mutex::new(State {
            config,
            features,
            auto_accept,
            projection,
            turn: None,
            stopping: false,
            cancellation_turn: None,
            next_turn: 0,
            dispatch: None,
            pending: BTreeMap::new(),
            running: HashMap::new(),
            abort_inflight: 0,
            abort_epoch: 0,
            abort_error: None,
            mcp_ready: false,
            modes: None,
            unrelated: HashSet::new(),
        }),
        runtime: Mutex::new(Some(runtime)),
        closed: AtomicBool::new(false),
        persist: options["persistSession"] != false,
        cancel,
        stream_cancel: Mutex::new(stream_cancel.clone()),
        ready,
        changed: Notify::new(),
        reconnect: tokio::sync::Mutex::new(()),
        start_gate: tokio::sync::Mutex::new(()),
        usage_key: uuid::Uuid::new_v4().to_string(),
    });
    Inner::spawn_stream(&inner, stream_cancel);
    let capabilities = json!({"supportsStreaming":true,"supportsSessionPersistence":true,"supportsSessionListing":true,"supportsDynamicModes":true,"supportsMcpServers":true,"supportsReasoningStream":true,"supportsToolInvocations":true,"supportsRewindConversation":false,"supportsRewindFiles":false,"supportsRewindBoth":true});
    Ok(Arc::new(LegacySession {
        inner,
        capabilities,
    }))
}
impl Inner {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }
    fn runtime(&self) -> Result<Arc<Runtime>> {
        self.runtime
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .ok_or_else(|| anyhow!("OpenCode session is closed"))
    }
    async fn request(
        &self,
        method: Method,
        path: &str,
        directory: &str,
        body: Option<Value>,
    ) -> Result<Value> {
        self.runtime()?
            .request(method, path, &query(directory), body, self.cancel.clone())
            .await
    }
    async fn emit(&self, e: Value, turn: Option<&str>) -> Result<()> {
        if self.closed.load(Ordering::Acquire) {
            return Ok(());
        }
        self.sink.emit(tagged(e, &self.spec.id, turn)).await
    }
    async fn terminal(&self, turn: &str, e: Value) -> Result<()> {
        let extra = {
            let mut st = self.state();
            if st.turn.as_deref() != Some(turn) {
                return Ok(());
            }
            let extra = if e["type"] == "turn_completed" {
                st.running.clear();
                vec![]
            } else {
                st.interrupted_tools()
            };
            st.turn = None;
            st.dispatch = None;
            st.projection.root.pending_text = None;
            st.projection.root.client_id = None;
            st.projection.root.steers.clear();
            extra
        };
        for item in extra {
            self.emit(item, Some(turn)).await?;
        }
        self.emit(e, Some(turn)).await?;
        self.changed.notify_waiters();
        Ok(())
    }
    async fn reconnect_if_needed(self: &Arc<Self>) -> Result<()> {
        if self.closed.load(Ordering::Acquire) {
            bail!("OpenCode session is closed");
        }
        if !self.runtime()?.is_exited() {
            return Ok(());
        }
        let _connection = self.reconnect.lock().await;
        if !self.runtime()?.is_exited() {
            return Ok(());
        }
        self.stream_cancel
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .cancel();
        self.server_exit("OpenCode helper server exited").await?;
        let config = self.state().config.clone();
        let next = self
            .manager
            .acquire(Some(&self.context), &config, self.cancel.clone())
            .await?;
        if self.closed.load(Ordering::Acquire) {
            bail!("OpenCode session is closed");
        }
        next.bind_session(&self.id, &self.context)?;
        self.manager.remember_session(&self.id, &next);
        if let Some(old) = self
            .runtime
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .replace(next)
        {
            old.unbind_session(&self.id, &self.context.agent_id);
        }
        self.state().mcp_ready = false;
        self.ready.send_replace(false);
        let cancel = self.cancel.child_token();
        *self.stream_cancel.lock().unwrap_or_else(|p| p.into_inner()) = cancel.clone();
        Self::spawn_stream(self, cancel);
        Ok(())
    }
    fn spawn_stream(this: &Arc<Self>, cancel: CancellationToken) {
        let weak = Arc::downgrade(this);
        tokio::spawn(async move {
            Self::stream_loop(weak, cancel).await;
        });
    }
    async fn stream_loop(weak: Weak<Self>, cancel: CancellationToken) {
        let mut backoff = 100u64;
        let mut connected = false;
        let mut first = true;
        loop {
            let Some(this) = weak.upgrade() else {
                return;
            };
            if cancel.is_cancelled() || this.sink.cancellation().is_cancelled() {
                return;
            }
            let runtime = match this.runtime() {
                Ok(r) => r,
                Err(_) => return,
            };
            if runtime.is_exited() {
                let _ = this.server_exit("OpenCode helper server exited").await;
                return;
            }
            let attempt = cancel.child_token();
            let response = tokio::select! {
                _ = cancel.cancelled()=>return,
                response=tokio::time::timeout(Duration::from_secs(if connected{30}else{5}),runtime.events("/global/event",&[],attempt.clone()))=>response
            };
            let mut delivered = false;
            if let Ok(Ok(response)) = response {
                let (tx, mut rx) = mpsc::channel(256);
                let read_cancel = attempt.clone();
                let reader = tokio::spawn(async move { read_sse(response, read_cancel, tx).await });
                let mut repair_pending = false;
                let mut next_repair = tokio::time::Instant::now();
                loop {
                    let raw = tokio::select! {
                        biased;
                        _ = cancel.cancelled()=>None,
                        _ = tokio::time::sleep_until(next_repair), if repair_pending=>{
                            let repair=tokio::select!{_ = cancel.cancelled()=>break,result=this.repair_gap()=>result};
                            repair_pending=repair.is_err();
                            next_repair=tokio::time::Instant::now()+Duration::from_secs(1);
                            continue;
                        },
                        result=tokio::time::timeout(Duration::from_secs(if connected{30}else{5}),rx.recv())=>result.ok().flatten()
                    };
                    let Some(raw) = raw else {
                        break;
                    };
                    delivered = true;
                    let e = raw.get("payload").unwrap_or(&raw);
                    let handle = async {
                        if e["type"] == "server.connected" {
                            connected = true;
                            this.ready.send_replace(true);
                            if first {
                                first = false;
                                if let Err(error) = this.hydrate_children(false).await {
                                    tracing::warn!(%error,"OpenCode child hydration failed");
                                }
                                if let Err(error) = this.reconcile_permissions().await {
                                    tracing::warn!(%error,"OpenCode permissions hydration failed");
                                }
                                if let Ok(status) = this.runner_status().await {
                                    if matches!(status.as_str(), "busy" | "retry") {
                                        this.consume(&json!({"type":"session.status","properties":{"sessionID":this.id,"status":{"type":status}}})).await?;
                                    }
                                }
                            } else if let Err(error) = this.repair_gap().await {
                                tracing::warn!(%error,"OpenCode event gap recovery failed");
                                repair_pending = true;
                                next_repair =
                                    tokio::time::Instant::now() + Duration::from_millis(100);
                            }
                        } else {
                            let id = session_id(e);
                            let unknown = {
                                let st = this.state();
                                !id.is_empty()
                                    && !st.projection.owns(id)
                                    && !st.unrelated.contains(id)
                            };
                            if unknown
                                && !matches!(s(&e["type"]), "session.created" | "session.updated")
                            {
                                if let Err(error) = this.hydrate_children(false).await {
                                    tracing::warn!(%error,"OpenCode child discovery failed");
                                }
                                let mut st = this.state();
                                if !st.projection.owns(id) {
                                    st.unrelated.insert(id.into());
                                }
                            }
                            this.consume(e).await?;
                        }
                        Ok::<(), anyhow::Error>(())
                    };
                    let result =
                        tokio::select! {biased;_ = cancel.cancelled()=>break,result=handle=>result};
                    if let Err(error) = result {
                        tracing::warn!(%error,"OpenCode event delivery failed");
                        if this.sink.cancellation().is_cancelled() {
                            this.cancel.cancel();
                            break;
                        }
                    }
                }
                attempt.cancel();
                match reader.await {
                    Ok(Err(error)) if !cancel.is_cancelled() => {
                        tracing::warn!(%error,"OpenCode event stream disconnected");
                    }
                    Err(error) => {
                        tracing::warn!(%error,"OpenCode event reader failed");
                    }
                    _ => {}
                }
            } else {
                attempt.cancel();
            }
            if cancel.is_cancelled() {
                return;
            }
            if runtime.is_exited() {
                let _ = this.server_exit("OpenCode helper server exited").await;
                return;
            }
            if delivered {
                backoff = 100;
            } else {
                backoff = (backoff * 2).min(5000);
            }
            tokio::select! {_ = cancel.cancelled()=>return,_=tokio::time::sleep(Duration::from_millis(backoff))=>{}}
        }
    }
    async fn server_exit(&self, error: &str) -> Result<()> {
        let (stop, turn) = {
            let mut st = self.state();
            (
                if st.stopping { st.finish_stop() } else { None },
                st.turn.clone(),
            )
        };
        if let Some((turn, e)) = stop {
            self.emit(e, Some(&turn)).await?;
        }
        if let Some(turn) = turn {
            self.terminal(&turn, json!({"type":"turn_failed","error":error}))
                .await?;
        }
        self.changed.notify_waiters();
        Ok(())
    }
    async fn consume(&self, e: &Value) -> Result<()> {
        let sid = session_id(e).to_owned();
        let kind = s(&e["type"]);
        let (discard, ack) = {
            let mut st = self.state();
            if st.stopping && sid == self.id {
                let idle = kind == "session.idle"
                    || (kind == "session.status" && e["properties"]["status"]["type"] == "idle");
                (true, if idle { st.finish_stop() } else { None })
            } else {
                (false, None)
            }
        };
        if let Some((turn, e)) = ack {
            self.emit(e, Some(&turn)).await?;
            self.changed.notify_waiters();
        }
        if discard {
            return Ok(());
        }
        let (active, new_turn, events, changed) = {
            let mut st = self.state();
            let start = st.turn.is_none()
                && sid == self.id
                && kind == "session.status"
                && matches!(s(&e["properties"]["status"]["type"]), "busy" | "retry");
            let new_turn = if start {
                st.projection.root.reset_turn();
                st.dispatch = None;
                Some(st.start())
            } else {
                None
            };
            let mut changed = None;
            if sid == self.id && kind == "message.updated" {
                if let Some(model) = model(&e["properties"]["info"]) {
                    if st.config["model"] != model {
                        st.config["model"] = json!(model);
                        changed = Some(runtime_info(&self.spec.id, &self.id, &st.config));
                    }
                }
            }
            let events = st.projection.translate(e);
            (st.turn.clone(), new_turn, events, changed)
        };
        if let Some(turn) = new_turn {
            self.emit(event("turn_started"), Some(&turn)).await?;
        }
        if let Some(runtime) = changed {
            self.emit(json!({"type":"model_changed","runtimeInfo":runtime}), None)
                .await?;
        }
        for e in events {
            if e["type"] == "provider_subagent" {
                let id = s(&e["event"]["id"]);
                if !id.is_empty() {
                    if e["event"]["type"] == "remove" {
                        self.manager.forget_session(id);
                    } else {
                        self.manager.remember_session(id, &self.runtime()?);
                    }
                }
                let duplicate = {
                    let mut st = self.state();
                    if e["event"]["type"] == "upsert" && !e["event"]["status"].is_null() {
                        if let Some(child) = st.projection.children.get_mut(id) {
                            let mut current = e["event"].clone();
                            if current["parentSubagentId"].is_null()
                                && child.status.get("parentSubagentId").is_some()
                            {
                                current["parentSubagentId"] =
                                    child.status["parentSubagentId"].clone();
                            }
                            let same = child.status == current;
                            child.status = current;
                            same
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                };
                if !duplicate {
                    self.emit(e, None).await?;
                }
                continue;
            }
            if e["type"] == "permission_requested" {
                let request = &e["request"];
                let id = s(&request["id"]);
                let (directory, auto, duplicate) = {
                    let st = self.state();
                    (
                        st.projection.directory(&sid),
                        st.auto_accept && request["kind"] == "tool",
                        st.pending.get(id).is_some_and(|p| p.request == *request),
                    )
                };
                if auto
                    && self
                        .request(
                            Method::POST,
                            &format!("/permission/{}/reply", segment(id)),
                            &directory,
                            Some(json!({"reply":"once"})),
                        )
                        .await
                        .is_ok()
                {
                    continue;
                }
                {
                    self.state().pending.insert(
                        id.into(),
                        Pending {
                            request: request.clone(),
                            directory,
                        },
                    );
                }
                if !duplicate {
                    self.emit(e, active.as_deref()).await?;
                }
                continue;
            }
            let Some(turn) = active.as_deref() else {
                continue;
            };
            if self.state().turn.as_deref() != Some(turn) {
                break;
            }
            if matches!(
                s(&e["type"]),
                "turn_completed" | "turn_failed" | "turn_canceled"
            ) {
                self.terminal(turn, e).await?;
                break;
            }
            if e["type"] == "timeline" && e["item"]["type"] == "tool_call" {
                let mut st = self.state();
                let id = s(&e["item"]["callId"]);
                if e["item"]["status"] == "running" {
                    st.running.insert(id.into(), e["item"].clone());
                } else {
                    st.running.remove(id);
                }
            }
            self.emit(e, Some(turn)).await?;
        }
        Ok(())
    }
    async fn runner_status(&self) -> Result<String> {
        let statuses = self
            .request(Method::GET, "/session/status", &self.cwd, None)
            .await?;
        let map = statuses
            .as_object()
            .ok_or_else(|| anyhow!("OpenCode returned invalid session status"))?;
        let kind = map.get(&self.id).map(|v| s(&v["type"])).unwrap_or("idle");
        if !matches!(kind, "idle" | "busy" | "retry") {
            bail!("Unknown OpenCode runner status '{kind}'");
        }
        Ok(kind.into())
    }
    async fn messages(&self, id: &str, cwd: &str, info: &Value) -> Result<Vec<Value>> {
        let response = self
            .request(
                Method::GET,
                &format!("/session/{}/message", segment(id)),
                cwd,
                None,
            )
            .await?;
        let mut messages = response
            .as_array()
            .cloned()
            .ok_or_else(|| anyhow!("OpenCode messages response is not an array"))?;
        filter_reverted(&mut messages, info);
        Ok(messages)
    }
    async fn hydrate_children(&self, recovered: bool) -> Result<()> {
        let mut queue = VecDeque::from([(self.id.clone(), self.cwd.clone())]);
        let mut visited = HashSet::new();
        let mut children = vec![];
        while let Some((id, cwd)) = queue.pop_front() {
            if visited.len() >= 100 {
                break;
            }
            if !visited.insert(id.clone()) {
                continue;
            }
            let values = self
                .request(
                    Method::GET,
                    &format!("/session/{}/children", segment(&id)),
                    &cwd,
                    None,
                )
                .await?;
            for info in values
                .as_array()
                .ok_or_else(|| anyhow!("Invalid OpenCode children response"))?
            {
                let child = s(&info["id"]);
                if child.is_empty() {
                    continue;
                }
                let directory = info["directory"].as_str().unwrap_or(&cwd).to_owned();
                children.push((info.clone(), directory.clone()));
                if queue.len() + visited.len() < 100 {
                    queue.push_back((child.into(), directory));
                }
            }
        }
        let mut statuses = HashMap::new();
        for (info, cwd) in children {
            if !statuses.contains_key(&cwd) {
                statuses.insert(
                    cwd.clone(),
                    self.request(Method::GET, "/session/status", &cwd, None)
                        .await
                        .ok(),
                );
            }
            let id = s(&info["id"]);
            let snapshot = statuses.get(&cwd).and_then(Option::as_ref);
            let kind = snapshot.map(|v| s(&v[id]["type"]));
            let active = snapshot.is_none() || matches!(kind, Some("busy" | "retry"));
            let detected = {
                self.state().projection.detect(
                    &info,
                    if recovered {
                        None
                    } else {
                        Some(if active { "running" } else { "completed" })
                    },
                )
            };
            for e in detected {
                self.emit(e, None).await?;
            }
            self.manager.remember_session(id, &self.runtime()?);
            let messages = match self.messages(id, &cwd, &info).await {
                Ok(messages) => messages,
                Err(error) => {
                    tracing::warn!(%error,session_id=id,"OpenCode child timeline hydration failed");
                    Vec::new()
                }
            };
            let hydrated = {
                let st = self.state();
                st.projection
                    .children
                    .get(id)
                    .is_some_and(|c| c.info["_paseoHydrated"] == true)
            };
            if recovered {
                for m in &messages {
                    self.consume(
                        &json!({"type":"message.updated","properties":{"info":m["info"]}}),
                    )
                    .await?;
                    for p in m["parts"].as_array().into_iter().flatten() {
                        self.consume(
                            &json!({"type":"message.part.updated","properties":{"part":p}}),
                        )
                        .await?;
                    }
                }
            } else if !hydrated {
                for m in &messages {
                    if m["info"]["role"] == "assistant" && m["info"]["time"]["completed"].is_null()
                    {
                        continue;
                    }
                    for e in history_message(m) {
                        let mut nested = json!({"type":"timeline","id":id,"item":e["item"]});
                        if let Some(time) = e.get("timestamp") {
                            nested["timestamp"] = time.clone();
                        }
                        self.emit(subagent(nested), None).await?;
                    }
                }
                let mut st = self.state();
                if let Some(child) = st.projection.children.get_mut(id) {
                    child.track.hydrate_completed(&messages);
                    child.info["_paseoHydrated"] = json!(true);
                }
            }
            let latest = messages
                .iter()
                .rev()
                .find(|m| m["info"]["role"] == "assistant");
            let presentation =
                latest.and_then(|m| self.state().projection.child_presentation(id, &m["info"]));
            if let Some(e) = presentation {
                self.emit(e, None).await?;
            }
            if recovered {
                let status = if matches!(kind, Some("busy" | "retry")) {
                    Some("running")
                } else if latest.is_some_and(|m| !m["info"]["error"].is_null()) {
                    Some("failed")
                } else if snapshot.is_some()
                    || latest.is_some_and(|m| !m["info"]["time"]["completed"].is_null())
                {
                    Some("completed")
                } else {
                    None
                };
                if let Some(status) = status {
                    self.emit(
                        subagent(json!({"type":"upsert","id":id,"status":status})),
                        None,
                    )
                    .await?;
                }
            }
        }
        Ok(())
    }
    async fn reconcile_permissions(&self) -> Result<()> {
        let directories = {
            let st = self.state();
            let mut dirs = HashSet::from([self.cwd.clone()]);
            for child in st.projection.children.values() {
                if let Some(cwd) = child.info["directory"].as_str() {
                    dirs.insert(cwd.into());
                }
            }
            dirs
        };
        for directory in directories {
            for (path, kind, native) in [
                ("/permission", "tool", "permission.asked"),
                ("/question", "question", "question.asked"),
            ] {
                let Ok(response) = self.request(Method::GET, path, &directory, None).await else {
                    continue;
                };
                let Some(requests) = response.as_array() else {
                    continue;
                };
                let mut live = HashSet::new();
                for p in requests {
                    if !self.state().projection.owns(s(&p["sessionID"])) {
                        continue;
                    }
                    live.insert(s(&p["id"]).to_owned());
                    self.consume(&json!({"type":native,"properties":p})).await?;
                }
                let removed = {
                    let mut st = self.state();
                    let removed: Vec<_> = st
                        .pending
                        .iter()
                        .filter(|(id, p)| {
                            p.directory == directory
                                && p.request["kind"] == kind
                                && !live.contains(*id)
                        })
                        .map(|(id, _)| id.clone())
                        .collect();
                    for id in &removed {
                        st.pending.remove(id);
                    }
                    removed
                };
                for id in removed {
                    self.emit(json!({"type":"permission_resolved","requestId":id,"resolution":{"behavior":"allow"}}),None).await?;
                }
            }
        }
        Ok(())
    }
    async fn repair_gap(&self) -> Result<()> {
        if let Err(error) = self.hydrate_children(true).await {
            tracing::warn!(%error,"OpenCode child gap repair failed");
        }
        self.reconcile_permissions().await?;
        let (turn, dispatch) = {
            let st = self.state();
            (st.turn.clone(), st.dispatch.clone())
        };
        let Some(turn) = turn else {
            return Ok(());
        };
        let status = self.runner_status().await?;
        let Some(dispatch) = dispatch else {
            if status == "idle" {
                self.terminal(&turn, event("turn_completed")).await?;
            }
            return Ok(());
        };
        let messages = self.messages(&self.id, &self.cwd, &Value::Null).await?;
        let Some(boundary) = messages.iter().position(|m| m["info"]["id"] == dispatch) else {
            if status == "idle" {
                self.terminal(
                    &turn,
                    json!({"type":"turn_failed","error":"Active dispatch not found"}),
                )
                .await?;
            }
            return Ok(());
        };
        for m in &messages[boundary..] {
            if self.state().turn.as_deref() != Some(&turn) {
                return Ok(());
            }
            self.consume(&json!({"type":"message.updated","properties":{"info":m["info"]}}))
                .await?;
            for p in m["parts"].as_array().into_iter().flatten() {
                self.consume(&json!({"type":"message.part.updated","properties":{"part":p}}))
                    .await?;
            }
        }
        if status == "idle" && self.state().turn.as_deref() == Some(&turn) {
            let error = messages[boundary..]
                .iter()
                .rev()
                .find(|m| m["info"]["role"] == "assistant")
                .and_then(|m| m["info"].get("error"));
            let e = if let Some(error) = error.filter(|v| !v.is_null()) {
                json!({"type":"session.error","properties":{"sessionID":self.id,"error":error}})
            } else {
                json!({"type":"session.idle","properties":{"sessionID":self.id}})
            };
            self.consume(&e).await?;
        }
        Ok(())
    }
    async fn wait_ready(&self) -> Result<()> {
        let mut ready = self.ready.subscribe();
        let wait = async {
            loop {
                if *ready.borrow_and_update() {
                    return Ok(());
                }
                ready
                    .changed()
                    .await
                    .map_err(|_| anyhow!("OpenCode event stream closed"))?;
            }
        };
        tokio::select! {_ = self.cancel.cancelled()=>bail!("OpenCode session closed"),result=tokio::time::timeout(Duration::from_secs(10),wait)=>result.map_err(|_|anyhow!("OpenCode server.connected event timed out; your message was not sent"))?}
    }
    async fn quiescent(&self) -> Result<()> {
        let wait = async {
            let mut delay = 100;
            loop {
                let notified = self.changed.notified();
                let (stopping, inflight, error) = {
                    let st = self.state();
                    (st.stopping, st.abort_inflight, st.abort_error.clone())
                };
                if let Some(error) = error {
                    bail!("OpenCode previous abort failed: {error}");
                }
                if !stopping && inflight == 0 {
                    return Ok(());
                }
                tokio::select! {_ = self.cancel.cancelled()=>bail!("OpenCode session closed"),_=notified=>{},_=tokio::time::sleep(Duration::from_millis(delay))=>{if stopping&&self.runner_status().await.is_ok_and(|s|s=="idle"){let ack={let mut st=self.state();if st.stopping{st.finish_stop()}else{None}};if let Some((turn,e))=ack{self.emit(e,Some(&turn)).await?;}}delay=(delay*2).min(1000);}}
            }
        };
        tokio::time::timeout(Duration::from_secs(10), wait)
            .await
            .map_err(|_| anyhow!("OpenCode previous turn has not stopped"))?
    }
    async fn configure_mcp(&self) -> Result<()> {
        let configured = {
            let st = self.state();
            if st.mcp_ready {
                return Ok(());
            }
            st.config["mcpServers"].clone()
        };
        let mut servers = configured.as_object().cloned().unwrap_or_default();
        if let Some(runtime) = self.context.mcp_servers.as_object() {
            servers.extend(runtime.clone());
        }
        for (name, server) in servers {
            let mut config = if server["type"] == "stdio" {
                let mut command = vec![server["command"].clone()];
                if let Some(args) = server["args"].as_array() {
                    command.extend(args.clone());
                }
                let mut v = json!({"type":"local","command":command,"enabled":true});
                if let Some(env) = server.get("env") {
                    v["environment"] = env.clone();
                }
                v
            } else {
                let mut v = json!({"type":"remote","url":server["url"],"enabled":true});
                if let Some(headers) = server.get("headers") {
                    v["headers"] = headers.clone();
                }
                v
            };
            if server["type"] == "local" || server["type"] == "remote" {
                config = server;
            }
            let response = self
                .request(
                    Method::POST,
                    "/mcp",
                    &self.cwd,
                    Some(json!({"name":name,"config":config})),
                )
                .await;
            let failure = match response {
                Ok(data) if data[&name]["status"] == "failed" => {
                    Some(diagnostic(&data[&name]["error"]))
                }
                Ok(_) => None,
                Err(error) => Some(error.to_string()),
            };
            if let Some(error) = failure {
                let lower = error.to_lowercase();
                if !["already", "exists", "connected"]
                    .iter()
                    .any(|p| lower.contains(p))
                {
                    bail!("Failed to add OpenCode MCP server '{name}': {error}");
                }
            }
        }
        self.state().mcp_ready = true;
        Ok(())
    }
    async fn slash(&self, prompt: &Value) -> Option<(String, String)> {
        let text = prompt.as_str()?.trim().strip_prefix('/')?;
        let (name, args) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
        if name.is_empty() || name.contains('/') {
            return None;
        }
        let runtime = self.runtime().ok()?;
        let commands = catalog::commands(&runtime, &self.cwd, self.cancel.clone())
            .await
            .ok()?;
        commands
            .iter()
            .any(|c| c["name"] == name)
            .then(|| (name.into(), args.trim().into()))
    }
    fn prompt_body(&self, prompt: &Value, options: &Value, id: &str) -> Result<Value> {
        let parts = prompt_parts(prompt)?;
        let config = self.state().config.clone();
        let mut body = json!({"messageID":id,"parts":parts});
        let system = [
            config["systemPrompt"].as_str(),
            config["daemonAppendSystemPrompt"].as_str(),
        ]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
        if !system.is_empty() {
            body["system"] = json!(system);
        }
        if let Some(model) = model_ref(&config["model"]) {
            body["model"] = model;
        }
        if let Some(mode) = normalize(config["modeId"].as_str()) {
            body["agent"] = json!(mode);
        }
        if let Some(variant) = normalize(config["thinkingOptionId"].as_str()) {
            body["variant"] = json!(variant);
        }
        if let Some(schema) = options.get("outputSchema").filter(|v| !v.is_null()) {
            body["format"] = json!({"type":"json_schema","schema":schema});
        }
        Ok(body)
    }
    async fn respond(&self, id: &str, response: Value) -> Result<()> {
        let (request, directory) = {
            let st = self.state();
            let p = st
                .pending
                .get(id)
                .ok_or_else(|| anyhow!("No pending permission request with id '{id}'"))?;
            (p.request.clone(), p.directory.clone())
        };
        let deny = response["behavior"] == "deny";
        let (path, body) = if request["kind"] == "question" {
            if deny {
                (format!("/question/{}/reject", segment(id)), None)
            } else {
                let answers: Vec<Vec<String>> = request["input"]["questions"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|q| {
                        response["updatedInput"]["answers"][s(&q["header"])]
                            .as_str()
                            .unwrap_or("")
                            .split(',')
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_owned)
                            .collect()
                    })
                    .collect();
                (
                    format!("/question/{}/reply", segment(id)),
                    Some(json!({"answers":answers})),
                )
            }
        } else {
            let mut body = json!({"reply":if deny{"reject"}else if response["selectedActionId"]=="allow_always"{"always"}else{"once"}});
            if deny {
                if let Some(message) = response.get("message") {
                    body["message"] = message.clone();
                }
            }
            (format!("/permission/{}/reply", segment(id)), Some(body))
        };
        self.request(Method::POST, &path, &directory, body).await?;
        self.state().pending.remove(id);
        Ok(())
    }
}
fn runtime_info(provider: &str, id: &str, config: &Value) -> Value {
    json!({"provider":provider,"sessionId":id,"model":config["model"],"modeId":config["modeId"],"thinkingOptionId":config["thinkingOptionId"]})
}

#[async_trait]
impl AgentSession for LegacySession {
    fn provider(&self) -> &str {
        &self.inner.spec.id
    }
    fn native_id(&self) -> Option<String> {
        Some(self.inner.id.clone())
    }
    fn capabilities(&self) -> &Value {
        &self.capabilities
    }
    fn features(&self) -> Arc<[Value]> {
        self.inner.state().features.clone()
    }
    fn initial_timeline(&self) -> &[Value] {
        &[]
    }
    fn describe_persistence(&self) -> Option<Value> {
        let st = self.inner.state();
        let mut metadata = json!({"cwd":self.inner.cwd});
        for key in ["modeId", "model"] {
            if let Some(v) = st.config.get(key).filter(|v| !v.is_null()) {
                metadata[key] = v.clone();
            }
        }
        Some(
            json!({"provider":self.provider(),"sessionId":self.inner.id,"nativeHandle":self.inner.id,"metadata":metadata}),
        )
    }
    fn pending_permissions(&self) -> Vec<Value> {
        self.inner
            .state()
            .pending
            .values()
            .map(|p| {
                let mut v = p.request.clone();
                v["provider"] = json!(self.provider());
                v
            })
            .collect()
    }
    fn usage_session(&self) -> Option<Value> {
        if self.inner.closed.load(Ordering::Acquire) {
            return None;
        }
        let runtime = self.inner.runtime().ok()?;
        Some(
            json!({"provider":self.provider(),"model":self.inner.state().config["model"],"env":runtime.environment,"sessionKey":self.inner.usage_key}),
        )
    }
    async fn start_turn(&self, prompt: Value, options: Value) -> Result<Value> {
        let this = &self.inner;
        let _start = this.start_gate.lock().await;
        let preparation_epoch = this.state().abort_epoch;
        if this.state().turn.is_some() {
            bail!("A foreground turn is already active");
        }
        this.reconnect_if_needed().await?;
        this.quiescent().await?;
        this.configure_mcp().await?;
        this.wait_ready().await?;
        let slash = this.slash(&prompt).await;
        let compact = slash
            .as_ref()
            .is_some_and(|(n, _)| n == "compact" || n == "summarize");
        let id = message_id();
        let body = if let Some((name, args)) = &slash {
            let config = this.state().config.clone();
            if compact {
                model_ref(&config["model"]).unwrap_or_else(|| json!({}))
            } else {
                let mut b = json!({"command":name,"arguments":args,"messageID":id});
                for (from, to) in [
                    ("model", "model"),
                    ("modeId", "agent"),
                    ("thinkingOptionId", "variant"),
                ] {
                    if let Some(v) = config.get(from).filter(|v| v.is_string()) {
                        b[to] = v.clone();
                    }
                }
                b
            }
        } else {
            this.prompt_body(&prompt, &options, &id)?
        };
        let text = prompt_text(&prompt)?;
        let turn = {
            let mut st = this.state();
            if this.closed.load(Ordering::Acquire) {
                bail!("OpenCode session is closed");
            }
            if st.abort_epoch != preparation_epoch {
                bail!("OpenCode turn was interrupted before dispatch");
            }
            if st.turn.is_some() || st.stopping || st.abort_inflight > 0 {
                bail!("OpenCode runner is not idle");
            }
            st.projection.root.reset_turn();
            st.projection.root.pending_text = Some(text);
            st.projection.root.client_id = options["clientMessageId"].as_str().map(str::to_owned);
            st.projection.root.suppress = compact;
            st.dispatch = if compact { None } else { Some(id) };
            st.start()
        };
        this.emit(event("turn_started"), Some(&turn)).await?;
        let operation = if compact {
            "summarize"
        } else if slash.is_some() {
            "command"
        } else {
            "prompt_async"
        };
        let command = operation == "command";
        let path = format!("/session/{}/{operation}", segment(&this.id));
        let owned = this.clone();
        let task_turn = turn.clone();
        let runtime = this.runtime()?;
        tokio::spawn(async move {
            if owned.state().turn.as_deref() != Some(task_turn.as_str()) {
                return;
            }
            if let Err(error) = runtime
                .request(
                    Method::POST,
                    &path,
                    &query(&owned.cwd),
                    Some(body),
                    owned.cancel.clone(),
                )
                .await
            {
                let message = error.to_string();
                let lower = message.to_lowercase();
                if command
                    && [
                        "headers timeout",
                        "headers_timeout",
                        "und_err_headers_timeout",
                    ]
                    .iter()
                    .any(|t| lower.contains(t))
                {
                    return;
                }
                let _ = owned
                    .terminal(&task_turn, json!({"type":"turn_failed","error":message}))
                    .await;
            }
        });
        Ok(json!({"turnId":turn}))
    }
    async fn steer_active_turn(&self, prompt: Value, options: Value) -> Result<Value> {
        let this = &self.inner;
        let expected = options["expectedTurnId"].as_str();
        if this.closed.load(Ordering::Acquire)
            || expected.is_none()
            || this.state().turn.as_deref() != expected
        {
            return Ok(json!({"status":"unavailable"}));
        }
        if this.slash(&prompt).await.is_some() || this.state().turn.as_deref() != expected {
            return Ok(json!({"status":"unavailable"}));
        }
        let id = message_id();
        let body = this.prompt_body(&prompt, &Value::Null, &id)?;
        this.state().projection.root.steers.insert(
            id.clone(),
            (
                prompt_text(&prompt)?,
                options["clientMessageId"].as_str().map(str::to_owned),
            ),
        );
        if let Err(error) = this
            .request(
                Method::POST,
                &format!("/session/{}/prompt_async", segment(&this.id)),
                &this.cwd,
                Some(body),
            )
            .await
        {
            let message = error.to_string().to_lowercase();
            if message.contains("404")
                || [
                    "session not found",
                    "session is not found",
                    "session inactive",
                    "session is inactive",
                    "session not active",
                    "session not running",
                ]
                .iter()
                .any(|s| message.contains(s))
            {
                this.state().projection.root.steers.remove(&id);
                return Ok(json!({"status":"unavailable"}));
            }
            return Err(error);
        }
        if options["clearPendingPermissions"] == true {
            let ids: Vec<_> = this.state().pending.keys().cloned().collect();
            for id in ids {
                if this.state().pending.contains_key(&id) {
                    this.respond(&id,json!({"behavior":"deny","message":"The user answered with a message instead of approving. Their message follows."})).await?;
                }
            }
        }
        Ok(json!({"status":"accepted"}))
    }
    async fn stream_history(&self, sink: EventSink) -> Result<()> {
        let this = &self.inner;
        this.reconnect_if_needed().await?;
        let info = this
            .request(
                Method::GET,
                &format!("/session/{}", segment(&this.id)),
                &this.cwd,
                None,
            )
            .await
            .unwrap_or(Value::Null);
        let messages = this.messages(&this.id, &this.cwd, &info).await?;
        for message in messages {
            for e in history_message(&message) {
                sink.emit(tagged(e, self.provider(), None)).await?;
            }
        }
        Ok(())
    }
    async fn runtime_info(&self) -> Result<Value> {
        Ok(runtime_info(
            self.provider(),
            &self.inner.id,
            &self.inner.state().config,
        ))
    }
    async fn available_modes(&self) -> Result<Vec<Value>> {
        if let Some(modes) = self.inner.state().modes.clone() {
            return Ok(modes);
        }
        self.inner.reconnect_if_needed().await?;
        let agents = self
            .inner
            .request(Method::GET, "/agent", &self.inner.cwd, None)
            .await
            .unwrap_or_else(|_| json!([]));
        let modes = catalog::modes(Generation::Legacy, &agents);
        self.inner.state().modes = Some(modes.clone());
        Ok(modes)
    }
    async fn current_mode(&self) -> Result<Option<String>> {
        Ok(normalize(self.inner.state().config["modeId"].as_str()))
    }
    async fn set_mode(&self, mode: &str) -> Result<Option<Value>> {
        let mode = normalize(Some(mode));
        let full = mode.as_deref() == Some("full-access");
        {
            let mut st = self.inner.state();
            st.config["modeId"] = if full { json!("build") } else { json!(mode) };
        }
        if full {
            self.set_feature("auto_accept", json!(true)).await?;
        }
        Ok(None)
    }
    async fn respond_to_permission(&self, id: &str, response: Value) -> Result<Option<Value>> {
        self.inner.respond(id, response).await?;
        Ok(None)
    }
    async fn interrupt(&self) -> Result<()> {
        let this = &self.inner;
        let runtime = this.runtime()?;
        let (turn, extra, epoch) = {
            let mut st = this.state();
            let turn = st.turn.take();
            if !st.stopping {
                st.stopping = turn.is_some();
                st.cancellation_turn = turn.clone();
            }
            let extra = st.interrupted_tools();
            st.projection.root.pending_text = None;
            st.projection.root.client_id = None;
            st.projection.root.steers.clear();
            st.abort_epoch += 1;
            st.abort_inflight += 1;
            st.abort_error = None;
            (turn, extra, st.abort_epoch)
        };
        for e in extra {
            this.emit(e, turn.as_deref()).await?;
        }
        let owned = this.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let path = format!("/session/{}/abort", segment(&owned.id));
            let location = query(&owned.cwd);
            let mut result = runtime
                .request(Method::POST, &path, &location, None, owned.cancel.clone())
                .await
                .map(|_| ());
            if result.is_err() && !owned.closed.load(Ordering::Acquire) {
                result = runtime
                    .request(Method::POST, &path, &location, None, owned.cancel.clone())
                    .await
                    .map(|_| ());
            }
            let ack = {
                let mut st = owned.state();
                st.abort_inflight = st.abort_inflight.saturating_sub(1);
                if st.abort_epoch == epoch {
                    st.abort_error = result.as_ref().err().map(ToString::to_string);
                }
                if result.is_ok() {
                    st.acknowledge_stop()
                } else {
                    None
                }
            };
            if let Some((turn, e)) = ack {
                let _ = owned.emit(e, Some(&turn)).await;
            }
            owned.changed.notify_waiters();
            let _ = tx.send(result);
        });
        match tokio::time::timeout(Duration::from_secs(2), rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(anyhow!("OpenCode abort task ended without a result")),
            Err(_) => Ok(()),
        }
    }
    async fn close(&self) -> Result<()> {
        let this = &self.inner;
        if this.closed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        this.cancel.cancel();
        this.stream_cancel
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .cancel();
        this.changed.notify_waiters();
        let runtime = this
            .runtime
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        let Some(runtime) = runtime else {
            return Ok(());
        };
        let path = format!("/session/{}/abort", segment(&this.id));
        let location = query(&this.cwd);
        if let Err(error) = runtime
            .request(
                Method::POST,
                &path,
                &location,
                None,
                CancellationToken::new(),
            )
            .await
        {
            tracing::warn!(%error,"Failed to abort OpenCode session during close");
        }
        if !this.persist {
            if let Err(error) = runtime
                .request(
                    Method::DELETE,
                    &format!("/session/{}", segment(&this.id)),
                    &query(&this.cwd),
                    None,
                    CancellationToken::new(),
                )
                .await
            {
                tracing::debug!(%error,"Failed to delete non-persistent OpenCode session");
            }
        }
        runtime.unbind_session(&this.id, &this.context.agent_id);
        this.state().turn = None;
        Ok(())
    }
    async fn list_commands(&self) -> Result<Vec<Value>> {
        self.inner.reconnect_if_needed().await?;
        catalog::commands(
            self.inner.runtime()?.as_ref(),
            &self.inner.cwd,
            self.inner.cancel.clone(),
        )
        .await
    }
    async fn set_model(&self, model: Option<&str>) -> Result<()> {
        let this = &self.inner;
        this.reconnect_if_needed().await?;
        let model = model.filter(|s| !s.trim().is_empty());
        let mut variant = this.state().config["thinkingOptionId"]
            .as_str()
            .map(str::to_owned);
        if model.is_none() {
            variant = None;
        } else if let (Some(model), Some(v)) = (model, variant.as_ref()) {
            let reference = model_ref(&json!(model)).unwrap();
            let response = this
                .request(Method::GET, "/provider", &this.cwd, None)
                .await?;
            let target = response["all"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|p| p["id"] == reference["providerID"])
                .and_then(|p| p["models"].get(s(&reference["modelID"])))
                .ok_or_else(|| anyhow!("OpenCode model unavailable: {model}"))?;
            if target["variants"].get(v).is_none() {
                variant = None;
            }
        }
        {
            let mut st = this.state();
            st.config["model"] = json!(model);
            st.config["thinkingOptionId"] = json!(variant);
            let window = model.and_then(|m| st.projection.windows.get(m)).copied();
            if !st.projection.root.usage.is_object() {
                st.projection.root.usage = json!({});
            }
            if let Some(window) = window {
                st.projection.root.usage["contextWindowMaxTokens"] = json!(window);
            } else {
                st.projection
                    .root
                    .usage
                    .as_object_mut()
                    .unwrap()
                    .remove("contextWindowMaxTokens");
            }
        }
        this.emit(
            json!({"type":"thinking_option_changed","thinkingOptionId":variant}),
            None,
        )
        .await
    }
    async fn set_thinking_option(&self, option: Option<&str>) -> Result<Option<Value>> {
        self.inner.state().config["thinkingOptionId"] = json!(normalize(option));
        Ok(None)
    }
    async fn set_feature(&self, feature: &str, value: Value) -> Result<()> {
        if feature != "auto_accept" {
            bail!("Unsupported OpenCode feature '{feature}'");
        }
        let mut st = self.inner.state();
        let enabled = value == true;
        st.auto_accept = enabled;
        if st.config["featureValues"][feature].as_bool() == Some(enabled) {
            return Ok(());
        }
        if !st.config["featureValues"].is_object() {
            st.config["featureValues"] = json!({});
        }
        st.config["featureValues"][feature] = json!(enabled);
        st.features = catalog::features(&st.config).into();
        Ok(())
    }
    async fn rewind(&self, target: RewindTarget, message_id: &str) -> Result<()> {
        if target != RewindTarget::Both {
            bail!("OpenCode supports reverting conversation and files together only");
        }
        self.inner.reconnect_if_needed().await?;
        self.inner
            .request(
                Method::POST,
                &format!("/session/{}/revert", segment(&self.inner.id)),
                &self.inner.cwd,
                Some(json!({"messageID":message_id})),
            )
            .await?;
        Ok(())
    }
    async fn try_out_of_band(&self, _prompt: &Value) -> Result<bool> {
        Ok(false)
    }
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(runtime) = self
            .runtime
            .get_mut()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            runtime.unbind_session(&self.id, &self.context.agent_id);
        }
    }
}
