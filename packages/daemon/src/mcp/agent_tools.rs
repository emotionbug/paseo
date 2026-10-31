use std::sync::Arc;

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::state::AppState;

const PROMPT_NOTIFICATION: &str = "You will get notified when the prompted agent finishes, errors, or needs permission. Do not poll for status; continue with other work until the notification arrives.";
const DISPATCH_ACKNOWLEDGED: &str = "Dispatch acknowledged. Observe completion using get_agent_status/get_agent_activity or SDK/RPC subscriptions and wait APIs.";

pub(crate) fn definitions(agent_scope: bool) -> Vec<Value> {
    let features = json!({"type":"object","additionalProperties":true,"description":"Provider-specific feature values, for example { fast_mode: true } for Codex."});
    let create_settings = json!({"type":"object","properties":{
        "modeId":{"type":"string","description":"Session mode to configure before the first run."},
        "thinkingOptionId":{"type":"string","description":"Thinking option ID."},
        "features":features
    },"additionalProperties":false,"description":"Initial runtime settings for the new agent."});
    let update_settings = json!({"type":"object","properties":{
        "modeId":{"type":"string","description":"Session mode ID."},
        "model":{"type":["string","null"],"description":"Model ID. Pass null to clear."},
        "thinkingOptionId":{"type":["string","null"],"description":"Thinking option ID. Pass null to clear."},
        "features":features
    },"additionalProperties":false,"description":"Runtime settings to apply to the agent."});
    let inspect_settings = json!({"type":"object","properties":{
        "modeId":{"type":"string","description":"Draft session mode ID."},
        "model":{"type":"string","description":"Draft model ID."},
        "thinkingOptionId":{"type":"string","description":"Draft thinking option ID."},
        "features":{"type":"object","additionalProperties":true,"description":"Draft provider feature values."}
    },"additionalProperties":false,"description":"Draft provider settings used to compute available features."});
    let id = json!({"agentId":{"type":"string"}});
    let empty = json!({});
    vec![
        definition(
            "create_agent",
            "Create agent",
            "Create an agent. Agent-scoped creation defaults to your workspace and creates your subagent. Top-level creation without workspaceId creates a new local workspace. Requires provider/model (for example codex/gpt-5.4) and an initial prompt. Do not guess; call list_providers and list_models first if uncertain.",
            json!({
                "title":{"type":"string","x-trim":true,"minLength":1,"maxLength":60,"description":"Short descriptive title (<= 60 chars) summarizing the agent's focus."},
                "provider":{"type":"string","x-trim":true,"description":"Required provider/model pair, for example codex/gpt-5.4."},
                "labels":{"type":"object","additionalProperties":{"type":"string"},"description":"Labels to set on the agent"},
                "settings":create_settings,
                "initialPrompt":{"type":"string","x-trim":true,"minLength":1,"description":"Required first task to run immediately after creation."},
                "workspaceId":{"type":"string","minLength":1,"description":"Existing workspace id. Agent-scoped calls default to the caller workspace; top-level calls create a new local workspace when omitted."},
                "notifyOnFinish":{"type":"boolean","default":agent_scope,"description":if agent_scope { "Get notified when the created agent finishes, errors, or needs permission. Set false for explicit reporting or no completion callback." } else { "Agent-scoped only: get notified when the created agent finishes, errors, or needs permission." }}
            }),
            &["title", "provider", "initialPrompt"],
        ),
        definition(
            "send_agent_prompt",
            "Send agent prompt",
            "Send a task. Agent callers receive an immediate queued receipt; delivery follows after two quiet seconds per recipient. Completion reports include the target’s last response by default. Paseo supplies caller identity.",
            json!({
                "agentId":{"type":"string"}, "prompt":{"type":"string"},
                "sessionMode":{"type":"string","description":"Optional mode to set before running the prompt."},
                "activeTurnBehavior":{"type":"string","enum":["interrupt","steer"],"description":"How to send into a running turn: \"steer\" joins it, falling back to interrupt when unavailable; \"interrupt\" replaces it. Defaults to steer for agent callers and interrupt otherwise."},
                "notifyOnFinish":{"type":"boolean","default":agent_scope,"description":if agent_scope { "Get notified when the prompted agent finishes, errors, or needs permission. Set false for explicit reporting or no completion callback." } else { "Agent-scoped only: get notified when the prompted agent finishes, errors, or needs permission." }}
            }),
            &["agentId", "prompt"],
        ),
        definition(
            "get_agent_status",
            "Get agent status",
            "Return the latest snapshot for an agent, including lifecycle state, capabilities, and pending permissions.",
            id.clone(),
            &["agentId"],
        ),
        definition(
            "list_agents",
            "List agents",
            "List recent agents as compact metadata.",
            json!({
                "includeBackground":{"type":"boolean","default":false}, "includeArchived":{"type":"boolean","default":false},
                "cwd":{"type":"string"}, "sinceHours":{"type":"integer","minimum":1,"maximum":720,"default":48},
                "statuses":{"type":"array","items":{"type":"string","enum":["initializing","idle","running","error","closed"]}},
                "limit":{"type":"integer","minimum":1,"maximum":200,"default":50}
            }),
            &[],
        ),
        definition(
            "cancel_agent",
            "Cancel agent run",
            "Abort the agent's current run but keep the agent alive for future tasks.",
            id.clone(),
            &["agentId"],
        ),
        definition(
            "archive_agent",
            "Archive agent",
            "Archive an agent (soft-delete). The agent is interrupted if running and removed from the active list.",
            id.clone(),
            &["agentId"],
        ),
        definition(
            "kill_agent",
            "Kill agent",
            "Terminate an agent session permanently.",
            id,
            &["agentId"],
        ),
        definition(
            "update_agent",
            "Update agent",
            "Update an agent name, labels, and/or runtime settings.",
            json!({
                "agentId":{"type":"string"},"name":{"type":"string"},
                "labels":{"type":"object","additionalProperties":{"type":"string"},"description":"Labels to set on the agent"},
                "settings":update_settings
            }),
            &["agentId"],
        ),
        definition(
            "list_providers",
            "List providers",
            "List configured agent providers, availability, and their modes.",
            empty.clone(),
            &[],
        ),
        definition(
            "list_models",
            "List models",
            "List models for an agent provider.",
            json!({"provider":{"type":"string"}}),
            &["provider"],
        ),
        definition(
            "list_profiles",
            "List agent profiles",
            "List agent profiles: named provider/model/mode bundles a human configured for specific kinds of work. Read each profile's `notes` to pick the one that fits the task you're delegating, then copy its `provider`, `model`, `modeId`, `thinkingOptionId`, and `featureValues` into create_agent (there is no `profile` parameter). Returns an empty list if none are configured.",
            empty.clone(),
            &[],
        ),
        definition(
            "inspect_provider",
            "Inspect provider",
            "Inspect compact provider capabilities for orchestration, including modes and draft feature settings. Use list_models for the full model list.",
            json!({
                "provider":{"type":"string","x-trim":true,"minLength":1,"description":"Provider ID, optionally with a model ID (for example codex or codex/gpt-5.4)."},
                "cwd":{"type":"string","description":"Working directory used to resolve provider feature availability."},"settings":inspect_settings
            }),
            &["provider"],
        ),
        definition(
            "get_agent_activity",
            "Get agent activity",
            "Return recent agent timeline entries as a curated summary.",
            json!({
                "agentId":{"type":"string"}, "limit":{"type":"number","description":"Optional limit for number of activities to include (most recent first)."}
            }),
            &["agentId"],
        ),
        definition(
            "set_agent_mode",
            "Set agent session mode",
            "Switch the agent's session mode (plan, bypassPermissions, read-only, auto, etc.).",
            json!({"agentId":{"type":"string"},"modeId":{"type":"string"}}),
            &["agentId", "modeId"],
        ),
        definition(
            "list_pending_permissions",
            "List pending permissions",
            "Return all pending permission requests across all agents with the normalized payloads.",
            empty,
            &[],
        ),
        definition(
            "respond_to_permission",
            "Respond to permission",
            "Approve or deny a pending permission request with an AgentManager-compatible response payload.",
            json!({
                "agentId":{"type":"string"},"requestId":{"type":"string"},
                "response":{"oneOf":[
                    {"type":"object","x-strip-unknown":true,"properties":{"behavior":{"const":"allow"},"selectedActionId":{"type":"string"},"updatedInput":{"type":"object","additionalProperties":true},"updatedPermissions":{"type":"array","items":{"type":"object","additionalProperties":true}}},"required":["behavior"],"additionalProperties":false},
                    {"type":"object","x-strip-unknown":true,"properties":{"behavior":{"const":"deny"},"selectedActionId":{"type":"string"},"message":{"type":"string"},"interrupt":{"type":"boolean"}},"required":["behavior"],"additionalProperties":false}
                ]}
            }),
            &["agentId", "requestId", "response"],
        ),
    ]
}

