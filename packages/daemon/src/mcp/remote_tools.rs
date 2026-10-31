use std::sync::Arc;

use anyhow::{Result, anyhow, bail};
use serde_json::{Map, Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::{
    state::AppState,
    transport::hosts::{HostClient, HostError},
};

pub(super) fn server_identity(app: &AppState) -> Result<String> {
    app.transport
        .identity()
        .get("serverId")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("Daemon server identity is not initialized"))
}

pub(super) async fn is_local(app: &Arc<AppState>, server_id: Option<&str>) -> Result<bool> {
    let local = server_identity(app)?;
    Ok(server_id.is_none_or(|destination| destination == local))
}

pub(super) async fn execute(
    app: &Arc<AppState>,
    caller: Option<&str>,
    name: &str,
    input: Value,
    cancel: CancellationToken,
    _updates: Option<mpsc::Sender<Value>>,
) -> Result<Value> {
    if name == "list_hosts" {
        return Ok(result(app.transport.hosts.list().await?));
    }
    let origin = server_identity(app)?;
    let destination = required(&input, "serverId")?;
    if destination == origin {
        return Err(super::input_error(
            "Local host operations must use the local tool handler",
        ));
    }
    let notify = caller.is_some() && input.get("notifyOnFinish") != Some(&Value::Bool(false));
    // Install notification intent before dispatch. An unacknowledged mutation must
    // keep that intent alive: closing the request transport does not undo admission.
    let registration = if name == "send_agent_prompt" && notify {
        Some(
            app.agents
                .register_finish_subscription(
                    json!({
                        "callerAgentId": caller,
                        "childAgentId": required(&input, "agentId")?,
                        "serverId": destination,
                    }),
                    true,
                )
                .await?,
        )
    } else {
        None
    };
    let mut prompt_submitted = false;
    let operation = async {
        if let Some(caller) = caller {
            if app
                .mcp
                .caller_context(caller)
                .await
                .as_ref()
                .and_then(|context| context.get("lockedCwd"))
                .and_then(Value::as_str)
                .is_some_and(|cwd| !cwd.is_empty())
            {
                bail!("This caller is restricted to its local working directory");
            }
        }
        let client = app.transport.hosts.tool_client(destination, cancel).await?;
        let operation = {
            let request = execute_remote(
                app,
                &client,
                caller,
                &origin,
                name,
                &input,
                &mut prompt_submitted,
            );
            tokio::pin!(request);
            loop {
                tokio::select! {
                    biased;
                    result = &mut request => break result,
                    event = client.next_event() => {
                        // This one-shot client owns no persistent observations, but
                        // must consume unsolicited frames while creation is running.
                        if let Err(error) = event { break Err(error); }
                    }
                }
            }
        };
        let closed = client.close().await;
        match operation {
            Err(error) => Err(error),
            Ok(value) => closed.map(|()| value),
        }
    }
    .await;
    if let Some(registration) = registration {
        let definite_failure = operation
            .as_ref()
            .err()
            .is_some_and(|error| !prompt_submitted || error.downcast_ref::<HostError>().is_some());
        let settled = if definite_failure {
            registration.reject().await
        } else {
            registration.accept().await
        };
        if let Err(error) = settled {
            return Ok(failure(destination, &error));
        }
    }
    match operation {
        Ok(mut structured) => {
            if name == "create_agent" && notify {
                let registered = app
                    .agents
                    .register_finish_subscription(
                        json!({
                            "callerAgentId": caller,
                            "childAgentId": required(&structured, "agentId")?,
                            "serverId": destination,
                            "requireParentOwnership": true,
                        }),
                        false,
                    )
                    .await;
                if let Err(error) = registered {
                    return Ok(failure(destination, &error));
                }
            }
            structured
                .as_object_mut()
                .ok_or_else(|| anyhow!("Remote tool result is not an object"))?
                .insert("serverId".into(), json!(destination));
            Ok(result(structured))
        }
        Err(error) => Ok(failure(destination, &error)),
    }
}

fn result(structured: Value) -> Value {
    json!({"content": [], "structuredContent": structured})
}

pub(super) fn failure(server_id: &str, error: &anyhow::Error) -> Value {
    let mut structured = json!({"serverId": server_id, "error": error.to_string()});
    if let Some(error) = error.downcast_ref::<HostError>() {
        structured["code"] = json!(error.code);
    }
    json!({"isError": true, "content": [{"type": "text", "text": structured.to_string()}], "structuredContent": structured})
}

fn required<'a>(input: &'a Value, key: &str) -> Result<&'a str> {
    input
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| super::input_error(format!("{key} is required")))
}

fn copy_fields(target: &mut Value, source: &Value, fields: &[(&str, &str)]) {
    for (from, to) in fields {
        if let Some(value) = source.get(*from) {
            target[*to] = value.clone();
        }
    }
}

