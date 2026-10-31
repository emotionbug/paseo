use crate::{
    providers::api::{AgentSession, EventSink, RewindTarget},
    state::AppState,
};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex as StdMutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Mutex, OnceCell, RwLock, broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

mod actions;
pub(crate) mod creation;
mod date_parse;
mod generation;
mod listing;
pub mod notifications;
pub mod receipts;
mod runtime;
pub mod storage;
pub mod timeline;
mod wire;
pub use generation::{StructuredAgentFallbackError, StructuredAgentResponseError};
pub(crate) use generation::{compile_schema, retry_prompt, validate_response};
pub use listing::select_agent_list_items;
use notifications::{AgentMessageDelivery, FinishSubscriptions};
use receipts::{CreationReceipts, MessageReceipts};
use storage::AgentStorage;
use timeline::{StreamCoalescer, SubagentStore, Timeline};

pub fn snapshot_payload(snapshot: &Value) -> Value {
    wire::snapshot_payload(snapshot)
}
pub fn serialize_stream_event(event: &Value) -> Option<Value> {
    wire::serialize_stream_event(event)
}
pub fn project_legacy_event(
    context: &crate::transport::RequestContext,
    message: &Value,
) -> Option<Value> {
    wire::project_legacy_event(context, message)
}
pub async fn legacy_attention_eligible(
    manager: &AgentManager,
    context: &crate::transport::RequestContext,
    snapshot: &Value,
) -> Result<bool> {
    wire::legacy_attention_eligible(manager, context, snapshot).await
}
fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
/// Every touch strictly advances `updatedAt`, even within one millisecond or after a
/// clock step backwards, so directory stamps and activity ordering never regress.
fn next_updated_at(previous: &Value) -> String {
    let now = chrono::Utc::now();
    let next = previous
        .as_str()
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .map(|previous| previous.with_timezone(&chrono::Utc))
        .filter(|previous| previous.timestamp_millis() >= now.timestamp_millis())
        .map_or(now, |previous| {
            chrono::DateTime::from_timestamp_millis(previous.timestamp_millis() + 1)
                .unwrap_or(now)
        });
    next.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
/// A stored record's change time is the later of `updatedAt` and `lastActivityAt`:
/// metadata writes on unloaded agents move one without the other.
fn stored_updated_at(record: &Value) -> Option<String> {
    // Reversed so `max_by_key` (last maximum wins) keeps `updatedAt` on equal times.
    ["lastActivityAt", "updatedAt"]
        .iter()
        .filter_map(|key| text(record, key).filter(|value| !value.is_empty()))
        .filter_map(|raw| {
            date_parse::parse_date(raw)
                .ok()
                .flatten()
                .map(|parsed| (parsed, raw))
        })
        .max_by_key(|(parsed, _)| *parsed)
        .map(|(_, raw)| raw.to_owned())
}
fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}
fn required<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    text(value, key).ok_or_else(|| anyhow!("Missing {key}"))
}
fn object(value: &mut Value) -> &mut serde_json::Map<String, Value> {
    if !value.is_object() {
        *value = json!({});
    }
    value.as_object_mut().unwrap()
}
/// runtime-mcp-config.ts stripInternalPaseoMcpServer: the daemon-owned server lives in
/// `config.mcpServers.paseo`; a map emptied by removing it is dropped.
fn strip_config_internal_servers(config: &mut Value) {
    let Some(servers) = config.get_mut("mcpServers").filter(|servers| servers.is_object()) else {
        return;
    };
    let before = servers.as_object().map_or(0, |servers| servers.len());
    *servers = crate::mcp::strip_internal_servers(servers.take());
    if servers
        .as_object()
        .is_some_and(|servers| servers.is_empty() && before > 0)
    {
        object(config).remove("mcpServers");
    }
}
fn merge(target: &mut Value, source: &Value) {
    if let Some(source) = source.as_object() {
        for (key, value) in source {
            object(target).insert(key.clone(), value.clone());
        }
    }
}
fn is_busy(snapshot: &Value) -> bool {
    matches!(
        text(snapshot, "lifecycle").or_else(|| text(snapshot, "status")),
        Some("running" | "initializing")
    )
}
fn with_cwd(mut handle: Value, cwd: &Value) -> Value {
    if handle.is_object() {
        if !handle["metadata"].is_object() {
            handle["metadata"] = json!({});
        }
        handle["metadata"]["cwd"] = cwd.clone();
    }
    handle
}

