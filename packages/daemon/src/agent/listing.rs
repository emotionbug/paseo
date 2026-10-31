use super::*;

fn timestamp(value: &Value) -> i64 {
    value
        .as_str()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.timestamp_millis())
        .unwrap_or(0)
}
fn activity_time(value: &Value) -> i64 {
    [
        "updatedAt",
        "lastUserMessageAt",
        "attentionTimestamp",
        "archivedAt",
        "createdAt",
    ]
    .iter()
    .map(|key| timestamp(&value[*key]))
    .max()
    .unwrap_or(0)
}
fn status_order(value: &Value) -> u8 {
    match text(value, "status") {
        Some("running") => 0,
        Some("initializing") => 1,
        Some("idle") => 2,
        Some("error") => 3,
        Some("closed") => 4,
        _ => 255,
    }
}
fn list_item(value: &Value) -> Value {
    let snapshot = if value.get("config").is_some() {
        snapshot_payload(value)
    } else {
        value.clone()
    };
    let mut item = json!({"shortId":text(&snapshot,"id").unwrap_or("").chars().take(7).collect::<String>(),"model":snapshot["runtimeInfo"].get("model").filter(|v|!v.is_null()).unwrap_or(&snapshot["model"]),"archivedAt":snapshot["archivedAt"],"requiresAttention":snapshot["requiresAttention"]==true,"attentionReason":snapshot["attentionReason"],"attentionTimestamp":snapshot["attentionTimestamp"]});
    for key in [
        "id",
        "title",
        "provider",
        "thinkingOptionId",
        "effectiveThinkingOptionId",
        "status",
        "cwd",
        "createdAt",
        "updatedAt",
        "lastUserMessageAt",
        "labels",
    ] {
        item[key] = snapshot.get(key).cloned().unwrap_or(Value::Null);
    }
    if snapshot["providerUnavailable"] == true {
        item["providerUnavailable"] = json!(true);
    }
    item
}
pub fn select_agent_list_items(agents: Vec<Value>, options: &Value) -> Result<Vec<Value>> {
    let since = chrono::Utc::now().timestamp_millis()
        - (options["sinceHours"].as_f64().unwrap_or(48.0) * 3_600_000.0) as i64;
    let statuses = options["statuses"].as_array().filter(|v| !v.is_empty());
    let cwd = text(options, "cwd").map(Path::new);
    let mut items = agents
        .iter()
        .map(list_item)
        .filter(|item| {
            cwd.is_none_or(|root| {
                text(item, "cwd").is_some_and(|path| Path::new(path).starts_with(root))
            })
        })
        .filter(|item| statuses.is_none_or(|statuses| statuses.contains(&item["status"])))
        .filter(|item| !item["archivedAt"].is_string() || activity_time(item) >= since)
        .collect::<Vec<_>>();
    items.sort_by(|a, b| {
        (b["requiresAttention"] == true)
            .cmp(&(a["requiresAttention"] == true))
            .then_with(|| status_order(a).cmp(&status_order(b)))
            .then_with(|| activity_time(b).cmp(&activity_time(a)))
    });
    let limit = options["limit"].as_i64().unwrap_or(50);
    let length = if limit < 0 {
        items.len().saturating_sub(limit.unsigned_abs() as usize)
    } else {
        limit as usize
    };
    items.truncate(length);
    Ok(items)
}
impl AgentManager {
    pub async fn list_agents(&self, actor: &Value, mut options: Value) -> Result<Vec<Value>> {
        let requested = text(&options, "cwd")
            .map(str::trim)
            .filter(|cwd| !cwd.is_empty())
            .map(str::to_owned);
        if let Some(cwd) = requested {
            let home = self.app()?.user_home.clone();
            let expanded = if cwd == "~" {
                home
            } else if let Some(suffix) = cwd.strip_prefix("~/") {
                home.join(suffix)
            } else {
                PathBuf::from(&cwd)
            };
            options["cwd"] = json!(expanded);
        } else if actor["kind"] == "agent" {
            options["cwd"] = json!(self.cwd(required(actor, "agentId")?).await?);
        } else if let Some(options) = options.as_object_mut() {
            options.remove("cwd");
        }
        let app = self.app()?;
        let mut selected = Vec::new();
        for snapshot in self.list_snapshots().await? {
            if snapshot["internal"] == true
                || snapshot["_live"] != true && snapshot["_providerAvailable"] == false
            {
                continue;
            }
            if options["includeArchived"] != true && snapshot["archivedAt"].is_string() {
                continue;
            }
            let workspace = if let Some(id) = text(&snapshot, "workspaceId") {
                app.projects.workspace(id).await?
            } else {
                None
            };
            if options["includeBackground"] != true
                && workspace.as_ref().is_some_and(|w| w["background"] == true)
            {
                continue;
            }
            selected.push(snapshot);
        }
        select_agent_list_items(selected, &options)
    }
    async fn timeline_cell(&self, id: &str) -> Result<Arc<AgentCell>> {
        if let Ok(cell) = self.cell(id).await {
            return Ok(cell);
        }
        self.ensure_loaded(id).await
    }
    pub async fn timeline(&self, id: &str, options: &Value) -> Result<Value> {
        let cell = self.timeline_cell(id).await?;
        let timeline = cell
            .timeline
            .lock()
            .map_err(|_| anyhow!("Timeline lock poisoned"))?;
        Ok(timeline.fetch(options))
    }
    pub async fn timeline_items(&self, id: &str) -> Result<Vec<Value>> {
        if let Ok(cell) = self.cell(id).await {
            return Ok(cell
                .timeline
                .lock()
                .map_err(|_| anyhow!("Timeline lock poisoned"))?
                .items());
        }
        let path = self.timeline_path(id)?;
        let id_owned = id.to_owned();
        let saved = tokio::task::spawn_blocking(move || Timeline::load(&path, &id_owned)).await??;
        if saved.next_seq() > 1 {
            return Ok(saved.items());
        }
        let cell = self.timeline_cell(id).await?;
        let items = cell
            .timeline
            .lock()
            .map_err(|_| anyhow!("Timeline lock poisoned"))?
            .items();
        Ok(items)
    }
    pub async fn timeline_search(&self, id: &str, options: &Value) -> Result<Value> {
        let cell = self.timeline_cell(id).await?;
        let result = cell
            .timeline
            .lock()
            .map_err(|_| anyhow!("Timeline lock poisoned"))?
            .search(options);
        result
    }
    pub async fn timeline_prompts(&self, id: &str) -> Result<Value> {
        let cell = self.timeline_cell(id).await?;
        let result = cell
            .timeline
            .lock()
            .map_err(|_| anyhow!("Timeline lock poisoned"))?
            .prompts();
        Ok(result)
    }
    pub async fn append_timeline(&self, id: &str, item: Value) -> Result<Value> {
        let cell = self.ensure_loaded(id).await?;
        let provider = cell.data.lock().await.snapshot["provider"].clone();
        let row = self
            .append_to_cell(id, &cell, item.clone(), &json!({}))
            .await?;
        self.publish_stream(
            id,
            &cell,
            json!({"type":"timeline","provider":provider,"item":item}),
            Some(row.clone()),
        )
        .await?;
        let epoch = cell
            .timeline
            .lock()
            .map_err(|_| anyhow!("Timeline lock poisoned"))?
            .epoch()
            .to_owned();
        Ok(json!({"seq":row["seq"],"epoch":epoch}))
    }
    pub async fn last_assistant_message(&self, id: &str) -> Result<Option<String>> {
        if let Ok(cell) = self.cell(id).await {
            let message = cell
                .timeline
                .lock()
                .map_err(|_| anyhow!("Timeline lock poisoned"))?
                .last_assistant_message()
                .map(str::to_owned);
            return Ok(message);
        }
        let path = self.timeline_path(id)?;
        let agent_id = id.to_owned();
        let saved = tokio::task::spawn_blocking(move || Timeline::load(&path, &agent_id)).await??;
        if saved.next_seq() > 1 {
            return Ok(saved.last_assistant_message().map(str::to_owned));
        }
        let cell = self.timeline_cell(id).await?;
        let message = cell
            .timeline
            .lock()
            .map_err(|_| anyhow!("Timeline lock poisoned"))?
            .last_assistant_message()
            .map(str::to_owned);
        Ok(message)
    }
    pub async fn activity(&self, id: &str, options: &Value) -> Result<Vec<Value>> {
        crate::activity::curate(&self.timeline_items(id).await?, options)
    }
    pub async fn activity_summary(&self, id: &str, limit: Option<f64>) -> Result<Value> {
        crate::activity::summarize(&self.timeline_items(id).await?, limit)
    }
    pub async fn fork_context(&self, id: &str, options: &Value) -> Result<Value> {
        let cell = self.timeline_cell(id).await?;
        let snapshot = cell.data.lock().await.snapshot.clone();
        let timeline = cell
            .timeline
            .lock()
            .map_err(|_| anyhow!("Timeline lock poisoned"))?;
        crate::activity::build_fork_context(
            &json!({"rows":timeline.rows(),"agentTitle":snapshot["title"],"cwd":snapshot["cwd"],"boundaryMessageId":options["boundaryMessageId"],"cursorBoundary":options.get("boundaryCursor").filter(|v|!v.is_null()).map(|cursor|json!({"cursor":cursor,"timelineEpoch":timeline.epoch()}))}),
        )
    }
    pub async fn subagents(&self, id: &str) -> Result<Vec<Value>> {
        let snapshot = self
            .agent_snapshot(id)
            .await?
            .ok_or_else(|| anyhow!("Unknown parent agent"))?;
        if snapshot["internal"] == true {
            bail!("Unknown parent agent");
        }
        if !snapshot["archivedAt"].is_string() {
            self.ensure_loaded(id).await?;
        }
        Ok(self
            .subagent_store
            .lock()
            .map_err(|_| anyhow!("Subagent lock poisoned"))?
            .list(id))
    }
    pub async fn subagent_timeline(
        &self,
        parent: &str,
        id: &str,
        options: &Value,
    ) -> Result<Value> {
        self.subagents(parent).await?;
        let store = self
            .subagent_store
            .lock()
            .map_err(|_| anyhow!("Subagent lock poisoned"))?;
        let mut page = store.fetch_timeline(parent, id, options)?;
        if let Some(descriptor) = store.get(parent, id) {
            page["provider"] = descriptor["provider"].clone();
        }
        Ok(page)
    }
    pub async fn list_provider_subagent_activity(&self) -> Result<Vec<Value>> {
        let public: HashSet<String> = self
            .list_snapshots()
            .await?
            .iter()
            .filter(|snapshot| snapshot["internal"] != true)
            .filter_map(|s| text(s, "id").map(str::to_owned))
            .collect();
        Ok(self
            .subagent_store
            .lock()
            .map_err(|_| anyhow!("Subagent lock poisoned"))?
            .list_all()
            .iter()
            .filter(|s| text(s, "parentAgentId").is_some_and(|id| public.contains(id)))
            .cloned()
            .collect())
    }
    pub async fn workspace_last_user_message_times(&self) -> Result<BTreeMap<String, String>> {
        let mut times = BTreeMap::new();
        for record in self.list_snapshots().await? {
            if record["internal"] == true {
                continue;
            }
            if let (Some(workspace), Some(time)) = (
                text(&record, "workspaceId"),
                text(&record, "lastUserMessageAt"),
            ) {
                let latest = times
                    .entry(workspace.to_owned())
                    .or_insert_with(|| time.to_owned());
                if time > latest.as_str() {
                    *latest = time.to_owned();
                }
            }
        }
        Ok(times)
    }
    pub async fn commands(&self, id: Option<&str>, config: &Value) -> Result<Vec<Value>> {
        if let Some(id) = id {
            if let Some(snapshot) = self.agent_snapshot(id).await? {
                if !snapshot["archivedAt"].is_string() {
                    let cell = self.ensure_loaded(id).await?;
                    let session = cell
                        .data
                        .lock()
                        .await
                        .session
                        .clone()
                        .ok_or_else(|| anyhow!("Agent has no session"))?;
                    return session.list_commands().await;
                }
            }
        }
        self.list_draft_commands(config).await
    }
    pub async fn list_draft_commands(&self, config: &Value) -> Result<Vec<Value>> {
        let config = self.normalize_config(config.clone(), false).await?;
        if text(&config, "model").is_none() {
            return Ok(Vec::new());
        }
        let app = self.app()?;
        let provider = app.providers.get(required(&config, "provider")?)?;
        if !provider
            .is_available(json!({}), app.cancel.child_token())
            .await?
        {
            bail!("Provider is unavailable");
        }
        provider.list_commands(&config).await
    }
    pub async fn features(&self, id: Option<&str>, config: &Value) -> Result<Vec<Value>> {
        if let Some(id) = id {
            if let Some(session) = self.session(id).await? {
                return Ok(session.features().to_vec());
            }
        }
        self.list_draft_features(config).await
    }
    pub async fn list_draft_features(&self, config: &Value) -> Result<Vec<Value>> {
        let config = self.normalize_config(config.clone(), false).await?;
        let app = self.app()?;
        let provider_id = required(&config, "provider")?;
        let provider = app.providers.get(provider_id)?;
        if !provider
            .is_available(json!({}), app.cancel.child_token())
            .await?
        {
            bail!("Provider '{provider_id}' is not available. Please ensure the CLI is installed.");
        }
        provider.list_features(&config).await
    }
    pub async fn import_sessions(&self, options: &Value) -> Result<Value> {
        let app = self.app()?;
        let filter = options["providers"].as_array();
        let limit = options["limit"].as_u64().unwrap_or(20) as usize;
        let stored = self.list_snapshots().await?;
        let mut imported = HashSet::new();
        let since = match options.get("since") {
            Some(value) if !value.is_null() && value != "" => {
                let value = value.as_str().ok_or_else(|| {
                    anyhow!("invalid_since: Invalid recent provider sessions since")
                })?;
                Some(super::date_parse::parse_date(value)?.ok_or_else(|| {
                    anyhow!("invalid_since: Invalid recent provider sessions since")
                })?)
            }
            _ => None,
        };
        let search = text(options, "query")
            .map(str::trim)
            .filter(|q| !q.is_empty())
            .map(str::to_lowercase);
        let mut imported_sessions = HashSet::new();
        for record in &stored {
            if record["archivedAt"].is_string() {
                continue;
            }
            if let Some(provider) = text(record, "provider") {
                if filter.is_some_and(|filter| {
                    !filter.iter().any(|value| value.as_str() == Some(provider))
                }) {
                    continue;
                }
                if let Some(handle) = text(&record["persistence"], "sessionId") {
                    imported_sessions.insert((provider.to_owned(), handle.to_owned()));
                }
                for key in ["sessionId", "nativeHandle"] {
                    if let Some(handle) = text(&record["persistence"], key) {
                        imported.insert((provider.to_owned(), handle.to_owned()));
                    }
                }
            }
        }
        let listing_limit = if search.is_some() {
            500
        } else {
            limit.saturating_add(imported_sessions.len())
        };
        let mut query = json!({"limit":listing_limit});
        if let Some(cwd) = options.get("cwd") {
            query["cwd"] = cwd.clone();
        }
        if let Some(search) = &search {
            query["query"] = json!(search);
            query["scanLimit"] = json!(500);
        }
        let mut pending = Vec::new();
        for spec in app.providers.specs() {
            if filter
                .is_some_and(|filter| !filter.iter().any(|value| value.as_str() == Some(&spec.id)))
            {
                continue;
            }
            let provider = app.providers.get(&spec.id)?;
            if provider.capabilities()["supportsSessionListing"] != true {
                continue;
            }
            let query = query.clone();
            let label = app
                .providers
                .definition(&spec.id)
                .and_then(|definition| definition.get("label").cloned())
                .unwrap_or(json!(spec.id));
            let handle = tokio::spawn(async move {
                tokio::time::timeout(
                    Duration::from_secs(90),
                    provider.list_importable_sessions(query),
                )
                .await
            });
            pending.push((spec.id, label, handle));
        }
        let mut sessions = Vec::new();
        let mut errors = Vec::new();
        for (provider, label, handle) in pending {
            let found = match handle.await {
                Ok(Ok(Ok(found))) => found,
                Ok(Ok(Err(error))) => {
                    errors.push(json!({"provider":provider,"message":error.to_string()}));
                    continue;
                }
                Ok(Err(_)) => {
                    errors.push(json!({"provider":provider,"message":format!("Timed out listing importable sessions for provider '{provider}' after 90000ms")}));
                    continue;
                }
                Err(error) => {
                    errors.push(json!({"provider":provider,"message":error.to_string()}));
                    continue;
                }
            };
            for mut session in found {
                if let Some(query) = &search {
                    let cwd = text(&session, "cwd").unwrap_or("").replace('\\', "/");
                    let basename = cwd.rsplit('/').find(|part| !part.is_empty()).unwrap_or("");
                    if !["title", "firstPromptPreview", "lastPromptPreview"]
                        .iter()
                        .filter_map(|key| text(&session, key))
                        .chain(std::iter::once(basename))
                        .any(|value| value.to_lowercase().contains(query))
                    {
                        continue;
                    }
                }
                session["provider"] = json!(provider);
                session["providerId"] = json!(provider);
                session["providerLabel"] = label.clone();
                sessions.push(session);
            }
        }
        sessions
            .sort_by(|a, b| timestamp(&b["lastActivityAt"]).cmp(&timestamp(&a["lastActivityAt"])));
        sessions.truncate(listing_limit);
        let mut entries = Vec::new();
        let mut filtered = 0;
        for session in sessions {
            if let Some(cwd) = text(options, "cwd") {
                if !same_cwd(
                    Path::new(cwd),
                    Path::new(text(&session, "cwd").unwrap_or("")),
                )
                .await
                {
                    continue;
                }
            }
            if since.is_some_and(|since| timestamp(&session["lastActivityAt"]) < since) {
                continue;
            }
            if text(&session, "firstPromptPreview").is_some_and(|prompt| {
                prompt
                    .trim_start()
                    .starts_with("Generate metadata for a coding agent based on the user prompt.")
            }) {
                continue;
            }
            if imported.contains(&(
                required(&session, "provider")?.to_owned(),
                required(&session, "providerHandleId")?.to_owned(),
            )) {
                filtered += 1;
                continue;
            }
            entries.push(session);
        }
        entries.truncate(limit);
        Ok(
            json!({"entries":entries,"sessions":entries,"filteredAlreadyImportedCount":filtered,"providerErrors":errors}),
        )
    }
    pub async fn import_provider_session(&self, actor: &Value, mut input: Value) -> Result<Value> {
        let provider = text(&input, "providerId")
            .or_else(|| text(&input, "provider"))
            .ok_or_else(|| anyhow!("Import requires providerId and providerHandleId"))?
            .to_owned();
        let handle = text(&input, "providerHandleId")
            .or_else(|| text(&input, "sessionId"))
            .ok_or_else(|| anyhow!("Import requires providerId and providerHandleId"))?
            .to_owned();
        let cwd = required(&input, "cwd")?.to_owned();
        let matching = self
            .storage()?
            .list_by_provider_session(&provider, &handle)
            .await?;
        let key = matching
            .first()
            .and_then(|record| text(record, "id"))
            .map(|id| format!("import\0agent\0{id}"))
            .unwrap_or_else(|| format!("import\0handle\0{provider}\0{handle}"));
        let lane = self.lane(&key).await;
        let _lane = lane.lock().await;
        let records = self
            .storage()?
            .list_by_provider_session(&provider, &handle)
            .await?;
        if records
            .iter()
            .any(|record| !record["archivedAt"].is_string())
        {
            bail!("Provider session is already imported: {handle}");
        }
        let app = self.app()?;
        let requested = text(&input, "workspaceId").map(str::to_owned);
        let placement_cwd = PathBuf::from(&cwd);
        app.projects.run_in_import_workspace(&placement_cwd,requested.as_deref(),|workspace|async move{
            let workspace_id=required(&workspace,"workspaceId")?.to_owned();input["provider"]=json!(provider);input["providerHandleId"]=json!(handle);input["workspaceId"]=json!(workspace_id);
            let snapshot=if let Some(record)=records.first(){
                if !same_cwd(Path::new(&cwd),Path::new(required(record,"cwd")?)).await{bail!("Provider session cwd does not match import cwd: {handle}");}
                let id=required(record,"id")?;let mut labels=input.get("labels").cloned().unwrap_or(json!({}));if record["labels"].get("paseo.parent-agent-id").is_some()||labels.get("paseo.parent-agent-id").is_some(){let parent=text(&labels,"paseo.parent-agent-id").map(str::trim).filter(|parent|!parent.is_empty()).map(str::to_owned);labels["paseo.parent-agent-id"]=json!(parent);}
                self.unarchive(id,&json!({"workspaceId":workspace_id,"labels":labels})).await?;
                match self.resume(id,json!({}),false).await{Ok(snapshot)=>snapshot,Err(error)=>{self.storage()?.upsert(record.clone()).await?;return Err(error);}}
            }else{self.import_session(input).await?};
            let id=required(&snapshot,"id")?;let _=actor;Ok(json!({"timelineSize":self.timeline_items(id).await?.len(),"snapshot":snapshot}))
        }).await
    }
}
async fn same_cwd(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (
        tokio::fs::canonicalize(left).await,
        tokio::fs::canonicalize(right).await,
    ) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}
