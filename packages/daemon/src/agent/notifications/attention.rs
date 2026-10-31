use super::FinishSubscriptions;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use regex::Regex;
use serde_json::{Value, json};
use std::sync::LazyLock;

const PRESENCE_THRESHOLD_MS: i64 = 180_000;
#[derive(Debug, PartialEq)]
struct Plan {
    recipient: Option<usize>,
    push: bool,
}
fn plan(
    states: &[Value],
    focus_field: &str,
    focus_id: &str,
    push_eligible: bool,
    now: i64,
) -> Plan {
    let mut latest: Option<(usize, i64)> = None;
    for (index, state) in states.iter().enumerate() {
        let Some(activity) = state["lastActivityAt"]
            .as_str()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.timestamp_millis().min(now))
        else {
            continue;
        };
        if now.saturating_sub(activity) > PRESENCE_THRESHOLD_MS {
            continue;
        }
        if state["appVisible"] == true && state[focus_field] == focus_id {
            return Plan {
                recipient: None,
                push: false,
            };
        }
        if latest.is_none_or(|(_, timestamp)| activity > timestamp) {
            latest = Some((index, activity));
        }
    }
    Plan {
        recipient: latest.map(|(index, _)| index),
        push: latest.is_none() && push_eligible,
    }
}
impl FinishSubscriptions {
    pub async fn heartbeat(
        &self,
        context: &crate::transport::RequestContext,
        _message: &Value,
    ) -> Result<()> {
        self.push.heartbeat(context).await
    }
    pub async fn handle(
        &self,
        context: &crate::transport::RequestContext,
        message: &Value,
    ) -> Result<bool> {
        self.push.handle(context, message).await
    }
    pub async fn attention(&self, snapshot: &Value, reason: &str, timestamp: &str) -> Result<()> {
        if snapshot["internal"] == true
            || snapshot["labels"]["paseo.parent-agent-id"]
                .as_str()
                .is_some_and(|id| !id.trim().is_empty())
        {
            return Ok(());
        }
        let Some(workspace) = snapshot["workspaceId"].as_str().filter(|id| !id.is_empty()) else {
            return Ok(());
        };
        let id = snapshot["id"]
            .as_str()
            .context("Attention snapshot has no agent id")?;
        let (transport, manager) = {
            let app = self
                .inner
                .app
                .upgrade()
                .context("Daemon is shutting down")?;
            (app.transport.clone(), app.agents.clone())
        };
        let candidates = transport
            .attention_sources("agent_attention_required", snapshot)
            .await?;
        let notification_sources = candidates
            .iter()
            .filter(|source| source.wants_notification("agent_attention_required"))
            .collect::<Vec<_>>();
        let states = notification_sources
            .iter()
            .map(|source| source.current_activity())
            .collect::<Vec<_>>();
        let decision = plan(
            &states,
            "focusedAgentId",
            id,
            reason != "error",
            Utc::now().timestamp_millis(),
        );
        let assistant = manager.last_assistant_message(id).await?;
        let identity = transport.identity();
        let server = identity["serverId"]
            .as_str()
            .context("Daemon server identity unavailable")?;
        let permission = snapshot["pendingPermissions"]
            .as_array()
            .and_then(|requests| requests.last());
        let notification = agent_payload(
            reason,
            server,
            workspace,
            id,
            assistant.as_deref(),
            permission,
        );
        if decision.push {
            if let Err(error) = self.push.enqueue(notification.clone()).await {
                tracing::warn!(
                    ?error,
                    agent_id = id,
                    "Failed to schedule push notification"
                );
            }
        }
        let notify_source = decision
            .recipient
            .map(|index| notification_sources[index].source_id());
        transport.publish_attention(json!({"type":"agent_attention_required","payload":{
            "agentId":id,"reason":reason,"timestamp":timestamp,"shouldNotify":false,"notification":notification
        }}), snapshot, notify_source).await
    }
    pub async fn notify_terminal_activity(&self, previous: &Value, terminal: &Value) -> Result<()> {
        let Some(reason) = terminal_reason(previous, &terminal["activity"]) else {
            return Ok(());
        };
        let id = terminal["id"]
            .as_str()
            .context("Terminal attention has no id")?;
        let cwd = terminal["cwd"]
            .as_str()
            .context("Terminal attention has no cwd")?;
        let transport = self
            .inner
            .app
            .upgrade()
            .context("Daemon is shutting down")?
            .transport
            .clone();
        let candidates = transport
            .attention_sources("terminal_attention_required", terminal)
            .await?;
        let notification_sources = candidates
            .iter()
            .filter(|source| source.wants_notification("terminal_attention_required"))
            .collect::<Vec<_>>();
        let states = notification_sources
            .iter()
            .map(|source| source.current_activity())
            .collect::<Vec<_>>();
        let decision = plan(
            &states,
            "focusedTerminalId",
            id,
            true,
            Utc::now().timestamp_millis(),
        );
        let title = if reason == "needs_input" {
            "Terminal needs input"
        } else {
            "Terminal finished"
        };
        let body = terminal["name"]
            .as_str()
            .context("Terminal attention has no name")?;
        let identity = transport.identity();
        let server = identity["serverId"]
            .as_str()
            .context("Daemon server identity unavailable")?;
        let mut data = json!({"serverId":server,"terminalId":id,"cwd":cwd});
        if let Some(workspace) = terminal["workspaceId"].as_str().filter(|s| !s.is_empty()) {
            data["workspaceId"] = json!(workspace);
        }
        if decision.push {
            if let Err(error) = self
                .push
                .enqueue(json!({"title":title,"body":body,"data":data}))
                .await
            {
                tracing::warn!(
                    ?error,
                    terminal_id = id,
                    "Failed to schedule terminal push notification"
                );
            }
        }
        let mut payload = data;
        payload["reason"] = json!(reason);
        payload["title"] = json!(title);
        payload["body"] = json!(body);
        payload["shouldNotify"] = json!(false);
        let notify_source = decision
            .recipient
            .map(|index| notification_sources[index].source_id());
        transport
            .publish_attention(
                json!({"type":"terminal_attention_required","payload":payload}),
                terminal,
                notify_source,
            )
            .await
    }
}
fn terminal_reason(previous: &Value, activity: &Value) -> Option<&'static str> {
    match activity["attentionReason"].as_str() {
        Some("finished") => Some("finished"),
        Some("needs_input") => Some("needs_input"),
        _ if activity["state"] == "attention" => Some("needs_input"),
        _ if previous["state"] == "working" && activity["state"] == "idle" => Some("finished"),
        _ => None,
    }
}
fn js_space(ch: char) -> bool {
    matches!(ch, '\u{0009}'..='\u{000d}' | ' ' | '\u{00a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}')
}
static MARKDOWN: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        (r"(?m)^\s*```[^\n]*$", ""), (r"(?m)^\s*~~~[^\n]*$", ""),
        (r"!\[([^\]]*)\]\((?:[^()\\]|\\.)*\)", "$1"), (r"\[([^\]]+)\]\((?:[^()\\]|\\.)*\)", "$1"),
        (r"(?m)^\s{0,3}#{1,6}\s+", ""), (r"(?m)^\s{0,3}>+\s?", ""),
        (r"(?m)^\s{0,3}(?:[*+-]|\d+\.)\s+", ""), (r"(?m)^\s{0,3}(?:[-*_]\s*){3,}$", ""),
        (r"`([^`]+)`", "$1"), (r"\*\*([^*]+)\*\*", "$1"), (r"__([^_]+)__", "$1"),
        (r"\*([^*\n]+)\*", "$1"), (r"_([^_\n]+)_", "$1"), (r"~~([^~]+)~~", "$1"), (r"<([^>\n]+)>", "$1"),
    ].into_iter().map(|(pattern, replacement)| {
        let pattern = pattern.replace(r"\s", r"[\u{0009}-\u{000d} \u{00a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}]").replace(r"\d", "[0-9]");
        (Regex::new(&pattern).expect("Static notification markdown pattern"), replacement)
    }).collect()
});
fn preview(text: Option<&str>) -> Option<String> {
    let mut text = text.filter(|s| !s.is_empty())?.replace("\r\n", "\n");
    for (pattern, replacement) in MARKDOWN.iter() {
        if let std::borrow::Cow::Owned(updated) = pattern.replace_all(&text, *replacement) {
            text = updated;
        }
    }
    let mut normalized = String::with_capacity(text.len());
    for part in text.split(js_space).filter(|s| !s.is_empty()) {
        if !normalized.is_empty() {
            normalized.push(' ');
        }
        normalized.push_str(part);
    }
    if normalized.is_empty() {
        return None;
    }
    if normalized.encode_utf16().count() <= 220 {
        return Some(normalized);
    }
    let prefix = String::from_utf16_lossy(&normalized.encode_utf16().take(217).collect::<Vec<_>>());
    let trimmed = prefix.trim_end_matches(js_space);
    Some(if trimmed.is_empty() {
        String::from_utf16_lossy(&normalized.encode_utf16().take(220).collect::<Vec<_>>())
    } else {
        format!("{trimmed}...")
    })
}
fn permission_details(request: &Value) -> Option<String> {
    let title = request["title"]
        .as_str()
        .map(|s| s.trim_matches(js_space))
        .filter(|s| !s.is_empty());
    let description = request["description"]
        .as_str()
        .map(|s| s.trim_matches(js_space))
        .filter(|s| !s.is_empty());
    match (title, description) {
        (Some(title), Some(description)) if title != description => {
            return Some(format!("{title} - {description}"));
        }
        (Some(title), _) => return Some(title.to_owned()),
        (_, Some(description)) => return Some(description.to_owned()),
        _ => {}
    }
    for key in ["input", "metadata"] {
        if let Some(value) = request.get(key).filter(|value| !value.is_null()) {
            return Some(value.to_string());
        }
    }
    request["name"]
        .as_str()
        .map(|s| s.trim_matches(js_space))
        .filter(|s| !s.is_empty())
        .or_else(|| request["kind"].as_str())
        .map(str::to_owned)
}
fn agent_payload(
    reason: &str,
    server: &str,
    workspace: &str,
    id: &str,
    assistant: Option<&str>,
    permission: Option<&Value>,
) -> Value {
    let (title, fallback) = match reason {
        "permission" => ("Agent needs permission", "Permission requested."),
        "error" => ("Agent needs attention", "Encountered an error."),
        _ => ("Agent finished", "Finished working."),
    };
    let body = match reason {
        "finished" => preview(assistant),
        "permission" => preview(permission.and_then(permission_details).as_deref()),
        _ => None,
    }
    .unwrap_or_else(|| fallback.to_owned());
    json!({"title":title,"body":body,"data":{"serverId":server,"workspaceId":workspace,"agentId":id,"reason":reason}})
}