fn definition(
    name: &str,
    title: &str,
    description: &str,
    properties: Value,
    required: &[&str],
) -> Value {
    json!({"name":name,"title":title,"description":description,"inputSchema":{
        "type":"object","properties":properties,"required":required,"additionalProperties":true
    }})
}

fn result(structured: Value) -> Value {
    json!({"content":[],"structuredContent":structured,"isError":false})
}

fn string<'a>(input: &'a Value, key: &str) -> Result<&'a str> {
    input
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("{key} is required"))
}

fn actor(caller: Option<&str>) -> Value {
    match caller {
        Some(id) => json!({"kind":"agent","agentId":id}),
        None => json!({"kind":"owner"}),
    }
}

fn provider_model(value: &str, required: bool) -> Result<(&str, Option<&str>)> {
    let value = value.trim();
    if let Some((provider, model)) = value.split_once('/') {
        let (provider, model) = (provider.trim(), model.trim());
        if !provider.is_empty() && !model.is_empty() {
            return Ok((provider, Some(model)));
        }
    } else if !required && !value.is_empty() {
        return Ok((value, None));
    }
    if required {
        bail!("provider must be provider/model, for example codex/gpt-5.4");
    }
    bail!("provider must be <provider> or <provider>/<model>")
}

pub(crate) async fn execute(
    app: &Arc<AppState>,
    caller: Option<&str>,
    name: &str,
    mut input: Value,
    cancel: CancellationToken,
    _updates: Option<mpsc::Sender<Value>>,
) -> Result<Value> {
    if cancel.is_cancelled() {
        bail!("Tool execution cancelled");
    }
    let actor = actor(caller);
    let structured = match name {
        "create_agent" => {
            prepare_create(&mut input, caller)?;
            let input = resolve_create(app, caller, input).await?;
            let notifying = caller.is_some() && input["notifyOnFinish"] == true;
            let created = app.agents.create(&actor, input).await?;
            let snapshot = created
                .get("snapshot")
                .ok_or_else(|| anyhow!("Agent creation returned no snapshot"))?;
            let live = created.get("liveSnapshot").unwrap_or(snapshot);
            let guidance = if notifying && created["initialPromptStarted"] == true {
                "You will get notified when the created agent finishes, errors, or needs permission. Do not poll for status; continue with other work until the notification arrives."
            } else {
                "Creation acknowledged. Observe completion using get_agent_status/get_agent_activity or SDK/RPC subscriptions and wait APIs."
            };
            let mut response = json!({
                "agentId":live["id"], "type":snapshot["provider"],
                "status":live["status"], "cwd":live["cwd"],
                "currentModeId":live["currentModeId"], "availableModes":live["availableModes"],
                "guidance":guidance
            });
            if let Some(workspace) = live.get("workspaceId").filter(|value| !value.is_null()) {
                response["workspaceId"] = workspace.clone();
            }
            response
        }
        "send_agent_prompt" => {
            let id = string(&input, "agentId")?.to_owned();
            let notifying = caller.is_some()
                && input
                    .get("notifyOnFinish")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
            // The source tool destructures these fields; MCP passthrough keys
            // must never become additional manager authority or run options.
            retain_fields(&mut input, &["prompt", "sessionMode", "activeTurnBehavior"]);
            input["notifyOnFinish"] = json!(notifying);
            if input.get("activeTurnBehavior").is_none() {
                input["activeTurnBehavior"] = json!(if caller.is_some() {
                    "steer"
                } else {
                    "interrupt"
                });
            }
            let receipt = app.agents.send(&actor, &id, input).await?;
            if caller.is_some() {
                json!({
                    "success":true, "deliveryStatus":"queued",
                    "messageId":receipt["messageId"], "status":receipt["status"],
                    "guidance":if notifying { format!("Queued for delivery after two quiet seconds. {PROMPT_NOTIFICATION}") } else { "Queued for delivery after two quiet seconds; provider dispatch has not occurred.".to_owned() }
                })
            } else {
                let snapshot = app.agents.agent_snapshot(&id).await?;
                let status = snapshot
                    .as_ref()
                    .and_then(|value| value.get("status"))
                    .cloned()
                    .unwrap_or(json!("idle"));
                json!({"success":true,"disposition":receipt["disposition"],"status":status,"guidance":DISPATCH_ACKNOWLEDGED})
            }
        }
        "get_agent_status" => {
            let id = string(&input, "agentId")?;
            let snapshot = app
                .agents
                .agent_snapshot(id)
                .await?
                .ok_or_else(|| anyhow!("Agent {id} not found"))?;
            json!({"status":snapshot["status"],"snapshot":snapshot})
        }
        "list_agents" => {
            retain_fields(
                &mut input,
                &[
                    "includeBackground",
                    "includeArchived",
                    "cwd",
                    "sinceHours",
                    "statuses",
                    "limit",
                ],
            );
            json!({"agents":app.agents.list_agents(&actor, input).await?})
        }
        "archive_agent" | "kill_agent" => {
            let id = string(&input, "agentId")?;
            app.agents
                .control(
                    &actor,
                    id,
                    if name == "kill_agent" {
                        "close"
                    } else {
                        "archive"
                    },
                    json!({}),
                )
                .await?;
            json!({"success":true})
        }
        "update_agent" => {
            let id = string(&input, "agentId")?;
            if let Some(settings) = input.get("settings") {
                for (key, action) in [
                    ("modeId", "set_mode"),
                    ("model", "set_model"),
                    ("thinkingOptionId", "set_thinking"),
                ] {
                    if let Some(value) = settings.get(key) {
                        let mut update = serde_json::Map::new();
                        update.insert(key.to_owned(), value.clone());
                        app.agents
                            .control(&actor, id, action, Value::Object(update))
                            .await?;
                    }
                }
                if let Some(features) = settings.get("features").and_then(Value::as_object) {
                    for (feature, value) in features {
                        app.agents
                            .control(
                                &actor,
                                id,
                                "set_feature",
                                json!({"featureId":feature,"value":value}),
                            )
                            .await?;
                    }
                }
            }
            let mut update = serde_json::Map::new();
            if let Some(title) = input
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                update.insert("name".to_owned(), json!(title));
            }
            if let Some(labels) = input
                .get("labels")
                .and_then(Value::as_object)
                .filter(|value| !value.is_empty())
            {
                update.insert("labels".to_owned(), Value::Object(labels.clone()));
            }
            if !update.is_empty() {
                app.agents
                    .control(&actor, id, "update", Value::Object(update))
                    .await?;
            }
            json!({"success":true})
        }
        "list_profiles" => {
            let mut config = app.config.mutable_snapshot();
            json!({"profiles":config.get_mut("agentProfiles").map(Value::take).unwrap_or_else(|| json!([]))})
        }
        "get_agent_activity" => {
            let id = string(&input, "agentId")?;
            let mut summary = app
                .agents
                .activity_summary(id, input.get("limit").and_then(Value::as_f64))
                .await?;
            let snapshot = app.agents.agent_snapshot(id).await?;
            summary["agentId"] = json!(id);
            summary["currentModeId"] = snapshot
                .as_ref()
                .and_then(|value| value.get("currentModeId"))
                .cloned()
                .unwrap_or(Value::Null);
            summary
        }
        "set_agent_mode" => {
            let id = string(&input, "agentId")?;
            let mode = string(&input, "modeId")?;
            app.agents
                .control(&actor, id, "set_mode", json!({"modeId":mode}))
                .await?;
            json!({"success":true,"newMode":mode})
        }
        "respond_to_permission" => {
            let id = string(&input, "agentId")?;
            app.agents
                .control(
                    &actor,
                    id,
                    "respond_permission",
                    json!({"requestId":string(&input,"requestId")?,"response":input["response"]}),
                )
                .await?;
            json!({"success":true})
        }
        _ => return execute_catalog_or_permissions(app, &actor, caller, name, input, cancel).await,
    };
    Ok(result(structured))
}