fn require_feature(client: &HostClient, feature: &str, message: &str) -> Result<()> {
    if client.features().get(feature) != Some(&Value::Bool(true)) {
        bail!("{message}");
    }
    Ok(())
}

async fn request(client: &HostClient, message: Value, response_type: &str) -> Result<Value> {
    let mut response = client.request(message, response_type).await?;
    response
        .as_object_mut()
        .and_then(|response| response.remove("payload"))
        .filter(Value::is_object)
        .ok_or_else(|| anyhow!("Invalid {response_type} payload"))
}

fn payload_error(payload: &Value) -> Result<()> {
    if let Some(error) = payload
        .get("error")
        .and_then(Value::as_str)
        .filter(|error| !error.is_empty())
    {
        bail!("{error}");
    }
    Ok(())
}

async fn fetch_agent(client: &HostClient, agent_id: &str) -> Result<Value> {
    let mut response = request(
        client,
        json!({"type":"fetch_agent_request", "agentId":agent_id}),
        "fetch_agent_response",
    )
    .await?;
    payload_error(&response)?;
    response
        .as_object_mut()
        .and_then(|response| response.remove("agent"))
        .filter(Value::is_object)
        .ok_or_else(|| anyhow!("Agent {agent_id} not found"))
}

fn entries(mut payload: Value) -> Result<Vec<Value>> {
    payload_error(&payload)?;
    match payload
        .as_object_mut()
        .and_then(|payload| payload.remove("entries"))
    {
        Some(Value::Array(entries)) => Ok(entries),
        _ => bail!("Remote response has no entries array"),
    }
}

fn workspace_summary(workspace: &Value) -> Value {
    let kind = workspace.get("workspaceKind").and_then(Value::as_str);
    let mut summary = json!({
        "isolation": if kind == Some("worktree") { "worktree" } else { "local" },
    });
    copy_fields(
        &mut summary,
        workspace,
        &[
            ("id", "workspaceId"),
            ("projectId", "projectId"),
            ("workspaceDirectory", "cwd"),
            ("workspaceKind", "kind"),
            ("title", "title"),
            ("background", "background"),
        ],
    );
    if kind == Some("checkout") {
        summary["kind"] = json!("local_checkout");
    }
    summary
}

