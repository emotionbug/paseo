use super::{catalog, events::Mapper, history, host_tools::Router, launch, transport::Transport};
use crate::providers::api::{AgentSession, EventSink, LaunchContext, ProviderSpec, RewindTarget};
use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt},
    sync::{mpsc, oneshot},
};
use tokio_util::sync::CancellationToken;

struct State {
    config: Value,
    native: Value,
    mode: Option<String>,
    active: Option<String>,
    mapper: Mapper,
    tree_entries: Vec<Value>,
    context_entries: Vec<Value>,
    branch: Option<String>,
    commands: Option<Vec<Value>>,
    extension_results: HashMap<String, oneshot::Sender<Result<Value>>>,
    features: Arc<[Value]>,
    dead: bool,
    prompt_id: Option<String>,
    prompt_results: HashMap<String, Value>,
    prompt_epoch: u64,
    turn_started: bool,
    turn_has_user: bool,
    live: bool,
    settling: bool,
    bridged_tools: Value,
    followed_children: HashSet<String>,
}
struct Inner {
    spec: ProviderSpec,
    context: LaunchContext,
    state: Mutex<State>,
    runtime: Mutex<Option<Arc<Transport>>>,
    assets: Mutex<Option<launch::Assets>>,
    host_tools: Mutex<Option<Arc<Router>>>,
    sink: EventSink,
    closed: AtomicBool,
    generation: AtomicU64,
    operation: tokio::sync::Mutex<()>,
    manual_compaction: AtomicBool,
    compaction_started: AtomicBool,
    usage_key: String,
    usage_env: Value,
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.context.cancel.cancel();
    }
}
struct CompactionGuard<'a> {
    running: &'a AtomicBool,
    started: &'a AtomicBool,
}
impl Drop for CompactionGuard<'_> {
    fn drop(&mut self) {
        if !self.started.load(Ordering::Acquire) {
            self.running.store(false, Ordering::Release);
        }
    }
}
pub struct Session {
    inner: Arc<Inner>,
    capabilities: Value,
    history_events: Option<Vec<Value>>,
    original_persistence: Option<Value>,
}
impl Inner {
    fn runtime(&self) -> Result<Arc<Transport>> {
        self.runtime
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| anyhow!("History session has no live runtime"))
    }
    async fn refresh(&self) -> Result<Value> {
        let state = self.runtime()?.request(json!({"type":"get_state"})).await?;
        let mut data = self.state.lock().unwrap();
        let features = catalog::features(&state);
        if data.features.as_ref() != features.as_slice() {
            data.features = features.into();
        }
        data.native = state.clone();
        Ok(state)
    }
    async fn emit(&self, event: Value) -> Result<()> {
        self.sink.emit(event).await
    }
    async fn out_of_band_message(&self, text: String) -> Result<()> {
        self.emit(json!({"type":"timeline","provider":self.spec.id,"item":{"type":"assistant_message","text":text}})).await
    }
    async fn accept_events(&self, events: Vec<Value>) -> Result<()> {
        for event in events {
            if matches!(
                event["type"].as_str(),
                Some("turn_completed" | "turn_failed" | "turn_canceled")
            ) {
                let mut state = self.state.lock().unwrap();
                if event["turnId"]
                    .as_str()
                    .is_some_and(|id| state.active.as_deref() != Some(id))
                {
                    continue;
                }
                state.active = None;
                state.prompt_id = None;
                state.prompt_results.clear();
                state.prompt_epoch += 1;
                state.settling = false;
            }
            self.emit(event).await?;
        }
        Ok(())
    }
    async fn settle(self: Arc<Self>, frame: Value, generation: u64, turn: Option<String>) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(600);
        loop {
            if self.closed.load(Ordering::Acquire)
                || self.generation.load(Ordering::Acquire) != generation
                || self.state.lock().unwrap().active != turn
            {
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                self.state.lock().unwrap().mapper.end_turn();
                let _=self.accept_events(vec![json!({"type":"turn_failed","provider":self.spec.id,"turnId":turn,"error":"OMP provider idle deadline exceeded"})]).await;
                return;
            }
            if let Ok(state) = self.refresh().await {
                if state["isStreaming"] != true && state["isCompacting"] != true {
                    let events = {
                        let mut state = self.state.lock().unwrap();
                        if state.active != turn {
                            return;
                        }
                        state.settling = false;
                        state.mapper.map(&frame)
                    };
                    let _ = self.accept_events(events).await;
                    self.poll_usage(turn).await;
                    return;
                }
            }
            tokio::select! {_ = self.context.cancel.cancelled()=>return,_=tokio::time::sleep(Duration::from_millis(100))=>{}}
        }
    }
    fn no_turn(self: &Arc<Self>, mut frame: Value, delay: Duration) {
        let (turn, epoch) = {
            let mut state = self.state.lock().unwrap();
            state.prompt_epoch += 1;
            (state.active.clone(), state.prompt_epoch)
        };
        let inner = self.clone();
        tokio::spawn(async move {
            if !delay.is_zero() {
                tokio::select! {_ = inner.context.cancel.cancelled()=>return,_=tokio::time::sleep(delay)=>{}}
            }
            tokio::task::yield_now().await;
            if let Some(error) = frame["error"]["message"].as_str().map(str::to_owned) {
                frame["error"] = json!(error);
            }
            let events = {
                let mut state = inner.state.lock().unwrap();
                if inner.closed.load(Ordering::Acquire)
                    || state.active != turn
                    || state.prompt_epoch != epoch
                    || state.turn_started
                    || state.turn_has_user
                {
                    return;
                }
                state.mapper.map(&frame)
            };
            let _ = inner.accept_events(events).await;
            inner.poll_usage(turn).await;
        });
    }
    async fn poll_usage(&self, completed_turn: Option<String>) {
        let (turn, epoch) = {
            let state = self.state.lock().unwrap();
            (state.active.clone(), state.prompt_epoch)
        };
        let Ok(runtime) = self.runtime() else {
            return;
        };
        let mut stats = runtime
            .request(json!({"type":"get_session_stats"}))
            .await
            .unwrap_or(Value::Null);
        if stats.get("tokens").is_none()
            && stats.get("cost").is_none()
            && stats.get("contextUsage").is_none()
        {
            if let Ok(state) = runtime.request(json!({"type":"get_state"})).await {
                stats = json!({"contextUsage":state["contextUsage"]});
            }
        }
        let events = {
            let mut state = self.state.lock().unwrap();
            if self.closed.load(Ordering::Acquire)
                || state.prompt_epoch != epoch
                || state.active != turn
            {
                return;
            }
            let mut events = state
                .mapper
                .map(&json!({"type":"session_stats","stats":stats}));
            if let Some(turn) = completed_turn {
                for event in &mut events {
                    event["turnId"] = json!(turn);
                }
            }
            events
        };
        let _ = self.accept_events(events).await;
    }
    async fn frame(self: &Arc<Self>, mut frame: Value, generation: u64) -> Result<()> {
        if self.generation.load(Ordering::Acquire) != generation
            || self.closed.load(Ordering::Acquire)
        {
            return Ok(());
        }
        if self.spec.family == "omp" {
            let router = self.host_tools.lock().unwrap().clone();
            if let Some(router) = router {
                if router.handle(&frame) {
                    return Ok(());
                }
            }
        }
        if self.spec.family == "omp"
            && frame["type"] == "response"
            && frame["command"] == "prompt"
            && frame["success"] == false
        {
            frame = json!({"type":"prompt_result","id":frame["id"],"agentInvoked":false,"error":frame["error"]});
        }
        if frame["type"] == "prompt_result" {
            let id = frame["id"].as_str().map(str::to_owned);
            let matched = {
                let mut state = self.state.lock().unwrap();
                if let Some(id) = id {
                    if state.prompt_id.as_deref() == Some(&id) && state.active.is_some() {
                        true
                    } else {
                        if state.prompt_id.is_none() && state.active.is_some() {
                            state.prompt_results.insert(id, frame.clone());
                        }
                        false
                    }
                } else {
                    false
                }
            };
            if matched {
                if frame["agentInvoked"] == false {
                    self.no_turn(
                        frame,
                        if self.spec.family == "omp" {
                            Duration::from_secs(5)
                        } else {
                            Duration::ZERO
                        },
                    );
                } else {
                    self.state.lock().unwrap().prompt_epoch += 1;
                }
            }
            return Ok(());
        }
        {
            let mut state = self.state.lock().unwrap();
            if frame["type"] == "agent_start" {
                state.live = true;
            } else if self.spec.family == "omp"
                && !state.live
                && matches!(
                    frame["type"].as_str(),
                    Some(
                        "turn_start"
                            | "message_start"
                            | "message_end"
                            | "message_update"
                            | "tool_execution_start"
                            | "tool_execution_update"
                            | "tool_execution_end"
                            | "agent_end"
                    )
                )
            {
                return Ok(());
            }
            if frame["type"] == "turn_start" {
                state.turn_started = true;
                state.prompt_epoch += 1;
            }
            if frame["type"] == "message_end" && frame["message"]["role"] == "user" {
                state.turn_has_user = true;
            }
        }
        if self.spec.family == "omp" && frame["type"] == "agent_end" {
            let turn = {
                let mut state = self.state.lock().unwrap();
                if state.settling {
                    return Ok(());
                }
                state.settling = true;
                state.active.clone()
            };
            tokio::spawn(self.clone().settle(frame, generation, turn));
            return Ok(());
        }
        if frame["type"] == "agent_start" {
            frame["sessionId"] = self.state.lock().unwrap().native["sessionId"].clone();
        }
        if frame["type"] == "available_commands_update" {
            if let Some(commands) = frame["commands"].as_array() {
                self.state.lock().unwrap().commands = Some(catalog::commands(commands, true));
            }
        }
        if frame["type"] == "process_exit" {
            self.state.lock().unwrap().dead = true;
            let router = self.host_tools.lock().unwrap().take();
            if let Some(router) = router {
                tokio::spawn(async move {
                    router.close().await;
                });
            }
        }
        if frame["type"] == "extension_ui_request" && frame["method"] == "notify" {
            if let Some(message) = frame["message"].as_str() {
                if let Some(payload) = message.strip_prefix("PASEO_SUBMITTED_USER_ENTRY ") {
                    let value: Value = serde_json::from_str(payload)?;
                    let events = {
                        let mut state = self.state.lock().unwrap();
                        state.turn_has_user = true;
                        state.mapper.submitted_user_entry(&value["entry"])
                    };
                    self.accept_events(events).await?;
                    return Ok(());
                }
                for marker in ["PASEO_ENTRY_CAPTURE ", "PASEO_COMMAND_RESULT "] {
                    if let Some(payload) = message.strip_prefix(marker) {
                        let value: Value = serde_json::from_str(payload)?;
                        let mut state = self.state.lock().unwrap();
                        if marker == "PASEO_ENTRY_CAPTURE " {
                            state.tree_entries =
                                value["treeEntries"].as_array().cloned().unwrap_or_default();
                            state.context_entries = value["contextEntries"]
                                .as_array()
                                .cloned()
                                .unwrap_or_default();
                        }
                        if let Some(id) = value["requestId"].as_str() {
                            if let Some(reply) = state.extension_results.remove(id) {
                                let result = if value["ok"] == false {
                                    Err(anyhow!(
                                        "{}",
                                        value["error"]
                                            .as_str()
                                            .unwrap_or("Pi extension command failed")
                                    ))
                                } else {
                                    Ok(value.clone())
                                };
                                let _ = reply.send(result);
                            }
                        }
                        return Ok(());
                    }
                }
            }
        }
        if self.spec.family == "pi"
            && self.manual_compaction.load(Ordering::Acquire)
            && matches!(
                frame["type"].as_str(),
                Some("compaction_start" | "compaction_end")
            )
        {
            let started = frame["type"] == "compaction_start";
            self.compaction_started.store(started, Ordering::Release);
            if !started {
                self.manual_compaction.store(false, Ordering::Release);
            }
            self.emit(json!({"type":"timeline","provider":self.spec.id,"item":{"type":"compaction","status":if started{"loading"}else{"completed"},"trigger":if frame["reason"]=="manual"{"manual"}else{"auto"}}})).await?;
            return Ok(());
        }
        let (events, responses, children) = {
            let mut state = self.state.lock().unwrap();
            let events = state.mapper.map(&frame);
            (
                events,
                state.mapper.drain_responses(),
                state.mapper.drain_child_sessions(),
            )
        };
        for response in responses {
            self.runtime()?.send(&response).await?;
        }
        for child in children {
            self.follow_child(child, generation);
        }
        let completed = events
            .iter()
            .find(|e| e["type"] == "turn_completed")
            .and_then(|e| e["turnId"].as_str())
            .map(str::to_owned);
        self.accept_events(events).await?;
        if completed.is_some() {
            let inner = self.clone();
            tokio::spawn(async move {
                inner.poll_usage(completed).await;
            });
        }
        Ok(())
    }
    fn follow_child(self: &Arc<Self>, child: Value, generation: u64) {
        let (Some(id), Some(file)) = (child["id"].as_str(), child["file"].as_str()) else {
            return;
        };
        let key = format!("{generation}:{id}:{file}");
        if !self.state.lock().unwrap().followed_children.insert(key) {
            return;
        }
        let (id, file) = (id.to_owned(), file.to_owned());
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut mapper = Mapper::new("pi", "pi");
            let mut offset = 0u64;
            let mut pending = Vec::<u8>::new();
            let mut items = 0usize;
            const MAX_BYTES: u64 = 2 * 1024 * 1024;
            loop {
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                if inner.closed.load(Ordering::Acquire)
                    || inner.generation.load(Ordering::Acquire) != generation
                    || offset >= MAX_BYTES
                    || items >= 200
                {
                    return;
                }
                if let Ok(mut handle) = tokio::fs::File::open(&file).await {
                    if let Ok(metadata) = handle.metadata().await {
                        let count = metadata.len().min(MAX_BYTES).saturating_sub(offset) as usize;
                        if count > 0 && handle.seek(std::io::SeekFrom::Start(offset)).await.is_ok()
                        {
                            let start = pending.len();
                            pending.resize(start + count, 0);
                            match handle.read(&mut pending[start..]).await {
                                Ok(read) => {
                                    offset += read as u64;
                                    pending.truncate(start + read);
                                }
                                Err(_) => pending.truncate(start),
                            }
                            if let Some(last) = pending.iter().rposition(|b| *b == b'\n') {
                                for line in pending[..last].split(|b| *b == b'\n') {
                                    let Ok(entry) = serde_json::from_slice::<Value>(line) else {
                                        continue;
                                    };
                                    if !entry["message"]["role"].is_string() {
                                        continue;
                                    }
                                    for event in mapper.replay_message(&entry["message"], None) {
                                        if event["type"] != "timeline" {
                                            continue;
                                        }
                                        let mut child = json!({"type":"timeline","id":id,"item":history::limit_child_item(event["item"].clone())});
                                        if let Some(timestamp) = entry.get("timestamp") {
                                            child["timestamp"] = timestamp.clone();
                                        }
                                        if inner.emit(json!({"type":"provider_subagent","provider":inner.spec.id,"event":child})).await.is_err(){return;}
                                        items += 1;
                                        if items >= 200 {
                                            return;
                                        }
                                    }
                                }
                                pending.drain(..=last);
                            }
                        }
                    }
                }
                tokio::select! {_ = inner.context.cancel.cancelled()=>return,_ = tokio::time::sleep(Duration::from_millis(250))=>{}}
            }
        });
    }
    fn attach(self: &Arc<Self>, mut receiver: mpsc::Receiver<Value>, generation: u64) {
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            while let Some(frame) = receiver.recv().await {
                let Some(inner) = weak.upgrade() else {
                    break;
                };
                if let Err(error) = inner.frame(frame, generation).await {
                    tracing::warn!(%error,"Provider event delivery failed");
                    break;
                }
            }
        });
    }
    async fn extension(&self, name: &str, mut payload: Value) -> Result<Value> {
        let id = uuid::Uuid::new_v4().to_string();
        payload["requestId"] = json!(id);
        let (send, receive) = oneshot::channel();
        self.state
            .lock()
            .unwrap()
            .extension_results
            .insert(id.clone(), send);
        let encoded = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload)?);
        let result = async {
            self.runtime()?
                .request(json!({"type":"prompt","message":format!("/{name} {encoded}")}))
                .await?;
            let timeout = self
                .state
                .lock()
                .unwrap()
                .config
                .pointer("/providerOptions/extensionTimeoutMs")
                .and_then(Value::as_u64)
                .unwrap_or(30000);
            tokio::time::timeout(Duration::from_millis(timeout), receive)
                .await
                .map_err(|_| anyhow!("Pi extension result timed out"))?
                .map_err(|_| anyhow!("Pi extension result channel closed"))?
        }
        .await;
        self.state.lock().unwrap().extension_results.remove(&id);
        result
    }
    async fn restart(self: &Arc<Self>, mode: &str) -> Result<()> {
        let (mut config, path, previous_id) = {
            let state = self.state.lock().unwrap();
            (
                state.config.clone(),
                if state.config["internal"] == true || state.config["persistSession"] == false {
                    None
                } else {
                    state.native["sessionFile"].as_str().map(str::to_owned)
                },
                state.native["sessionId"].clone(),
            )
        };
        config["modeId"] = json!(mode);
        let (next, mut receiver) = Transport::spawn(
            launch::command(&self.spec, &config, &self.context, path.as_deref(), None)?,
            launch::timeout(&self.spec, &config),
            self.context.cancel.child_token(),
        )
        .await?;
        // A resumed runtime can replay more than the bounded event channel holds.
        // Discard its startup replay, just as the source attaches after get_state.
        let done = CancellationToken::new();
        let drain_done = done.clone();
        let drain = tokio::spawn(async move {
            loop {
                tokio::select! {biased;_=drain_done.cancelled()=>break,event=receiver.recv()=>if event.is_none(){break;}}
            }
            receiver
        });
        let initialized = async {
            next.negotiate(launch::ready_timeout(&self.spec, &config))
                .await?;
            let state = initialized_state(&next, &config, true, true).await?;
            let env = launch::external_env(&self.spec, &self.context);
            let router = Router::register(
                next.clone(),
                &self.context,
                Path::new(config["cwd"].as_str().unwrap_or(".")),
                &env,
            )
            .await?;
            Ok::<_, anyhow::Error>((state, router))
        }
        .await;
        done.cancel();
        let receiver = match drain.await {
            Ok(receiver) => receiver,
            Err(error) => {
                next.close().await.ok();
                return Err(error.into());
            }
        };
        let (state, router) = match initialized {
            Ok(value) => value,
            Err(error) => {
                next.close().await.ok();
                return Err(error);
            }
        };
        if self.closed.load(Ordering::Acquire) {
            router.close().await;
            next.close().await.ok();
            bail!("OMP session is closed");
        }
        let session_id = state["sessionId"].clone();
        let identities = router.identities();
        let old_tools = self.host_tools.lock().unwrap().replace(router);
        let previous = self.runtime.lock().unwrap().replace(next.clone());
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        {
            let mut data = self.state.lock().unwrap();
            data.config = config;
            data.mode = Some(mode.into());
            data.features = catalog::features(&state).into();
            data.native = state;
            data.dead = false;
            data.prompt_id = None;
            data.prompt_results.clear();
            data.prompt_epoch += 1;
            data.live = false;
            data.bridged_tools = identities.clone();
            data.mapper.set_bridged_tools(&identities);
        }
        self.attach(receiver, generation);
        subscribe(&next).await;
        if let Some(old_tools) = old_tools {
            old_tools.close().await;
        }
        if let Some(previous) = previous {
            previous.close().await.ok();
        }
        if session_id != previous_id {
            self.emit(
                json!({"type":"thread_started","provider":self.spec.id,"sessionId":session_id}),
            )
            .await?;
        }
        Ok(())
    }
}
async fn subscribe(runtime: &Transport) {
    if runtime
        .request(json!({"type":"set_subagent_subscription","level":"events"}))
        .await
        .is_err()
    {
        if let Err(error) = runtime
            .request(json!({"type":"set_subagent_subscription","level":"progress"}))
            .await
        {
            tracing::debug!(%error,"OMP subagent subscriptions unavailable");
        }
    }
}
async fn initialized_state(
    runtime: &Transport,
    config: &Value,
    resume: bool,
    omp: bool,
) -> Result<Value> {
    let mut state = runtime.request(json!({"type":"get_state"})).await?;
    if resume {
        if let Some((provider, id)) = config["model"]
            .as_str()
            .and_then(|model| model.split_once('/').or_else(|| model.split_once(':')))
            .filter(|(p, i)| !p.is_empty() && !i.is_empty())
        {
            if state["model"]["provider"] != provider || state["model"]["id"] != id {
                let models = runtime
                    .request(json!({"type":"get_available_models"}))
                    .await?;
                if models["models"].as_array().is_some_and(|models| {
                    models
                        .iter()
                        .any(|model| model["provider"] == provider && model["id"] == id)
                }) {
                    runtime
                        .request(json!({"type":"set_model","provider":provider,"modelId":id}))
                        .await?;
                    state = runtime.request(json!({"type":"get_state"})).await?;
                } else {
                    tracing::warn!(
                        "Requested resume model unavailable; preserving provider's resolved model"
                    );
                }
            }
        }
    }
    if omp
        && config["featureValues"]["fast_mode"].is_boolean()
        && state["fastModeEnabled"].is_boolean()
        && config["featureValues"]["fast_mode"] != state["fastModeEnabled"]
    {
        let result = runtime
            .request(json!({"type":"set_fast_mode","enabled":config["featureValues"]["fast_mode"]}))
            .await?;
        state["fastModeEnabled"] = result["enabled"].clone();
        state["fastModeActive"] = result["active"].clone();
    }
    Ok(state)
}
impl Session {
    pub async fn open(
        spec: ProviderSpec,
        config: Value,
        mut context: LaunchContext,
        persistence: Option<Value>,
        options: Value,
        sink: EventSink,
    ) -> Result<Arc<Self>> {
        launch::validate_options(&spec.family, &config["providerOptions"])?;
        context.cancel = context.cancel.child_token();
        let omp = spec.family == "omp";
        let path = persistence
            .as_ref()
            .and_then(|p| p["nativeHandle"].as_str().or(p["sessionId"].as_str()))
            .map(str::to_owned);
        let history_only = omp && options["purpose"] == "history";
        let mut loaded = None;
        if let Some(path) = path.as_deref() {
            loaded = Some(
                history::load_with_metadata(
                    &spec.family,
                    &spec.id,
                    Path::new(path),
                    None,
                    &persistence
                        .as_ref()
                        .map(|p| p["metadata"].clone())
                        .unwrap_or(Value::Null),
                )
                .await?,
            );
        }
        if history_only && loaded.is_none() {
            bail!("OMP history session file is unavailable");
        }
        let assets = if !omp && !history_only {
            Some(launch::pi_assets(&spec, &config, &context).await?)
        } else {
            None
        };
        let supports_mcp = omp || assets.as_ref().is_some_and(|a| a.mcp);
        let launch_command = if history_only {
            None
        } else {
            Some(launch::command(
                &spec,
                &config,
                &context,
                path.as_deref(),
                assets.as_ref(),
            )?)
        };
        let native = if history_only {
            json!({"sessionId":persistence.as_ref().unwrap()["sessionId"],"sessionFile":path,"model":loaded.as_ref().unwrap().descriptor["model"],"thinkingLevel":config["thinkingOptionId"]})
        } else {
            json!({})
        };
        let mode = if omp {
            Some(config["modeId"].as_str().unwrap_or("full").to_string())
        } else {
            None
        };
        let mut mapper = Mapper::new(&spec.family, &spec.id);
        let bridged_tools = persistence
            .as_ref()
            .map(|p| p["metadata"]["bridgedTools"].clone())
            .unwrap_or(Value::Null);
        mapper.set_bridged_tools(&bridged_tools);
        let usage_env = json!(launch::external_env(&spec, &context));
        let inner = Arc::new(Inner {
            state: Mutex::new(State {
                config: config.clone(),
                native,
                mode,
                active: None,
                mapper,
                tree_entries: vec![],
                context_entries: vec![],
                branch: None,
                commands: None,
                extension_results: HashMap::new(),
                features: Arc::from([]),
                dead: false,
                prompt_id: None,
                prompt_results: HashMap::new(),
                prompt_epoch: 0,
                turn_started: false,
                turn_has_user: false,
                live: persistence.is_none(),
                settling: false,
                bridged_tools,
                followed_children: HashSet::new(),
            }),
            spec,
            context,
            runtime: Mutex::new(None),
            assets: Mutex::new(assets),
            host_tools: Mutex::new(None),
            sink,
            closed: AtomicBool::new(false),
            generation: AtomicU64::new(1),
            operation: tokio::sync::Mutex::new(()),
            manual_compaction: AtomicBool::new(false),
            compaction_started: AtomicBool::new(false),
            usage_key: uuid::Uuid::new_v4().to_string(),
            usage_env,
        });
        if let Some(command) = launch_command {
            let (runtime, receiver) = Transport::spawn(
                command,
                launch::timeout(&inner.spec, &config),
                inner.context.cancel.child_token(),
            )
            .await?;
            *inner.runtime.lock().unwrap() = Some(runtime.clone());
            // Drain events before any control request: extensions may emit more than
            // the bounded channel capacity during startup, while replies must flow.
            inner.attach(receiver, 1);
            let initialize=async {
                if omp{runtime.negotiate(launch::ready_timeout(&inner.spec,&config)).await?;}
                let native=initialized_state(&runtime,&config,persistence.is_some(),omp).await?;
                {let mut state=inner.state.lock().unwrap();if let Some(model)=catalog::model_id(&native["model"]){state.config["model"]=json!(model);}if native["thinkingLevel"].is_string(){state.config["thinkingOptionId"]=native["thinkingLevel"].clone();}state.features=catalog::features(&native).into();state.native=native;}
                if omp {
                    let cwd=config["cwd"].as_str().ok_or_else(||anyhow!("OMP requires cwd"))?;
                    let env=launch::external_env(&inner.spec,&inner.context);
                    let router=Router::register(runtime.clone(),&inner.context,Path::new(cwd),&env).await?;
                    let identities=router.identities();
                    {let mut state=inner.state.lock().unwrap();state.bridged_tools=identities.clone();state.mapper.set_bridged_tools(&identities);}
                    *inner.host_tools.lock().unwrap()=Some(router);
                    subscribe(&runtime).await;
                }
                let session_id=inner.state.lock().unwrap().native["sessionId"].clone();
                inner.emit(json!({"type":"thread_started","provider":inner.spec.id,"sessionId":session_id})).await?;
                Ok::<_,anyhow::Error>(())
            }.await;
            if let Err(error) = initialize {
                inner.closed.store(true, Ordering::Release);
                inner.context.cancel.cancel();
                let router = inner.host_tools.lock().unwrap().take();
                if let Some(router) = router {
                    router.close().await;
                }
                runtime.close().await.ok();
                return Err(error);
            }
            let weak = Arc::downgrade(&inner);
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_millis(250));
                let mut count = 0u32;
                loop {
                    interval.tick().await;
                    let Some(inner) = weak.upgrade() else {
                        break;
                    };
                    if inner.closed.load(Ordering::Acquire) {
                        break;
                    }
                    let events = inner.state.lock().unwrap().mapper.poll();
                    if inner.accept_events(events).await.is_err() {
                        break;
                    }
                    count = count.wrapping_add(1);
                    let active = inner.state.lock().unwrap().active.is_some();
                    if count % 12 == 0 && active {
                        inner.poll_usage(None).await;
                    }
                }
            });
        }
        Ok(Arc::new(Self {
            inner,
            capabilities: catalog::capabilities(omp, supports_mcp),
            history_events: if history_only {
                loaded.map(|h| h.events)
            } else {
                None
            },
            original_persistence: if history_only { persistence } else { None },
        }))
    }
    async fn prompt(&self, prompt: Value, options: Value, behavior: Option<&str>) -> Result<Value> {
        if self.history_events.is_some() {
            bail!("OMP history session cannot start a turn");
        }
        let _gate = self.inner.operation.lock().await;
        if self.inner.closed.load(Ordering::Acquire) {
            bail!("Provider session is closed");
        }
        if self.inner.state.lock().unwrap().active.is_some() {
            bail!("A provider turn is already active");
        }
        let dead = self.inner.state.lock().unwrap().dead;
        if dead && self.inner.spec.family == "omp" {
            let mode = self
                .inner
                .state
                .lock()
                .unwrap()
                .mode
                .clone()
                .unwrap_or_else(|| "full".into());
            self.inner.restart(&mode).await?;
        }
        let model = self.inner.state.lock().unwrap().native["model"].clone();
        let mut command = super::prompt::convert(&prompt, &model)?;
        command["type"] = json!("prompt");
        if let Some(behavior) = behavior {
            command["streamingBehavior"] = json!(behavior);
        }
        let turn = uuid::Uuid::new_v4().to_string();
        {
            let mut state = self.inner.state.lock().unwrap();
            state.active = Some(turn.clone());
            state.prompt_id = None;
            state.prompt_results.clear();
            state.prompt_epoch += 1;
            state.turn_started = false;
            state.turn_has_user = false;
            state.live = true;
            state.settling = false;
            state.mapper.begin_turn(&turn);
            state.mapper.queue_client_message(
                command["message"].as_str().unwrap_or(""),
                options["clientMessageId"].as_str(),
            );
        }
        if behavior.is_some() {
            let event = self.inner.state.lock().unwrap().mapper.announce_turn();
            self.inner.emit(event).await?;
        }
        let inner = self.inner.clone();
        let return_turn = turn.clone();
        let slash = command["message"]
            .as_str()
            .is_some_and(|text| text.trim_start().starts_with('/'));
        let request_timeout = {
            let state = inner.state.lock().unwrap();
            launch::timeout(&inner.spec, &state.config)
        };
        tokio::spawn(async move {
            let outcome = async {
                inner
                    .runtime()?
                    .request_with_timeout(command, Some(request_timeout))
                    .await
            }
            .await;
            match outcome {
                Ok((id, ack)) => {
                    let correlated = {
                        let mut state = inner.state.lock().unwrap();
                        if state.active.as_deref() != Some(&turn) {
                            return;
                        }
                        state.prompt_id = Some(id.clone());
                        state.prompt_results.remove(&id)
                    };
                    let is_correlated = correlated.is_some();
                    let result=correlated.unwrap_or_else(||json!({"type":"prompt_result","id":id,"agentInvoked":ack["agentInvoked"]}));
                    if result["agentInvoked"] == false {
                        inner.no_turn(
                            result,
                            if is_correlated && inner.spec.family == "omp" {
                                Duration::from_secs(5)
                            } else {
                                Duration::ZERO
                            },
                        );
                    } else if result["agentInvoked"].is_null() && slash && inner.spec.family == "pi"
                    {
                        tokio::task::yield_now().await;
                        if let Ok(native) = inner.refresh().await {
                            if native["isStreaming"] != true
                                && native["isCompacting"] != true
                                && native["queuedMessageCount"].as_u64().unwrap_or(0) == 0
                            {
                                inner.no_turn(
                                    json!({"type":"prompt_result","agentInvoked":false}),
                                    Duration::ZERO,
                                );
                            }
                        }
                    }
                }
                Err(error) => {
                    let _=inner.accept_events(vec![json!({"type":if error.to_string().to_lowercase().contains("abort"){"turn_canceled"}else{"turn_failed"},"provider":inner.spec.id,"turnId":turn,"error":error.to_string(),"reason":error.to_string()})]).await;
                    inner.state.lock().unwrap().mapper.end_turn();
                }
            }
        });
        Ok(json!({"turnId":return_turn}))
    }
}
#[async_trait]
impl AgentSession for Session {
    fn provider(&self) -> &str {
        &self.inner.spec.id
    }
    fn native_id(&self) -> Option<String> {
        self.inner.state.lock().unwrap().native["sessionId"]
            .as_str()
            .map(str::to_owned)
    }
    fn capabilities(&self) -> &Value {
        &self.capabilities
    }
    fn features(&self) -> Arc<[Value]> {
        self.inner.state.lock().unwrap().features.clone()
    }
    fn initial_timeline(&self) -> &[Value] {
        &[]
    }
    fn describe_persistence(&self) -> Option<Value> {
        if let Some(handle) = &self.original_persistence {
            return Some(handle.clone());
        }
        let state = self.inner.state.lock().unwrap();
        let mut metadata = json!({"cwd":state.config["cwd"]});
        for key in ["model", "thinkingOptionId"] {
            if let Some(v) = state.config.get(key).filter(|v| !v.is_null()) {
                metadata[key] = v.clone();
            }
        }
        if let Some(mode) = &state.mode {
            metadata["modeId"] = json!(mode);
        }
        if self.inner.spec.family == "omp" && state.bridged_tools.is_array() {
            metadata["bridgedTools"] = state.bridged_tools.clone();
        }
        let id = state.native["sessionId"].as_str()?;
        let mut handle = json!({"provider":self.provider(),"sessionId":id,"metadata":metadata});
        if let Some(path) = state.native["sessionFile"].as_str() {
            handle["nativeHandle"] = json!(path);
        }
        Some(handle)
    }
    fn pending_permissions(&self) -> Vec<Value> {
        self.inner
            .state
            .lock()
            .unwrap()
            .mapper
            .pending_permissions()
    }
    fn usage_session(&self) -> Option<Value> {
        if self.history_events.is_some() || self.inner.closed.load(Ordering::Acquire) {
            return None;
        }
        let state = self.inner.state.lock().unwrap();
        let mut usage = json!({"provider":self.inner.spec.family,"env":self.inner.usage_env,"sessionKey":self.inner.usage_key});
        if let Some(model) = catalog::model_id(&state.native["model"]) {
            usage["model"] = json!(model);
        }
        Some(usage)
    }
    async fn start_turn(&self, prompt: Value, options: Value) -> Result<Value> {
        self.prompt(prompt, options, None).await
    }
    async fn steer_active_turn(&self, prompt: Value, options: Value) -> Result<Value> {
        let expected = options["expectedTurnId"].as_str();
        if self.inner.closed.load(Ordering::Acquire)
            || self.inner.state.lock().unwrap().active.as_deref() != expected
        {
            return Ok(json!({"status":"unavailable"}));
        }
        let model = self.inner.state.lock().unwrap().native["model"].clone();
        let mut command = super::prompt::convert(&prompt, &model)?;
        if command["message"]
            .as_str()
            .is_some_and(|s| s.trim_start().starts_with('/'))
        {
            return Ok(json!({"status":"unavailable"}));
        }
        command["type"] = json!("steer");
        let omp = self.inner.spec.family == "omp";
        if omp {
            self.inner
                .state
                .lock()
                .unwrap()
                .mapper
                .queue_client_message(
                    command["message"].as_str().unwrap_or(""),
                    options["clientMessageId"].as_str(),
                );
        }
        if let Err(error) = self.inner.runtime()?.request(command.clone()).await {
            if omp {
                self.inner
                    .state
                    .lock()
                    .unwrap()
                    .mapper
                    .forget_client_message(
                        command["message"].as_str().unwrap_or(""),
                        options["clientMessageId"].as_str(),
                    );
            }
            if omp
                && error
                    .downcast_ref::<super::transport::CommandError>()
                    .is_some()
            {
                return Ok(json!({"status":"unavailable"}));
            }
            let text = error.to_string().to_lowercase();
            if text.contains("unknown command")
                || text.contains("not streaming")
                || text.contains("no active")
                || text.contains("extension command")
            {
                return Ok(json!({"status":"unavailable"}));
            }
            return Err(error);
        }
        if self.inner.state.lock().unwrap().active.as_deref() != expected {
            return Ok(json!({"status":"unavailable"}));
        }
        if !omp {
            self.inner
                .state
                .lock()
                .unwrap()
                .mapper
                .queue_client_message(
                    command["message"].as_str().unwrap_or(""),
                    options["clientMessageId"].as_str(),
                );
        }
        if options["clearPendingPermissions"] == true {
            for request in self.pending_permissions() {
                if let Some(id) = request["id"].as_str().or(request["requestId"].as_str()) {
                    self.respond_to_permission(id,json!({"behavior":"deny","message":"The user sent a message instead of approving."})).await?;
                }
            }
        }
        Ok(json!({"status":"accepted"}))
    }
    async fn stream_history(&self, sink: EventSink) -> Result<()> {
        if let Some(events) = &self.history_events {
            for event in events {
                sink.emit(event.clone()).await?;
            }
            return Ok(());
        }
        if self.inner.spec.family == "omp" {
            let (path, branch, metadata, phases) = {
                let state = self.inner.state.lock().unwrap();
                (
                    state.native["sessionFile"].as_str().map(str::to_owned),
                    state.branch.clone(),
                    json!({"bridgedTools":state.bridged_tools}),
                    state.native["todoPhases"].clone(),
                )
            };
            if let Some(path) = path {
                let history = history::load_live(
                    "omp",
                    self.provider(),
                    Path::new(&path),
                    branch.as_deref(),
                    &metadata,
                )
                .await?;
                for event in history.events {
                    sink.emit(event).await?;
                }
            }
            if let Some(todo) = history::todo_state(&[
                json!({"message":{"role":"toolResult","toolName":"todo","details":{"phases":phases}}}),
            ]) {
                sink.emit(json!({"type":"timeline","provider":self.provider(),"item":todo}))
                    .await?;
            }
            return Ok(());
        }
        self.inner
            .extension("paseo_capture_entries", json!({}))
            .await?;
        let response = self
            .inner
            .runtime()?
            .request(json!({"type":"get_messages"}))
            .await?;
        let mut mapper = Mapper::new(&self.inner.spec.family, self.provider());
        let entries = self.inner.state.lock().unwrap().context_entries.clone();
        let mut index = 0;
        for message in response["messages"].as_array().into_iter().flatten() {
            let mut message = message.clone();
            if message["role"] == "user" {
                if let Some(entry) = entries.get(index) {
                    message["id"] = entry["id"].clone();
                }
                index += 1;
            }
            for event in mapper.replay_message(&message, message["id"].as_str()) {
                sink.emit(event).await?;
            }
        }
        let mut remaining = 16 * 1024 * 1024;
        for child in mapper.drain_child_sessions() {
            let (Some(id), Some(path)) = (child["id"].as_str(), child["file"].as_str()) else {
                continue;
            };
            if remaining == 0 {
                break;
            }
            let budget = remaining.min(2 * 1024 * 1024);
            remaining -= budget;
            for event in history::pi_child_history(id, Path::new(path), budget).await {
                sink.emit(
                    json!({"type":"provider_subagent","provider":self.provider(),"event":event}),
                )
                .await?;
            }
        }
        Ok(())
    }
    async fn runtime_info(&self) -> Result<Value> {
        if self.history_events.is_none() {
            self.inner.refresh().await?;
        }
        let state = self.inner.state.lock().unwrap();
        Ok(
            json!({"provider":self.provider(),"sessionId":state.native["sessionId"],"model":catalog::model_id(&state.native["model"]).or_else(||state.config["model"].as_str().map(str::to_owned)),"thinkingOptionId":state.native["thinkingLevel"],"modeId":state.mode,"features":catalog::features(&state.native)}),
        )
    }
    async fn available_modes(&self) -> Result<Vec<Value>> {
        Ok(catalog::modes(self.inner.spec.family == "omp"))
    }
    async fn current_mode(&self) -> Result<Option<String>> {
        Ok(self.inner.state.lock().unwrap().mode.clone())
    }
    async fn set_mode(&self, mode: &str) -> Result<Option<Value>> {
        if self.history_events.is_some() {
            bail!("OMP history session cannot change mode");
        }
        if self.inner.spec.family != "omp" {
            bail!("Pi does not expose selectable modes");
        }
        if !matches!(mode, "full" | "write" | "ask") {
            bail!("Invalid OMP mode '{mode}'");
        }
        let _gate = self.inner.operation.lock().await;
        {
            let state = self.inner.state.lock().unwrap();
            if state.mode.as_deref() == Some(mode) {
                return Ok(None);
            }
            if state.active.is_some()
                || state.native["isStreaming"] == true
                || state.native["isCompacting"] == true
            {
                return Ok(Some(
                    json!({"type":"warning","message":"Change approval mode once the current turn ends"}),
                ));
            }
        }
        self.inner.restart(mode).await?;
        Ok(None)
    }
    async fn respond_to_permission(&self, id: &str, response: Value) -> Result<Option<Value>> {
        if self.history_events.is_some() {
            bail!("OMP history session cannot answer permissions");
        }
        let (frame, extra) = {
            let mut state = self.inner.state.lock().unwrap();
            let frame = state.mapper.permission_response(id, &response)?;
            (frame, state.mapper.drain_responses())
        };
        let runtime = self.inner.runtime()?;
        if !frame.is_null() {
            runtime.send(&frame).await?;
        }
        for frame in extra {
            runtime.send(&frame).await?;
        }
        let turn = self.inner.state.lock().unwrap().active.clone();
        let mut event = json!({"type":"permission_resolved","provider":self.provider(),"requestId":id,"resolution":response});
        if let Some(turn) = turn {
            event["turnId"] = json!(turn);
        }
        self.inner.emit(event).await?;
        Ok(None)
    }
    async fn interrupt(&self) -> Result<()> {
        if self.history_events.is_some() {
            return Ok(());
        }
        let runtime = self.inner.runtime()?;
        if self.inner.spec.family == "pi" {
            if let Err(error) = runtime.stop_work(json!({"type":"clear_queue"})).await {
                let text = error.to_string().to_lowercase();
                if !(text.contains("clear_queue")
                    && (text.contains("unknown command")
                        || text.contains("not supported")
                        || text.contains("unrecognized")))
                {
                    return Err(error);
                }
            }
        }
        runtime.stop_work(json!({"type":"abort"})).await?;
        let (turn, events) = {
            let mut state = self.inner.state.lock().unwrap();
            let events = state.mapper.cancel_work();
            state.mapper.end_turn();
            (state.active.clone(), events)
        };
        self.inner.accept_events(events).await?;
        if let Some(turn) = turn {
            self.inner.accept_events(vec![json!({"type":"turn_canceled","provider":self.provider(),"turnId":turn,"reason":"interrupted"})]).await?;
        }
        Ok(())
    }
    async fn close(&self) -> Result<()> {
        if self.inner.closed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        self.inner.context.cancel.cancel();
        let router = self.inner.host_tools.lock().unwrap().take();
        if let Some(router) = router {
            router.close().await;
        }
        {
            let mut state = self.inner.state.lock().unwrap();
            for (_, sender) in state.extension_results.drain() {
                let _ = sender.send(Err(anyhow!("Pi session closed")));
            }
        }
        let runtime = self.inner.runtime.lock().unwrap().take();
        let result = if let Some(runtime) = runtime {
            runtime.close().await
        } else {
            Ok(())
        };
        self.inner.assets.lock().unwrap().take();
        result
    }
    async fn list_commands(&self) -> Result<Vec<Value>> {
        if let Some(commands) = self.inner.state.lock().unwrap().commands.clone() {
            return Ok(commands);
        }
        let response=self.inner.runtime()?.request(json!({"type":if self.inner.spec.family=="omp"{"get_available_commands"}else{"get_commands"}})).await?;
        Ok(catalog::commands(
            response["commands"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or(&[]),
            self.inner.spec.family == "omp",
        ))
    }
    async fn set_model(&self, model: Option<&str>) -> Result<()> {
        let Some(model) = model.filter(|s| !s.is_empty()) else {
            return Ok(());
        };
        let (provider, id) = model
            .split_once('/')
            .or_else(|| model.split_once(':'))
            .filter(|(p, i)| !p.is_empty() && !i.is_empty())
            .ok_or_else(|| anyhow!("Model id must include a provider: {model}"))?;
        let result = self
            .inner
            .runtime()?
            .request(json!({"type":"set_model","provider":provider,"modelId":id}))
            .await?;
        self.inner.refresh().await?;
        self.inner.state.lock().unwrap().config["model"] =
            json!(catalog::model_id(&result).unwrap_or_else(|| model.into()));
        Ok(())
    }
    async fn set_thinking_option(&self, option: Option<&str>) -> Result<Option<Value>> {
        let level = option
            .filter(|s| launch::valid_thinking(s, self.inner.spec.family == "omp"))
            .unwrap_or("medium");
        self.inner
            .runtime()?
            .request(json!({"type":"set_thinking_level","level":level}))
            .await?;
        self.inner.refresh().await?;
        self.inner.state.lock().unwrap().config["thinkingOptionId"] = json!(level);
        Ok(None)
    }
    async fn set_feature(&self, feature: &str, value: Value) -> Result<()> {
        if self.inner.spec.family != "omp" || feature != "fast_mode" {
            bail!("Unknown provider feature: {feature}");
        }
        if !value.is_boolean() {
            bail!("OMP fast mode requires a boolean");
        }
        if !self.inner.state.lock().unwrap().native["fastModeEnabled"].is_boolean() {
            bail!("OMP fast mode is unavailable");
        }
        let result = self
            .inner
            .runtime()?
            .request(json!({"type":"set_fast_mode","enabled":value}))
            .await?;
        let mut state = self.inner.state.lock().unwrap();
        state.native["fastModeEnabled"] = result["enabled"].clone();
        state.native["fastModeActive"] = result["active"].clone();
        if !state.config["featureValues"].is_object() {
            state.config["featureValues"] = json!({});
        }
        state.config["featureValues"]["fast_mode"] = result["enabled"].clone();
        state.features = catalog::features(&state.native).into();
        Ok(())
    }
    async fn rewind(&self, target: RewindTarget, id: &str) -> Result<()> {
        if target != RewindTarget::Conversation {
            bail!("Provider does not support file rewind");
        }
        let _gate = self.inner.operation.lock().await;
        let id = id.trim();
        if self.inner.state.lock().unwrap().active.is_some() {
            bail!("Cannot rewind conversation while a turn is active");
        }
        if id.trim().is_empty() {
            bail!("Rewind requires a user message id");
        }
        if self.inner.spec.family == "omp" {
            let result = self
                .inner
                .runtime()?
                .request(json!({"type":"branch","entryId":id}))
                .await?;
            if result["cancelled"] == true {
                bail!("OMP branch was cancelled");
            }
            if !result["text"].is_string() {
                bail!("OMP branch response did not include restored prompt text");
            }
            self.inner.state.lock().unwrap().branch = Some(id.into());
        } else {
            self.inner
                .extension("paseo_capture_entries", json!({}))
                .await?;
            if !self
                .inner
                .state
                .lock()
                .unwrap()
                .tree_entries
                .iter()
                .any(|e| e["id"] == id)
            {
                bail!("Pi rewind target {id} was not found in captured tree entries");
            }
            self.inner
                .extension("paseo_tree", json!({"targetId":id}))
                .await?;
        }
        self.inner.refresh().await?;
        self.inner.state.lock().unwrap().mapper.end_turn();
        Ok(())
    }
    async fn try_out_of_band(&self, prompt: &Value) -> Result<bool> {
        let Some(text) = prompt.as_str().and_then(|s| s.trim().strip_prefix('/')) else {
            return Ok(false);
        };
        let (name, args) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
        let name = name.to_lowercase();
        let args = args.trim();
        match name.as_str() {
            "compact" => {
                if self.inner.manual_compaction.swap(true, Ordering::AcqRel) {
                    bail!("A compact command is already running");
                }
                let _guard = CompactionGuard {
                    running: &self.inner.manual_compaction,
                    started: &self.inner.compaction_started,
                };
                self.inner
                    .compaction_started
                    .store(false, Ordering::Release);
                let omp = self.inner.spec.family == "omp";
                if omp {
                    self.inner.emit(json!({"type":"timeline","provider":self.provider(),"item":{"type":"compaction","status":"loading","trigger":"manual"}})).await?;
                }
                let mut command = json!({"type":"compact"});
                if !args.is_empty() {
                    command["customInstructions"] = json!(args);
                }
                let result = self
                    .inner
                    .runtime()?
                    .request_with_timeout(command, None)
                    .await;
                if omp
                    || (result.is_err()
                        && self.inner.compaction_started.swap(false, Ordering::AcqRel))
                {
                    self.inner.emit(json!({"type":"timeline","provider":self.provider(),"item":{"type":"compaction","status":"completed","trigger":"manual"}})).await?;
                }
                if let Err(error) = result {
                    self.inner
                        .out_of_band_message(format!("[Error] Failed to compact context: {error}"))
                        .await?;
                } else if omp {
                    self.inner.poll_usage(None).await;
                }
            }
            "autocompact" => {
                let enabled = match args.to_lowercase().as_str() {
                    "on" | "true" | "enable" | "enabled" => true,
                    "off" | "false" | "disable" | "disabled" => false,
                    "" | "toggle" => {
                        let state = self.inner.refresh().await?;
                        let Some(enabled) = state["autoCompactionEnabled"].as_bool() else {
                            self.inner.out_of_band_message("[Error] Auto-compaction state is unavailable. Use /autocompact on or /autocompact off.".into()).await?;
                            return Ok(true);
                        };
                        !enabled
                    }
                    _ => {
                        self.inner
                            .out_of_band_message(
                                "[Error] Usage: /autocompact [on|off|toggle]".into(),
                            )
                            .await?;
                        return Ok(true);
                    }
                };
                match self
                    .inner
                    .runtime()?
                    .request(json!({"type":"set_auto_compaction","enabled":enabled}))
                    .await
                {
                    Ok(_) => {
                        self.inner.state.lock().unwrap().native["autoCompactionEnabled"] =
                            json!(enabled);
                        self.inner
                            .out_of_band_message(format!(
                                "Auto-compaction {}.",
                                if enabled { "enabled" } else { "disabled" }
                            ))
                            .await?;
                    }
                    Err(error) => {
                        self.inner
                            .out_of_band_message(format!(
                                "[Error] Failed to set auto-compaction: {error}"
                            ))
                            .await?
                    }
                }
            }
            "handoff" if self.inner.spec.family == "omp" => {
                self.inner.state.lock().unwrap().live = true;
                let mut command = json!({"type":"handoff"});
                if !args.is_empty() {
                    command["customInstructions"] = json!(args);
                }
                if let Err(error) = self.inner.runtime()?.request(command).await {
                    self.inner
                        .out_of_band_message(format!("[Error] Failed to hand off turn: {error}"))
                        .await?;
                }
            }
            "steer" | "follow-up" if self.inner.spec.family == "omp" && !args.is_empty() => {
                let behavior = if name == "steer" { "steer" } else { "followUp" };
                if self.inner.state.lock().unwrap().active.is_none() {
                    self.prompt(json!(args), json!({}), Some(behavior)).await?;
                } else {
                    self.inner
                        .runtime()?
                        .request(
                            json!({"type":"prompt","message":args,"streamingBehavior":behavior}),
                        )
                        .await?;
                }
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}