fn prepare_create(input: &mut Value, caller: Option<&str>) -> Result<()> {
    if [
        "worktreeName",
        "branchName",
        "baseBranch",
        "refName",
        "githubPrNumber",
    ]
    .iter()
    .any(|key| input.get(key).is_some())
        || input
            .pointer("/workspace/source/kind")
            .and_then(Value::as_str)
            == Some("worktree")
    {
        bail!(
            "Agent worktree placement was removed. Use create_workspace first, then create_agent with workspaceId."
        );
    }
    let legacy = ["relationship", "workspace", "cwd"]
        .iter()
        .any(|key| input.get(key).is_some());
    let allowed = [
        "title",
        "provider",
        "labels",
        "settings",
        "initialPrompt",
        "notifyOnFinish",
    ];
    for key in input
        .as_object()
        .ok_or_else(|| anyhow!("Tool input must be an object"))?
        .keys()
    {
        let placement = if legacy {
            key == "relationship"
                || key == "workspace"
                || (caller.is_none()
                    && ["cwd", "mode", "thinking", "features"].contains(&key.as_str()))
        } else {
            key == "workspaceId"
        };
        if !allowed.contains(&key.as_str()) && !placement {
            bail!("Unrecognized create_agent argument: {key}");
        }
    }
    for key in ["cwd", "mode", "thinking"] {
        if input.get(key).is_some_and(|value| !value.is_string()) {
            bail!("{key} must be a string");
        }
    }
    if input
        .get("features")
        .is_some_and(|value| !value.is_object())
    {
        bail!("features must be an object");
    }
    for key in ["title", "provider", "initialPrompt"] {
        let trimmed = string(input, key)?.trim().to_owned();
        if trimmed.is_empty() {
            bail!("{key} is required");
        }
        if key == "title" && trimmed.encode_utf16().count() > 60 {
            bail!("Title must be 60 characters or fewer");
        }
        input[key] = json!(trimmed);
    }
    provider_model(string(input, "provider")?, true)?;
    let notifying = caller.is_some()
        && input
            .get("notifyOnFinish")
            .and_then(Value::as_bool)
            .unwrap_or(true);
    input["notifyOnFinish"] = json!(notifying);
    input["kind"] = json!("mcp");
    input["promptFailure"] = json!("throw");
    Ok(())
}

