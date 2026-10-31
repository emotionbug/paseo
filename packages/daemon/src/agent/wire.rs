use super::AgentManager;
use crate::transport::RequestContext;
use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};

#[path = "wire_creation.rs"]
mod creation;
#[path = "wire_observation.rs"]
mod observation;
#[path = "wire_projection.rs"]
mod projection;
pub(crate) use observation::legacy_attention_eligible;
pub(crate) use projection::{project_legacy_event, serialize_stream_event, snapshot_payload};

pub(super) async fn handle(
    manager: &AgentManager,
    context: &RequestContext,
    message: &Value,
) -> Result<bool> {
    if observation::handle(manager, context, message).await?
        || creation::handle(manager, context, message).await?
    {
        return Ok(true);
    }
    let kind = text(message, "type");
    let id = text(message, "agentId");
    match kind {
        "send_agent_message_request" => {
            let resolved = resolve(manager, id).await;
            let (target, result) = match resolved {
                Ok(Some(target)) => {
                    let mut input = json!({});
                    for field in [
                        "text",
                        "images",
                        "attachments",
                        "messageId",
                        "sourceAgentId",
                        "sourceAgentServerId",
                        "activeTurnBehavior",
                        "sessionMode",
                    ] {
                        if let Some(value) = message.get(field) {
                            input[field] = value.clone();
                        }
                    }
                    let result = manager.send(context.principal(), &target, input).await;
                    (target, result)
                }
                Ok(None) => (
                    id.to_owned(),
                    Err(anyhow!("Agent not found: {}", id.trim())),
                ),
                Err(error) => (id.to_owned(), Err(error)),
            };
            if context.cancellation().is_cancelled() {
                return Ok(true);
            }
            let mut payload = action_payload(message, &target, result);
            payload.as_object_mut().unwrap().remove("notice");
            reply(context, "send_agent_message_response", payload).await?;
        }
        "wait_for_finish_request" => {
            let resolved = resolve(manager, id).await;
            let target = resolved
                .as_ref()
                .ok()
                .and_then(|v| v.as_deref())
                .unwrap_or(id);
            let result = match &resolved {
                Ok(Some(target)) => {
                    let mut options = message.clone();
                    options.as_object_mut().unwrap().remove("timeoutMs");
                    let wait =
                        manager.wait(context.principal(), target, options, context.cancellation());
                    if let Some(timeout) = message["timeoutMs"].as_u64().filter(|value| *value > 0)
                    {
                        match tokio::time::timeout(std::time::Duration::from_millis(timeout), wait)
                            .await
                        {
                            Ok(result) => result,
                            Err(_) => Ok(json!({"status":"timeout","lastMessage":null})),
                        }
                    } else {
                        wait.await
                    }
                }
                Ok(None) => Err(anyhow!("Agent not found: {}", id.trim())),
                Err(error) => Err(anyhow!("{error}")),
            };
            if context.cancellation().is_cancelled() {
                return Ok(true);
            }
            let final_agent = manager
                .agent_snapshot(target)
                .await?
                .as_ref()
                .map(snapshot_payload);
            let payload = match result {
                Ok(value) => {
                    let status = if value["status"] == "timeout" {
                        "timeout"
                    } else if value["permission"].is_object() || value["status"] == "permission" {
                        "permission"
                    } else if value["status"] == "error" {
                        "error"
                    } else {
                        "idle"
                    };
                    let error = if status == "error" {
                        final_agent
                            .as_ref()
                            .and_then(|a| a.get("lastError"))
                            .filter(|v| v.as_str().is_some_and(|s| !s.trim().is_empty()))
                            .cloned()
                            .unwrap_or_else(|| json!("Agent failed"))
                    } else {
                        Value::Null
                    };
                    json!({"status":status,"final":final_agent,"error":error,"lastMessage":value["lastMessage"]})
                }
                Err(error) => {
                    json!({"status":"error","final":final_agent,"error":error.to_string(),"lastMessage":null})
                }
            };
            reply(
                context,
                "wait_for_finish_response",
                correlated(message, payload),
            )
            .await?;
        }
        "delete_agent_request" | "archive_agent_request" => {
            let action = if kind == "delete_agent_request" {
                "delete"
            } else {
                "archive"
            };
            let result = manager
                .control(context.principal(), id, action, message.clone())
                .await?;
            let mut payload = json!({"agentId":id});
            if action == "archive" {
                payload["archivedAt"] = result["archivedAt"].clone();
            }
            reply(
                context,
                if action == "delete" {
                    "agent_deleted"
                } else {
                    "agent_archived"
                },
                correlated(message, payload),
            )
            .await?;
        }
        "close_items_request" => {
            let mut agents = Vec::new();
            for agent_id in strings(&message["agentIds"]) {
                if let Ok(value) = manager
                    .control(context.principal(), agent_id, "archive", message.clone())
                    .await
                {
                    agents.push(json!({"agentId":agent_id,"archivedAt":value["archivedAt"]}));
                }
            }
            let app = manager.app()?;
            let mut terminals = Vec::new();
            for terminal_id in strings(&message["terminalIds"]) {
                terminals.push(
                    app.terminals
                        .kill_terminal_for_close(terminal_id)
                        .await
                        .unwrap_or_else(|_| json!({"terminalId":terminal_id,"success":false})),
                );
            }
            reply(
                context,
                "close_items_response",
                correlated(message, json!({"agents":agents,"terminals":terminals})),
            )
            .await?;
        }
        "update_agent_request"
        | "agent.detach.request"
        | "agent.rewind.request"
        | "set_agent_mode_request"
        | "set_agent_model_request"
        | "set_agent_feature_request"
        | "set_agent_thinking_request"
        | "agent.config.apply.request" => {
            let (action, response, failure) = match kind {
                "update_agent_request" => {
                    ("update", "update_agent_response", "Failed to update agent")
                }
                "agent.detach.request" => {
                    ("detach", "agent.detach.response", "Failed to detach agent")
                }
                "agent.rewind.request" => {
                    ("rewind", "agent.rewind.response", "Failed to rewind agent")
                }
                "set_agent_mode_request" => (
                    "set_mode",
                    "set_agent_mode_response",
                    "Failed to set agent mode",
                ),
                "set_agent_model_request" => (
                    "set_model",
                    "set_agent_model_response",
                    "Failed to set agent model",
                ),
                "set_agent_feature_request" => (
                    "set_feature",
                    "set_agent_feature_response",
                    "Failed to set agent feature",
                ),
                "set_agent_thinking_request" => (
                    "set_thinking",
                    "set_agent_thinking_response",
                    "Failed to set agent thinking option",
                ),
                _ => (
                    "config_apply",
                    "agent.config.apply.response",
                    "Failed to apply agent config",
                ),
            };
            let mut input = message.clone();
            if action == "rewind" {
                input["_initiatingSourceId"] = json!(context.source_id());
            }
            let result = manager
                .control(context.principal(), id, action, input)
                .await;
            if let Err(error) = &result {
                // updateAgentCommand reports "nothing to update" as a plain rejection.
                if !matches!(action, "detach" | "rewind")
                    && error.to_string() != super::actions::NOTHING_TO_UPDATE
                {
                    activity_error(manager, format!("{failure}: {error}")).await?;
                }
            }
            let mut payload = action_payload(message, id, result);
            if action == "rewind" {
                let accepted = payload.as_object_mut().unwrap().remove("accepted").unwrap();
                payload["ok"] = accepted;
            }
            reply(context, response, payload).await?;
        }
        "cancel_agent_request" => {
            let result = manager
                .control(context.principal(), id, "cancel", message.clone())
                .await;
            if message.get("requestId").is_some() {
                reply(context, "cancel_agent_response", correlated(message, json!({"agentId":id,"agent":manager.agent_snapshot(id).await?.as_ref().map(snapshot_payload),"error":result.err().map(|e|e.to_string())}))).await?;
            } else if let Err(error) = result {
                activity_error(
                    manager,
                    format!("Failed to cancel running agent on request: {error}"),
                )
                .await?;
            }
        }
        "agent_permission_response" => {
            if let Err(error) = manager
                .control(context.principal(), id, "permission", message.clone())
                .await
            {
                activity_error(manager, format!("Failed to respond to permission: {error}"))
                    .await?;
                return Err(error);
            }
            if context.is_modern() {
                reply(
                    context,
                    "agent_permission_resolved",
                    correlated(
                        message,
                        json!({"agentId":id,"resolution":message["response"]}),
                    ),
                )
                .await?;
            }
        }
        "clear_agent_attention" => {
            let ids = if let Some(id) = message["agentId"].as_str() {
                vec![id]
            } else {
                strings(&message["agentId"])
            };
            let mut agents = Vec::with_capacity(ids.len());
            for id in ids {
                // Source intentionally treats attention clearing as best effort, without a rejected response.
                if manager
                    .control(context.principal(), id, "clear_attention", message.clone())
                    .await
                    .is_err()
                {
                    return Ok(true);
                }
                if let Some(agent) = manager.agent_snapshot(id).await? {
                    agents.push(snapshot_payload(&agent));
                }
            }
            if message.get("requestId").is_some() {
                reply(
                    context,
                    "clear_agent_attention_response",
                    correlated(
                        message,
                        json!({"agentId":message["agentId"],"agents":agents}),
                    ),
                )
                .await?;
            }
        }
        "list_commands_request" => {
            let payload = match manager.commands(Some(id), &message["draftConfig"]).await {
                Ok(commands) => json!({"agentId":id,"commands":commands,"error":null}),
                Err(error) => json!({"agentId":id,"commands":[],"error":error.to_string()}),
            };
            reply(
                context,
                "list_commands_response",
                correlated(message, payload),
            )
            .await?;
        }
        _ => return Ok(false),
    }
    Ok(true)
}