async fn execute_remote(
    app: &Arc<AppState>,
    client: &HostClient,
    caller: Option<&str>,
    origin: &str,
    name: &str,
    input: &Value,
    prompt_submitted: &mut bool,
) -> Result<Value> {
    match name {
        "create_agent" => create_agent(client, caller, origin, input).await,
        "create_workspace" => create_workspace(client, input).await,
        "list_workspaces" => {
            let background = input.get("includeBackground") == Some(&Value::Bool(true));
            if background {
                require_feature(
                    client,
                    "backgroundWorkspaces",
                    "Update the host to use background workspaces.",
                )?;
            }
            let workspaces = entries(
                request(
                    client,
                    json!({"type":"fetch_workspaces_request", "includeBackground":background}),
                    "fetch_workspaces_response",
                )
                .await?,
            )?;
            Ok(json!({"workspaces":workspaces.iter().map(workspace_summary).collect::<Vec<_>>()}))
        }
        "send_agent_prompt" => {
            let agent_id = required(input, "agentId")?;
            if caller.is_none() {
                if let Some(mode) = input.get("sessionMode") {
                    let response = request(
                        client,
                        json!({"type":"set_agent_mode_request", "agentId":agent_id,"modeId":mode}),
                        "set_agent_mode_response",
                    )
                    .await?;
                    if response.get("accepted") != Some(&Value::Bool(true)) {
                        bail!(
                            "{}",
                            response
                                .get("error")
                                .and_then(Value::as_str)
                                .unwrap_or("setAgentMode rejected")
                        );
                    }
                }
            }
            let agent = fetch_agent(client, agent_id).await?;
            let mut message = json!({
                "type":"send_agent_message_request", "agentId":agent_id,
                "text":input["prompt"], "messageId":uuid::Uuid::new_v4().to_string(),
                "activeTurnBehavior":input.get("activeTurnBehavior").cloned().unwrap_or_else(|| json!(if caller.is_some() {"steer"} else {"interrupt"})),
            });
            if let Some(caller) = caller {
                require_feature(
                    client,
                    "agentMessageProvenance",
                    "Update the Paseo host to send messages with agent provenance.",
                )?;
                require_feature(
                    client,
                    "agentMessageBatching",
                    "Update the Paseo host to send messages with a qualified agent address.",
                )?;
                message["sourceAgentId"] = json!(caller);
                message["sourceAgentServerId"] = json!(origin);
                copy_fields(&mut message, input, &[("sessionMode", "sessionMode")]);
            }
            *prompt_submitted = true;
            let response = request(client, message, "send_agent_message_response").await?;
            if response.get("accepted") != Some(&Value::Bool(true)) {
                *prompt_submitted = false;
                bail!(
                    "{}",
                    response
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("sendAgentMessage rejected")
                );
            }
            let mut receipt = json!({
                "success":true, "status":agent["status"],
                "guidance":if response.get("deliveryStatus").and_then(Value::as_str) == Some("queued") {
                    "Queued for delivery after two quiet seconds. You will get notified when the prompted agent finishes, errors, or needs permission unless notifyOnFinish is false."
                } else {
                    "Dispatch acknowledged. Observe completion using get_agent_status/get_agent_activity or SDK/RPC subscriptions and wait APIs."
                },
            });
            copy_fields(
                &mut receipt,
                &response,
                &[("deliveryStatus", "deliveryStatus")],
            );
            Ok(receipt)
        }
        "get_agent_status" => {
            let agent = fetch_agent(client, required(input, "agentId")?).await?;
            Ok(json!({"status":agent["status"], "snapshot":agent}))
        }
        "get_agent_activity" => {
            let agent_id = required(input, "agentId")?;
            let timeline = entries(request(client, json!({
                "type":"fetch_agent_timeline_request", "agentId":agent_id,
                "direction":"tail", "limit":input.get("limit").cloned().unwrap_or_else(|| json!(0)),
            }), "fetch_agent_timeline_response").await?)?;
            let items = timeline
                .into_iter()
                .map(|mut row| {
                    row.as_object_mut()
                        .and_then(|row| row.remove("item"))
                        .ok_or_else(|| anyhow!("Remote timeline entry has no item"))
                })
                .collect::<Result<Vec<_>>>()?;
            let agent = fetch_agent(client, agent_id).await?;
            let mut summary =
                crate::activity::summarize(&items, input.get("limit").and_then(Value::as_f64))?;
            summary["agentId"] = json!(agent_id);
            copy_fields(&mut summary, &agent, &[("currentModeId", "currentModeId")]);
            Ok(summary)
        }
        "list_agents" => {
            let background = input.get("includeBackground") == Some(&Value::Bool(true));
            if background {
                require_feature(
                    client,
                    "backgroundWorkspaces",
                    "Update the host to use background workspaces.",
                )?;
            }
            let rows = entries(request(client, json!({
                "type":"fetch_agents_request", "includeBackground":background,
                "filter":{"includeArchived":input.get("includeArchived") == Some(&Value::Bool(true))},
            }), "fetch_agents_response").await?)?;
            let agents = rows
                .into_iter()
                .map(|mut row| {
                    row.as_object_mut()
                        .and_then(|row| row.remove("agent"))
                        .filter(Value::is_object)
                        .ok_or_else(|| anyhow!("Remote agent entry has no snapshot"))
                })
                .collect::<Result<Vec<_>>>()?;
            let mut options = json!({});
            copy_fields(
                &mut options,
                input,
                &[
                    ("statuses", "statuses"),
                    ("sinceHours", "sinceHours"),
                    ("limit", "limit"),
                ],
            );
            if let Some(cwd) = input
                .get("cwd")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|cwd| !cwd.is_empty())
            {
                options["cwd"] =
                    json!(super::workspace_tools::expand_path(app, cwd, None)?.to_string_lossy());
            }
            Ok(json!({"agents":crate::agent::select_agent_list_items(agents, &options)?}))
        }
        _ => Err(super::input_error(format!("Unknown remote tool: {name}"))),
    }
}

