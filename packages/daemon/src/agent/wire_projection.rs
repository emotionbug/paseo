use serde_json::{Map, Value, json};

/// Whitelist the wire projection; configuration, owner, and provider MCP credentials remain internal.
pub(crate) fn snapshot_payload(agent: &Value) -> Value {
    let stored = agent["_live"] == false
        || (agent.get("lifecycle").is_none() && agent.get("status").is_none());
    let config = &agent["config"];
    let attention = agent.get("attention").unwrap_or(agent);
    let requires_attention = attention["requiresAttention"] == true;
    let thinking = config
        .get("thinkingOptionId")
        .or_else(|| agent.get("thinkingOptionId"))
        .cloned()
        .unwrap_or(Value::Null);
    let runtime = project_fields(
        &agent["runtimeInfo"],
        &[
            "provider",
            "sessionId",
            "model",
            "thinkingOptionId",
            "modeId",
            "extra",
        ],
    );
    let effective = crate::directory_sync::resolve_effective_thinking_option_id(
        Some(&runtime),
        Some(&thinking),
    );
    let status = agent
        .get("lifecycle")
        .or_else(|| agent.get("status"))
        .or_else(|| agent.get("lastStatus"))
        .cloned()
        .unwrap_or(Value::Null);
    let updated = agent
        .get("updatedAt")
        .or_else(|| agent.get("lastActivityAt"))
        .unwrap_or(&agent["createdAt"]);
    let mut out = json!({
        "id":agent["id"], "provider":agent["provider"], "cwd":agent["cwd"],
        "model":config.get("model").or_else(||agent.get("model")),
        "thinkingOptionId":thinking,"effectiveThinkingOptionId":effective,
        "createdAt":agent["createdAt"],"updatedAt":updated,"lastUserMessageAt":agent["lastUserMessageAt"],
        "status":status,"currentModeId":agent.get("currentModeId").or_else(||agent.get("lastModeId")),
        "availableModes":agent.get("availableModes").filter(|v|v.is_array()).cloned().unwrap_or_else(||json!([])),
        "pendingPermissions":agent.get("pendingPermissions").filter(|v|v.is_array()).cloned().unwrap_or_else(||json!([])),
        "title":agent.get("title").or_else(||config.get("title")), "archivedAt":agent["archivedAt"],
        "requiresAttention":requires_attention,
        "attentionReason":if requires_attention || stored {attention["attentionReason"].clone()} else {Value::Null},
        "attentionTimestamp":if requires_attention || stored {attention["attentionTimestamp"].clone()} else {Value::Null},
        "labels":agent["labels"].as_object().map(|labels|labels.iter().filter(|(_,v)|v.is_string()).map(|(k,v)|(k.clone(),v.clone())).collect::<Map<_,_>>()).unwrap_or_default()
    });
    out["capabilities"] = agent.get("capabilities").filter(|v|v.is_object()).cloned().unwrap_or_else(||json!({
        "supportsStreaming":false,"supportsSessionPersistence":true,"supportsDynamicModes":false,"supportsMcpServers":false,
        "supportsReasoningStream":false,"supportsToolInvocations":true,"supportsRewindConversation":false,"supportsRewindFiles":false,"supportsRewindBoth":false
    }));
    if !stored {
        out["features"] = agent
            .get("features")
            .filter(|v| v.is_array())
            .cloned()
            .unwrap_or_else(|| json!([]));
        out["activeTurn"] = agent.get("activeTurn").cloned().unwrap_or_else(|| {
            if let Some(turn) = agent.get("activeTurnId").filter(|v| v.is_string()) {
                json!({"turnId":turn,"startedAt":agent["activeTurnStartedAt"]})
            } else {
                Value::Null
            }
        });
    }
    for key in [
        "workspaceId",
        "lastError",
        "internal",
        "providerUnavailable",
    ] {
        if let Some(value) = agent.get(key).filter(|v| !v.is_null() && **v != false) {
            out[key] = value.clone();
        }
    }
    if !runtime.as_object().unwrap().is_empty() {
        out["runtimeInfo"] = runtime;
    }
    let usage_fields = [
        "inputTokens",
        "cachedInputTokens",
        "outputTokens",
        "totalCostUsd",
        "contextWindowMaxTokens",
        "contextWindowUsedTokens",
    ];
    if agent["lastUsage"].is_object()
        && usage_fields
            .iter()
            .all(|key| agent["lastUsage"][*key].is_null() || agent["lastUsage"][*key].is_number())
    {
        let usage = project_fields(&agent["lastUsage"], &usage_fields);
        if !usage.as_object().unwrap().is_empty() {
            out["lastUsage"] = usage;
        }
    }
    if agent["_providerAvailable"] == false {
        out["providerUnavailable"] = json!(true);
    }
    out["persistence"] = if agent["persistence"].is_object() && out["providerUnavailable"] != true {
        let mut handle = project_fields(
            &agent["persistence"],
            &["provider", "sessionId", "nativeHandle", "metadata"],
        );
        if let Some(metadata) = handle.get_mut("metadata").and_then(Value::as_object_mut) {
            metadata.remove("mcpServers");
            if metadata.is_empty() {
                handle.as_object_mut().unwrap().remove("metadata");
            }
        }
        handle
    } else {
        Value::Null
    };
    out
}
fn project_fields(source: &Value, keys: &[&str]) -> Value {
    Value::Object(
        keys.iter()
            .filter_map(|key| {
                source
                    .get(*key)
                    .map(|value| ((*key).to_owned(), value.clone()))
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strips_private_configuration_and_mcp_handles() {
        let result = snapshot_payload(
            &json!({"id":"a","config":{"model":"m","env":{"SECRET":"value"},"title":"Title"},"owner":{"token":"private"},"persistence":{"provider":"claude","sessionId":"s","metadata":{"mcpServers":{"password":"secret"},"cwd":"/tmp"}},"runtimeInfo":{"provider":"claude","sessionId":"s","thinkingOptionId":"  high  "},"lifecycle":"idle"}),
        );
        assert!(result.get("config").is_none());
        assert!(result.get("owner").is_none());
        assert_eq!(result["persistence"]["metadata"], json!({"cwd":"/tmp"}));
        assert_eq!(result["effectiveThinkingOptionId"], "high");
        assert_eq!(result["title"], "Title");
    }
    #[test]
    fn explicit_runtime_null_overrides_configured_thinking() {
        let result = snapshot_payload(
            &json!({"config":{"thinkingOptionId":"high"},"runtimeInfo":{"thinkingOptionId":null},"lastStatus":"closed","labels":{"valid":"yes","invalid":3}}),
        );
        assert!(result["effectiveThinkingOptionId"].is_null());
        assert_eq!(result["labels"], json!({"valid":"yes"}));
        assert!(result.get("activeTurn").is_none());
    }
    #[test]
    fn unavailable_stored_provider_does_not_advertise_resumption() {
        let result = snapshot_payload(
            &json!({"_live":false,"_providerAvailable":false,"lifecycle":"closed","status":"closed","persistence":{"provider":"removed-provider","sessionId":"session"},"features":[{"id":"private"}]}),
        );
        assert_eq!(result["providerUnavailable"], true);
        assert!(result["persistence"].is_null());
        assert!(result.get("features").is_none());
        assert!(result.get("activeTurn").is_none());
    }
    #[test]
    fn stream_projection_excludes_internal_lifecycle_events() {
        assert!(
            serialize_stream_event(
                &json!({"type":"runtime_info","provider":"claude","private":"value"})
            )
            .is_none()
        );
        let event = serialize_stream_event(&json!({"type":"turn_completed","provider":"claude","turnId":"turn","timestamp":"internal","usage":{"inputTokens":1}})).unwrap();
        assert!(event.get("timestamp").is_none());
        assert_eq!(event["usage"]["inputTokens"], 1);
    }
}

pub(crate) fn serialize_stream_event(event: &Value) -> Option<Value> {
    let fields: &[&str] = match event["type"].as_str()? {
        "thread_started" => &["type", "provider", "sessionId"],
        "turn_started" => &["type", "provider", "turnId"],
        "turn_completed" => &["type", "provider", "turnId", "usage"],
        "turn_failed" => &["type", "provider", "turnId", "error", "code", "diagnostic"],
        "turn_canceled" => &["type", "provider", "turnId", "reason"],
        "timeline" => &["type", "provider", "item", "turnId"],
        "permission_requested" => &["type", "provider", "request"],
        "permission_resolved" => &["type", "provider", "requestId", "resolution"],
        "attention_required" => &[
            "type",
            "provider",
            "reason",
            "timestamp",
            "shouldNotify",
            "notification",
        ],
        _ => return None,
    };
    let mut projected = project_fields(event, fields);
    if event["type"] == "attention_required" && projected.get("shouldNotify").is_none() {
        projected["shouldNotify"] = json!(false);
    }
    Some(projected)
}

pub(crate) fn project_legacy_event(
    context: &crate::transport::RequestContext,
    message: &Value,
) -> Option<Value> {
    if context.is_modern() {
        return None;
    }
    let supports = |name: &str| {
        context
            .capabilities()
            .as_array()
            .is_some_and(|values| values.iter().any(|v| v == name))
            || context.capabilities()[name] == true
    };
    let item_allowed = |item: &Value| match item["type"].as_str() {
        Some("notification") => supports("timeline_notifications"),
        Some("plugin") => supports("plugin_timeline_items"),
        _ => true,
    };
    let mut projected = message.clone();
    match message["type"].as_str()? {
        "agent_stream" => {
            if supports("selective_agent_timeline") {
                return None;
            }
            let event = &message["payload"]["event"];
            if event["type"] == "timeline" && !item_allowed(&event["item"]) {
                return None;
            }
            projected["payload"]["event"] = serialize_stream_event(event)?;
        }
        "agent.timeline.replacement" if !supports("timeline_replacement_invalidation") => {
            return None;
        }
        "agent.provider_subagents.update" => {
            if !supports("provider_subagents") {
                return None;
            }
            if message["payload"]["kind"] == "timeline"
                && (!supports("projected_subagent_timeline")
                    || !item_allowed(&message["payload"]["item"]))
            {
                return None;
            }
        }
        "agent_update" if message["payload"]["kind"] == "upsert" => {
            projected["payload"]["agent"] = snapshot_payload(&message["payload"]["agent"]);
        }
        "agent_update"
        | "agent_attention_required"
        | "agent_permission_request"
        | "agent_permission_resolved"
        | "activity_log"
        | "agent.timeline.replacement" => {}
        _ => return None,
    }
    Some(projected)
}