#[cfg(test)]
mod tests {
    use super::*;
    fn state(at: i64, visible: bool, agent: &str) -> Value {
        json!({"lastActivityAt":DateTime::<Utc>::from_timestamp_millis(at).unwrap().to_rfc3339(),"appVisible":visible,"focusedAgentId":agent})
    }
    #[test]
    fn notification_plan_focus_presence_future_clamp_and_error_push_rules() {
        let now = 1_700_000_000_000;
        assert_eq!(
            plan(&[], "focusedAgentId", "a", true, now),
            Plan {
                recipient: None,
                push: true
            }
        );
        assert_eq!(
            plan(&[], "focusedAgentId", "a", false, now),
            Plan {
                recipient: None,
                push: false
            }
        );
        let states = [state(now - 100, false, "b"), state(now + 100, false, "a")];
        assert_eq!(
            plan(&states, "focusedAgentId", "a", true, now),
            Plan {
                recipient: Some(1),
                push: false
            }
        );
        assert_eq!(
            plan(&[state(now, true, "a")], "focusedAgentId", "a", true, now),
            Plan {
                recipient: None,
                push: false
            }
        );
        assert_eq!(
            plan(
                &[state(now - PRESENCE_THRESHOLD_MS - 1, true, "a")],
                "focusedAgentId",
                "a",
                true,
                now
            ),
            Plan {
                recipient: None,
                push: true
            }
        );
    }
    #[test]
    fn previews_preserve_message_content_without_markdown() {
        let payload = agent_payload(
            "finished",
            "server",
            "workspace",
            "agent",
            Some("# Done\n\n**fixed** [the bug](https://example.com)\n```rs\nlet x = 1;\n```"),
            None,
        );
        assert_eq!(payload["body"], "Done fixed the bug let x = 1;");
        assert_eq!(payload["title"], "Agent finished");
        assert_eq!(
            preview(Some(&"x".repeat(300))).unwrap(),
            format!("{}...", "x".repeat(217))
        );
        let permission = json!({"title":"Write file", "description":"Write file", "name":"Write", "kind":"tool"});
        assert_eq!(
            agent_payload("permission", "s", "w", "a", None, Some(&permission))["body"],
            "Write file"
        );
    }
    #[test]
    fn terminal_transitions_do_not_infer_unknown_as_idle() {
        assert_eq!(
            terminal_reason(&json!({"state":"working"}), &Value::Null),
            None
        );
        assert_eq!(
            terminal_reason(&json!({"state":"working"}), &json!({"state":"idle"})),
            Some("finished")
        );
        assert_eq!(
            terminal_reason(
                &Value::Null,
                &json!({"state":"idle","attentionReason":"needs_input"})
            ),
            Some("needs_input")
        );
    }
}
