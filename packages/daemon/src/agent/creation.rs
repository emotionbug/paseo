use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use serde_json::{Map, Value, json};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use super::{
    AgentManager,
    receipts::{CreationAdmission, CreationGuard, CreationHooks, CreationInput},
    storage::KeyLocks,
    wire::snapshot_payload,
};
use crate::providers::prompt;

#[derive(Default)]
pub(super) struct CreationState {
    executions: KeyLocks,
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct AgentCreationError {
    pub code: String,
    pub message: String,
    pub details: Value,
}

pub fn error_details(error: &anyhow::Error) -> Option<Value> {
    if let Some(error) =
        error.downcast_ref::<crate::providers::options::ProviderOptionsValidationError>()
    {
        return Some(
            json!({"code":"provider_options_invalid","provider":error.provider,"issues":error.issues,"message":error.to_string()}),
        );
    }
    if let Some(error) =
        error.downcast_ref::<crate::providers::options::ToolPolicyUnsupportedError>()
    {
        return Some(
            json!({"code":"tool_policy_unsupported","provider":error.provider,"message":error.to_string()}),
        );
    }
    if let Some(error) = error.downcast_ref::<crate::projects::RequestError>() {
        return Some(json!({"code":error.code,"message":error.message}));
    }
    error.downcast_ref::<AgentCreationError>().map(|error| {
        let mut value = json!({"code":error.code,"message":error.message});
        if let Some(details) = error.details.as_object() {
            value.as_object_mut().unwrap().extend(details.clone());
        }
        value
    })
}

impl AgentManager {
    /// The internal command accepts both source session and MCP command forms.
    /// Public wire-only restrictions are applied before resolving placement.
    pub async fn create(&self, actor: &Value, input: Value) -> Result<Value> {
        self.create_command(actor, input, None, None).await
    }

