use super::*;

impl AgentManager {
    fn launch_config(&self, mut config: Value) -> Result<Value> {
        object(&mut config).remove("daemonAppendSystemPrompt");
        if let Some(prompt) = self
            .app()?
            .config
            .snapshot()
            .get("appendSystemPrompt")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            config["daemonAppendSystemPrompt"] = json!(prompt);
        }
        Ok(config)
    }
    async fn open_context(
        &self,
        id: &str,
        config: &Value,
        workspace: Option<&Value>,
        reason: &str,
        purpose: &str,
        env: BTreeMap<String, String>,
    ) -> Result<crate::providers::api::LaunchContext> {
        let app = self.app()?;
        let request = json!({"agentId":id,"workspaceId":workspace.cloned().unwrap_or(Value::Null),"provider":config["provider"],"cwd":config["cwd"],"reason":reason,"purpose":purpose,"env":env});
        let transformed = app.plugins.before("agent.session_open", request).await?;
        let env = serde_json::from_value::<BTreeMap<String, String>>(transformed["env"].clone())
            .context("Invalid plugin session environment")?;
        app.mcp.launch_context(id, config, env).await
    }
    pub(super) async fn create_session(
        &self,
        mut config: Value,
        id: Option<String>,
        mut options: Value,
    ) -> Result<Value> {
        if self.closing.load(Ordering::Acquire) {
            bail!("Agent manager is shutting down");
        }
        let id = id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        uuid::Uuid::parse_str(&id).context("createAgent: agentId must be a UUID")?;
        let lane = self.lane(&id).await;
        let _lane = lane.lock().await;
        if self.agent_snapshot(&id).await?.is_some() {
            bail!("Agent '{id}' already exists");
        }
        let app = self.app()?;
        if config["internal"] != true {
            let mut request = json!({"config":config});
            if let Some(env) = options.get("env") {
                request["env"] = env.clone();
            }
            let transformed = app.plugins.before("agent.create", request).await?;
            config = transformed["config"].clone();
            if let Some(env) = transformed.get("env") {
                options["env"] = env.clone();
            } else {
                object(&mut options).remove("env");
            }
        }
        let config = self.normalize_config(config, true).await?;
        let provider = required(&config, "provider")?;
        let factory = app.providers.get(provider)?;
        if !factory
            .is_available(json!({}), app.cancel.child_token())
            .await?
        {
            bail!("Provider '{provider}' is not available. Please ensure the CLI is installed.");
        }
        let timestamp = now();
        let mut snapshot = json!({"id":id,"provider":provider,"cwd":config["cwd"],"config":config,"createdAt":timestamp,"updatedAt":timestamp,"lastUserMessageAt":null,"title":options.get("initialTitle").or_else(||config.get("title")).cloned().unwrap_or(Value::Null),"labels":options.get("labels").cloned().unwrap_or(json!({})),"internal":config["internal"]==true,"persistence":null,"currentModeId":null,"requiresAttention":false,"attentionReason":null,"attentionTimestamp":null,"attention":{"requiresAttention":false},"archivedAt":null});
        for field in ["workspaceId", "owner"] {
            if let Some(value) = options.get(field) {
                snapshot[field] = value.clone();
            }
        }
        let (cell, receiver) = self.make_cell(&id, snapshot, true).await?;
        *cell.task.lock().await = Some(self.start_events(&id, &cell, receiver));
        let result = async {
            let mut env = app.config.environment();
            if let Some(workspace) = text(&options, "workspaceId") {
                env.extend(app.projects.teardown_environment(workspace).await?);
            }
            if let Some(overrides) = options.get("env").and_then(Value::as_object) {
                for (key, value) in overrides {
                    if let Some(value) = value.as_str() {
                        env.insert(key.clone(), value.to_owned());
                    }
                }
            }
            env.insert("PASEO_AGENT_ID".into(), id.clone());
            env.insert("PASEO_AGENT_CWD".into(), required(&config, "cwd")?.into());
            let context = self
                .open_context(
                    &id,
                    &config,
                    options.get("workspaceId"),
                    "create",
                    "interactive",
                    env,
                )
                .await?;
            let launch = self.launch_config(config.clone())?;
            let session = factory
                .create_session(launch, context, options.clone(), cell.sink.clone())
                .await?;
            let snapshot = self
                .register_session(&id, &cell, session, None, false)
                .await?;
            if snapshot["internal"] != true {
                let _ = app
                    .plugins
                    .emit("agent.created", json!({"agent":hook_agent(&snapshot)}))
                    .await;
            }
            Ok(snapshot)
        }
        .await;
        if result.is_err() {
            self.discard_failed_registration(&id, &cell).await;
        }
        result
    }
    pub(super) async fn discard_failed_registration(&self, id: &str, cell: &Arc<AgentCell>) {
        let _mutation = cell.mutation.lock().await;
        cell.cancel.cancel();
        let session = cell.data.lock().await.session.take();
        if let Some(session) = session {
            if let Err(error) = session.close().await {
                tracing::warn!(agent_id=id,%error,"Failed to close unregistered provider session");
            }
        }
        self.agents.write().await.remove(id);
        if let Ok(app) = self.app() {
            app.mcp.revoke_agent(id).await;
        }
        if let Some(task) = cell.task.lock().await.take() {
            let _ = task.await;
        }
    }
    async fn register_session(
        &self,
        id: &str,
        cell: &Arc<AgentCell>,
        session: Arc<dyn AgentSession>,
        persistence: Option<Value>,
        restoring: bool,
    ) -> Result<Value> {
        let _mutation = cell.mutation.lock().await;
        {
            cell.data.lock().await.session = Some(session.clone());
        }
        if self.closing.load(Ordering::Acquire) || cell.cancel.is_cancelled() {
            bail!("Agent manager is shutting down");
        }
        self.drain_events(cell).await?;
        let capabilities = session.capabilities().clone();
        let config = cell.data.lock().await.snapshot["config"].clone();
        if config["mcpServers"]
            .as_object()
            .is_some_and(|servers| !servers.is_empty())
            && capabilities["supportsMcpServers"] != true
        {
            bail!(
                "Provider '{}' does not support MCP servers",
                session.provider()
            );
        }
        let modes = session.available_modes().await.unwrap_or_default();
        let mode = session.current_mode().await.unwrap_or(None);
        let info = session.runtime_info().await.ok();
        let prime =
            !session.initial_timeline().is_empty() && !cell.data.lock().await.history_primed;
        if prime {
            self.hydrate_history(id, cell, false).await?;
        }
        let _commit = cell.commit.lock().await;
        let mut snapshot = cell.data.lock().await.snapshot.clone();
        snapshot["capabilities"] = capabilities;
        snapshot["features"] = json!(session.features().as_ref());
        snapshot["availableModes"] = json!(modes);
        snapshot["currentModeId"] = json!(mode);
        if let Some(mode) = mode {
            snapshot["config"]["modeId"] = json!(mode);
        }
        if let Some(info) = info {
            snapshot["runtimeInfo"] = info;
        }
        if let Some(handle) = session.describe_persistence().or(persistence) {
            snapshot["persistence"] = with_cwd(handle, &snapshot["cwd"]);
        }
        let permissions = session.pending_permissions();
        if !permissions.is_empty() {
            snapshot["pendingPermissions"] = json!(permissions);
        }
        if snapshot["lifecycle"] == "initializing" {
            snapshot["lifecycle"] = json!("idle");
            snapshot["status"] = json!("idle");
        }
        // registerSession: restoring keeps the stored activity stamp so reopening a chat
        // does not rewrite the workspace's "last used" ordering.
        if !restoring {
            snapshot["updatedAt"] = json!(next_updated_at(&snapshot["updatedAt"]));
        }
        if !prime {
            for entry in session.initial_timeline() {
                for item in
                    notifications::project_agent_messages(entry.get("item").unwrap_or(entry))
                {
                    self.append_to_cell(id, cell, item, entry).await?;
                }
            }
        }
        if self.closing.load(Ordering::Acquire) || cell.cancel.is_cancelled() {
            bail!("Agent manager is shutting down");
        }
        self.persist_snapshot(&snapshot).await?;
        {
            let mut data = cell.data.lock().await;
            data.ready = true;
            data.snapshot = snapshot.clone();
        }
        let _ = self
            .events
            .send(json!({"type":"agent_state","agent":snapshot}));
        Ok(snapshot)
    }
    pub(super) async fn resume(
        &self,
        id: &str,
        overrides: Value,
        force_history: bool,
    ) -> Result<Value> {
        let lane = self.lane(id).await;
        let _lane = lane.lock().await;
        if let Ok(cell) = self.cell(id).await {
            let ready = cell.data.lock().await.ready;
            if ready && !force_history && overrides.as_object().is_none_or(|o| o.is_empty()) {
                return Ok(cell.data.lock().await.snapshot.clone());
            }
        }
        self.resume_unlocked(id, overrides, force_history, "resume")
            .await
    }
    pub(super) async fn reload(
        &self,
        id: &str,
        overrides: Value,
        force_history: bool,
    ) -> Result<Value> {
        let lane = self.lane(id).await;
        let _lane = lane.lock().await;
        self.resume_unlocked(id, overrides, force_history, "refresh")
            .await
    }
    async fn resume_unlocked(
        &self,
        id: &str,
        overrides: Value,
        force_history: bool,
        reason: &str,
    ) -> Result<Value> {
        if self.closing.load(Ordering::Acquire) {
            bail!("Agent manager is shutting down");
        }
        let mut record = self
            .agent_snapshot(id)
            .await?
            .ok_or_else(|| anyhow!("Agent not found: {id}"))?;
        let stored_provider = text(&record, "provider").unwrap_or("").to_owned();
        if self.app()?.providers.spec(&stored_provider).is_none() {
            bail!("Agent {id} references unavailable provider '{stored_provider}'");
        }
        let existing = { self.agents.read().await.get(id).cloned() };
        if let Some(cell) = existing {
            self.close_cell(id, &cell).await?;
        }
        let handle = record.get("persistence").filter(|v| v.is_object()).cloned();
        if handle.is_none() {
            // agent-loading.ts: without a provider handle this starts the agent's first
            // session, so stored attention is not carried over.
            record["requiresAttention"] = json!(false);
            record["attentionReason"] = Value::Null;
            record["attentionTimestamp"] = Value::Null;
            record["attention"] = json!({"requiresAttention":false});
        }
        let provider = handle
            .as_ref()
            .and_then(|h| text(h, "provider"))
            .or_else(|| text(&record, "provider"))
            .ok_or_else(|| anyhow!("Agent has no provider"))?
            .to_owned();
        let mut config = handle
            .as_ref()
            .and_then(|h| h.get("metadata"))
            .cloned()
            .unwrap_or(json!({}));
        merge(&mut config, &record["config"]);
        if let Some(mode) = record.get("lastModeId").filter(|v| !v.is_null()) {
            config["modeId"] = mode.clone();
        }
        merge(&mut config, &overrides);
        config["provider"] = json!(provider);
        if config.get("cwd").is_none() {
            config["cwd"] = record["cwd"].clone();
        }
        let history = record.get("archivedAt").is_some_and(|v| !v.is_null());
        let config = self.normalize_config_for(config, true, history).await?;
        record["config"] = config.clone();
        record["cwd"] = config["cwd"].clone();
        record["_historyOnly"] = json!(history);
        let app = self.app()?;
        let factory = app.providers.get(&provider)?;
        if !factory
            .is_available(json!({}), app.cancel.child_token())
            .await?
        {
            bail!("Provider '{provider}' is not available. Please ensure the CLI is installed.");
        }
        let mut env = app.config.environment();
        if let Some(workspace) = text(&record, "workspaceId") {
            env.extend(app.projects.teardown_environment(workspace).await?);
        }
        env.insert("PASEO_AGENT_ID".into(), id.into());
        env.insert("PASEO_AGENT_CWD".into(), required(&config, "cwd")?.into());
        let workspace = record.get("workspaceId").cloned();
        let record_labels = record.get("labels").cloned().unwrap_or(json!({}));
        let record_owner = record.get("owner").filter(|owner| !owner.is_null()).cloned();
        let config_internal = config["internal"] == true;
        let (cell, receiver) = self.make_cell(id, record, false).await?;
        *cell.task.lock().await = Some(self.start_events(id, &cell, receiver));
        let result = async {
            let context = self
                .open_context(
                    id,
                    &config,
                    workspace.as_ref(),
                    reason,
                    if history { "history" } else { "interactive" },
                    env,
                )
                .await?;
            let launch = self.launch_config(config)?;
            let session = if let Some(handle) = handle.as_ref() {
                factory
                    .resume_session(
                        handle.clone(),
                        launch,
                        context,
                        json!({"purpose":if history{"history"}else{"interactive"}}),
                        cell.sink.clone(),
                    )
                    .await?
            } else {
                let mut options = json!({"labels":record_labels,"workspaceId":workspace});
                if let Some(owner) = &record_owner {
                    options["owner"] = owner.clone();
                }
                if config_internal {
                    options["persistSession"] = json!(false);
                }
                factory
                    .create_session(launch, context, options, cell.sink.clone())
                    .await?
            };
            self.register_session(id, &cell, session, handle.clone(), handle.is_some())
                .await?;
            let needs_history = force_history || !cell.data.lock().await.history_primed;
            if needs_history {
                self.hydrate_history(id, &cell, force_history).await?;
            }
            Ok(cell.data.lock().await.snapshot.clone())
        }
        .await;
        if result.is_err() {
            self.discard_failed_registration(id, &cell).await;
        }
        result
    }
    pub(super) async fn import_session(&self, input: Value) -> Result<Value> {
        let id = uuid::Uuid::new_v4().to_string();
        let app = self.app()?;
        let config = self
            .normalize_config(
                json!({"provider":input["provider"],"cwd":input["cwd"]}),
                true,
            )
            .await?;
        let factory = app.providers.get(required(&config, "provider")?)?;
        let timestamp = now();
        let snapshot = json!({"id":id,"provider":config["provider"],"cwd":config["cwd"],"config":config,"createdAt":timestamp,"updatedAt":timestamp,"lastUserMessageAt":null,"workspaceId":input["workspaceId"],"labels":input.get("labels").cloned().unwrap_or(json!({})),"internal":false,"title":null,"archivedAt":null,"requiresAttention":false,"attentionReason":null,"attentionTimestamp":null});
        let (cell, receiver) = self.make_cell(&id, snapshot, true).await?;
        *cell.task.lock().await = Some(self.start_events(&id, &cell, receiver));
        let result = async {
            let mut env = app.config.environment();
            if let Some(workspace) = text(&input, "workspaceId") {
                env.extend(app.projects.teardown_environment(workspace).await?);
            }
            env.insert("PASEO_AGENT_ID".into(), id.clone());
            env.insert("PASEO_AGENT_CWD".into(), required(&config, "cwd")?.into());
            let context = self
                .open_context(
                    &id,
                    &config,
                    input.get("workspaceId"),
                    "import",
                    "interactive",
                    env,
                )
                .await?;
            let imported = factory
                .import_session(
                    input.clone(),
                    self.launch_config(config.clone())?,
                    config,
                    context,
                    cell.sink.clone(),
                )
                .await?;
            {
                let mut config = imported.config;
                strip_config_internal_servers(&mut config);
                object(&mut config).remove("daemonAppendSystemPrompt");
                let mut data = cell.data.lock().await;
                // resolveImportedAgentTitle: the first non-empty imported user message
                // titles the chat, preferring an explicit config title.
                let first_prompt = imported.timeline.iter().find_map(|entry| {
                    notifications::project_agent_messages(entry.get("item").unwrap_or(entry))
                        .into_iter()
                        .find_map(|item| {
                            (item["type"] == "user_message")
                                .then(|| item["text"].as_str().map(str::trim).map(str::to_owned))
                                .flatten()
                                .filter(|text| !text.is_empty())
                        })
                });
                data.snapshot["title"] = json!(first_prompt.and_then(|prompt| {
                    super::creation::title(text(&config, "title"), &prompt)
                }));
                data.snapshot["config"] = config;
            }
            for entry in imported.timeline {
                for item in
                    notifications::project_agent_messages(entry.get("item").unwrap_or(&entry))
                {
                    self.append_to_cell(&id, &cell, item, &entry).await?;
                }
            }
            let snapshot = self
                .register_session(
                    &id,
                    &cell,
                    imported.session,
                    Some(imported.persistence),
                    false,
                )
                .await?;
            for event in imported.provider_subagent_events {
                self.apply_event(&id, &cell, event, true).await?;
            }
            Ok(snapshot)
        }
        .await;
        if result.is_err() {
            self.discard_failed_registration(&id, &cell).await;
        }
        result
    }
    pub(super) async fn ensure_loaded(&self, id: &str) -> Result<Arc<AgentCell>> {
        if let Ok(cell) = self.cell(id).await {
            let ready = {
                let data = cell.data.lock().await;
                data.ready && data.session.is_some()
            };
            if ready {
                return Ok(cell);
            }
        }
        self.resume(id, json!({}), false).await?;
        self.cell(id).await
    }
    pub(super) async fn hydrate_history(
        &self,
        id: &str,
        cell: &Arc<AgentCell>,
        force: bool,
    ) -> Result<()> {
        self.hydrate_history_for(id, cell, force, None).await
    }
    pub(super) async fn hydrate_history_for(
        &self,
        id: &str,
        cell: &Arc<AgentCell>,
        force: bool,
        initiating_source: Option<&str>,
    ) -> Result<()> {
        let session = cell
            .data
            .lock()
            .await
            .session
            .clone()
            .ok_or_else(|| anyhow!("Agent has no session"))?;
        let (sink, mut receiver) = EventSink::channel(256, cell.cancel.child_token());
        let collect = async {
            let mut events = Vec::new();
            while let Some(event) = receiver.recv().await {
                events.push(event);
            }
            events
        };
        let (result, events) = tokio::join!(session.stream_history(sink), collect);
        result?;
        self.drain_events(cell).await?;
        let _commit = cell.commit.lock().await;
        let timeline = cell.timeline.clone();
        let path = self.timeline_path(id)?;
        let id_owned = id.to_owned();
        let events_for_timeline = events.clone();
        let epoch = tokio::task::spawn_blocking(move || {
            let mut current = timeline
                .lock()
                .map_err(|_| anyhow!("Timeline lock poisoned"))?;
            if !force && current.next_seq() > 1 {
                return Ok::<_, anyhow::Error>(current.epoch().to_owned());
            }
            let mut replacement = Timeline::new();
            for event in events_for_timeline {
                if event["type"] == "timeline" {
                    for item in notifications::project_agent_messages(&event["item"]) {
                        replacement.append(item, &event);
                    }
                }
            }
            replacement.persist(&path, &id_owned)?;
            let epoch = replacement.epoch().to_owned();
            *current = replacement;
            Ok(epoch)
        })
        .await??;
        for event in events {
            if event["type"] == "provider_subagent" {
                self.apply_subagent_event(id, cell, &event).await?;
            }
        }
        cell.data.lock().await.history_primed = true;
        let mut event = json!({"type":"timeline_replacement","agentId":id,"epoch":epoch});
        if let Some(source) = initiating_source {
            event["initiatingSourceId"] = json!(source);
        }
        let _ = self.events.send(event);
        Ok(())
    }
    pub(super) fn timeline_path(&self, id: &str) -> Result<PathBuf> {
        Ok(self
            .app()?
            .home
            .join("agent-timelines")
            .join(format!("{id}.json")))
    }
    pub(super) async fn append_to_cell(
        &self,
        id: &str,
        cell: &Arc<AgentCell>,
        item: Value,
        options: &Value,
    ) -> Result<Value> {
        let persistent = cell.data.lock().await.snapshot["internal"] != true;
        let timeline = cell.timeline.clone();
        let path = self.timeline_path(id)?;
        let id = id.to_owned();
        let options = options.clone();
        tokio::task::spawn_blocking(move || {
            let mut timeline = timeline
                .lock()
                .map_err(|_| anyhow!("Timeline lock poisoned"))?;
            if persistent {
                timeline.append_persisted(&path, &id, item, &options)
            } else {
                Ok(timeline.append(item, &options))
            }
        })
        .await?
    }
    pub(super) async fn apply_event(
        &self,
        id: &str,
        cell: &Arc<AgentCell>,
        mut event: Value,
        from_history: bool,
    ) -> Result<()> {
        if event["type"] == "timeline" {
            event["item"] = timeline::limit_item_content(event["item"].take());
        }
        if !from_history {
            let mut data = cell.data.lock().await;
            if data.staged_events.is_some() {
                let bytes = event_size(&event);
                if data
                    .staged_events
                    .as_ref()
                    .is_some_and(|events| events.len() >= 4096)
                    || data.staged_bytes.saturating_add(bytes) > 16 * 1024 * 1024
                {
                    bail!("Provider exceeded bounded pending-turn event buffer");
                }
                data.staged_bytes += bytes;
                data.staged_events.as_mut().unwrap().push(event);
                return Ok(());
            }
        }
        let _commit = cell.commit.lock().await;
        if event["type"] == "provider_subagent" {
            self.apply_subagent_event(id, cell, &event).await?;
            return Ok(());
        }
        let mut snapshot = cell.data.lock().await.snapshot.clone();
        let ready = cell.data.lock().await.ready;
        let previous_lifecycle = snapshot["lifecycle"].as_str().map(str::to_owned);
        let kind = required(&event, "type")?.to_owned();
        let mut turn = text(&event, "turnId").map(str::to_owned);
        if turn.is_none()
            && !from_history
            && matches!(
                kind.as_str(),
                "turn_started"
                    | "turn_completed"
                    | "turn_failed"
                    | "turn_canceled"
                    | "timeline"
                    | "permission_requested"
                    | "permission_resolved"
            )
        {
            turn = text(&snapshot, "activeTurnId")
                .map(str::to_owned)
                .or_else(|| {
                    (kind == "turn_started").then(|| format!("autonomous-{}", uuid::Uuid::new_v4()))
                });
            if let Some(turn) = &turn {
                event["turnId"] = json!(turn);
            }
        }
        let stale = turn.as_ref().is_some_and(|turn| {
            text(&snapshot, "activeTurnId").is_some_and(|active| active != turn)
        }) || if let Some(turn) = &turn {
            cell.data.lock().await.finalized.contains(turn)
        } else {
            false
        };
        if stale
            && matches!(
                kind.as_str(),
                "turn_started" | "turn_completed" | "turn_failed" | "turn_canceled"
            )
        {
            return Ok(());
        }
        let mut streams = Vec::new();
        let mut resolutions = Vec::new();
        let mut dispatch = true;
        let mut attention = None;
        match kind.as_str() {
            "thread_started" => {
                let session = cell.data.lock().await.session.clone();
                if let Some(handle) = session.and_then(|session| session.describe_persistence()) {
                    snapshot["persistence"] = with_cwd(handle, &snapshot["cwd"]);
                } else if snapshot["persistence"].is_null() {
                    snapshot["persistence"] = with_cwd(
                        json!({"provider":snapshot["provider"],"sessionId":event["sessionId"]}),
                        &snapshot["cwd"],
                    );
                }
            }
            "turn_started" => {
                // agent-manager.ts onStreamTurnStarted: the manager already published the
                // accepted foreground start, so the provider's duplicate is suppressed.
                let foreground = turn.is_some()
                    && text(&snapshot, "activeForegroundTurnId") == turn.as_deref();
                if !from_history && !stale && !foreground {
                    snapshot["lifecycle"] = json!("running");
                    snapshot["activeTurnId"] = json!(turn);
                    snapshot["activeTurn"] = json!({"turnId":turn,"startedAt":now()});
                    snapshot["lastError"] = Value::Null;
                } else {
                    dispatch = false;
                }
            }
            "turn_completed" | "turn_canceled" | "turn_failed" => {
                if stale {
                    dispatch = false;
                } else if !from_history {
                    let replacing = {
                        let mut data = cell.data.lock().await;
                        if let Some(turn) = &turn {
                            if !data.finalized.contains(turn) {
                                data.finalized.push_back(turn.clone());
                                while data.finalized.len() > 4096 {
                                    data.finalized.pop_front();
                                }
                            }
                        }
                        data.pending_replacement
                    };
                    // finalizeForegroundTurn: a replacing prompt holds the agent busy unless the
                    // settled turn failed.
                    snapshot["lifecycle"] = json!(if kind == "turn_failed" {
                        "error"
                    } else if replacing {
                        "running"
                    } else {
                        "idle"
                    });
                    snapshot["activeTurnId"] = Value::Null;
                    snapshot["activeForegroundTurnId"] = Value::Null;
                    snapshot["activeTurn"] = Value::Null;
                    if kind != "turn_completed" {
                        if let Some(pending) = snapshot["pendingPermissions"].as_array() {
                            for request in pending {
                                let request_id = required(request, "id")?;
                                let resolution = json!({"type":"permission_resolved","provider":event["provider"],"requestId":request_id,"resolution":{"behavior":"deny","message":if kind=="turn_failed"{"Turn failed"}else{"Interrupted"}}});
                                cell.data
                                    .lock()
                                    .await
                                    .resolutions
                                    .insert(request_id.into(), resolution.clone());
                                resolutions.push(resolution);
                            }
                        }
                        snapshot["pendingPermissions"] = json!([]);
                    }
                    snapshot["lastTurnOutcome"] = json!(kind);
                    if let Some(usage) = event.get("usage") {
                        merge(&mut snapshot["lastUsage"], usage);
                    }
                    if kind == "turn_failed" {
                        snapshot["lastError"] = event["error"].clone();
                        // appendSystemErrorTimelineMessage: the failure is an assistant row,
                        // skipped when it repeats the latest item.
                        let item = json!({"type":"assistant_message","text":format!("{SYSTEM_ERROR_PREFIX} {}", turn_failed_message(&event))});
                        let repeated = cell
                            .timeline
                            .lock()
                            .map_err(|_| anyhow!("Timeline lock poisoned"))?
                            .last_item()
                            .is_some_and(|last| {
                                last["type"] == "assistant_message" && last["text"] == item["text"]
                            });
                        if !repeated {
                            let row = self
                                .append_to_cell(id, cell, item.clone(), &json!({}))
                                .await?;
                            streams.push((
                                json!({"type":"timeline","provider":event["provider"],"item":item}),
                                row,
                            ));
                        }
                    } else {
                        object(&mut snapshot).remove("lastError");
                    }
                }
            }
            "usage_updated" => snapshot["lastUsage"] = event["usage"].clone(),
            "mode_changed" => {
                snapshot["currentModeId"] = event["currentModeId"].clone();
                snapshot["availableModes"] = event["availableModes"].clone();
                if !event["currentModeId"].is_null() {
                    snapshot["config"]["modeId"] = event["currentModeId"].clone();
                }
                if snapshot["runtimeInfo"].is_object() {
                    snapshot["runtimeInfo"]["modeId"] = event["currentModeId"].clone();
                }
                dispatch = false;
            }
            "model_changed" => {
                snapshot["runtimeInfo"] = event["runtimeInfo"].clone();
                if let Some(mode) = event["runtimeInfo"].get("modeId") {
                    snapshot["currentModeId"] = mode.clone();
                    if !mode.is_null() {
                        snapshot["config"]["modeId"] = mode.clone();
                    }
                }
                dispatch = false;
            }
            "thinking_option_changed" => {
                if event["thinkingOptionId"].is_null() {
                    object(&mut snapshot["config"]).remove("thinkingOptionId");
                } else {
                    snapshot["config"]["thinkingOptionId"] = event["thinkingOptionId"].clone();
                }
                if snapshot["runtimeInfo"].is_object() {
                    snapshot["runtimeInfo"]["thinkingOptionId"] = event["thinkingOptionId"].clone();
                }
                dispatch = false;
            }
            "permission_requested" => {
                let request = &event["request"];
                let request_id = required(request, "id")?;
                let mut data = cell.data.lock().await;
                if data.resolutions.remove(request_id).is_none() {
                    let permissions = snapshot["pendingPermissions"]
                        .as_array_mut()
                        .ok_or_else(|| anyhow!("Invalid permissions state"))?;
                    // onStreamPermissionRequested: only the first outstanding request raises
                    // attention, and it is a broadcast, not the agent's unread state.
                    if permissions.is_empty() {
                        attention = Some("permission");
                    }
                    if let Some(previous) = permissions
                        .iter_mut()
                        .find(|value| text(value, "id") == Some(request_id))
                    {
                        *previous = request.clone();
                    } else {
                        permissions.push(request.clone());
                    }
                } else {
                    dispatch = false;
                }
            }
            "permission_resolved" => {
                let request_id = required(&event, "requestId")?;
                if let Some(permissions) = snapshot["pendingPermissions"].as_array_mut() {
                    let before = permissions.len();
                    permissions.retain(|value| text(value, "id") != Some(request_id));
                    if before == permissions.len() {
                        cell.data
                            .lock()
                            .await
                            .resolutions
                            .insert(request_id.into(), event.clone());
                        dispatch = false;
                    }
                }
                if snapshot["pendingPermissions"]
                    .as_array()
                    .is_some_and(Vec::is_empty)
                    && snapshot["attentionReason"] == "permission"
                {
                    snapshot["requiresAttention"] = json!(false);
                    snapshot["attentionReason"] = Value::Null;
                    snapshot["attentionTimestamp"] = Value::Null;
                    snapshot["attention"] = json!({"requiresAttention":false});
                }
            }
            "attention_required" => {
                if event.get("timestamp").is_none_or(|value| !value.is_string()) {
                    event["timestamp"] = json!(now());
                }
            }
            "timeline" => {
                dispatch = false;
                for mut item in notifications::project_agent_messages(&event["item"]) {
                    let mut options = event.clone();
                    if item["type"] == "user_message" {
                        if let Some(message_id) = text(&item, "clientMessageId").map(str::to_owned)
                        {
                            let exists = cell
                                .timeline
                                .lock()
                                .map_err(|_| anyhow!("Timeline lock poisoned"))?
                                .submitted_user_message(&message_id)
                                .is_some();
                            if exists {
                                if let Some(provider_id) =
                                    text(&item, "messageId").map(str::to_owned)
                                {
                                    let timeline = cell.timeline.clone();
                                    let path = self.timeline_path(id)?;
                                    let agent_id = id.to_owned();
                                    let internal = snapshot["internal"] == true;
                                    tokio::task::spawn_blocking(move || {
                                        let mut timeline = timeline
                                            .lock()
                                            .map_err(|_| anyhow!("Timeline lock poisoned"))?;
                                        if internal {
                                            timeline.enrich_submitted_user_message(
                                                &message_id,
                                                &provider_id,
                                            );
                                            Ok::<_, anyhow::Error>(())
                                        } else {
                                            timeline
                                                .enrich_submitted_persisted(
                                                    &path,
                                                    &agent_id,
                                                    &message_id,
                                                    &provider_id,
                                                )
                                                .map(|_| ())
                                        }
                                    })
                                    .await??;
                                }
                                continue;
                            }
                            if let Some(provider_id) = item.get("messageId") {
                                options["providerMessageId"] = provider_id.clone();
                            }
                            item["messageId"] = json!(message_id);
                        }
                    }
                    if from_history {
                        // History replay rows keep only their source timestamp.
                        options = match event.get("timestamp").filter(|value| value.is_string()) {
                            Some(timestamp) => json!({"timestamp":timestamp}),
                            None => json!({}),
                        };
                    }
                    let row = self
                        .append_to_cell(id, cell, item.clone(), &options)
                        .await?;
                    if item["type"] == "user_message" && !from_history {
                        snapshot["lastUserMessageAt"] = json!(now());
                    }
                    let mut stream = event.clone();
                    stream["item"] = item;
                    streams.push((stream, row));
                }
            }
            _ => {}
        }
        if attention.is_none() && !from_history {
            attention = transition_attention(previous_lifecycle.as_deref(), &mut snapshot);
        }
        let attention_timestamp = if attention == Some("permission") {
            now()
        } else {
            text(&snapshot, "attentionTimestamp")
                .map(str::to_owned)
                .unwrap_or_else(now)
        };
        if !from_history {
            snapshot["updatedAt"] = json!(now());
        }
        snapshot["status"] = snapshot["lifecycle"].clone();
        if ready {
            self.persist_snapshot(&snapshot).await?;
        }
        cell.data.lock().await.snapshot = snapshot.clone();
        if ready && !from_history {
            let _ = self
                .events
                .send(json!({"type":"agent_state","agent":snapshot}));
            for (stream, row) in streams {
                self.publish_stream(id, cell, stream, Some(row)).await?;
            }
            for resolution in resolutions {
                self.publish_stream(id, cell, resolution, None).await?;
            }
            if let Some(reason) = attention.filter(|_| snapshot["internal"] != true) {
                self.publish_stream(id,cell,json!({"type":"attention_required","provider":snapshot["provider"],"reason":reason,"timestamp":attention_timestamp}),None).await?;
            }
            if dispatch {
                self.publish_stream(id, cell, event, None).await?;
            }
        }
        Ok(())
    }
    async fn apply_subagent_event(
        &self,
        id: &str,
        cell: &Arc<AgentCell>,
        event: &Value,
    ) -> Result<()> {
        let checkpoint = self
            .subagent_store
            .lock()
            .map_err(|_| anyhow!("Subagent lock poisoned"))?
            .checkpoint(id);
        let mut candidate = SubagentStore::default();
        candidate.restore_parent(&checkpoint)?;
        let output = candidate.apply(id, required(event, "provider")?, &event["event"])?;
        let next = candidate.checkpoint(id);
        let snapshot = cell.data.lock().await.snapshot.clone();
        if snapshot["internal"] != true {
            storage::atomic_json(
                &self
                    .app()?
                    .home
                    .join("agent-subagents")
                    .join(format!("{id}.json")),
                &next,
            )
            .await?;
        }
        self.subagent_store
            .lock()
            .map_err(|_| anyhow!("Subagent lock poisoned"))?
            .restore_parent(&next)?;
        if cell.data.lock().await.ready {
            let _ = self
                .events
                .send(json!({"type":"provider_subagent","event":output}));
            if snapshot["internal"] != true {
                let payload = match text(&output, "type") {
                    Some("upsert") => json!({"kind":"upsert","subagent":output["subagent"]}),
                    Some("timeline") => {
                        json!({"kind":"timeline","parentAgentId":id,"subagentId":output["subagentId"],"provider":output["provider"],"item":output["row"]["item"],"timestamp":output["row"]["timestamp"],"seq":output["row"]["seq"],"epoch":output["epoch"]})
                    }
                    _ => {
                        json!({"kind":"remove","parentAgentId":id,"subagentId":output["subagentId"]})
                    }
                };
                self.app()?
                    .transport
                    .publish_event(
                        json!({"type":"agent.provider_subagents.update","payload":payload}),
                    )
                    .await?;
            }
        }
        Ok(())
    }
    pub(super) async fn publish_stream(
        &self,
        id: &str,
        cell: &Arc<AgentCell>,
        mut event: Value,
        row: Option<Value>,
    ) -> Result<()> {
        if event["type"] == "timeline" {
            event["item"] = timeline::limit_item_content(event["item"].take());
        }
        let mut output = json!({"type":"agent_stream","agentId":id,"event":event});
        if let Some(row) = row {
            output["seq"] = row["seq"].clone();
            output["timestamp"] = row["timestamp"].clone();
            output["epoch"] = json!(
                cell.timeline
                    .lock()
                    .map_err(|_| anyhow!("Timeline lock poisoned"))?
                    .epoch()
            );
        }
        let _ = self.events.send(output.clone());
        let snapshot = cell.data.lock().await.snapshot.clone();
        if snapshot["internal"] == true {
            return Ok(());
        }
        let app = self.app()?;
        if let Some((name, payload)) = hook_stream(&snapshot, &event) {
            let _ = app.plugins.emit(name, payload).await;
        }
        if event["type"] == "permission_requested"
            && app.speech.is_active_for_agent(id).await
            && crate::speech::is_voice_permission_allowed(&event["request"])
        {
            let session = cell.data.lock().await.session.clone();
            let sink = cell.sink.clone();
            let id = id.to_owned();
            let request_id = required(&event["request"], "id")?.to_owned();
            if let Some(session) = session {
                tokio::spawn(async move {
                    match session
                        .respond_to_permission(&request_id, json!({"behavior":"allow"}))
                        .await
                    {
                        Ok(_) => {
                            let _=sink.emit(json!({"type":"permission_resolved","provider":session.provider(),"requestId":request_id,"resolution":{"behavior":"allow"}})).await;
                        }
                        Err(error) => {
                            tracing::warn!(agent_id=%id,%error,"Failed to auto-allow voice permission")
                        }
                    }
                });
            }
        }
        if let Some(serialized) =
            serialize_stream_event(&event).filter(|_| event["type"] != "attention_required")
        {
            let mut payload = json!({"agentId":id,"event":serialized,"timestamp":output.get("timestamp").cloned().unwrap_or(json!(now()))});
            for key in ["seq", "epoch"] {
                if let Some(value) = output.get(key) {
                    payload[key] = value.clone();
                }
            }
            app.transport
                .publish_event(json!({"type":"agent_stream","payload":payload}))
                .await?;
        }
        match text(&event,"type"){
            Some("permission_requested")=>app.transport.publish_event(json!({"type":"agent_permission_request","payload":{"agentId":id,"request":event["request"]}})).await?,
            Some("permission_resolved")=>app.transport.publish_event(json!({"type":"agent_permission_resolved","payload":{"agentId":id,"requestId":event["requestId"],"resolution":event["resolution"]}})).await?,
            Some("attention_required")=>self.notifications.attention(&snapshot,required(&event,"reason")?,required(&event,"timestamp")?).await?,
            _=>{}
        }
        Ok(())
    }
    pub(super) async fn close_cell(&self, id: &str, cell: &Arc<AgentCell>) -> Result<()> {
        let _mutation = cell.mutation.lock().await;
        let session = cell.data.lock().await.session.clone();
        let event_error = if let Some(session) = session {
            session.close().await?;
            self.drain_events(cell).await.err()
        } else {
            None
        };
        let children = self
            .subagent_store
            .lock()
            .map_err(|_| anyhow!("Subagent lock poisoned"))?
            .list(id);
        {
            let _commit = cell.commit.lock().await;
            for child in children
                .into_iter()
                .filter(|child| child["status"] == "running")
            {
                self.apply_subagent_event(id,cell,&json!({"provider":child["provider"],"event":{"type":"upsert","id":child["id"],"status":"canceled"}})).await?;
            }
        }
        let mut snapshot = cell.data.lock().await.snapshot.clone();
        snapshot["lifecycle"] = json!("closed");
        snapshot["status"] = json!("closed");
        snapshot["activeTurnId"] = Value::Null;
        snapshot["activeForegroundTurnId"] = Value::Null;
        snapshot["activeTurn"] = Value::Null;
        snapshot["pendingPermissions"] = json!([]);
        snapshot["_live"] = json!(false);
        self.commit_snapshot(cell, snapshot.clone()).await?;
        cell.data.lock().await.session = None;
        if snapshot["internal"] != true {
            let _ = self
                .app()?
                .plugins
                .emit("agent.closed", json!({"agent":hook_agent(&snapshot)}))
                .await;
        }
        cell.cancel.cancel();
        if let Some(task) = cell.task.lock().await.take() {
            let _ = task.await;
        }
        self.agents.write().await.remove(id);
        self.app()?.mcp.revoke_agent(id).await;
        if let Some(error) = event_error {
            Err(error)
        } else {
            Ok(())
        }
    }
    pub async fn shutdown(&self) -> Result<()> {
        self.notifications.shutdown().await?;
        self.message_delivery.shutdown().await?;
        self.closing.store(true, Ordering::Release);
        let agents = self
            .agents
            .read()
            .await
            .iter()
            .map(|(id, cell)| (id.clone(), cell.clone()))
            .collect::<Vec<_>>();
        let mut failure = None;
        for (id, cell) in agents {
            if let Err(error) = self.close_cell(&id, &cell).await {
                failure.get_or_insert(error);
            }
        }
        if let Some(error) = failure {
            Err(error)
        } else {
            Ok(())
        }
    }
}
pub(super) fn hook_agent(snapshot: &Value) -> Value {
    json!({"id":snapshot["id"],"workspaceId":snapshot["workspaceId"],"parentAgentId":snapshot["labels"]["paseo.parent-agent-id"],"provider":snapshot["provider"],"cwd":snapshot["cwd"],"title":snapshot["title"]})
}
fn hook_stream(snapshot: &Value, event: &Value) -> Option<(&'static str, Value)> {
    let agent = hook_agent(snapshot);
    Some(match text(event, "type")? {
        "turn_started" => (
            "agent.turn_started",
            json!({"agent":agent,"turnId":event["turnId"]}),
        ),
        "turn_completed" => (
            "agent.turn_ended",
            json!({"agent":agent,"turnId":event["turnId"],"timeline":[],"outcome":{"kind":"completed"}}),
        ),
        "turn_failed" => {
            let mut outcome = json!({"kind":"failed","error":{"message":event["error"]}});
            if let Some(code) = event.get("code") {
                outcome["error"]["code"] = code.clone();
            }
            (
                "agent.turn_ended",
                json!({"agent":agent,"turnId":event["turnId"],"timeline":[],"outcome":outcome}),
            )
        }
        "turn_canceled" => {
            let mut outcome = json!({"kind":"canceled"});
            if let Some(reason) = event.get("reason") {
                outcome["reason"] = reason.clone();
            }
            (
                "agent.turn_ended",
                json!({"agent":agent,"turnId":event["turnId"],"timeline":[],"outcome":outcome}),
            )
        }
        "permission_requested" => (
            "agent.permission_requested",
            json!({"agent":agent,"request":event["request"]}),
        ),
        "permission_resolved" => (
            "agent.permission_resolved",
            json!({"agent":agent,"requestId":event["requestId"],"resolution":event["resolution"]}),
        ),
        _ => return None,
    })
}