struct AgentData {
    snapshot: Value,
    session: Option<Arc<dyn AgentSession>>,
    ready: bool,
    history_primed: bool,
    finalized: VecDeque<String>,
    resolutions: HashMap<String, Value>,
    event_error: Option<String>,
    staged_events: Option<Vec<Value>>,
    staged_bytes: usize,
    /// agent-manager.ts pendingReplacement: a replacing prompt holds the agent busy
    /// while the interrupted turn settles.
    pending_replacement: bool,
}
struct AgentCell {
    data: Mutex<AgentData>,
    mutation: Mutex<()>,
    commit: Mutex<()>,
    timeline: Arc<StdMutex<Timeline>>,
    cancel: CancellationToken,
    sink: EventSink,
    barriers: Mutex<HashMap<String, oneshot::Sender<()>>>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

pub struct AgentManager {
    app: Weak<AppState>,
    pub(super) storage: OnceCell<AgentStorage>,
    pub(super) receipts: OnceCell<MessageReceipts>,
    pub(super) creations: OnceCell<CreationReceipts>,
    creation_state: creation::CreationState,
    pub directory_sync: crate::directory_sync::DirectorySyncService,
    pub notifications: FinishSubscriptions,
    pub message_delivery: AgentMessageDelivery,
    agents: RwLock<HashMap<String, Arc<AgentCell>>>,
    lifecycle: Mutex<HashMap<String, Weak<Mutex<()>>>>,
    subagent_store: StdMutex<SubagentStore>,
    events: broadcast::Sender<Value>,
    closing: AtomicBool,
}

impl AgentManager {
    pub fn new(app: Weak<AppState>) -> Self {
        let (events, _) = broadcast::channel(2048);
        Self {
            notifications: FinishSubscriptions::new(app.clone()),
            message_delivery: AgentMessageDelivery::new(app.clone()),
            app,
            storage: OnceCell::new(),
            receipts: OnceCell::new(),
            creations: OnceCell::new(),
            creation_state: creation::CreationState::default(),
            directory_sync: crate::directory_sync::DirectorySyncService::new(),
            agents: RwLock::new(HashMap::new()),
            lifecycle: Mutex::new(HashMap::new()),
            subagent_store: StdMutex::new(SubagentStore::default()),
            events,
            closing: AtomicBool::new(false),
        }
    }
    pub(super) fn app(&self) -> Result<Arc<AppState>> {
        self.app
            .upgrade()
            .ok_or_else(|| anyhow!("Agent manager is shutting down"))
    }
    pub(super) fn storage(&self) -> Result<&AgentStorage> {
        self.storage
            .get()
            .ok_or_else(|| anyhow!("Agent storage is not initialized"))
    }
    pub(super) fn receipts(&self) -> Result<&MessageReceipts> {
        self.receipts
            .get()
            .ok_or_else(|| anyhow!("Agent receipts are not initialized"))
    }
    pub(super) fn creations(&self) -> Result<&CreationReceipts> {
        self.creations
            .get()
            .ok_or_else(|| anyhow!("Creation receipts are not initialized"))
    }
    pub async fn initialize(&self) -> Result<()> {
        let home = self.app()?.home.clone();
        self.storage
            .get_or_try_init(|| async {
                let store = AgentStorage::new(home.clone());
                store.initialize().await?;
                Ok::<_, anyhow::Error>(store)
            })
            .await?;
        self.receipts
            .get_or_init(|| async { MessageReceipts::new(home.clone()) })
            .await;
        self.creations
            .get_or_init(|| async { CreationReceipts::new(home.clone()) })
            .await;
        for record in self.storage()?.list().await? {
            let Some(id) = text(&record, "id") else {
                continue;
            };
            if let Some(bytes) =
                storage::read_optional(&home.join("agent-subagents").join(format!("{id}.json")))
                    .await?
            {
                let checkpoint: Value = serde_json::from_slice(&bytes)?;
                self.subagent_store
                    .lock()
                    .map_err(|_| anyhow!("Subagent lock poisoned"))?
                    .restore_parent(&checkpoint)?;
            }
        }
        self.notifications.initialize().await
    }
    pub async fn handle(
        &self,
        context: &crate::transport::RequestContext,
        message: &Value,
    ) -> Result<bool> {
        if self.notifications.handle(context, message).await? {
            return Ok(true);
        }
        wire::handle(self, context, message).await
    }
    pub fn subscribe_events(&self) -> broadcast::Receiver<Value> {
        self.events.subscribe()
    }
    pub async fn register_finish_subscription(
        &self,
        input: Value,
        pending: bool,
    ) -> Result<notifications::FinishRegistration> {
        self.notifications.register(input, pending).await
    }
    pub async fn list_persisted_records(&self) -> Result<Vec<Value>> {
        let home = self.app()?.home.clone();
        self.storage
            .get_or_try_init(|| async {
                let store = AgentStorage::new(home);
                store.initialize().await?;
                Ok::<_, anyhow::Error>(store)
            })
            .await?
            .list()
            .await
    }
    pub async fn backfill_workspace_id(&self, id: &str, workspace_id: &str) -> Result<bool> {
        self.list_persisted_records().await?;
        let lane = self.lane(id).await;
        let _lane = lane.lock().await;
        let mut changed = false;
        self.storage()?
            .mutate(id, |previous| {
                let mut record = previous
                    .cloned()
                    .ok_or_else(|| anyhow!("Agent not found"))?;
                if text(&record, "workspaceId").is_none_or(str::is_empty) {
                    record["workspaceId"] = json!(workspace_id);
                    changed = true;
                }
                Ok(record)
            })
            .await?;
        if changed {
            if let Ok(cell) = self.cell(id).await {
                cell.data.lock().await.snapshot["workspaceId"] = json!(workspace_id);
            }
        }
        Ok(changed)
    }
    pub async fn has_in_flight_run(&self, id: &str) -> Result<bool> {
        let cell = self.cell(id).await?;
        let data = cell.data.lock().await;
        Ok(data.session.is_some()
            && (is_busy(&data.snapshot)
                || data.snapshot["activeForegroundTurnId"].is_string()
                || data.staged_events.is_some()))
    }
    async fn lane(&self, id: &str) -> Arc<Mutex<()>> {
        let mut lanes = self.lifecycle.lock().await;
        if let Some(lane) = lanes.get(id).and_then(Weak::upgrade) {
            return lane;
        }
        lanes.retain(|_, lane| lane.strong_count() > 0);
        let lane = Arc::new(Mutex::new(()));
        lanes.insert(id.to_owned(), Arc::downgrade(&lane));
        lane
    }
    async fn cell(&self, id: &str) -> Result<Arc<AgentCell>> {
        self.agents
            .read()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow!("Unknown agent '{id}'"))
    }
    pub async fn session(&self, id: &str) -> Result<Option<Arc<dyn AgentSession>>> {
        Ok(match self.agents.read().await.get(id).cloned() {
            Some(cell) => cell.data.lock().await.session.clone(),
            None => None,
        })
    }
    pub async fn agent_snapshot(&self, id: &str) -> Result<Option<Value>> {
        if let Some(cell) = self.agents.read().await.get(id).cloned() {
            let data = cell.data.lock().await;
            if data.ready {
                return Ok(Some(data.snapshot.clone()));
            }
        }
        Ok(self
            .storage()?
            .get(id)
            .await?
            .map(|record| self.stored_snapshot(record)))
    }
    fn stored_snapshot(&self, mut record: Value) -> Value {
        let available = self.app.upgrade().is_some_and(|app| {
            text(&record, "provider").is_some_and(|id| app.providers.spec(id).is_some())
        });
        record["_live"] = json!(false);
        record["_providerAvailable"] = json!(available);
        record["status"] = record.get("lastStatus").cloned().unwrap_or(json!("idle"));
        record["lifecycle"] = record["status"].clone();
        if let Some(updated) = stored_updated_at(&record) {
            record["updatedAt"] = json!(updated);
        }
        record["currentModeId"] = record.get("lastModeId").cloned().unwrap_or(Value::Null);
        record["availableModes"] = json!([]);
        record["pendingPermissions"] = json!([]);
        record["activeTurn"] = Value::Null;
        record["attention"] = json!({"requiresAttention":record["requiresAttention"]==true,"attentionReason":record["attentionReason"],"attentionTimestamp":record["attentionTimestamp"]});
        if record.get("capabilities").is_none() {
            record["capabilities"] = json!({"supportsStreaming":false,"supportsSessionPersistence":true,"supportsDynamicModes":false,"supportsMcpServers":false,"supportsReasoningStream":false,"supportsToolInvocations":true,"supportsRewindConversation":false,"supportsRewindFiles":false,"supportsRewindBoth":false});
        }
        record
    }
    pub async fn list_snapshots(&self) -> Result<Vec<Value>> {
        let mut all: HashMap<String, Value> = self
            .storage()?
            .list()
            .await?
            .into_iter()
            .filter_map(|record| {
                text(&record, "id")
                    .map(str::to_owned)
                    .map(|id| (id, self.stored_snapshot(record)))
            })
            .collect();
        let cells = self
            .agents
            .read()
            .await
            .iter()
            .map(|(id, cell)| (id.clone(), cell.clone()))
            .collect::<Vec<_>>();
        for (id, cell) in cells {
            let data = cell.data.lock().await;
            if data.ready {
                all.insert(id, data.snapshot.clone());
            }
        }
        Ok(all.into_values().collect())
    }
    pub async fn cwd(&self, id: &str) -> Result<PathBuf> {
        let snapshot = self
            .agent_snapshot(id)
            .await?
            .ok_or_else(|| anyhow!("Unknown agent '{id}'"))?;
        Ok(PathBuf::from(required(&snapshot, "cwd")?))
    }
    pub(super) async fn authorize(&self, actor: &Value, id: &str) -> Result<()> {
        let snapshot = self
            .agent_snapshot(id)
            .await?
            .ok_or_else(|| anyhow!("Unknown agent '{id}'"))?;
        match text(actor, "kind") {
            Some("daemon") => {
                let owner = &snapshot["owner"];
                if owner["kind"] != "daemon"
                    || owner["daemonId"] != actor["daemonId"]
                    || owner["executionId"] != actor["executionId"]
                {
                    bail!("Agent is not owned by this daemon execution");
                }
            }
            Some("agent") => {
                let caller = required(actor, "agentId")?;
                if self.agent_snapshot(caller).await?.is_none() {
                    bail!("Caller agent is unavailable");
                }
            }
            Some("owner" | "system") => {}
            _ => {
                if actor.is_null() {
                    bail!("Missing agent operation authority");
                }
            }
        }
        Ok(())
    }
    pub(super) async fn normalize_config(
        &self,
        config: Value,
        resolve_default_model: bool,
    ) -> Result<Value> {
        self.normalize_config_for(config, resolve_default_model, false)
            .await
    }
    async fn normalize_config_for(
        &self,
        mut config: Value,
        resolve_default_model: bool,
        history: bool,
    ) -> Result<Value> {
        let app = self.app()?;
        let provider = required(&config, "provider")?.to_owned();
        app.providers.get(&provider)?;
        let options = config.get("providerOptions").cloned().unwrap_or(json!({}));
        config["providerOptions"] = app.providers.validate_options(&provider, &options)?;
        let cwd = required(&config, "cwd")?;
        let expanded = if cwd == "~" {
            app.user_home.clone()
        } else if let Some(rest) = cwd.strip_prefix("~/") {
            app.user_home.join(rest)
        } else {
            PathBuf::from(cwd)
        };
        let cwd = if expanded.is_absolute() {
            expanded
        } else {
            std::env::current_dir()?.join(expanded)
        };
        if !history
            && !tokio::fs::metadata(&cwd)
                .await
                .with_context(|| format!("Working directory does not exist: {}", cwd.display()))?
                .is_dir()
        {
            bail!("Working directory is not a directory: {}", cwd.display());
        }
        config["cwd"] = json!(cwd);
        object(&mut config).remove("daemonAppendSystemPrompt");
        strip_config_internal_servers(&mut config);
        if let Some(model) = text(&config, "model") {
            let model = model.trim().to_owned();
            if model.is_empty() || model == "default" {
                object(&mut config).remove("model");
            } else {
                config["model"] = json!(model);
            }
        }
        if resolve_default_model && config.get("model").is_none() {
            if let Ok(catalog) = app.providers.catalog(&provider, Some(&cwd), true).await {
                if let Some(models) = catalog["models"].as_array() {
                    if let Some(model) = models
                        .iter()
                        .find(|m| m["isDefault"] == true)
                        .or_else(|| models.first())
                        .and_then(|m| m.get("id"))
                    {
                        config["model"] = model.clone();
                    }
                }
            }
        }
        if let Some(grants) = config["toolPolicy"]["preapproved"].as_array() {
            for grant in grants {
                let server = required(grant, "server")?;
                if config["mcpServers"].get(server).is_none() {
                    bail!(
                        "toolPolicy preapproval '{}.{}' requires MCP server '{}' in the same agent request",
                        server,
                        text(grant, "tool").unwrap_or(""),
                        server
                    );
                }
            }
        }
        if let Some(policy) = config.get("toolPolicy").cloned() {
            config = app
                .providers
                .apply_tool_policy(&provider, config, &policy)?;
        }
        Ok(config)
    }
    async fn make_cell(
        &self,
        id: &str,
        mut snapshot: Value,
        history: bool,
    ) -> Result<(Arc<AgentCell>, mpsc::Receiver<Value>)> {
        let app = self.app()?;
        // Drain final provider events after daemon cancellation, before committing closed state.
        let cancel = CancellationToken::new();
        let (sink, receiver) = EventSink::channel(256, cancel.clone());
        let path = app.home.join("agent-timelines").join(format!("{id}.json"));
        let owned_id = id.to_owned();
        let timeline =
            tokio::task::spawn_blocking(move || Timeline::load(&path, &owned_id)).await??;
        let history = history || timeline.next_seq() > 1;
        snapshot["_live"] = json!(true);
        snapshot["_providerAvailable"] = json!(true);
        snapshot["lifecycle"] = json!("initializing");
        snapshot["status"] = json!("initializing");
        snapshot["pendingPermissions"] = json!([]);
        snapshot["availableModes"] = json!([]);
        snapshot["activeTurn"] = Value::Null;
        let cell = Arc::new(AgentCell {
            data: Mutex::new(AgentData {
                snapshot,
                session: None,
                ready: false,
                history_primed: history,
                finalized: VecDeque::new(),
                resolutions: HashMap::new(),
                event_error: None,
                staged_events: None,
                staged_bytes: 0,
                pending_replacement: false,
            }),
            mutation: Mutex::new(()),
            commit: Mutex::new(()),
            timeline: Arc::new(StdMutex::new(timeline)),
            cancel,
            sink,
            barriers: Mutex::new(HashMap::new()),
            task: Mutex::new(None),
        });
        self.agents
            .write()
            .await
            .insert(id.to_owned(), cell.clone());
        Ok((cell, receiver))
    }
    fn start_events(
        &self,
        id: &str,
        cell: &Arc<AgentCell>,
        mut receiver: mpsc::Receiver<Value>,
    ) -> tokio::task::JoinHandle<()> {
        let app = self.app.clone();
        let id = id.to_owned();
        let cell = Arc::downgrade(cell);
        tokio::spawn(async move {
            let mut coalescer = StreamCoalescer::new(60);
            loop {
                let Some(current) = cell.upgrade() else { break };
                let cancel = current.cancel.clone();
                drop(current);
                let deadline = coalescer.next_deadline().map(|deadline| {
                    Duration::from_millis(
                        deadline
                            .saturating_sub(chrono::Utc::now().timestamp_millis().max(0) as u64),
                    )
                });
                let event = tokio::select! {biased;event=receiver.recv()=>event,_=cancel.cancelled()=>None,_=async{match deadline{Some(delay)=>tokio::time::sleep(delay).await,None=>std::future::pending::<()>().await}}=>Some(json!({"type":"__flush"}))};
                let Some(state) = app.upgrade() else { break };
                let manager = state.agents.clone();
                drop(state);
                let Some(current) = cell.upgrade() else { break };
                let now_ms = chrono::Utc::now().timestamp_millis().max(0) as u64;
                let mut flushed = coalescer.flush_due(now_ms);
                let closing = event.is_none();
                let mut direct = None;
                if let Some(event) = event {
                    if event["type"] != "__flush" {
                        if matches!(text(&event, "type"), Some("__barrier" | "__release_stage")) {
                            flushed.extend(coalescer.flush_for(&id, now_ms));
                            direct = Some(event);
                        } else if let Some(output) = coalescer.handle(&id, &event, now_ms) {
                            flushed.extend(output);
                        } else {
                            flushed.extend(coalescer.flush_for(&id, now_ms));
                            direct = Some(event);
                        }
                    }
                } else {
                    flushed.extend(coalescer.flush_all(now_ms));
                }
                let result=async {for output in flushed{let mut event=json!({"type":"timeline","provider":output["provider"],"item":output["item"]});if let Some(turn)=output.get("turnId"){event["turnId"]=turn.clone();}manager.apply_event(&id,&current,event,false).await?;}
                    if let Some(event)=direct {
                        if matches!(text(&event,"type"),Some("__barrier"|"__release_stage")){
                            if event["type"]=="__release_stage" {
                                let staged={let mut data=current.data.lock().await;data.staged_bytes=0;data.staged_events.take().unwrap_or_default()};
                                for event in staged{manager.apply_event(&id,&current,event,false).await?;}
                            }
                            if let Some(key)=text(&event,"key"){if let Some(sender)=current.barriers.lock().await.remove(key){let _=sender.send(());}}
                        }else{manager.apply_event(&id,&current,event,false).await?;}
                    }
                    Ok::<_,anyhow::Error>(())}.await;
                if let Err(error) = result {
                    current.data.lock().await.event_error = Some(error.to_string());
                    current.cancel.cancel();
                    tracing::error!(agent_id=%id,error=%error,"Failed to persist agent provider event");
                    break;
                }
                if closing {
                    break;
                }
            }
        })
    }
    async fn drain_events(&self, cell: &Arc<AgentCell>) -> Result<()> {
        if let Some(error) = cell.data.lock().await.event_error.clone() {
            bail!("Agent event persistence failed: {error}");
        }
        let key = uuid::Uuid::new_v4().to_string();
        let (sender, receiver) = oneshot::channel();
        cell.barriers.lock().await.insert(key.clone(), sender);
        cell.sink
            .emit(json!({"type":"__barrier","key":key}))
            .await?;
        tokio::select! {result=receiver=>result.map_err(|_|anyhow!("Agent event consumer closed")),_=cell.cancel.cancelled()=>Err(anyhow!("Agent event consumer closed"))}
    }
    async fn release_event_stage(&self, cell: &Arc<AgentCell>) -> Result<()> {
        let key = uuid::Uuid::new_v4().to_string();
        let (sender, receiver) = oneshot::channel();
        cell.barriers.lock().await.insert(key.clone(), sender);
        cell.sink
            .emit(json!({"type":"__release_stage","key":key}))
            .await?;
        tokio::select! {result=receiver=>result.map_err(|_|anyhow!("Agent event consumer closed")),_=cell.cancel.cancelled()=>Err(anyhow!("Agent event consumer closed"))}
    }
    async fn persist_snapshot(&self, snapshot: &Value) -> Result<()> {
        if snapshot["internal"] == true {
            return Ok(());
        }
        let mut record = snapshot.clone();
        let fields = record
            .as_object_mut()
            .ok_or_else(|| anyhow!("Invalid agent snapshot"))?;
        for field in [
            "_live",
            "_providerAvailable",
            "_historyOnly",
            "status",
            "lifecycle",
            "session",
            "pendingPermissions",
            "availableModes",
            "capabilities",
            "activeTurn",
            "activeTurnId",
            "activeForegroundTurnId",
            "attention",
            "model",
            "thinkingOptionId",
            "effectiveThinkingOptionId",
        ] {
            fields.remove(field);
        }
        record["lastStatus"] = snapshot["lifecycle"].clone();
        record["lastModeId"] = snapshot
            .get("currentModeId")
            .filter(|mode| mode.is_string())
            .or_else(|| snapshot["config"].get("modeId").filter(|mode| mode.is_string()))
            .cloned()
            .unwrap_or(Value::Null);
        if snapshot["requiresAttention"] != true {
            record["attentionReason"] = Value::Null;
            record["attentionTimestamp"] = Value::Null;
        }
        record["lastActivityAt"] = snapshot["updatedAt"].clone();
        if let Some(config) = record.get_mut("config") {
            object(config).remove("daemonAppendSystemPrompt");
            strip_config_internal_servers(config);
        }
        if let Some(metadata) = record
            .get_mut("persistence")
            .and_then(|handle| handle.get_mut("metadata"))
            .filter(|value| value.is_object())
        {
            object(metadata).remove("daemonAppendSystemPrompt");
            strip_config_internal_servers(metadata);
        }
        self.storage()?.upsert(record).await
    }
    async fn commit_snapshot(&self, cell: &Arc<AgentCell>, mut snapshot: Value) -> Result<()> {
        let _commit = cell.commit.lock().await;
        snapshot["updatedAt"] = json!(next_updated_at(&snapshot["updatedAt"]));
        snapshot["status"] = snapshot["lifecycle"].clone();
        let ready = cell.data.lock().await.ready;
        if ready {
            self.persist_snapshot(&snapshot).await?;
        }
        cell.data.lock().await.snapshot = snapshot.clone();
        if ready {
            let _ = self
                .events
                .send(json!({"type":"agent_state","agent":snapshot}));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_runtime_server_is_stripped_from_config_servers() {
        let internal = json!({"type":"http","url":"http://127.0.0.1:1/mcp/agents?callerAgentId=a","headers":{"Authorization":"Bearer t"}});
        let mut only = json!({"provider":"codex","mcpServers":{"paseo":internal}});
        strip_config_internal_servers(&mut only);
        assert!(only.get("mcpServers").is_none());
        let mut mixed = json!({"mcpServers":{"paseo":internal,"other":{"type":"stdio","command":"x"}}});
        strip_config_internal_servers(&mut mixed);
        assert_eq!(mixed["mcpServers"], json!({"other":{"type":"stdio","command":"x"}}));
        let mut empty = json!({"mcpServers":{}});
        strip_config_internal_servers(&mut empty);
        assert_eq!(empty["mcpServers"], json!({}));
    }

    #[test]
    fn updated_at_strictly_advances_past_previous_stamp() {
        let future = "2999-01-01T00:00:00.000Z";
        assert_eq!(next_updated_at(&json!(future)), "2999-01-01T00:00:00.001Z");
        let before = chrono::Utc::now().timestamp_millis();
        let fresh = next_updated_at(&json!("2001-01-01T00:00:00.000Z"));
        let parsed = chrono::DateTime::parse_from_rfc3339(&fresh)
            .unwrap()
            .timestamp_millis();
        assert!(parsed >= before);
        assert!(chrono::DateTime::parse_from_rfc3339(&next_updated_at(&Value::Null)).is_ok());
    }

    #[test]
    fn stored_updated_at_uses_latest_of_updated_and_activity() {
        let record = json!({"updatedAt":"2026-01-01T00:00:00.000Z","lastActivityAt":"2026-02-01T00:00:00.000Z"});
        assert_eq!(
            stored_updated_at(&record).as_deref(),
            Some("2026-02-01T00:00:00.000Z")
        );
        let renamed = json!({"updatedAt":"2026-03-01T00:00:00.000Z","lastActivityAt":"2026-02-01T00:00:00.000Z"});
        assert_eq!(
            stored_updated_at(&renamed).as_deref(),
            Some("2026-03-01T00:00:00.000Z")
        );
        assert_eq!(stored_updated_at(&json!({"updatedAt":"bad"})), None);
    }
}