    async fn create_command(
        &self,
        actor: &Value,
        input: Value,
        mut receipt: Option<&mut CreationGuard>,
        cancel: Option<&CancellationToken>,
    ) -> Result<Value> {
        let app = self.app()?;
        let is_mcp = input["kind"] == "mcp";
        let public = input
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|kind| matches!(kind, "agent.create.request" | "create_agent_request"));
        if public {
            validate_public_intent(&input)?;
        }
        let actor_agent = (actor["kind"] == "agent")
            .then(|| text(actor, "agentId"))
            .flatten();
        let caller_id = text(&input, "callerAgentId").or(actor_agent);
        if let (Some(bound), Some(requested)) = (actor_agent, caller_id) {
            if bound != requested {
                bail!("Caller agent does not match authenticated agent");
            }
        }
        let parent = if let Some(id) = caller_id {
            self.authorize(actor, id).await?;
            let parent = self
                .agent_snapshot(id)
                .await?
                .filter(|parent| parent["_live"] != false);
            Some(match parent {
                Some(parent) => parent,
                None if is_mcp => bail!("Parent agent {id} not found"),
                None => bail!("Caller agent {id} not found"),
            })
        } else {
            None
        };
        let mut config = input
            .get("config")
            .filter(|v| v.is_object())
            .cloned()
            .unwrap_or_else(|| json!({}));
        if is_mcp {
            let provider = text(&input, "provider")
                .context("provider is required")?
                .trim();
            if provider.is_empty() {
                bail!("provider is required");
            }
            let (provider, model) = provider
                .split_once('/')
                .map_or((provider, None), |(p, m)| (p.trim(), Some(m.trim())));
            if provider.is_empty() || model.is_some_and(str::is_empty) {
                bail!("provider must be provider/model, for example codex/gpt-5.4");
            }
            config["provider"] = json!(provider);
            if let Some(model) = model {
                config["model"] = json!(model);
            }
            copy_as(&input, "thinking", &mut config, "thinkingOptionId");
            copy_as(&input, "internal", &mut config, "internal");
            if config.get("title").is_none_or(Value::is_null) {
                copy_as(&input, "title", &mut config, "title");
            }
        }
        let initial_prompt = text(&input, "initialPrompt").unwrap_or("").trim();
        let initial_title = title(text(&config, "title"), initial_prompt);
        if let Some(title) = initial_title.as_ref() {
            config["title"] = json!(title);
        }
        let first_context = input.get("firstAgentContext").cloned().unwrap_or_else(|| {
            let mut value = json!({"prompt":initial_prompt});
            copy_as(&input, "attachments", &mut value, "attachments");
            value
        });
        let requested_cwd = if is_mcp {
            text(&input, "cwd").or_else(|| text(&config, "cwd"))
        } else {
            text(&config, "cwd")
        }
        .map(str::to_owned);
        let requested = requested_cwd.as_deref();
        let mut cwd = if is_mcp {
            if let Some(parent) = parent.as_ref() {
                if let Some(locked) =
                    text(&input["callerContext"], "lockedCwd").filter(|s| !s.trim().is_empty())
                {
                    expand_path(locked.trim(), &app.user_home)?
                } else if input["callerContext"]["allowCustomCwd"] == false
                    || requested.is_none_or(|s| s.trim().is_empty())
                {
                    PathBuf::from(required(parent, "cwd")?)
                } else {
                    resolve_from(
                        Path::new(required(parent, "cwd")?),
                        requested.unwrap().trim(),
                        &app.user_home,
                    )?
                }
            } else {
                expand_path(requested.unwrap_or("."), &app.user_home)?
            }
        } else {
            expand_path(requested.context("Agent cwd is required")?, &app.user_home)?
        };
        let mut labels = Map::new();
        if is_mcp {
            merge_object(
                &mut labels,
                input["callerContext"].get("childAgentDefaultLabels"),
            );
        }
        merge_object(&mut labels, input.get("labels"));
        if input["detached"] == true {
            labels.remove("paseo.parent-agent-id");
        } else if let Some(id) = caller_id {
            labels.insert("paseo.parent-agent-id".into(), json!(id));
        }
        let caller_workspace = parent.as_ref().and_then(|p| text(p, "workspaceId"));
        let mut created_worktree = None;
        let mut created_agent = None;
        let operation = async {
            require_active(cancel)?;
            let mut name_created_directory = false;
            let workspace_id = if (is_mcp || public) && wants_worktree(&input["worktree"]) {
                let worktree = &input["worktree"];
                if public {
                    require_directory(&cwd).await?;
                }
                if text(worktree,"worktreeName").is_some() && !["baseBranch","refName","action","githubPrNumber","mode"].iter().any(|key| worktree.get(*key).is_some()) {
                    bail!("baseBranch is required when creating a worktree");
                }
                let mut request = json!({"source":worktree_source(worktree,&cwd),"firstAgentContext":first_context});
                if let Some(id) = caller_id { request["callerAgentId"] = json!(id); }
                copy_as(&input,"background",&mut request,"background");
                let workspace = app.projects.create_workspace(&request).await?;
                cwd = PathBuf::from(required(&workspace,"cwd")?);
                let id = required(&workspace,"workspaceId")?.to_owned();
                created_worktree = Some(workspace.get("createdWorktree").cloned().context("Worktree creation did not return its owned resource metadata")?);
                id
            } else if let Some(id) = text(&input,"workspaceId") {
                let workspace = app.projects.workspace(id).await?.filter(|w| w.get("archivedAt").is_none_or(Value::is_null)).with_context(|| format!("Workspace {id} not found"))?;
                if !is_mcp || (requested.is_none() && parent.is_none()) { cwd = PathBuf::from(required(&workspace,"cwd")?); }
                id.to_owned()
            } else if let Some(parent) = parent.as_ref() {
                let id = text(parent,"workspaceId").with_context(|| format!("Caller agent {} has no workspace",caller_id.unwrap_or_default()))?;
                cwd = PathBuf::from(required(parent,"cwd")?);
                id.to_owned()
            } else {
                require_directory(&cwd).await?;
                let workspace = if public && input.get("background").is_some() {
                    app.projects.create_workspace(&json!({"source":{"kind":"directory","path":cwd},"background":input["background"],"firstAgentContext":first_context})).await?
                } else {
                    let workspace = app.projects.ensure_workspace_for_create(&cwd,&first_context,caller_workspace).await?;
                    name_created_directory = true;
                    workspace
                };
                required(&workspace,"workspaceId")?.to_owned()
            };
            require_directory(&cwd).await?;
            config["cwd"] = json!(cwd);
            let requested_mode = if is_mcp { input.get("mode").or_else(|| config.get("modeId")) } else { config.get("modeId") }.cloned();
            let features = if is_mcp { input.get("features").or_else(|| config.get("featureValues")) } else { config.get("featureValues") }.cloned();
            let resolved = self.resolve_creation_config(&config, requested_mode, features, if is_mcp { parent.as_ref() } else { None }, input["unattended"] == true).await?;
            config.as_object_mut().unwrap().remove("modeId");
            merge_value(&mut config,&resolved);
            let mut options = json!({"workspaceId":workspace_id,"labels":labels});
            for key in ["env","owner","persistSession"] { copy_as(&input,key,&mut options,key); }
            if let Some(owner) = input.get("owner").filter(|owner| !owner.is_null()) {
                if actor["kind"] != "system" && actor["kind"] != "owner" && actor != owner {
                    bail!("Agent owner does not match authenticated owner");
                }
            }
            if config["internal"] == true { options["persistSession"] = json!(false); }
            if !is_mcp {
                options["initialPrompt"] = json!(initial_prompt);
                options["initialTitle"] = json!(initial_title);
            }
            require_active(cancel)?;
            let snapshot = self.create_session(config.take(),text(&input,"agentId").map(str::to_owned),options).await?;
            let id = required(&snapshot,"id")?.to_owned();
            created_agent = Some(id.clone());
            let prompt = if is_mcp { json!(initial_prompt) } else { prompt::build(initial_prompt,array(&input,"images"),array(&input,"attachments")) };
            let has_prompt = prompt.as_str().is_some_and(|s| !s.is_empty()) || prompt.as_array().is_some_and(|blocks| !blocks.is_empty());
            if let Some(receipt) = receipt.as_deref_mut() { receipt.agent_ready(snapshot_payload(&snapshot),has_prompt).await?; }
            require_active(cancel)?;
            let notification = if is_mcp && input["notifyOnFinish"] == true && has_prompt {
                if let Some(caller) = caller_id {
                    Some(self.register_finish_subscription(json!({"childAgentId":id,"callerAgentId":caller,"requireParentOwnership":input["detached"] != true}),true).await?)
                } else { None }
            } else { None };
            let archive_events = (input["autoArchive"] == true).then(|| self.subscribe_events());
            let mut live_snapshot = snapshot.clone();
            let mut started = false;
            let mut prompt_error = Value::Null;
            if has_prompt {
                let mut send = json!({"prompt":prompt,"clearPendingPermissions":true,"notifyOnFinish":false});
                let mut run_options = json!({});
                for key in ["outputSchema","clientMessageId"] { copy_as(&input,key,&mut run_options,key); }
                send["runOptions"] = run_options;
                if let Some(source) = input.get("source") { send["source"] = source.clone(); }
                else if let Some(parent) = caller_id { send["source"] = json!({"kind":"agent-message","agentId":parent}); }
                let failure = if is_mcp { text(&input,"promptFailure").unwrap_or("log") } else { "return-error" };
                match self.send_immediate(actor,&id,send).await {
                    Ok(_) => {
                        started = true;
                        live_snapshot = self.agent_snapshot(&id).await?.unwrap_or_else(|| snapshot.clone());
                    }
                    Err(error) if failure == "throw" => {
                        if let Some(notification) = notification { notification.reject().await?; }
                        return Err(error);
                    }
                    Err(error) if failure == "return-error" => {
                        prompt_error = json!(error.to_string());
                        live_snapshot = self.agent_snapshot(&id).await?.unwrap_or_else(|| snapshot.clone());
                    }
                    Err(error) => tracing::error!(agent_id = %id, ?error, "Failed to run initial prompt"),
                }
            }
            if let Some(notification) = notification { notification.accept().await?; }
            if let Some(events) = archive_events {
                self.archive_after_creation_turn(id.clone(),created_worktree.clone(),events)?;
            }
            require_active(cancel)?;
            // Directory workspaces minted for this agent are named from the first prompt even
            // when that prompt fails to start; the prompt text, not the turn, seeds the name.
            if name_created_directory && !initial_prompt.is_empty() {
                app.projects.schedule_directory_auto_name(&workspace_id,&first_context).await?;
            }
            let mut result = json!({"snapshot":snapshot,"liveSnapshot":live_snapshot,"initialPromptStarted":started,"initialPromptError":prompt_error});
            if let Some(worktree) = created_worktree.as_ref() { result["createdWorktree"] = worktree.clone(); }
            Ok::<_,anyhow::Error>(result)
        }.await;
        if operation.is_err() && created_agent.is_none() && cancel.is_none() {
            if let Some(worktree) = created_worktree
                .as_ref()
                .filter(|worktree| worktree["created"] == true)
            {
                if let Some(id) = text(&worktree["workspace"], "workspaceId") {
                    if let Err(error) = app.projects.archive_workspace(actor, id).await {
                        tracing::warn!(?error, "Failed to clean up newly created worktree");
                    }
                }
            }
        }
        // Hub failures, unlike ordinary first-turn failures, revoke only resources
        // created by this invocation. Replayed executions never enter this branch.
        if operation.is_err() && cancel.is_some() {
            self.cleanup_creation(actor, created_agent.as_deref(), created_worktree.as_ref())
                .await?;
        }
        operation
    }