async fn create_agent(
    client: &HostClient,
    caller: Option<&str>,
    origin: &str,
    input: &Value,
) -> Result<Value> {
    const ALLOWED: &[&str] = &[
        "serverId",
        "provider",
        "title",
        "initialPrompt",
        "workspaceId",
        "labels",
        "notifyOnFinish",
        "idempotencyKey",
        "settings",
    ];
    let object = input
        .as_object()
        .ok_or_else(|| super::input_error("Tool input must be an object"))?;
    if let Some(key) = object.keys().find(|key| !ALLOWED.contains(&key.as_str())) {
        return Err(super::input_error(format!(
            "Unrecognized remote create_agent argument: {key}"
        )));
    }
    let title = required(input, "title")?.trim();
    if title.is_empty() || title.encode_utf16().count() > 60 {
        return Err(super::input_error(
            "title must contain between 1 and 60 characters",
        ));
    }
    let provider = required(input, "provider")?.trim();
    let (provider, model) = provider
        .split_once('/')
        .map(|(provider, model)| (provider.trim(), model.trim()))
        .filter(|(provider, model)| !provider.is_empty() && !model.is_empty())
        .ok_or_else(|| {
            super::input_error("provider must be provider/model, for example codex/gpt-5.4")
        })?;
    let workspace_id = required(input, "workspaceId")?;
    let prompt = required(input, "initialPrompt")?;
    let workspaces = entries(
        request(
            client,
            json!({"type":"fetch_workspaces_request"}),
            "fetch_workspaces_response",
        )
        .await?,
    )?;
    let workspace = workspaces
        .iter()
        .find(|workspace| workspace.get("id").and_then(Value::as_str) == Some(workspace_id))
        .ok_or_else(|| anyhow!("Workspace {workspace_id} not found"))?;
    let mut labels = match input.get("labels") {
        Some(Value::Object(labels)) => Value::Object(labels.clone()),
        None => Value::Object(Map::new()),
        _ => return Err(super::input_error("labels must be a string record")),
    };
    if labels
        .as_object()
        .is_some_and(|labels| labels.values().any(|value| !value.is_string()))
    {
        return Err(super::input_error("labels must be a string record"));
    }
    if let Some(caller) = caller {
        labels["paseo.parent-agent-id"] = json!(caller);
        labels["paseo.parent-server-id"] = json!(origin);
    }
    let mut config = json!({"provider":provider,"model":model,"cwd":required(workspace,"workspaceDirectory")?,"title":title});
    if let Some(settings) = input.get("settings") {
        copy_fields(
            &mut config,
            settings,
            &[
                ("modeId", "modeId"),
                ("thinkingOptionId", "thinkingOptionId"),
                ("features", "featureValues"),
            ],
        );
    }
    let modern = client.features().get("creationLifecycle") == Some(&Value::Bool(true));
    let mut message = json!({
        "type":if modern {"agent.create.request"} else {"create_agent_request"},
        "config":config,"workspaceId":workspace_id,"labels":labels,"initialPrompt":prompt,
    });
    if modern {
        message["idempotencyKey"] = input
            .get("idempotencyKey")
            .cloned()
            .unwrap_or_else(|| json!(uuid::Uuid::new_v4().to_string()));
    }
    // Old hosts cannot accept an idempotency key together with an initial prompt.
    // Never replay either modern or legacy creation after a lost response.
    let mut response = request(
        client,
        message,
        if modern {
            "agent.create.response"
        } else {
            "status"
        },
    )
    .await?;
    payload_error(&response)?;
    let snapshot = response
        .as_object_mut()
        .and_then(|response| response.remove("agent"))
        .filter(Value::is_object)
        .ok_or_else(|| anyhow!("Agent creation failed"))?;
    let mut summary = json!({});
    copy_fields(
        &mut summary,
        &snapshot,
        &[
            ("id", "agentId"),
            ("provider", "type"),
            ("status", "status"),
            ("cwd", "cwd"),
            ("workspaceId", "workspaceId"),
            ("currentModeId", "currentModeId"),
            ("availableModes", "availableModes"),
        ],
    );
    required(&summary, "agentId")?;
    Ok(summary)
}

async fn create_workspace(client: &HostClient, input: &Value) -> Result<Value> {
    let isolation = required(input, "isolation")?;
    let path = required(input, "path")?;
    let mut source = match isolation {
        "local" => json!({"kind":"directory", "path":path}),
        "worktree" => {
            let mut source = json!({
                "kind":"worktree","cwd":path,
                "action":if matches!(input.get("mode").and_then(Value::as_str), Some("checkout-branch" | "checkout-pr")) {"checkout"} else {"branch-off"},
            });
            copy_fields(
                &mut source,
                input,
                &[
                    ("baseBranch", "baseBranch"),
                    ("branchName", "branchName"),
                    ("branch", "refName"),
                    ("prNumber", "githubPrNumber"),
                    ("worktreeSlug", "worktreeSlug"),
                ],
            );
            source
        }
        _ => return Err(super::input_error("isolation must be local or worktree")),
    };
    copy_fields(&mut source, input, &[("projectId", "projectId")]);
    if input.get("background").is_some() {
        require_feature(
            client,
            "backgroundWorkspaces",
            "Update the host to use background workspaces.",
        )?;
    }
    let mut message = json!({"type":"workspace.create.request","source":source});
    copy_fields(
        &mut message,
        input,
        &[("title", "title"), ("background", "background")],
    );
    if client.features().get("creationLifecycle") == Some(&Value::Bool(true))
        || client.features().get("workspaceRequestReceipts") == Some(&Value::Bool(true))
    {
        message["idempotencyKey"] = json!(uuid::Uuid::new_v4().to_string());
    }
    let mut response = request(client, message, "workspace.create.response").await?;
    payload_error(&response)?;
    let workspace = response
        .as_object_mut()
        .and_then(|response| response.remove("workspace"))
        .filter(Value::is_object)
        .ok_or_else(|| anyhow!("Workspace creation failed"))?;
    Ok(workspace_summary(&workspace))
}
