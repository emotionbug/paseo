use super::super::{
    catalog,
    runtime::{Runtime, RuntimeManager, read_sse},
};
use super::{
    Call, Operation, Snapshot, array, component, permissions, s, session_path,
    timeline::{OUTPUT_TOOL, Timeline, timestamp},
    usage,
};
use crate::providers::api::{EventSink, LaunchContext};
use anyhow::{Result, anyhow, bail};
use reqwest::Method;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    sync::{mpsc, oneshot},
    time::{Instant, MissedTickBehavior},
};
use tokio_util::sync::CancellationToken;

struct Turn {
    id: String,
    accepted: bool,
    denied: bool,
    output: Option<jsonschema::Validator>,
}
struct Pending {
    request: Value,
    owner: String,
    form: Option<Value>,
}
struct Child {
    timeline: Timeline,
    signature: String,
}
pub(super) struct Session {
    runtime: Arc<Runtime>,
    manager: Arc<RuntimeManager>,
    id: String,
    info: Value,
    config: Value,
    context: LaunchContext,
    options: Value,
    sink: EventSink,
    snapshot: Arc<Mutex<Snapshot>>,
    cancel: CancellationToken,
    stream_cancel: CancellationToken,
    closed: CancellationToken,
    close_error: Arc<Mutex<Option<String>>>,
    stream: Option<tokio::task::JoinHandle<Result<()>>>,
    events: mpsc::Receiver<Value>,
    event_sender: mpsc::Sender<Value>,
    timeline: Timeline,
    history: Vec<Value>,
    pending: HashMap<String, Pending>,
    pending_order: Vec<String>,
    children: HashMap<String, Child>,
    modes: Vec<Value>,
    turn: Option<Turn>,
    sequence: u64,
    execution: Option<Value>,
    stop_waiters: Vec<oneshot::Sender<Result<Value>>>,
    stop_error: Option<String>,
    rewind_waiters: Vec<(String, oneshot::Sender<Result<Value>>)>,
    deferred: Vec<Call>,
    connected: bool,
    reconcile_at: Option<Instant>,
    context_limits: HashMap<String, Option<Value>>,
}
impl Session {
    pub fn new(
        runtime: Arc<Runtime>,
        manager: Arc<RuntimeManager>,
        id: String,
        info: Value,
        config: Value,
        context: LaunchContext,
        options: Value,
        sink: EventSink,
        snapshot: Arc<Mutex<Snapshot>>,
        cancel: CancellationToken,
        closed: CancellationToken,
        close_error: Arc<Mutex<Option<String>>>,
    ) -> Self {
        let (event_sender, events) = mpsc::channel(256);
        Self {
            runtime,
            manager,
            id,
            info,
            config,
            context,
            options,
            sink,
            snapshot,
            stream_cancel: cancel.child_token(),
            cancel,
            closed,
            close_error,
            stream: None,
            events,
            event_sender,
            timeline: Timeline::default(),
            history: Vec::new(),
            pending: HashMap::new(),
            pending_order: Vec::new(),
            children: HashMap::new(),
            modes: Vec::new(),
            turn: None,
            sequence: 0,
            execution: None,
            stop_waiters: Vec::new(),
            stop_error: None,
            rewind_waiters: Vec::new(),
            deferred: Vec::new(),
            connected: false,
            reconcile_at: None,
            context_limits: HashMap::new(),
        }
    }
    fn cwd(&self) -> &str {
        s(&self.config, "cwd")
    }
    fn environment(&self) -> Value {
        let mut environment = self.runtime.environment.clone();
        environment.extend(
            self.context
                .env
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
        json!(environment)
    }
    fn publish(&self) {
        let mut metadata = self
            .options
            .get("persistenceConfig")
            .cloned()
            .unwrap_or_else(|| self.config.clone());
        for key in [
            "model",
            "modeId",
            "thinkingOptionId",
            "featureValues",
            "openCodeRuntimeNotices",
        ] {
            if let Some(value) = self.config.get(key) {
                metadata[key] = value.clone();
            }
        }
        if let Some(map) = metadata.as_object_mut() {
            map.remove("daemonAppendSystemPrompt");
        }
        let mut snapshot = self.snapshot.lock().unwrap();
        snapshot.info = self.info.clone();
        snapshot.config = metadata;
        snapshot.modes = self.modes.clone();
        snapshot.pending = self
            .pending_order
            .iter()
            .filter_map(|id| self.pending.get(id))
            .map(|v| v.request.clone())
            .collect();
        if snapshot
            .features
            .first()
            .and_then(|v| v.get("value"))
            .and_then(Value::as_bool)
            != Some(self.config["featureValues"]["auto_accept"] == true)
        {
            snapshot.features = catalog::features_v2(&self.config).into();
        }
    }
    async fn request(
        &self,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<Value>,
    ) -> Result<Value> {
        self.runtime
            .request(method, path, query, body, self.cancel.clone())
            .await
    }
    async fn get(&self, path: &str) -> Result<Value> {
        self.request(Method::GET, path, &[], None).await
    }
    async fn post(&self, suffix: &str, body: Value) -> Result<Value> {
        self.request(
            Method::POST,
            &session_path(&self.id, suffix),
            &[],
            Some(body),
        )
        .await
    }
    async fn emit(&self, event: Value) -> Result<()> {
        tokio::select! {_=self.cancel.cancelled()=>bail!("OpenCode session closed"),result=self.sink.emit(event)=>result}
    }
    async fn emit_timeline(&self, mut event: Value) -> Result<()> {
        if let Some(turn) = &self.turn {
            event["turnId"] = json!(turn.id);
        }
        self.emit(event).await
    }
    async fn messages(&self, id: &str) -> Result<Vec<Value>> {
        let mut result = Vec::new();
        let mut cursor: Option<String> = None;
        let mut seen = HashSet::new();
        loop {
            let mut query = vec![("limit".to_owned(), "100".to_owned())];
            if let Some(cursor) = &cursor {
                query.push(("cursor".to_owned(), cursor.clone()));
            } else {
                query.push(("order".to_owned(), "asc".to_owned()));
            }
            let page = self
                .request(Method::GET, &session_path(id, "/message"), &query, None)
                .await?;
            result.extend(array(&page["data"]).iter().cloned());
            cursor = page["cursor"]["next"].as_str().map(str::to_owned);
            let Some(next) = &cursor else {
                break;
            };
            if !seen.insert(next.clone()) {
                bail!("OpenCode repeated a message pagination cursor");
            }
        }
        Ok(result)
    }
    async fn configure(&mut self) -> Result<()> {
        catalog::wait_ready(&self.runtime, self.cwd(), false, self.cancel.clone()).await?;
        if self.options["requiresPaseoPlugin"] == true {
            catalog::wait_ready(&self.runtime, self.cwd(), true, self.cancel.clone()).await?;
        }
        self.runtime.bind_session(&self.id, &self.context)?;
        self.manager.remember_session(&self.id, &self.runtime);
        if let Some(servers) = self.config["mcpServers"].as_object() {
            for (name, server) in servers {
                let config = if s(server, "type") == "stdio" {
                    let mut command = vec![server["command"].clone()];
                    command.extend(array(&server["args"]).iter().cloned());
                    let mut config = json!({"type":"local","command":command,"codemode":false});
                    if let Some(environment) = server.get("env") {
                        config["environment"] = environment.clone();
                    }
                    config
                } else {
                    let mut config =
                        json!({"type":"remote","url":server["url"],"oauth":false,"codemode":false});
                    if let Some(headers) = server.get("headers") {
                        config["headers"] = headers.clone();
                    }
                    config
                };
                self.request(
                    Method::PUT,
                    &format!("/api/experimental/mcp/{}", component(name)),
                    &catalog::location_query(self.cwd()),
                    Some(json!({"config":config})),
                )
                .await?;
                tokio::time::timeout(Duration::from_secs(30),async {
                    loop {
                        let catalog=self.request(Method::GET,"/api/mcp",&catalog::location_query(self.cwd()),None).await?;
                        if let Some(entry)=array(&catalog["data"]).iter().find(|v|s(v,"name")==name){
                            match s(&entry["status"],"status"){"connected"=>return Ok(()),"pending"=>{},status=>bail!("OpenCode MCP server {name} failed to connect: {}",entry["status"]["error"].as_str().unwrap_or(status))}
                        }
                        tokio::select!{_=self.cancel.cancelled()=>bail!("OpenCode session closed"),_=tokio::time::sleep(Duration::from_millis(50))=>{}}
                    }
                }).await.map_err(|_|anyhow!("OpenCode MCP server {name} did not connect within 30000ms"))??;
            }
        }
        let system = [
            self.config["systemPrompt"].as_str(),
            self.config["daemonAppendSystemPrompt"].as_str(),
        ]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
        if !system.is_empty() {
            self.request(
                Method::PUT,
                &format!(
                    "/api/experimental/session/{}/instructions/entries/paseo",
                    component(&self.id)
                ),
                &[],
                Some(json!({"value":system})),
            )
            .await?;
        }
        Ok(())
    }
    async fn start_stream(&mut self) -> Result<()> {
        self.stream_cancel.cancel();
        if let Some(task) = self.stream.take() {
            let _ = task.await;
        }
        while self.events.try_recv().is_ok() {}
        self.stream_cancel = self.cancel.child_token();
        self.connected = false;
        self.timeline.reset_streams();
        let response = self
            .runtime
            .events("/api/event", &[], self.stream_cancel.clone())
            .await?;
        let cancel = self.stream_cancel.clone();
        let sender = self.event_sender.clone();
        self.stream = Some(tokio::spawn(async move {
            read_sse(response, cancel, sender).await
        }));
        Ok(())
    }
    async fn await_connected(&mut self) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(30),async{
            loop{
                tokio::select!{
                    _=self.cancel.cancelled()=>bail!("OpenCode session closed"),
                    event=self.events.recv()=>{
                        self.observe(event.ok_or_else(||anyhow!("OpenCode event stream ended before connecting"))?).await?;
                        if self.connected{return Ok(());}
                    }
                    _=tokio::time::sleep(Duration::from_millis(25))=>{
                        if self.stream.as_ref().is_none_or(|task|task.is_finished()){
                            if let Some(task)=self.stream.take(){task.await??;}
                            bail!("OpenCode event stream ended before connecting");
                        }
                    }
                }
            }
        }).await.map_err(|_|anyhow!("OpenCode event stream did not connect within 30000ms"))?
    }
    async fn initialize(&mut self) -> Result<()> {
        self.configure().await?;
        let agents = self
            .request(
                Method::GET,
                "/api/agent",
                &catalog::location_query(self.cwd()),
                None,
            )
            .await?;
        self.modes = catalog::modes(super::super::runtime::Generation::V2, &agents);
        self.history = self.messages(&self.id).await?;
        self.timeline.messages(&self.history, true)?;
        self.start_stream().await?;
        self.await_connected().await?;
        self.publish();
        Ok(())
    }
    pub async fn run(
        mut self,
        mut calls: mpsc::Receiver<Call>,
        ready: oneshot::Sender<Result<()>>,
    ) {
        if let Err(error) = self.initialize().await {
            let _ = ready.send(Err(error));
            self.cleanup().await;
            return;
        }
        let _ = ready.send(Ok(()));
        let mut tick = tokio::time::interval(Duration::from_millis(25));
        tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut recovery = Instant::now() + Duration::from_secs(5);
        let mut retry_stream = Instant::now();
        loop {
            tokio::select! {
                _=self.cancel.cancelled()=>break,
                call=calls.recv()=>{let Some(call)=call else{break;};if self.handle(call).await{break;}},
                event=self.events.recv()=>{if let Some(event)=event{if let Err(error)=self.observe(event).await{tracing::warn!(%error,"OpenCode event reconciliation failed");self.stream_cancel.cancel();self.reconcile_at=Some(Instant::now()+Duration::from_millis(100));}}},
                _=tick.tick()=>{
                    if self.runtime.is_exited(){
                        if self.turn.is_some(){let _=self.fail("OpenCode helper server exited".to_owned()).await;}
                    }else if self.stream.as_ref().is_none_or(|task|task.is_finished())&&Instant::now()>=retry_stream {
                        if let Err(error)=self.start_stream().await{tracing::warn!(%error,"OpenCode event stream reconnect failed");}retry_stream=Instant::now()+Duration::from_millis(100);
                    }
                    if self.reconcile_at.is_some_and(|at|Instant::now()>=at){self.reconcile_at=None;if let Err(error)=self.reconcile().await{tracing::warn!(%error,"OpenCode reconciliation failed");}}
                    if self.turn.is_some()&&Instant::now()>=recovery{if let Err(error)=self.reconcile_turn().await{tracing::warn!(%error,"OpenCode turn recovery failed");}recovery=Instant::now()+Duration::from_secs(5);}
                    if self.turn.is_none(){self.finish_stops().await;}
                    if self.stop_waiters.is_empty()&&self.rewind_waiters.is_empty()&&!self.deferred.is_empty(){let deferred=std::mem::take(&mut self.deferred);for call in deferred{if self.handle(call).await{self.cancel.cancel();break;}}}
                }
            }
        }
        self.cleanup().await;
    }
    async fn cleanup(&mut self) {
        self.cancel.cancel();
        self.stream_cancel.cancel();
        if let Some(task) = self.stream.take() {
            let _ = task.await;
        }
        self.runtime
            .unbind_session(&self.id, &self.context.agent_id);
        for id in self.children.keys() {
            self.runtime.unbind_session(id, &self.context.agent_id);
        }
        self.turn = None;
        for reply in self.stop_waiters.drain(..) {
            let _ = reply.send(Err(anyhow!("OpenCode session closed")));
        }
        for (_, reply) in self.rewind_waiters.drain(..) {
            let _ = reply.send(Err(anyhow!("OpenCode session closed")));
        }
        if self.options["persistSession"] == false {
            let result = tokio::time::timeout(
                Duration::from_secs(30),
                self.runtime.request(
                    Method::DELETE,
                    &session_path(&self.id, ""),
                    &[],
                    None,
                    CancellationToken::new(),
                ),
            )
            .await;
            let error = match result {
                Ok(Ok(_)) => None,
                Ok(Err(error)) => Some(error.to_string()),
                Err(_) => Some("OpenCode session removal timed out".to_owned()),
            };
            *self.close_error.lock().unwrap() = error;
        }
        self.closed.cancel();
    }
    async fn ensure_runtime(&mut self) -> Result<()> {
        if !self.runtime.is_exited() {
            return Ok(());
        }
        if self.turn.is_some() {
            self.fail("OpenCode helper server exited".to_owned())
                .await?;
        }
        self.stream_cancel.cancel();
        if let Some(task) = self.stream.take() {
            let _ = task.await;
        }
        let next = self
            .manager
            .acquire(Some(&self.context), &self.config, self.cancel.clone())
            .await?;
        let old = std::mem::replace(&mut self.runtime, next);
        let connected = async {
            self.configure().await?;
            for id in self.children.keys() {
                self.runtime.bind_session(id, &self.context)?;
                self.manager.remember_session(id, &self.runtime);
            }
            self.start_stream().await?;
            self.await_connected().await
        }
        .await;
        if let Err(error) = connected {
            self.stream_cancel.cancel();
            if let Some(task) = self.stream.take() {
                let _ = task.await;
            }
            self.runtime = old;
            self.manager.remember_session(&self.id, &self.runtime);
            return Err(error);
        }
        Ok(())
    }
    async fn reconcile(&mut self) -> Result<()> {
        let info = self.get(&session_path(&self.id, "")).await?;
        let history = self.messages(&self.id).await?;
        self.info = info["data"].clone();
        self.history = history;
        for event in self.timeline.messages(&self.history, true)? {
            self.emit_timeline(event).await?;
        }
        self.reconcile_permissions(&self.id.clone()).await?;
        self.publish();
        Ok(())
    }
    async fn reconcile_permissions(&mut self, id: &str) -> Result<()> {
        let requests = self.get(&session_path(id, "/permission")).await?;
        let forms = self.get(&session_path(id, "/form")).await?;
        for request in array(&requests["data"]) {
            let request_id = s(request, "id").to_owned();
            if self.pending.contains_key(&request_id) {
                continue;
            }
            let mapped = permissions::permission(request);
            self.pending.insert(
                request_id.clone(),
                Pending {
                    request: mapped.clone(),
                    owner: id.to_owned(),
                    form: None,
                },
            );
            self.pending_order.push(request_id.clone());
            self.publish();
            if self.config.get("toolPolicy").is_none_or(Value::is_null)
                && self.config["featureValues"]["auto_accept"] == true
            {
                self.respond(&request_id, json!({"behavior":"allow"}))
                    .await?;
            } else {
                self.emit(
                    json!({"type":"permission_requested","provider":"opencode","request":mapped}),
                )
                .await?;
            }
        }
        for form in array(&forms["data"]) {
            let request_id = s(form, "id").to_owned();
            if self.pending.contains_key(&request_id) {
                continue;
            }
            let mapped = permissions::form(form);
            self.pending_order.push(request_id.clone());
            self.pending.insert(
                request_id,
                Pending {
                    request: mapped.clone(),
                    owner: id.to_owned(),
                    form: Some(form.clone()),
                },
            );
            self.publish();
            self.emit(
                json!({"type":"permission_requested","provider":"opencode","request":mapped}),
            )
            .await?;
        }
        Ok(())
    }
    async fn resolve(&mut self, id: &str, response: Value) -> Result<()> {
        if self.pending.remove(id).is_some() {
            self.pending_order.retain(|entry| entry != id);
            self.publish();
            self.emit(json!({"type":"permission_resolved","provider":"opencode","requestId":id,"resolution":response})).await?;
        }
        Ok(())
    }
    async fn respond(&mut self, id: &str, response: Value) -> Result<()> {
        let pending = self.pending.get(id);
        let owner = pending.map(|p| p.owner.as_str()).unwrap_or(&self.id);
        let own = owner == self.id;
        let deny = s(&response, "behavior") == "deny";
        if let Some(form) = pending.and_then(|p| p.form.as_ref()) {
            let path = session_path(owner, &format!("/form/{}", component(id)));
            if deny {
                self.request(Method::DELETE, &path, &[], None).await?;
            } else {
                let answer = permissions::answer(form, &response)?;
                self.request(
                    Method::POST,
                    &format!("{path}/reply"),
                    &[],
                    Some(json!({"answer":answer})),
                )
                .await?;
            }
        } else {
            let decision = if deny {
                "reject"
            } else if s(&response, "selectedActionId") == "always" {
                "always"
            } else {
                "once"
            };
            self.request(
                Method::POST,
                &session_path(owner, &format!("/permission/{}/reply", component(id))),
                &[],
                Some(json!({"decision":decision})),
            )
            .await?;
        }
        self.resolve(id, response).await?;
        if deny && own {
            if let Some(turn) = self.turn.as_mut() {
                turn.denied = true;
            }
            self.reconcile_at = Some(Instant::now());
        }
        Ok(())
    }
    async fn reconcile_children(&mut self, parent: &str) -> Result<()> {
        let mut parents = vec![parent.to_owned()];
        let mut visited = HashSet::new();
        while let Some(parent) = parents.pop() {
            if !visited.insert(parent.clone()) {
                continue;
            }
            let mut cursor: Option<String> = None;
            let mut seen = HashSet::new();
            loop {
                let mut query = vec![("limit".to_owned(), "100".to_owned())];
                if let Some(cursor) = &cursor {
                    query.push(("cursor".to_owned(), cursor.clone()));
                } else {
                    query.push(("parentID".to_owned(), parent.clone()));
                }
                let page = self
                    .request(Method::GET, "/api/session", &query, None)
                    .await?;
                for child in array(&page["data"]) {
                    self.reconcile_child(child.clone()).await?;
                    parents.push(s(child, "id").to_owned());
                }
                cursor = page["cursor"]["next"].as_str().map(str::to_owned);
                let Some(next) = &cursor else {
                    break;
                };
                if !seen.insert(next.clone()) {
                    bail!("OpenCode repeated a child pagination cursor");
                }
            }
        }
        Ok(())
    }
    async fn reconcile_child(&mut self, info: Value) -> Result<()> {
        let id = s(&info, "id").to_owned();
        self.reconcile_permissions(&id).await?;
        if !self.children.contains_key(&id) {
            self.runtime.bind_session(&id, &self.context)?;
            self.manager.remember_session(&id, &self.runtime);
            self.children.insert(
                id.clone(),
                Child {
                    timeline: Timeline::default(),
                    signature: String::new(),
                },
            );
        }
        let active = self.get("/api/session/active").await?;
        let status = if active["data"]
            .get(&id)
            .is_some_and(|v| !v.is_null() && v != false)
        {
            "running"
        } else {
            match s(&info, "outcome") {
                "failed" => "failed",
                "interrupted" => "canceled",
                _ => "completed",
            }
        };
        let parent = if s(&info, "parentID") == self.id {
            Value::Null
        } else {
            info["parentID"].clone()
        };
        let presentation = json!({"type":"upsert","id":id,"parentSubagentId":parent,"title":info["title"],"status":status,"cwd":info["location"]["directory"],"timestamp":timestamp(&info["time"]["updated"])});
        let signature = serde_json::to_string(&presentation)?;
        let changed = self
            .children
            .get(&id)
            .is_some_and(|child| child.signature != signature);
        if changed {
            self.children.get_mut(&id).unwrap().signature = signature;
            self.emit(
                json!({"type":"provider_subagent","provider":"opencode","event":presentation}),
            )
            .await?;
        }
        let history = self.messages(&id).await?;
        let events = self
            .children
            .get_mut(&id)
            .unwrap()
            .timeline
            .messages(&history, true)?;
        for event in events {
            self.emit(json!({"type":"provider_subagent","provider":"opencode","event":{"type":"timeline","id":id,"item":event["item"],"timestamp":event["timestamp"]}})).await?;
        }
        Ok(())
    }
    async fn observe(&mut self, event: Value) -> Result<()> {
        let kind = s(&event, "type");
        let data = &event["data"];
        if kind == "server.connected" {
            self.timeline.reset_streams();
            self.request(
                Method::PUT,
                &session_path(&self.id, "/environment"),
                &[],
                Some(json!({"variables":self.environment()})),
            )
            .await?;
            self.reconcile().await?;
            self.reconcile_children(&self.id.clone()).await?;
            self.reconcile_turn().await?;
            self.connected = true;
            return Ok(());
        }
        if kind == "session.created" {
            let parent = s(data, "parentID");
            if parent == self.id || self.children.contains_key(parent) {
                self.reconcile_children(parent).await?;
            }
        }
        let event_session = s(data, "sessionID");
        if self.children.contains_key(event_session) {
            let info = self.get(&session_path(event_session, "")).await?;
            self.reconcile_child(info["data"].clone()).await?;
        }
        if kind == "permission.replied" {
            self.resolve(
                s(data, "requestID"),
                json!({"behavior":if s(data,"reply")=="reject"{"deny"}else{"allow"}}),
            )
            .await?;
        }
        if kind == "form.replied" || kind == "form.cancelled" {
            self.resolve(
                s(data, "id"),
                json!({"behavior":if kind=="form.cancelled"{"deny"}else{"allow"}}),
            )
            .await?;
        }
        if kind == "form.created" && s(&data["form"], "sessionID") == self.id {
            self.reconcile_at = Some(Instant::now() + Duration::from_millis(25));
        }
        if event_session != self.id {
            return Ok(());
        }
        let structured = self.turn.as_ref().is_some_and(|t| t.output.is_some());
        match kind {
            "session.text.started" | "session.reasoning.started" => {
                let text = kind == "session.text.started";
                if !text || !structured {
                    self.timeline
                        .start(data, if text { "text" } else { "reasoning" });
                }
                return Ok(());
            }
            "session.text.delta" | "session.reasoning.delta" => {
                let text = kind == "session.text.delta";
                if !text || !structured {
                    if let Some(event) = self
                        .timeline
                        .delta(data, if text { "text" } else { "reasoning" })?
                    {
                        self.emit_timeline(event).await?;
                    }
                }
                return Ok(());
            }
            "session.step.ended" => {
                if let Err(error) = self.report_usage(&data["tokens"]).await {
                    tracing::warn!(%error,"OpenCode context usage update failed");
                }
            }
            "session.execution.started"
            | "session.execution.succeeded"
            | "session.execution.failed"
            | "session.execution.interrupted" => {
                let seq = event["durable"]["seq"].as_u64().unwrap_or(0);
                if seq > self.sequence {
                    if kind == "session.execution.started" && self.turn.is_none() {
                        self.create_turn(None, true).await?;
                    }
                    self.sequence = seq;
                    self.execution = Some(event.clone());
                    if let Err(error) = self.reconcile_turn().await {
                        tracing::warn!(%error,"OpenCode turn reconciliation failed; retrying");
                    }
                }
            }
            _ => {}
        }
        if self.reconcile_at.is_none() {
            self.reconcile_at = Some(Instant::now() + Duration::from_millis(25));
        }
        Ok(())
    }
    async fn report_usage(&mut self, tokens: &Value) -> Result<()> {
        let model = &self.info["model"];
        let key = format!("{}/{}", s(model, "providerID"), s(model, "id"));
        if !model.is_null() && !self.context_limits.contains_key(&key) {
            let catalog = self
                .request(
                    Method::GET,
                    "/api/model",
                    &catalog::location_query(self.cwd()),
                    None,
                )
                .await?;
            let limit = array(&catalog["data"])
                .iter()
                .find(|v| v["providerID"] == model["providerID"] && v["id"] == model["id"])
                .and_then(|v| v["limit"].get("context"))
                .cloned();
            self.context_limits.insert(key.clone(), limit);
        }
        let mut usage = usage(&self.info);
        usage["contextWindowUsedTokens"] = json!(
            ["input", "output", "reasoning"]
                .iter()
                .map(|k| tokens[*k].as_u64().unwrap_or(0))
                .sum::<u64>()
                + tokens["cache"]["read"].as_u64().unwrap_or(0)
                + tokens["cache"]["write"].as_u64().unwrap_or(0)
        );
        if let Some(Some(limit)) = self.context_limits.get(&key) {
            usage["contextWindowMaxTokens"] = limit.clone();
        }
        self.emit(json!({"type":"usage_updated","provider":"opencode","usage":usage}))
            .await
    }
    async fn create_turn(
        &mut self,
        output: Option<jsonschema::Validator>,
        accepted: bool,
    ) -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        self.turn = Some(Turn {
            id: id.clone(),
            accepted,
            denied: false,
            output,
        });
        self.execution = None;
        self.emit(json!({"type":"turn_started","provider":"opencode","turnId":id}))
            .await?;
        Ok(id)
    }
    async fn fail(&mut self, error: String) -> Result<()> {
        if self.cancel.is_cancelled() {
            self.turn = None;
            return Ok(());
        }
        if let Some(turn) = self.turn.take() {
            self.emit(
                json!({"type":"turn_failed","provider":"opencode","turnId":turn.id,"error":error}),
            )
            .await?;
        }
        Ok(())
    }
    async fn read_execution(&mut self) -> Result<()> {
        let query = vec![
            ("after".to_owned(), self.sequence.to_string()),
            ("follow".to_owned(), "false".to_owned()),
        ];
        let response = self
            .runtime
            .events(
                &format!("/api/experimental/session/{}/log", component(&self.id)),
                &query,
                self.cancel.clone(),
            )
            .await?;
        let (sender, mut receiver) = mpsc::channel(128);
        let cancel = self.cancel.clone();
        let task = tokio::spawn(async move { read_sse(response, cancel, sender).await });
        while let Some(event) = receiver.recv().await {
            let seq = event["durable"]["seq"].as_u64().unwrap_or(0);
            if s(&event, "type").starts_with("session.execution.") && seq > self.sequence {
                self.sequence = seq;
                self.execution = Some(event);
            }
        }
        task.await??;
        Ok(())
    }
    async fn reconcile_turn(&mut self) -> Result<()> {
        let active = self.get("/api/session/active").await?;
        if active["data"]
            .get(&self.id)
            .is_some_and(|v| !v.is_null() && v != false)
        {
            if self.turn.is_none() {
                self.create_turn(None, true).await?;
            }
            return Ok(());
        }
        if !self.turn.as_ref().is_some_and(|t| t.accepted) {
            return Ok(());
        }
        self.read_execution().await?;
        let Some(execution) = self.execution.clone() else {
            return Ok(());
        };
        let kind = s(&execution, "type");
        if kind == "session.execution.started"
            || (kind == "session.execution.interrupted"
                && s(&execution["data"], "reason") == "shutdown"
                && !self.turn.as_ref().is_some_and(|t| t.denied))
        {
            return Ok(());
        }
        self.reconcile().await?;
        if kind == "session.execution.succeeded" {
            if let Some(validator) = self.turn.as_ref().and_then(|t| t.output.as_ref()) {
                if let Err(error) = assert_output(validator, &self.history) {
                    self.fail(error.to_string()).await?;
                    return Ok(());
                }
            }
        }
        let Some(turn) = self.turn.take() else {
            return Ok(());
        };
        let event = match kind {
            "session.execution.interrupted" => {
                json!({"type":"turn_canceled","provider":"opencode","turnId":turn.id,"reason":"OpenCode interrupted execution"})
            }
            "session.execution.failed" => {
                json!({"type":"turn_failed","provider":"opencode","turnId":turn.id,"error":execution["data"]["error"]["message"]})
            }
            _ => {
                json!({"type":"turn_completed","provider":"opencode","turnId":turn.id,"usage":usage(&self.info)})
            }
        };
        self.emit(event).await?;
        Ok(())
    }
    async fn dispatch(&self, mut input: Value, options: &Value) -> Result<()> {
        let text = s(&input, "text");
        let command = text
            .strip_prefix('/')
            .filter(|rest| rest.chars().next().is_some_and(|ch| !ch.is_whitespace()))
            .map(|rest| {
                rest.split_once(char::is_whitespace)
                    .map(|(name, args)| (name, Some(args.trim_start())))
                    .unwrap_or((rest, None))
            });
        let mut suffix = "/prompt";
        if let Some((name, args)) = command {
            if name == "compact" || name == "summarize" {
                self.post("/compact", json!({})).await?;
                return Ok(());
            }
            let commands =
                catalog::commands(&self.runtime, self.cwd(), self.cancel.clone()).await?;
            if let Some(selected) = commands.iter().find(|v| s(v, "name") == name) {
                if s(selected, "kind") != "skill" {
                    input = json!({"name":name,"text":args.unwrap_or(""),"files":input["files"]});
                    suffix = "/command";
                } else {
                    input = json!({"text":args.map(str::to_owned).unwrap_or_else(||format!("Use the {name} skill.")),"files":input["files"],"skills":[{"id":name}]});
                }
            }
        }
        if suffix == "/prompt" {
            let mut metadata = json!({});
            if options["clientMessageId"]
                .as_str()
                .is_some_and(|v| !v.is_empty())
            {
                metadata["paseoClientMessageId"] = options["clientMessageId"].clone();
            }
            if let Some(schema) = options.get("outputSchema") {
                metadata["paseoOutputSchema"] = schema.clone();
            }
            input["metadata"] = metadata;
        }
        self.post(suffix, input).await?;
        Ok(())
    }
    async fn finish_stops(&mut self) {
        for reply in self.stop_waiters.drain(..) {
            let _ = reply.send(Ok(Value::Null));
        }
        let rewinds = std::mem::take(&mut self.rewind_waiters);
        for (id, reply) in rewinds {
            let result = async {
                self.ensure_runtime().await?;
                self.post("/revert/stage", json!({"messageID":id,"files":true}))
                    .await?;
                self.post("/revert/commit", json!({})).await?;
                Ok(Value::Null)
            }
            .await;
            let _ = reply.send(result);
        }
    }
    async fn handle(&mut self, call: Call) -> bool {
        if matches!(&call.operation, Operation::Start(..))
            && (!self.stop_waiters.is_empty() || !self.rewind_waiters.is_empty())
        {
            self.deferred.push(call);
            return false;
        }
        let Call { operation, reply } = call;
        // A failed mutation is never retried: only a helper already known dead is replaced.
        if !matches!(
            &operation,
            Operation::Steer(..) | Operation::Interrupt | Operation::Rewind(..)
        ) {
            if let Err(error) = self.ensure_runtime().await {
                let _ = reply.send(Err(error));
                return false;
            }
        }
        match operation {
            Operation::Start(prompt, options) => {
                if let Some(error) = &self.stop_error {
                    let _ = reply.send(Err(anyhow!("{error}")));
                    return false;
                }
                if self.turn.is_some() {
                    let _ = reply.send(Err(anyhow!("OpenCode session already has an active turn")));
                    return false;
                }
                let input = match prompt_input(&prompt) {
                    Ok(input) => input,
                    Err(error) => {
                        let _ = reply.send(Err(error));
                        return false;
                    }
                };
                let output = match options.get("outputSchema").map(compile_schema).transpose() {
                    Ok(output) => output,
                    Err(error) => {
                        let _ = reply.send(Err(error));
                        return false;
                    }
                };
                let id = match self.create_turn(output, false).await {
                    Ok(id) => id,
                    Err(error) => {
                        let _ = reply.send(Err(error));
                        return false;
                    }
                };
                let _ = reply.send(Ok(json!({"turnId":id})));
                match self.dispatch(input, &options).await {
                    Ok(()) => {
                        if let Some(turn) = self.turn.as_mut() {
                            turn.accepted = true;
                        }
                        if let Err(error) = self.reconcile_turn().await {
                            tracing::warn!(%error,"OpenCode turn admission reconciliation failed");
                        }
                    }
                    Err(error) => {
                        let _ = self.fail(error.to_string()).await;
                    }
                }
            }
            Operation::Interrupt | Operation::Rewind(_) => {
                let rewind = if let Operation::Rewind(id) = operation {
                    Some(id)
                } else {
                    None
                };
                if !self.stop_waiters.is_empty() || !self.rewind_waiters.is_empty() {
                    if let Some(id) = rewind {
                        self.rewind_waiters.push((id, reply));
                    } else {
                        self.stop_waiters.push(reply);
                    }
                    return false;
                }
                match self.post("/interrupt", json!({})).await {
                    Ok(_) => {
                        self.stop_error = None;
                        if let Some(id) = rewind {
                            self.rewind_waiters.push((id, reply));
                        } else {
                            self.stop_waiters.push(reply);
                        }
                        if let Err(error) = self.reconcile_turn().await {
                            tracing::warn!(%error,"OpenCode interruption reconciliation failed");
                        }
                        if self.turn.is_none() {
                            self.finish_stops().await;
                        }
                    }
                    Err(error) => {
                        self.stop_error = Some(error.to_string());
                        let _ = reply.send(Err(error));
                    }
                }
            }
            operation => {
                let result = self.operation(operation).await;
                self.publish();
                let _ = reply.send(result);
            }
        }
        false
    }
    async fn operation(&mut self, operation: Operation) -> Result<Value> {
        match operation {
            Operation::Steer(prompt, options) => {
                if self.turn.as_ref().map(|t| t.id.as_str()) != options["expectedTurnId"].as_str()
                    || !self.stop_waiters.is_empty()
                    || !self.rewind_waiters.is_empty()
                {
                    return Ok(json!({"status":"unavailable"}));
                }
                let mut input = prompt_input(&prompt)?;
                input["delivery"] = json!("steer");
                if options["clientMessageId"]
                    .as_str()
                    .is_some_and(|v| !v.is_empty())
                {
                    input["metadata"] = json!({"paseoClientMessageId":options["clientMessageId"]});
                }
                self.post("/prompt", input).await?;
                if options["clearPendingPermissions"] == true {
                    let ids = self.pending_order.clone();
                    for id in ids {
                        self.respond(&id, json!({"behavior":"deny"})).await?;
                    }
                }
                Ok(json!({"status":"accepted"}))
            }
            Operation::History(sink) => {
                let history = self.messages(&self.id).await?;
                let mut timeline = Timeline::default();
                for event in
                    super::notices::merge(&self.config, timeline.messages(&history, false)?)
                {
                    tokio::select! {_=self.cancel.cancelled()=>bail!("OpenCode session closed"),result=sink.emit(event)=>result?}
                }
                Ok(Value::Null)
            }
            Operation::RuntimeInfo => {
                self.info = self.get(&session_path(&self.id, "")).await?["data"].clone();
                Ok(self.runtime_info())
            }
            Operation::Mode(mode) => {
                self.post("/agent", json!({"agent":mode})).await?;
                self.info["agent"] = json!(mode);
                self.config["modeId"] = json!(mode);
                self.emit(json!({"type":"mode_changed","provider":"opencode","currentModeId":mode,"availableModes":self.modes})).await?;
                Ok(Value::Null)
            }
            Operation::Permission(id, response) => {
                self.respond(&id, response).await?;
                if self.turn.as_ref().is_some_and(|t| t.denied) {
                    if let Err(error) = self.reconcile_turn().await {
                        tracing::warn!(%error,"OpenCode denied request reconciliation failed");
                    }
                }
                Ok(Value::Null)
            }
            Operation::Commands => Ok(Value::Array(
                catalog::commands(&self.runtime, self.cwd(), self.cancel.clone()).await?,
            )),
            Operation::Model(model) => {
                let selected = if let Some(model) = &model {
                    model_ref(model)?
                } else {
                    self.request(
                        Method::GET,
                        "/api/model/default",
                        &catalog::location_query(self.cwd()),
                        None,
                    )
                    .await?["data"]
                        .clone()
                };
                if selected.is_null() {
                    bail!("OpenCode has no default model");
                }
                let catalog = self
                    .request(
                        Method::GET,
                        "/api/model",
                        &catalog::location_query(self.cwd()),
                        None,
                    )
                    .await?;
                let target = array(&catalog["data"])
                    .iter()
                    .find(|v| {
                        v["enabled"] == true
                            && v["providerID"] == selected["providerID"]
                            && v["id"] == selected["id"]
                    })
                    .ok_or_else(|| {
                        anyhow!(
                            "OpenCode model unavailable: {}/{}",
                            s(&selected, "providerID"),
                            s(&selected, "id")
                        )
                    })?;
                let retained = &self.config["thinkingOptionId"];
                let variant = if retained != "default"
                    && array(&target["variants"])
                        .iter()
                        .any(|v| v["id"] == *retained)
                {
                    retained.clone()
                } else {
                    Value::Null
                };
                let mut next = json!({"id":selected["id"],"providerID":selected["providerID"]});
                if !variant.is_null() {
                    next["variant"] = variant.clone();
                }
                self.post("/model", json!({"model":next})).await?;
                self.info["model"] = next;
                self.config["model"] = json!(model);
                self.config["thinkingOptionId"] = variant.clone();
                self.emit(json!({"type":"thinking_option_changed","provider":"opencode","thinkingOptionId":variant})).await?;
                self.emit(json!({"type":"model_changed","provider":"opencode","runtimeInfo":self.runtime_info()})).await?;
                Ok(Value::Null)
            }
            Operation::Thinking(variant) => {
                let current = &self.info["model"];
                if current.is_null() {
                    bail!("Select an OpenCode model before changing its variant");
                }
                let mut model = json!({"id":current["id"],"providerID":current["providerID"]});
                if let Some(value) = variant.as_deref().filter(|v| !v.is_empty()) {
                    model["variant"] = json!(value);
                }
                self.post("/model", json!({"model":model})).await?;
                self.config["thinkingOptionId"] = json!(variant);
                self.info = self.get(&session_path(&self.id, "")).await?["data"].clone();
                self.emit(json!({"type":"thinking_option_changed","provider":"opencode","thinkingOptionId":variant})).await?;
                Ok(Value::Null)
            }
            Operation::Feature(id, value) => {
                if id != "auto_accept" || !value.is_boolean() {
                    bail!("Unknown OpenCode feature");
                }
                if !self.config["featureValues"].is_object() {
                    self.config["featureValues"] = json!({});
                }
                self.config["featureValues"][&id] = value.clone();
                if value == true && self.config.get("toolPolicy").is_none_or(Value::is_null) {
                    let ids: Vec<_> = self
                        .pending_order
                        .iter()
                        .filter(|id| {
                            self.pending
                                .get(*id)
                                .is_some_and(|p| s(&p.request, "kind") != "question")
                        })
                        .cloned()
                        .collect();
                    for id in ids {
                        self.respond(&id, json!({"behavior":"allow"})).await?;
                    }
                }
                Ok(Value::Null)
            }
            Operation::Start(..) | Operation::Interrupt | Operation::Rewind(..) => {
                unreachable!("control operation handled by actor")
            }
        }
    }
    fn runtime_info(&self) -> Value {
        let model = &self.info["model"];
        json!({"provider":"opencode","sessionId":self.id,"model":if model.is_null(){Value::Null}else{json!(format!("{}/{}",s(model,"providerID"),s(model,"id")))},"modeId":self.info["agent"],"thinkingOptionId":model["variant"]})
    }
}
fn prompt_input(prompt: &Value) -> Result<Value> {
    if let Some(text) = prompt.as_str() {
        return Ok(json!({"text":text,"files":[]}));
    }
    let parts = prompt
        .as_array()
        .ok_or_else(|| anyhow!("OpenCode prompt must be text or content blocks"))?;
    let mut text = Vec::new();
    let mut files = Vec::new();
    for part in parts {
        match s(part, "type") {
            "text" => text.push(s(part, "text").to_owned()),
            "image" => files.push(
                json!({"uri":format!("data:{};base64,{}",s(part,"mimeType"),s(part,"data"))}),
            ),
            _ => text.push(crate::providers::prompt::render_attachment(part)?),
        }
    }
    Ok(json!({"text":text.join("\n"),"files":files}))
}
fn model_ref(model: &str) -> Result<Value> {
    let Some((provider, id)) = model.split_once('/') else {
        bail!("OpenCode model must be provider/model");
    };
    if provider.is_empty() || id.is_empty() {
        bail!("OpenCode model must be provider/model");
    }
    Ok(json!({"providerID":provider,"id":id}))
}
fn compile_schema(schema: &Value) -> Result<jsonschema::Validator> {
    if !schema.is_boolean() && !schema.is_object() {
        bail!("OpenCode output schema must be a boolean or object");
    }
    jsonschema::options()
        .with_draft(jsonschema::Draft::Draft7)
        .should_validate_formats(false)
        .build(schema)
        .map_err(|error| anyhow!("Invalid OpenCode output schema: {error}"))
}
fn assert_output(validator: &jsonschema::Validator, history: &[Value]) -> Result<()> {
    let start = history
        .iter()
        .rposition(|m| s(m, "type") == "user")
        .map(|i| i + 1)
        .unwrap_or(0);
    for message in &history[start..] {
        if s(message, "type") != "assistant" {
            continue;
        }
        for part in array(&message["content"]) {
            if s(part, "type") != "tool"
                || s(part, "name") != OUTPUT_TOOL
                || s(&part["state"], "status") != "completed"
            {
                continue;
            }
            let Some(result) = part["state"]["metadata"].get("paseoStructuredOutput") else {
                continue;
            };
            let errors = validator
                .iter_errors(result)
                .map(|error| error.to_string())
                .collect::<Vec<_>>();
            if !errors.is_empty() {
                bail!("Invalid OpenCode structured output: {}", errors.join(", "));
            }
            return Ok(());
        }
    }
    bail!("OpenCode finished without submitting the required structured output")
}