    async fn resolve_creation_config(
        &self,
        config: &Value,
        requested_mode: Option<Value>,
        features: Option<Value>,
        parent: Option<&Value>,
        unattended: bool,
    ) -> Result<Value> {
        let app = self.app()?;
        let provider_name = required(config, "provider")?;
        let mut request = json!({"provider":provider_name,"cwd":config["cwd"],"unattended":unattended,"parent":null});
        if let Some(mode) = requested_mode {
            request["requestedMode"] = mode;
        }
        if let Some(features) = features {
            request["featureValues"] = features;
        }
        if let Some(parent) = parent {
            let parent_config = parent.get("config").unwrap_or(parent);
            let parent_provider_name = text(parent, "provider")
                .or_else(|| text(parent_config, "provider"))
                .context("Parent provider is missing")?;
            let modes = parent
                .get("availableModes")
                .filter(|modes| !modes.is_null())
                .cloned()
                .or_else(|| {
                    app.providers
                        .definition(parent_provider_name)
                        .and_then(|definition| definition.get("modes").cloned())
                })
                .unwrap_or_else(|| json!([]));
            let policy = json!({"modeId":parent["currentModeId"],"config":parent_config,"features":parent["features"],"availableModes":modes});
            let inherited_unattended = app
                .providers
                .is_create_config_unattended(parent_provider_name, &policy)?;
            request["parent"] = json!({"provider":parent_provider_name,"modeId":parent["currentModeId"],"isUnattended":inherited_unattended});
            request["unattended"] = json!(unattended || inherited_unattended);
        }
        app.providers.resolve_create_config(&request).await
    }