fn event_size(value: &Value) -> usize {
    match value {
        Value::String(value) => value.len(),
        Value::Array(values) => values.iter().map(event_size).sum(),
        Value::Object(values) => values
            .iter()
            .map(|(key, value)| key.len() + event_size(value))
            .sum(),
        _ => 16,
    }
}

/// agent-manager.ts SYSTEM_ERROR_PREFIX.
const SYSTEM_ERROR_PREFIX: &str = "[System Error]";

/// agent-manager.ts formatTurnFailedMessage.
pub(super) fn turn_failed_message(event: &Value) -> String {
    let base = event["error"].as_str().unwrap_or("").trim();
    let mut parts = vec![if base.is_empty() {
        "Provider run failed"
    } else {
        base
    }];
    let code = event["code"].as_str().map(str::trim).unwrap_or("");
    let code = (!code.is_empty()).then(|| format!("code: {code}"));
    if let Some(code) = code.as_deref() {
        parts.push(code);
    }
    let diagnostic = event["diagnostic"].as_str().map(str::trim).unwrap_or("");
    if !diagnostic.is_empty() && diagnostic != base {
        parts.push(diagnostic);
    }
    parts.join("\n\n")
}

/// agent-manager.ts checkAndSetAttention: attention is an edge-triggered unread signal,
/// raised once on running -> idle (finished) or on entering error, and never replacing
/// an attention the user has not cleared yet.
pub(super) fn transition_attention(
    previous: Option<&str>,
    snapshot: &mut Value,
) -> Option<&'static str> {
    if snapshot["internal"] == true || snapshot["requiresAttention"] == true {
        return None;
    }
    let current = text(snapshot, "lifecycle");
    let reason = if previous == Some("running") && current == Some("idle") {
        "finished"
    } else if previous != Some("error") && current == Some("error") {
        "error"
    } else {
        return None;
    };
    let timestamp = now();
    snapshot["requiresAttention"] = json!(true);
    snapshot["attentionReason"] = json!(reason);
    snapshot["attentionTimestamp"] = json!(timestamp);
    snapshot["attention"] =
        json!({"requiresAttention":true,"attentionReason":reason,"attentionTimestamp":timestamp});
    Some(reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_failure_message_includes_code_and_distinct_diagnostic() {
        assert_eq!(
            turn_failed_message(&json!({"error":"  boom  ","code":" E42 ","diagnostic":"boom"})),
            "boom\n\ncode: E42"
        );
        assert_eq!(
            turn_failed_message(&json!({"error":"","diagnostic":"stderr tail"})),
            "Provider run failed\n\nstderr tail"
        );
    }

    #[test]
    fn attention_is_edge_triggered_and_keeps_unread_state() {
        let mut finished = json!({"lifecycle":"idle","requiresAttention":false});
        assert_eq!(
            transition_attention(Some("running"), &mut finished),
            Some("finished")
        );
        assert_eq!(finished["attention"]["attentionReason"], "finished");

        // An unread finish is not replaced by a later failure.
        let mut failed = finished.clone();
        failed["lifecycle"] = json!("error");
        assert_eq!(transition_attention(Some("running"), &mut failed), None);
        assert_eq!(failed["attentionReason"], "finished");

        let mut errored = json!({"lifecycle":"error","requiresAttention":false});
        assert_eq!(
            transition_attention(Some("running"), &mut errored),
            Some("error")
        );
        let mut still_error = json!({"lifecycle":"error","requiresAttention":false});
        assert_eq!(transition_attention(Some("error"), &mut still_error), None);

        let mut started = json!({"lifecycle":"running","requiresAttention":false});
        assert_eq!(transition_attention(Some("idle"), &mut started), None);

        let mut internal = json!({"lifecycle":"idle","internal":true});
        assert_eq!(transition_attention(Some("running"), &mut internal), None);
    }
}