fn action_payload(message: &Value, id: &str, result: Result<Value>) -> Value {
    let mut payload = match result {
        Ok(value) => {
            let mut payload = json!({"accepted":true,"error":null});
            for key in ["accepted", "error", "notice", "deliveryStatus"] {
                if let Some(field) = value.get(key) {
                    payload[key] = field.clone();
                }
            }
            payload
        }
        Err(error) => json!({"accepted":false,"error":error.to_string()}),
    };
    payload["agentId"] = json!(id);
    correlated(message, payload)
}
fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}
fn strings(value: &Value) -> Vec<&str> {
    value
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}
fn correlated(request: &Value, mut payload: Value) -> Value {
    if let Some(id) = request.get("requestId") {
        payload["requestId"] = id.clone();
    }
    payload
}
async fn reply(context: &RequestContext, kind: &str, payload: Value) -> Result<()> {
    context.send(json!({"type":kind,"payload":payload})).await
}
async fn rpc_error(
    context: &RequestContext,
    request: &Value,
    code: &str,
    error: &anyhow::Error,
) -> Result<()> {
    reply(context, "rpc_error", correlated(request, json!({"requestType":request["type"],"code":error_code(error,code),"error":error.to_string()}))).await
}
async fn activity_error(manager: &AgentManager, content: String) -> Result<()> {
    manager.app()?.transport.publish_event(json!({"type":"activity_log","payload":{"id":uuid::Uuid::new_v4().to_string(),"timestamp":chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis,true),"type":"error","content":content}})).await
}
async fn resolve(manager: &AgentManager, identifier: &str) -> Result<Option<String>> {
    let id = identifier.trim();
    if id.is_empty() {
        bail!("Agent identifier cannot be empty");
    }
    if manager.agent_snapshot(id).await?.is_some() {
        return Ok(Some(id.to_owned()));
    }
    let agents = manager.list_snapshots().await?;
    let visible: Vec<_> = agents.iter().filter(|a| a["internal"] != true).collect();
    for (label, matches) in [
        (
            "identifier",
            visible
                .iter()
                .filter(|a| text(a, "id").starts_with(id))
                .copied()
                .collect::<Vec<_>>(),
        ),
        (
            "title",
            visible
                .iter()
                .filter(|a| text(a, "title") == id)
                .copied()
                .collect::<Vec<_>>(),
        ),
    ] {
        if matches.len() == 1 {
            return Ok(Some(text(matches[0], "id").to_owned()));
        }
        if matches.len() > 1 {
            let names = matches
                .iter()
                .take(5)
                .map(|a| text(a, "id").chars().take(8).collect::<String>())
                .collect::<Vec<_>>()
                .join(", ");
            bail!(
                "Agent {label} \"{id}\" is ambiguous ({names}{})",
                if matches.len() > 5 { ", …" } else { "" }
            );
        }
    }
    Ok(None)
}

fn error_code(error: &anyhow::Error, fallback: &str) -> String {
    if let Some(error) = error.downcast_ref::<crate::agent::creation::AgentCreationError>() {
        return error.code.clone();
    }
    if let Some(error) = error.downcast_ref::<crate::projects::RequestError>() {
        return error.code.to_owned();
    }
    if let Some(error) = error.downcast_ref::<crate::projects::store::StoreError>() {
        return error.code.to_owned();
    }
    if error
        .downcast_ref::<crate::projects::worktrees::WorktreeError>()
        .is_some()
    {
        return crate::projects::worktrees::wire_error(error)["code"]
            .as_str()
            .unwrap_or(fallback)
            .to_owned();
    }
    fallback.to_owned()
}