    pub async fn hub_validate(&self, input: &Value) -> Result<Value> {
        let app = self.app()?;
        let name = required(input, "provider")?;
        if app.providers.definition(name).is_none() {
            return Ok(
                json!([{"path":["provider"],"message":format!("Provider '{name}' is not configured")}]),
            );
        }
        let catalog = app.providers.catalog(name, None, true).await?;
        if catalog["enabled"] == false {
            return Ok(
                json!([{"path":["provider"],"message":format!("Provider '{name}' is disabled")}]),
            );
        }
        if catalog["status"] != "ready" {
            let message = if catalog["status"] == "error" {
                text(&catalog, "error")
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("Provider '{name}' is not available"))
            } else {
                format!("Provider '{name}' is not available")
            };
            return Ok(json!([{"path":["provider"],"message":message}]));
        }
        let models = || {
            array(&catalog, "models")
                .iter()
                .filter(|model| model["isSelectable"] != false)
        };
        let requested_model = text(input, "model").filter(|model| !model.is_empty());
        let selected = match requested_model {
            Some(requested) => models().find(|model| {
                text(model, "id") == Some(requested)
                    || array(model, "aliases")
                        .iter()
                        .any(|alias| alias.as_str() == Some(requested))
            }),
            None => models()
                .find(|model| model["isDefault"] == true)
                .or_else(|| models().next()),
        };
        let mut issues = Vec::new();
        if let Some(model) = requested_model.filter(|_| selected.is_none()) {
            issues.push(issue(
                "model",
                format!("Model '{model}' is not available for provider '{name}'"),
            ));
        }
        if let Some(mode) = text(input, "modeId").filter(|mode| {
            !mode.is_empty()
                && !array(&catalog, "modes")
                    .iter()
                    .any(|candidate| text(candidate, "id") == Some(*mode))
        }) {
            issues.push(issue(
                "modeId",
                format!("Mode '{mode}' is not available for provider '{name}'"),
            ));
        }
        if let Some(thinking) = text(input, "thinkingOptionId").filter(|option| {
            !option.is_empty()
                && !selected.is_some_and(|model| {
                array(model, "thinkingOptions")
                    .iter()
                    .any(|candidate| text(candidate, "id") == Some(*option))
            })
        }) {
            issues.push(issue(
                "thinkingOptionId",
                format!("Thinking option '{thinking}' is not available for provider '{name}'"),
            ));
        }
        Ok(Value::Array(issues))
    }

    pub async fn find_owned_execution(
        &self,
        daemon_id: &str,
        execution_id: &str,
    ) -> Result<Option<Value>> {
        let key = serde_json::to_string(&(daemon_id, execution_id))?;
        let _lock = self.creation_state.executions.lock(&key).await;
        self.owned_execution(
            &json!({"kind":"daemon","daemonId":daemon_id,"executionId":execution_id}),
        )
        .await
    }

    async fn owned_execution(&self, owner: &Value) -> Result<Option<Value>> {
        let Some(record) = self.storage()?.find_by_daemon_execution(owner).await? else {
            return Ok(None);
        };
        let id = required(&record, "id")?;
        if text(&record, "workspaceId").is_none() {
            bail!("Hub execution agent {id} has no workspaceId");
        }
        Ok(Some(self.agent_snapshot(id).await?.unwrap_or(record)))
    }

    pub async fn hub_create(
        &self,
        input: &Value,
        owner: &Value,
        cancel: CancellationToken,
    ) -> Result<Value> {
        for key in ["executionId", "prompt", "cwd"] {
            if required(input, key)?.trim().is_empty() {
                bail!("Hub agent {key} cannot be blank");
            }
        }
        if !Path::new(required(input, "cwd")?).is_absolute() {
            bail!("Hub agent cwd must be absolute");
        }
        let daemon_id = required(owner, "daemonId")?;
        let execution_id = required(owner, "executionId")?;
        if owner["kind"] != "daemon" || text(input, "executionId") != Some(execution_id) {
            bail!("Hub execution owner does not match request");
        }
        let key = serde_json::to_string(&(daemon_id, execution_id))?;
        let _lock = self.creation_state.executions.lock(&key).await;
        require_active(Some(&cancel))?;
        if let Some(agent) = self.owned_execution(owner).await? {
            require_active(Some(&cancel))?;
            let mut projected = snapshot_payload(&agent);
            if agent["_live"] == false {
                projected["status"] = json!("closed");
            }
            return Ok(json!({"executionId":execution_id,"agent":projected,"created":false}));
        }
        if input["mcpServers"].get("paseo").is_some() {
            bail!("Hub execution MCP server name \"paseo\" is reserved by the daemon");
        }
        for grant in array(&input["toolPolicy"], "preapproved") {
            let server = required(grant, "server")?;
            if input["mcpServers"].get(server).is_none() {
                bail!(
                    "Hub tool preapproval '{}.{}' requires MCP server '{}' in the same create request",
                    server,
                    required(grant, "tool")?,
                    server
                );
            }
        }
        let mut config = json!({});
        for field in ["model", "mcpServers", "providerOptions", "toolPolicy"] {
            copy_as(input, field, &mut config, field);
        }
        let mut command = json!({"kind":"mcp","provider":input["provider"],"cwd":input["cwd"],"title":input["prompt"],"initialPrompt":input["prompt"],"promptFailure":"throw","notifyOnFinish":false,"owner":owner,"config":config});
        for (from, to) in [
            ("modeId", "mode"),
            ("thinkingOptionId", "thinking"),
            ("featureValues", "features"),
            ("env", "env"),
            ("worktree", "worktree"),
        ] {
            copy_as(input, from, &mut command, to);
        }
        let result = self
            .create_command(owner, command, None, Some(&cancel))
            .await?;
        let mut owned = json!({"executionId":execution_id,"agent":snapshot_payload(&result["liveSnapshot"]),"created":true});
        copy_as(&result, "createdWorktree", &mut owned, "createdWorktree");
        Ok(owned)
    }

    pub async fn hub_cleanup_created(&self, created: &Value, owner: &Value) -> Result<()> {
        if created["created"] != true {
            return Ok(());
        }
        let id = required(&created["agent"], "id")?;
        let key = serde_json::to_string(&(
            required(owner, "daemonId")?,
            required(owner, "executionId")?,
        ))?;
        let _lock = self.creation_state.executions.lock(&key).await;
        if let Some(agent) = self.agent_snapshot(id).await? {
            if agent["owner"] != *owner {
                bail!("Hub execution owner does not match created agent");
            }
        } else if let Some(agent) = self.storage()?.get(id).await? {
            if agent["owner"] != *owner {
                bail!("Hub execution owner does not match created agent");
            }
        }
        self.cleanup_creation(owner, Some(id), created.get("createdWorktree"))
            .await
    }

    fn archive_after_creation_turn(
        &self,
        id: String,
        worktree: Option<Value>,
        mut events: broadcast::Receiver<Value>,
    ) -> Result<()> {
        let app = self.app()?;
        let cancel = app.cancel.clone();
        let weak = std::sync::Arc::downgrade(&app);
        tokio::spawn(async move {
            loop {
                let event = tokio::select! {
                    _ = cancel.cancelled() => break,
                    event = events.recv() => match event {
                        Ok(event) => event,
                        Err(broadcast::error::RecvError::Closed) => break,
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    },
                };
                if event["type"] != "agent_stream" || text(&event, "agentId") != Some(id.as_str()) {
                    continue;
                }
                if !matches!(
                    text(&event["event"], "type"),
                    Some("turn_completed" | "turn_failed" | "turn_canceled")
                ) {
                    continue;
                }
                let Some(app) = weak.upgrade() else {
                    break;
                };
                let actor = json!({"kind":"system"});
                let result = if let Some(workspace) = worktree
                    .as_ref()
                    .filter(|w| w["created"] == true)
                    .and_then(|w| text(&w["workspace"], "workspaceId"))
                {
                    app.projects.archive_workspace(&actor, workspace).await
                } else {
                    app.agents.control(&actor, &id, "archive", json!({})).await
                };
                if let Err(error) = result {
                    tracing::warn!(agent_id = %id,?error,"Failed to auto-archive agent");
                }
                break;
            }
        });
        Ok(())
    }

    async fn cleanup_creation(
        &self,
        actor: &Value,
        agent_id: Option<&str>,
        worktree: Option<&Value>,
    ) -> Result<()> {
        let mut failure = None;
        if let Some(id) = agent_id {
            if let Err(error) = self.control(actor, id, "delete", json!({})).await {
                failure = Some(error);
            }
            if let Err(error) = self.storage()?.remove(id).await {
                if failure.is_none() {
                    failure = Some(error);
                }
            }
        }
        if let Some(worktree) = worktree.filter(|worktree| worktree["created"] == true) {
            if let Some(id) = text(&worktree["workspace"], "workspaceId") {
                if let Err(error) = self.app()?.projects.archive_workspace(actor, id).await {
                    if failure.is_none() {
                        failure = Some(error);
                    }
                }
            }
        }
        if let Some(error) = failure {
            Err(error)
        } else {
            Ok(())
        }
    }

    pub fn subscribe_creation_events(&self) -> broadcast::Receiver<Value> {
        self.creations
            .get()
            .expect("agent creation receipts initialized before admission")
            .subscribe_events()
    }

    pub async fn creation_snapshot(&self, kind: &str, key: &str) -> Result<Option<Value>> {
        Ok(self.creations()?.subscribe(kind, key, self).await?.snapshot)
    }

    pub async fn create_with_receipt(&self, actor: &Value, input: Value) -> Result<Value> {
        self.create_receipted(actor, input, "agent").await
    }

    pub async fn create_workspace_with_receipt(
        &self,
        actor: &Value,
        input: Value,
    ) -> Result<Value> {
        self.create_receipted(actor, input, "workspace").await
    }

    async fn create_receipted(&self, actor: &Value, input: Value, kind: &str) -> Result<Value> {
        let key = text(&input, "idempotencyKey")
            .or_else(|| text(&input, "requestId"))
            .context("Creation requestId is required")?;
        let mut intent = input.clone();
        let object = intent
            .as_object_mut()
            .context("Creation request must be an object")?;
        for field in ["type", "requestId", "subscribe", "idempotencyKey"] {
            object.remove(field);
        }
        let agent = if kind == "agent" {
            Some(&input)
        } else {
            input.get("agent").filter(|v| !v.is_null())
        };
        let admission = self
            .creations()?
            .begin(
                &CreationInput {
                    kind: kind.to_owned(),
                    key: key.to_owned(),
                    request: intent,
                    workspace_id: text(&input, "workspaceId").map(str::to_owned),
                    agent_id: agent.and_then(|a| text(a, "agentId")).map(str::to_owned),
                    has_agent: agent.is_some(),
                    has_prompt: agent.is_some_and(has_initial_prompt),
                },
                self,
            )
            .await?;
        match admission {
            CreationAdmission::Replay(snapshot) => Ok(snapshot),
            CreationAdmission::Running(mut subscription) => subscription.completion().await,
            CreationAdmission::Run(guard) => {
                // A physical socket/request cancellation must not abandon a claimed
                // external effect. The receipt runner owns completion independently.
                let manager = self.app()?.agents.clone();
                let actor = actor.clone();
                let kind = kind.to_owned();
                tokio::spawn(async move {
                    manager
                        .run_creation_receipt(&actor, input, &kind, guard)
                        .await
                })
                .await
                .context("Creation task failed")?
            }
        }
    }

    async fn run_creation_receipt(
        &self,
        actor: &Value,
        input: Value,
        kind: &str,
        mut guard: CreationGuard,
    ) -> Result<Value> {
        let operation = async {
            let app = self.app()?;
            if kind == "workspace" && guard.snapshot().get("workspace").is_none() {
                guard.begin_stage("workspace").await?;
                let mut request = input.clone();
                request["workspaceId"] = guard.snapshot()["workspaceId"].clone();
                if let Some(agent) = input.get("agent") {
                    if agent.get("background").is_some() {
                        bail!("Configure background on the workspace, not its initial agent.");
                    }
                    request["firstAgentContext"] =
                        json!({"prompt":agent["initialPrompt"],"attachments":agent["attachments"]});
                }
                request.as_object_mut().unwrap().remove("agent");
                let workspace = app.projects.create_workspace(&request).await?;
                let descriptor = app
                    .projects
                    .descriptor(required(&workspace, "workspaceId")?)
                    .await?;
                let setup_skipped = workspace
                    .get("untrustedSource")
                    .filter(|source| !source.is_null())
                    .map(crate::projects::automation_blocked_message);
                guard.workspace_ready(descriptor, setup_skipped).await?;
            }
            let agent_input = if kind == "agent" {
                Some(&input)
            } else {
                input.get("agent").filter(|v| !v.is_null())
            };
            if let Some(agent_input) =
                agent_input.filter(|_| guard.snapshot().get("agent").is_none())
            {
                guard.begin_stage("agent").await?;
                let mut command = agent_input.clone();
                command["agentId"] = guard.snapshot()["agentId"].clone();
                if kind == "workspace" {
                    let workspace = &guard.snapshot()["workspace"];
                    let destination = text(workspace, "workspaceDirectory")
                        .context("Created workspace has no directory")?;
                    let source = self.workspace_source_cwd(&input["source"]).await?;
                    let agent_cwd =
                        expand_path(required(&command["config"], "cwd")?, &app.user_home)?;
                    let relative = agent_cwd.strip_prefix(&source).map_err(|_| {
                        anyhow!("Agent directory must be inside the workspace source")
                    })?;
                    command["config"]["cwd"] = json!(Path::new(destination).join(relative));
                    command["workspaceId"] = workspace["id"].clone();
                    command["type"] = json!("create_agent_request");
                }
                let result = self
                    .create_command(actor, command, Some(&mut guard), None)
                    .await?;
                let agent = snapshot_payload(&result["liveSnapshot"]);
                if result["initialPromptStarted"] == true {
                    guard.prompt_started(agent.clone()).await?;
                }
                guard.complete(Some(agent)).await?;
            } else {
                guard.complete(None).await?;
            }
            Ok::<_, anyhow::Error>(())
        }
        .await;
        if let Err(error) = operation {
            let details = error_details(&error);
            let code = details.as_ref().and_then(|d| text(d, "code")).or_else(|| {
                (guard.snapshot().get("workspace").is_some() || kind == "agent")
                    .then_some("unknown")
            });
            guard.fail(&error.to_string(), code, self).await?;
        }
        Ok(guard.snapshot().clone())
    }

    async fn workspace_source_cwd(&self, source: &Value) -> Result<PathBuf> {
        let app = self.app()?;
        if let Some(path) = text(source, "path").or_else(|| text(source, "cwd")) {
            return expand_path(path, &app.user_home);
        }
        let id = required(source, "projectId")?;
        let project = app
            .projects
            .project(id)
            .await?
            .with_context(|| format!("Project {id} not found"))?;
        expand_path(required(&project, "rootPath")?, &app.user_home)
    }
}

