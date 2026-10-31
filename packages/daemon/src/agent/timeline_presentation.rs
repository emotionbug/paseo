use super::utf16_prefix;
use serde_json::{Value, json};
fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}
fn summary(text: &str) -> String {
    let partial = text.encode_utf16().count() > 480;
    let collapsed = utf16_prefix(text, 480)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if collapsed.is_empty() {
        return collapsed;
    }
    if collapsed.encode_utf16().count() <= 120 && !partial {
        return collapsed;
    }
    utf16_prefix(&collapsed, 119).trim_end().to_owned() + "…"
}
fn humanize(name: &str) -> String {
    let sentence = name
        .split(['.', '_', '-'])
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let mut chars = sentence.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => sentence,
    }
}
fn leaf(name: &str) -> Option<String> {
    let name = name.trim().to_lowercase();
    if name
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|s| !s.is_empty())
        .next_back()
        == Some("speak")
    {
        return None;
    }
    if name.contains("__") {
        let segments: Vec<&str> = name.split("__").filter(|s| !s.is_empty()).collect();
        if segments.len() >= 3
            && segments[0] == "mcp"
            && (segments[1] == "paseo" || segments[1].starts_with("paseo_"))
        {
            return Some(segments[2..].join("__"));
        }
    } else if let Some((namespace, leaf)) = name.split_once('.') {
        if namespace == "paseo" || namespace.starts_with("paseo_") {
            return Some(leaf.into());
        }
    }
    None
}
pub(super) fn external_tool_name(name: &str) -> bool {
    let name = name.trim().to_lowercase();
    if name.is_empty() {
        return false;
    }
    if name
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|s| !s.is_empty())
        .next_back()
        == Some("speak")
    {
        return true;
    }
    if name.contains(['.', ':', '/']) {
        return true;
    }
    let segments: Vec<&str> = name.split("__").filter(|s| !s.is_empty()).collect();
    segments.len() >= 3 || (segments.len() == 2 && segments[1].contains('_'))
}
pub(super) fn agent_message_display(message: &Value) -> (String, String) {
    let label = match text(message, "event") {
        "message" => "Message received",
        "finished" => "Agent finished",
        "errored" => "Agent errored",
        "permission-required" => "Agent needs permission",
        "closed" => "Agent closed",
        _ => "",
    };
    let sender = &message["sender"];
    let name = sender
        .get("title")
        .filter(|v| !v.is_null())
        .and_then(Value::as_str)
        .unwrap_or_else(|| text(sender, "id"));
    (label.into(), summary(name))
}
fn present(value: &Value) -> bool {
    value.as_str().is_some_and(|s| !s.trim().is_empty())
        || value.as_array().is_some_and(|a| !a.is_empty())
        || value.is_number()
}
fn first<'a>(input: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().map(|key| &input[*key]).find(|v| present(v))
}
fn context(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => summary(s),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Array(values)) => {
            let head = values
                .first()
                .and_then(Value::as_str)
                .map(summary)
                .unwrap_or_default();
            if !head.is_empty() && values.len() > 1 {
                format!("{head} +{}", values.len() - 1)
            } else {
                head
            }
        }
        _ => String::new(),
    }
}
fn paseo_display(name: &str, input: &Value, target: Option<&Value>) -> (String, String) {
    let (label, keys): (&str, &[&str]) = match name {
        "create_agent" => ("Create agent", &["title", "initialPrompt"]),
        "send_agent_prompt" => ("Send message", &["prompt"]),
        "get_agent_status" => ("Check agent", &["agentId"]),
        "get_agent_activity" => ("Read agent activity", &["agentId"]),
        "list_agents" => ("List agents", &[]),
        "update_agent" => ("Update agent", &["name", "agentId"]),
        "set_agent_mode" => ("Set agent mode", &["modeId"]),
        "cancel_agent" => ("Cancel agent", &["agentId"]),
        "archive_agent" => ("Archive agent", &["agentId"]),
        "kill_agent" => ("Kill agent", &["agentId"]),
        "list_pending_permissions" => ("List permissions", &[]),
        "respond_to_permission" => ("Respond to permission", &["agentId"]),
        "create_workspace" => ("Create workspace", &["title", "branchName", "branch"]),
        "list_hosts" => ("List hosts", &[]),
        "list_workspaces" => ("List workspaces", &[]),
        "rename_workspace" => ("Rename workspace", &["title"]),
        "archive_workspace" => ("Archive workspace", &["workspaceId"]),
        "list_workspace_scripts" => ("List scripts", &[]),
        "start_workspace_script" => ("Start script", &["scriptName"]),
        "stop_workspace_script" => ("Stop script", &["scriptName"]),
        "create_terminal" => ("Create terminal", &["name", "command"]),
        "list_terminals" => ("List terminals", &[]),
        "capture_terminal" => ("Read terminal", &["terminalId"]),
        "send_terminal_keys" => ("Send keys", &["keys"]),
        "kill_terminal" => ("Kill terminal", &["terminalId"]),
        "create_schedule" => ("Create schedule", &["name", "cron"]),
        "create_heartbeat" => ("Create heartbeat", &["name", "cron"]),
        "update_schedule" => ("Update schedule", &["name", "id"]),
        "list_schedules" => ("List schedules", &[]),
        "inspect_schedule" => ("Inspect schedule", &["id"]),
        "schedule_logs" => ("Read schedule logs", &["id"]),
        "run_schedule_once" => ("Run schedule", &["id"]),
        "pause_schedule" => ("Pause schedule", &["id"]),
        "resume_schedule" => ("Resume schedule", &["id"]),
        "delete_schedule" => ("Delete schedule", &["id"]),
        "delete_heartbeat" => ("Delete heartbeat", &["id"]),
        "list_providers" => ("List providers", &[]),
        "list_models" => ("List models", &["provider"]),
        "list_profiles" => ("List profiles", &[]),
        "inspect_provider" => ("Inspect provider", &["provider"]),
        "browser_list_tabs" => ("List browser tabs", &[]),
        "browser_new_tab" => ("Open browser tab", &["url"]),
        "browser_close_tab" => ("Close browser tab", &["browserId"]),
        "browser_navigate" => ("Navigate", &["url"]),
        "browser_back" => ("Go back", &[]),
        "browser_forward" => ("Go forward", &[]),
        "browser_reload" => ("Reload page", &[]),
        "browser_snapshot" => ("Snapshot page", &[]),
        "browser_screenshot" => ("Take screenshot", &[]),
        "browser_click" => ("Click element", &["ref"]),
        "browser_hover" => ("Hover element", &["ref"]),
        "browser_drag" => ("Drag element", &["sourceRef"]),
        "browser_fill" => ("Fill field", &["ref"]),
        "browser_select" => ("Select option", &["value", "ref"]),
        "browser_type" => ("Type text", &["text"]),
        "browser_keypress" => ("Press key", &["key"]),
        "browser_upload" => ("Upload files", &["filePaths"]),
        "browser_wait" => ("Wait for page", &["text", "url"]),
        "browser_scroll" => ("Scroll page", &[]),
        "browser_resize" => ("Resize browser", &[]),
        "browser_logs" => ("Read browser logs", &[]),
        "browser_evaluate" => ("Run JavaScript", &["function"]),
        _ => return (humanize(name), String::new()),
    };
    let value = keys.iter().find_map(|key| {
        if *key == "agentId" {
            target
                .filter(|v| !v.is_null())
                .or_else(|| first(input, &[key]))
        } else {
            first(input, &[key])
        }
    });
    let mut summary = context(value);
    if name == "create_workspace" && value.is_none() {
        summary = text(input, "path")
            .split(['/', '\\'])
            .rfind(|s| !s.is_empty())
            .map(super::presentation::summary)
            .unwrap_or_default();
    }
    if name == "browser_resize" && input["width"].is_number() && input["height"].is_number() {
        summary = format!("{}×{}", input["width"], input["height"]);
    }
    (label.into(), summary)
}
/// Canonical presentation shared by curation and other daemon consumers.
pub fn tool_display(item: &Value) -> Value {
    let name = text(item, "name");
    let detail = &item["detail"];
    let (display_name, summary) = if item["agentMessage"].is_object() {
        agent_message_display(&item["agentMessage"])
    } else if let Some(leaf) = leaf(name) {
        paseo_display(
            &leaf,
            if detail["type"] == "unknown" {
                &detail["input"]
            } else {
                &Value::Null
            },
            item.get("targetAgentTitle"),
        )
    } else {
        let (label, mut summary) = match text(detail, "type") {
            "shell" => ("Shell", text(detail, "command")),
            "read" => ("Read", text(detail, "filePath")),
            "edit" => ("Edit", text(detail, "filePath")),
            "write" => ("Write", text(detail, "filePath")),
            "search" => ("Search", text(detail, "query")),
            "fetch" => ("Fetch", text(detail, "url")),
            "worktree_setup" => ("Worktree setup", text(detail, "branchName")),
            "sub_agent" => (
                if text(detail, "subAgentType").is_empty() {
                    "Task"
                } else {
                    text(detail, "subAgentType")
                },
                text(detail, "description"),
            ),
            "plan" => ("Plan", ""),
            "plain_text" => ("", text(detail, "label")),
            _ => ("", ""),
        };
        if matches!(text(detail, "type"), "read" | "edit" | "write") {
            if let Some(cwd) = item["cwd"].as_str().filter(|s| !s.is_empty()) {
                let cwd = cwd.trim_end_matches('/');
                if summary == cwd {
                    summary = ".";
                } else if let Some(rest) =
                    summary.strip_prefix(cwd).and_then(|p| p.strip_prefix('/'))
                {
                    summary = rest;
                }
            }
        }
        let mut label = if label.is_empty() {
            let trimmed = name.trim();
            if trimmed.contains([':', '.', '/']) || trimmed.contains("__") {
                trimmed.into()
            } else {
                humanize(name)
            }
        } else {
            label.to_owned()
        };
        let normalized = name.trim().to_lowercase();
        if detail["type"] == "unknown" && normalized == "task" {
            label = "Task".into();
            summary = text(&item["metadata"], "subAgentActivity");
        } else if detail["type"] == "unknown" && normalized == "thinking" {
            label = "Thinking".into();
        } else if normalized == "terminal" {
            label = "Terminal".into();
            if detail["type"] == "plain_text" {
                summary = text(detail, "label");
            }
        }
        (label, summary.into())
    };
    let mut out = json!({"displayName":display_name});
    if !summary.is_empty() {
        out["summary"] = json!(summary);
    }
    if item["status"] == "failed" && !item["error"].is_null() {
        let error = &item["error"];
        let text = error
            .as_str()
            .or_else(|| error["content"].as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| serde_json::to_string_pretty(error).unwrap_or_default());
        if !text.is_empty() {
            out["errorText"] = json!(text);
        }
    }
    out
}