async fn execute_catalog_or_permissions(
    app: &Arc<AppState>,
    actor: &Value,
    caller: Option<&str>,
    name: &str,
    input: Value,
    cancel: CancellationToken,
) -> Result<Value> {
    let structured = match name {
        "cancel_agent" => {
            let id = string(&input, "agentId")?;
            let outcome = app.agents.control(actor, id, "cancel", json!({})).await?;
            match outcome.get("status").and_then(Value::as_str) {
                Some("settled") => json!({"success":true}),
                Some("not_running") => json!({"success":false}),
                Some("refused") => bail!(
                    "Cannot stop agent {id} because its active run cancellation was not acknowledged"
                ),
                Some(status) => bail!("Agent cancellation did not settle: {status}"),
                None => bail!("Agent cancellation returned no outcome"),
            }
        }
        "list_pending_permissions" => {
            json!({"permissions":app.agents.pending_permissions().await?})
        }
        _ => return execute_provider(app, caller, name, input, cancel).await,
    };
    Ok(result(structured))
}

async fn resolve_create(
    app: &Arc<AppState>,
    caller: Option<&str>,
    mut input: Value,
) -> Result<Value> {
    let parent = match caller {
        Some(id) => Some(
            app.agents
                .agent_snapshot(id)
                .await?
                .ok_or_else(|| anyhow!("Parent agent {id} not found"))?,
        ),
        None => None,
    };
    let context = match caller {
        Some(id) => app.mcp.caller_context(id).await,
        None => None,
    };
    let parent_workspace = parent
        .as_ref()
        .and_then(|value| value.get("workspaceId"))
        .and_then(Value::as_str);
    let legacy = input.get("relationship").is_some()
        || input.get("workspace").is_some()
        || input.get("cwd").is_some();
    let mut detached = false;
    let placement = if legacy {
        if input.get("relationship").is_some() != input.get("workspace").is_some() {
            bail!("relationship and workspace must be provided together");
        }
        if let Some(relationship) = input.get("relationship") {
            strict_keys(relationship, &["kind"])?;
            detached = match string(relationship, "kind")? {
                "detached" => true,
                "subagent" if caller.is_some() => false,
                "subagent" => bail!("relationship subagent requires an agent-scoped tool session"),
                _ => bail!("relationship kind must be subagent or detached"),
            };
            input["workspace"].clone()
        } else {
            if caller.is_some() {
                bail!("relationship and workspace are required for legacy agent-scoped creation");
            }
            let cwd = input
                .get("cwd")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    anyhow!("cwd is required for legacy top-level create_agent calls")
                })?;
            detached = true;
            json!({"kind":"create","source":{"kind":"directory","path":cwd}})
        }
    } else if let Some(id) = input.get("workspaceId") {
        json!({"kind":"existing","workspaceId":id})
    } else if caller.is_some() {
        json!({"kind":"current"})
    } else {
        json!({"kind":"create","source":{"kind":"directory","path":std::env::current_dir()?}})
    };
    if placement.get("cwd").is_some_and(|value| !value.is_string()) {
        bail!("workspace cwd must be a string");
    }
    let (cwd, workspace_id) = match string(&placement, "kind")? {
        "current" => {
            strict_keys(&placement, &["kind", "cwd"])?;
            let caller_id = caller.ok_or_else(|| {
                anyhow!("workspace current requires an agent-scoped tool session")
            })?;
            let id = parent_workspace
                .ok_or_else(|| anyhow!("Caller agent {caller_id} has no current workspace"))?;
            (
                Some(placement.get("cwd").cloned().unwrap_or_else(|| {
                    parent.as_ref().expect("caller resolved above")["cwd"].clone()
                })),
                id.to_owned(),
            )
        }
        "existing" => {
            strict_keys(&placement, &["kind", "workspaceId", "cwd"])?;
            let id = string(&placement, "workspaceId")?;
            let workspace = app
                .projects
                .workspace(id)
                .await?
                .filter(|value| value.get("archivedAt").is_none_or(Value::is_null))
                .ok_or_else(|| anyhow!("Workspace {id} not found"))?;
            let cwd = if let Some(requested) = placement
                .get("cwd")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
            {
                super::workspace_tools::resolve_scoped_cwd(app, caller, Some(requested)).await?
            } else {
                std::path::PathBuf::from(string(&workspace, "cwd")?)
            };
            if context
                .as_ref()
                .and_then(|value| value.get("lockedCwd"))
                .and_then(Value::as_str)
                .is_some_and(|value| !value.trim().is_empty())
            {
                let locked = super::workspace_tools::resolve_scoped_cwd(app, caller, None).await?;
                if !cwd.starts_with(&locked) {
                    bail!("Workspace {id} is outside the allowed cwd");
                }
            }
            (Some(json!(cwd)), id.to_owned())
        }
        "create" => {
            strict_keys(&placement, &["kind", "source"])?;
            let source = placement
                .get("source")
                .ok_or_else(|| anyhow!("workspace source is required"))?;
            strict_keys(source, &["kind", "path"])?;
            if source.get("path").is_some_and(|value| !value.is_string()) {
                bail!("workspace source path must be a string");
            }
            if string(source, "kind")? != "directory" {
                bail!("Use create_workspace first, then create_agent with workspaceId.");
            }
            let cwd = super::workspace_tools::resolve_scoped_cwd(
                app,
                caller,
                source.get("path").and_then(Value::as_str),
            )
            .await?;
            let workspace = app
                .projects
                .ensure_workspace_for_create(
                    &cwd,
                    &json!({"prompt":input["initialPrompt"]}),
                    parent_workspace,
                )
                .await?;
            (
                Some(json!(cwd)),
                string(&workspace, "workspaceId")?.to_owned(),
            )
        }
        _ => bail!("workspace kind must be current, existing, or create"),
    };
    let selected_provider = provider_model(string(&input, "provider")?, true)?.0;
    let inherited = parent
        .as_ref()
        .filter(|value| value.get("provider").and_then(Value::as_str) == Some(selected_provider))
        .and_then(|value| value.pointer("/config/providerOptions"))
        .cloned();
    let object = input
        .as_object_mut()
        .ok_or_else(|| anyhow!("Tool input must be an object"))?;
    object.remove("workspace");
    object.remove("relationship");
    object.remove("cwd");
    if let Some(cwd) = cwd {
        object.insert("cwd".to_owned(), cwd);
    }
    object.insert("workspaceId".to_owned(), json!(workspace_id));
    object.insert("detached".to_owned(), json!(detached));
    if caller.is_some() && !detached {
        let identity = app.transport.identity();
        let server_id = string(&identity, "serverId")?;
        let labels = object.entry("labels").or_insert_with(|| json!({}));
        labels
            .as_object_mut()
            .ok_or_else(|| anyhow!("labels must be an object"))?
            .insert("paseo.parent-server-id".to_owned(), json!(server_id));
    }
    if let Some(context) = context {
        object.insert("callerContext".to_owned(), context);
    }
    if let Some(options) = inherited {
        object.insert("config".to_owned(), json!({"providerOptions":options}));
    }
    if let Some(settings) = object.remove("settings") {
        for (key, target) in [
            ("modeId", "mode"),
            ("thinkingOptionId", "thinking"),
            ("features", "features"),
        ] {
            // Legacy top-level settings take precedence over their nested equivalents.
            if !object.contains_key(target) {
                if let Some(value) = settings.get(key) {
                    object.insert(target.to_owned(), value.clone());
                }
            }
        }
    }
    Ok(input)
}

