use super::*;

const EVENTS: &[&str] = &[
    "agent.created",
    "agent.closed",
    "agent.turn_started",
    "agent.turn_ended",
    "agent.permission_requested",
    "agent.permission_resolved",
    "agent.archived",
    "workspace.created",
    "workspace.archived",
];
/// Top-level keys of `WorkspaceCreateRequestSchema` without `type`/`requestId`.
/// The source's wire parse strips every other key before the hook sees it.
pub(crate) const WORKSPACE_CREATE_FIELDS: &[&str] = &[
    "workspaceId",
    "agent",
    "subscribe",
    "idempotencyKey",
    "title",
    "firstAgentContext",
    "internal",
    "background",
    "callerAgentId",
    "source",
];

impl PluginService {
    pub async fn before(&self, name: &str, mut input: Value) -> Result<Value> {
        validate_before(name, &input)?;
        let mut workers: Vec<Arc<PluginWorker>> = self
            .loaded
            .read()
            .unwrap()
            .values()
            .filter_map(|plugin| plugin.worker.as_ref())
            .filter(|worker| {
                worker.is_live()
                    && worker.metadata.read().unwrap()["hooks"]["before"]
                        .as_array()
                        .is_some_and(|hooks| hooks.iter().any(|hook| hook == name))
            })
            .cloned()
            .collect();
        workers.sort_by(|left, right| crate::json::locale_compare(&left.id, &right.id));
        for worker in workers {
            let next = worker
                .request(
                    json!({"type":"hook","kind":"before","name":name,"input":input}),
                    self.stopping.clone(),
                )
                .await
                .map_err(|error| anyhow!("Plugin {} before {name} failed: {error}", worker.id))?;
            validate_before_result(name, &input, &next)
                .map_err(|error| anyhow!("Plugin {} before {name} failed: {error}", worker.id))?;
            input = next;
        }
        Ok(input)
    }
    pub async fn emit(&self, name: &str, event: Value) -> Result<()> {
        if !EVENTS.contains(&name) {
            bail!("Unknown lifecycle event: {name}");
        }
        let workers: Vec<Arc<PluginWorker>> = self
            .loaded
            .read()
            .unwrap()
            .values()
            .filter_map(|plugin| plugin.worker.as_ref())
            .filter(|worker| {
                worker.is_live()
                    && worker.metadata.read().unwrap()["hooks"]["events"]
                        .as_array()
                        .is_some_and(|hooks| hooks.iter().any(|hook| hook == name))
            })
            .cloned()
            .collect();
        for worker in workers {
            if self.stopping.is_cancelled() {
                return Ok(());
            }
            let permit = self
                .event_slots
                .clone()
                .try_acquire_owned()
                .context("Plugin lifecycle event queue is full")?;
            let input = event.clone();
            let name = name.to_owned();
            let cancel = self.stopping.clone();
            let app = self.app.clone();
            let task = tokio::spawn(async move {
                let _permit = permit;
                if let Err(error) = worker
                    .request(
                        json!({"type":"hook","kind":"event","name":name,"input":input}),
                        cancel,
                    )
                    .await
                {
                    if let Some(app) = app.upgrade() {
                        app.plugins.append_log(
                            &worker.id,
                            "stderr",
                            &format!("Lifecycle hook {name} failed: {error}"),
                        );
                    }
                    tracing::warn!(plugin_id = %worker.id, hook = %name, %error, "Plugin lifecycle hook failed");
                }
            });
            let mut events = self.events.lock().unwrap();
            events.retain(|task| !task.is_finished());
            events.push(task);
        }
        Ok(())
    }
    pub async fn drain_events(&self) -> Result<()> {
        let events = std::mem::take(&mut *self.events.lock().unwrap());
        for task in events {
            if let Err(error) = task.await {
                tracing::warn!(%error, "Plugin lifecycle event task failed");
            }
        }
        Ok(())
    }
}
fn validate_before(name: &str, input: &Value) -> Result<()> {
    let object = input
        .as_object()
        .context("Plugin before request must be an object")?;
    match name {
        "agent.session_open" => {
            const FIELDS: &[&str] = &[
                "agentId",
                "workspaceId",
                "provider",
                "cwd",
                "reason",
                "purpose",
                "env",
            ];
            if object.keys().any(|key| !FIELDS.contains(&key.as_str())) {
                bail!("Invalid agent.session_open field");
            }
            for key in ["agentId", "provider", "cwd"] {
                if !input[key].is_string() {
                    bail!("agent.session_open requires {key}");
                }
            }
            if !object.contains_key("workspaceId")
                || !(input["workspaceId"].is_null() || input["workspaceId"].is_string())
            {
                bail!("Invalid agent.session_open workspaceId");
            }
            if !["create", "resume", "refresh", "import"]
                .contains(&input["reason"].as_str().unwrap_or(""))
            {
                bail!("Invalid agent.session_open reason");
            }
            if !["interactive", "history"].contains(&input["purpose"].as_str().unwrap_or("")) {
                bail!("Invalid agent.session_open purpose");
            }
            if !input["env"]
                .as_object()
                .is_some_and(|env| env.values().all(Value::is_string))
            {
                bail!("Invalid agent.session_open environment");
            }
        }
        "agent.create" => {
            if object
                .keys()
                .any(|key| !["config", "env"].contains(&key.as_str()))
                || !input["config"].is_object()
            {
                bail!("Invalid agent.create hook request");
            }
            if let Some(env) = object.get("env") {
                if !env
                    .as_object()
                    .is_some_and(|env| env.values().all(Value::is_string))
                {
                    bail!("Invalid agent.create environment");
                }
            }
        }
        "workspace.create" => {
            // `WorkspaceCreateRequestSchema.omit({type, requestId}).strict()`:
            // unknown top-level keys and a missing/unknown source kind are rejected.
            if object
                .keys()
                .any(|key| !WORKSPACE_CREATE_FIELDS.contains(&key.as_str()))
            {
                bail!("Invalid workspace.create hook request");
            }
            let optional_string = |key: &str| object.get(key).is_none_or(Value::is_string);
            let optional_bool = |key: &str| object.get(key).is_none_or(Value::is_boolean);
            if !["title", "callerAgentId", "workspaceId", "idempotencyKey"]
                .into_iter()
                .all(optional_string)
                || !["subscribe", "internal", "background"]
                    .into_iter()
                    .all(optional_bool)
                || !matches!(
                    input["source"]["kind"].as_str(),
                    Some("directory" | "worktree")
                )
                || (input["source"]["kind"] == "directory" && !input["source"]["path"].is_string())
            {
                bail!("Invalid workspace.create hook request");
            }
        }
        _ => bail!("Unknown before hook: {name}"),
    }
    Ok(())
}
fn validate_before_result(name: &str, previous: &Value, next: &Value) -> Result<()> {
    validate_before(name, next)?;
    if name == "agent.session_open"
        && [
            "agentId",
            "workspaceId",
            "provider",
            "cwd",
            "reason",
            "purpose",
        ]
        .iter()
        .any(|key| previous[*key] != next[*key])
    {
        bail!("agent.session_open hooks can only change env");
    }
    if name == "agent.create" && previous["config"]["cwd"] != next["config"]["cwd"] {
        bail!("agent.create hooks cannot change the workspace directory");
    }
    Ok(())
}