#[async_trait]
impl CreationHooks for AgentManager {
    async fn exists(&self, kind: &str, id: &str) -> Result<bool> {
        if kind == "workspace" {
            Ok(self.app()?.projects.workspace(id).await?.is_some())
        } else {
            Ok(
                self.agent_snapshot(id).await?.is_some()
                    || self.storage()?.get(id).await?.is_some(),
            )
        }
    }
    async fn read_agent(&self, id: &str) -> Result<Option<Value>> {
        Ok(self
            .agent_snapshot(id)
            .await?
            .map(|agent| snapshot_payload(&agent)))
    }
    async fn validate_completed(&self, snapshot: &Value) -> Result<()> {
        if let Some(id) = text(snapshot, "agentId") {
            if !self.exists("agent", id).await? {
                bail!("Previously created agent no longer exists");
            }
        }
        if snapshot["kind"] == "workspace" {
            if let Some(id) = text(snapshot, "workspaceId") {
                if !self.exists("workspace", id).await? {
                    bail!("Previously created workspace no longer exists");
                }
            }
        }
        Ok(())
    }
}

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}
fn required<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    text(value, key).with_context(|| format!("Missing {key}"))
}
fn array<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}
fn copy_as(source: &Value, from: &str, target: &mut Value, to: &str) {
    if let Some(value) = source.get(from).filter(|v| !v.is_null()) {
        target[to] = value.clone();
    }
}
fn merge_object(target: &mut Map<String, Value>, value: Option<&Value>) {
    if let Some(value) = value.and_then(Value::as_object) {
        target.extend(
            value
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
    }
}
fn merge_value(target: &mut Value, source: &Value) {
    merge_object(target.as_object_mut().unwrap(), Some(source));
}
fn issue(field: &str, message: String) -> Value {
    json!({"path":[field],"message":message})
}
/// Public `create_agent_request` placement rules. A worktree target is first-class
/// placement; only the removed git/worktreeName fields are rejected.
fn validate_public_intent(input: &Value) -> Result<()> {
    if input.get("git").is_some() || input.get("worktreeName").is_some() {
        bail!(
            "Legacy git/worktreeName agent placement was removed. Use createWorkspace({{ source, agent, idempotencyKey }}) to create a workspace with an agent, or create the workspace first and pass workspaceId."
        );
    }
    if input["internal"] == true || input["config"]["internal"] == true {
        bail!("Public internal agents were removed; create a background workspace instead.");
    }
    if input.get("background").is_some()
        && (text(input, "workspaceId").is_some()
            || (text(input, "callerAgentId").is_some() && input["worktree"].is_null()))
    {
        bail!(
            "background is workspace creation intent; configure the selected workspace at creation."
        );
    }
    Ok(())
}
fn has_initial_prompt(input: &Value) -> bool {
    text(input, "initialPrompt").is_some_and(|s| !s.trim().is_empty())
        || !array(input, "images").is_empty()
        || !array(input, "attachments").is_empty()
}
fn require_active(cancel: Option<&CancellationToken>) -> Result<()> {
    if cancel.is_some_and(CancellationToken::is_cancelled) {
        bail!("Hub relationship authority is no longer active");
    }
    Ok(())
}
async fn require_directory(path: &Path) -> Result<()> {
    if !tokio::fs::metadata(path)
        .await
        .map(|metadata| metadata.is_dir())
        .unwrap_or(false)
    {
        bail!(
            "Working directory does not exist or is not a directory: {}",
            path.display()
        );
    }
    Ok(())
}
fn expand_path(path: &str, home: &Path) -> Result<PathBuf> {
    let expanded = if path == "~" {
        home.to_path_buf()
    } else if let Some(rest) = path.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(path)
    };
    let absolute = if expanded.is_absolute() {
        expanded
    } else {
        std::env::current_dir()?.join(expanded)
    };
    Ok(normalize_path(&absolute))
}
fn resolve_from(base: &Path, path: &str, home: &Path) -> Result<PathBuf> {
    if path == "~" || path.starts_with("~/") || Path::new(path).is_absolute() {
        expand_path(path, home)
    } else {
        Ok(normalize_path(&base.join(path)))
    }
}
fn normalize_path(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            other => result.push(other.as_os_str()),
        }
    }
    result
}
pub(super) fn title(explicit: Option<&str>, prompt: &str) -> Option<String> {
    if let Some(explicit) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
        return Some(explicit.to_owned());
    }
    let line = prompt
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    let mut normalized = String::with_capacity(line.len().min(60));
    let mut units = 0;
    let mut pending_space = false;
    for character in line.chars() {
        if character.is_whitespace() {
            pending_space = !normalized.is_empty();
            continue;
        }
        if pending_space {
            if units == 60 {
                break;
            }
            normalized.push(' ');
            units += 1;
            pending_space = false;
        }
        if units == 60 {
            break;
        }
        if units + character.len_utf16() > 60 {
            normalized.push('\u{fffd}');
            break;
        }
        normalized.push(character);
        units += character.len_utf16();
    }
    normalized.truncate(normalized.trim_end().len());
    Some(normalized)
}
fn wants_worktree(value: &Value) -> bool {
    [
        "worktreeName",
        "refName",
        "action",
        "githubPrNumber",
        "mode",
    ]
    .iter()
    .any(|key| value.get(*key).is_some_and(|v| !v.is_null() && v != ""))
}
fn worktree_source(worktree: &Value, cwd: &Path) -> Value {
    let mut source = json!({"kind":"worktree","cwd":cwd});
    match text(worktree, "mode") {
        Some("branch-off") => {
            source["action"] = json!("branch-off");
            copy_as(worktree, "base", &mut source, "refName");
            copy_as(worktree, "newBranch", &mut source, "worktreeSlug");
            copy_as(worktree, "newBranch", &mut source, "branchName");
        }
        Some("checkout-branch") => {
            source["action"] = json!("checkout");
            copy_as(worktree, "branch", &mut source, "refName");
        }
        Some("checkout-pr") => {
            source["action"] = json!("checkout");
            copy_as(worktree, "prNumber", &mut source, "githubPrNumber");
        }
        _ => {
            for key in [
                "action",
                "refName",
                "baseBranch",
                "branchName",
                "githubPrNumber",
                "checkoutSource",
            ] {
                copy_as(worktree, key, &mut source, key);
            }
            copy_as(worktree, "worktreeName", &mut source, "worktreeSlug");
        }
    }
    source
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_title_uses_first_content_line_and_utf16_limit() {
        assert_eq!(
            title(None, "\n  first    line \nsecond"),
            Some("first line".into())
        );
        assert_eq!(title(Some("  chosen  "), "prompt"), Some("chosen".into()));
        let prompt = format!("{}x", "😀".repeat(30));
        assert_eq!(title(None, &prompt), Some("😀".repeat(30)));
        assert_eq!(title(None, " \n\t"), None);
    }

    #[test]
    fn worktree_targets_keep_branch_and_checkout_intent_distinct() {
        let base = Path::new("/repository");
        assert_eq!(
            worktree_source(
                &json!({"mode":"branch-off","base":"origin/main","newBranch":"feature"}),
                base
            ),
            json!({"kind":"worktree","cwd":base,"action":"branch-off","refName":"origin/main","worktreeSlug":"feature","branchName":"feature"})
        );
        assert_eq!(
            worktree_source(&json!({"mode":"checkout-branch","branch":"existing"}), base),
            json!({"kind":"worktree","cwd":base,"action":"checkout","refName":"existing"})
        );
        assert!(!wants_worktree(&json!({"branchName":"unused"})));
    }

    #[test]
    fn receipt_prompt_intent_includes_attachments_without_text() {
        assert!(!has_initial_prompt(&json!({"initialPrompt":"   "})));
        assert!(has_initial_prompt(
            &json!({"attachments":[{"type":"text","text":"context"}]})
        ));
        assert!(has_initial_prompt(
            &json!({"images":[{"data":"x","mimeType":"image/png"}]})
        ));
    }

    #[test]
    fn public_create_accepts_worktree_targets_and_scopes_background() {
        let worktree = json!({"mode":"branch-off","newBranch":"feature","base":"main"});
        assert!(validate_public_intent(&json!({"worktree":worktree})).is_ok());
        assert!(wants_worktree(&worktree));
        assert!(
            validate_public_intent(
                &json!({"worktree":worktree,"callerAgentId":"parent","background":true})
            )
            .is_ok()
        );
        assert!(
            validate_public_intent(&json!({"callerAgentId":"parent","background":true})).is_err()
        );
        assert!(
            validate_public_intent(&json!({"workspaceId":"w","worktree":worktree,"background":true}))
                .is_err()
        );
        assert!(validate_public_intent(&json!({"worktreeName":"legacy"})).is_err());
        assert!(validate_public_intent(&json!({"git":{}})).is_err());
    }
}