fn strict_keys(value: &Value, allowed: &[&str]) -> Result<()> {
    for key in value
        .as_object()
        .ok_or_else(|| anyhow!("Expected an object"))?
        .keys()
    {
        if !allowed.contains(&key.as_str()) {
            bail!("Unrecognized argument: {key}");
        }
    }
    Ok(())
}

async fn execute_provider(
    app: &Arc<AppState>,
    caller: Option<&str>,
    name: &str,
    input: Value,
    cancel: CancellationToken,
) -> Result<Value> {
    let structured = match name {
        "list_providers" => {
            let entries = tokio::select! {
                _ = cancel.cancelled() => bail!("Tool execution cancelled"),
                entries = app.providers.list(None, true) => entries?,
            };
            let providers = entries
                .iter()
                .map(provider_summary)
                .collect::<Result<Vec<_>>>()?;
            json!({"providers":providers})
        }
        "list_models" => {
            let provider = string(&input, "provider")?;
            let entry = tokio::select! {
                _ = cancel.cancelled() => bail!("Tool execution cancelled"),
                entry = app.providers.catalog(provider, None, true) => entry?,
            };
            if entry.get("enabled").and_then(Value::as_bool) != Some(true) {
                bail!("Provider '{provider}' is disabled");
            }
            match entry.get("status").and_then(Value::as_str) {
                Some("ready") => {}
                Some("error") => bail!(
                    "{}",
                    entry
                        .get("error")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("Failed to load provider '{provider}'"))
                ),
                _ => bail!("Provider '{provider}' is not available"),
            }
            let models: Vec<&Value> = entry
                .get("models")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|model| model.get("isSelectable") != Some(&Value::Bool(false)))
                .collect();
            json!({"provider":provider,"models":models})
        }
        "inspect_provider" => {
            let (provider, model) = provider_model(string(&input, "provider")?, false)?;
            let cwd = super::workspace_tools::resolve_scoped_cwd(
                app,
                caller,
                input.get("cwd").and_then(Value::as_str),
            )
            .await?;
            let entry = tokio::select! {
                _ = cancel.cancelled() => bail!("Tool execution cancelled"),
                entry = app.providers.catalog(provider, Some(&cwd), true) => entry?,
            };
            if entry.get("enabled").and_then(Value::as_bool) != Some(true) {
                bail!("Provider '{provider}' is disabled");
            }
            if entry.get("status").and_then(Value::as_str) != Some("ready") {
                bail!(
                    "{}",
                    entry
                        .get("error")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("Provider '{provider}' is unavailable"))
                );
            }
            let settings = input.get("settings");
            let selected_model = settings
                .and_then(|value| value.get("model"))
                .and_then(Value::as_str)
                .or(model);
            let mut config = json!({"provider":provider,"cwd":cwd});
            if let Some(model) = selected_model.filter(|value| !value.is_empty()) {
                config["model"] = json!(model);
            }
            if let Some(settings) = settings {
                for (key, target) in [
                    ("modeId", "modeId"),
                    ("thinkingOptionId", "thinkingOptionId"),
                    ("features", "featureValues"),
                ] {
                    if let Some(value) = settings
                        .get(key)
                        .filter(|value| value.as_str().is_none_or(|value| !value.is_empty()))
                    {
                        config[target] = value.clone();
                    }
                }
            }
            let features = app.agents.list_draft_features(&config).await?;
            let summary = provider_summary(&entry)?;
            json!({
                "provider":provider, "label":summary["label"], "description":summary["description"],
                "enabled":summary["enabled"], "status":summary["status"], "modes":summary["modes"],
                "selectedModel":selected_model, "features":features
            })
        }
        _ => bail!("Unknown agent tool: {name}"),
    };
    Ok(result(structured))
}

fn provider_summary(entry: &Value) -> Result<Value> {
    let provider = string(entry, "provider")?;
    let status = string(entry, "status")?;
    let mut summary = json!({
        "id":provider,
        "label":entry.get("label").filter(|value| !value.is_null()).unwrap_or(&entry["provider"]),
        "description":entry.get("description").and_then(Value::as_str).unwrap_or(""),
        "enabled":entry["enabled"],
        "modes":entry.get("modes").filter(|value| !value.is_null()).cloned().unwrap_or_else(|| json!([])),
        "status":if status == "ready" { "available" } else { status }
    });
    if let Some(error) = entry
        .get("error")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        summary["error"] = json!(error);
    }
    Ok(summary)
}

fn retain_fields(input: &mut Value, fields: &[&str]) {
    if let Some(object) = input.as_object_mut() {
        object.retain(|key, _| fields.contains(&key.as_str()));
    }
}
