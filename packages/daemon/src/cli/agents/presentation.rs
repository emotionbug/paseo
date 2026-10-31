use super::*;
fn model(agent: &Value) -> Option<&str> {
    [
        agent["runtimeInfo"]["model"].as_str(),
        agent["model"].as_str(),
    ]
    .into_iter()
    .flatten()
    .map(str::trim)
    .find(|v| !v.is_empty() && !v.eq_ignore_ascii_case("default"))
}
fn relative_time(date: &str) -> String {
    let Ok(date) = chrono::DateTime::parse_from_rfc3339(date) else {
        return "Invalid date".into();
    };
    let seconds = chrono::Utc::now().signed_duration_since(date).num_seconds();
    if seconds < 60 {
        "just now".into()
    } else if seconds < 3600 {
        format!("{} minutes ago", seconds / 60)
    } else if seconds < 86400 {
        format!("{} hours ago", seconds / 3600)
    } else {
        format!("{} days ago", seconds / 86400)
    }
}
pub(super) fn list_item(agent: &Value) -> Value {
    let cwd = text(agent, "cwd");
    let shortened = std::env::var("HOME")
        .ok()
        .and_then(|home| cwd.strip_prefix(&home).map(|rest| format!("~{rest}")))
        .unwrap_or_else(|| cwd.into());
    json!({"id":agent["id"],"shortId":short(text(agent,"id")),"name":agent["title"].as_str().unwrap_or("-"),"provider":model(agent).map(|model|format!("{}/{model}",text(agent,"provider"))).unwrap_or_else(||text(agent,"provider").into()),"thinking":agent["effectiveThinkingOptionId"].as_str().unwrap_or("auto"),"status":agent["status"],"cwd":shortened,"created":relative_time(text(agent,"createdAt"))})
}
pub(super) fn inspect(
    agent: &Value,
    options: &super::super::output::OutputOptions,
) -> Result<Output> {
    let usage = &agent["lastUsage"];
    let caps = &agent["capabilities"];
    let data = json!({"Id":agent["id"],"Name":agent["title"].as_str().unwrap_or("-"),"Provider":agent["provider"],"Model":model(agent).unwrap_or("-"),"Thinking":agent["effectiveThinkingOptionId"].as_str().unwrap_or("auto"),"Status":agent["status"],"Archived":!agent["archivedAt"].is_null(),"ArchivedAt":agent["archivedAt"],"Mode":agent["currentModeId"].as_str().unwrap_or("default"),"Cwd":agent["cwd"],"CreatedAt":agent["createdAt"],"UpdatedAt":agent["updatedAt"],
        "LastUsage":if usage.is_null(){Value::Null}else{json!({"InputTokens":usage["inputTokens"].as_u64().unwrap_or(0),"OutputTokens":usage["outputTokens"].as_u64().unwrap_or(0),"CachedTokens":usage["cachedInputTokens"].as_u64().unwrap_or(0),"CostUsd":usage["totalCostUsd"].as_f64().unwrap_or(0.0)})},
        "Capabilities":if caps.is_null(){Value::Null}else{json!({"Streaming":caps["supportsStreaming"].as_bool().unwrap_or(false),"Persistence":caps["supportsSessionPersistence"].as_bool().unwrap_or(false),"DynamicModes":caps["supportsDynamicModes"].as_bool().unwrap_or(false),"McpServers":caps["supportsMcpServers"].as_bool().unwrap_or(false)})},
        "AvailableModes":agent["availableModes"].as_array().map(|modes|modes.iter().map(|m|json!({"id":m["id"],"label":m["label"]})).collect::<Vec<_>>()),
        "PendingPermissions":array(&agent["pendingPermissions"]).iter().map(|p|json!({"id":p["id"],"tool":p["name"].as_str().unwrap_or("unknown")})).collect::<Vec<_>>(),"Worktree":agent["labels"]["paseo.worktree"],"ParentAgentId":agent["labels"]["paseo.parent-agent-id"]});
    if options.format != super::super::output::Format::Table && !options.quiet {
        return Ok(Output::new(data));
    }
    let mut rows = Vec::new();
    for key in [
        "Id",
        "Name",
        "Provider",
        "Model",
        "Thinking",
        "Status",
        "Archived",
        "ArchivedAt",
        "Mode",
        "Cwd",
        "CreatedAt",
        "UpdatedAt",
    ] {
        let mut value = data[key]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| data[key].to_string());
        if key == "Cwd" {
            if let Ok(home) = std::env::var("HOME") {
                if let Some(rest) = value.strip_prefix(&home) {
                    value = format!("~{rest}");
                }
            }
        }
        rows.push(json!({"key":key,"value":value}));
    }
    if data["LastUsage"].is_object() {
        let usage = &data["LastUsage"];
        let cost = usage["CostUsd"].as_f64().unwrap_or(0.0);
        let cost = if cost != 0.0 && cost < 0.01 {
            format!("${cost:.4}")
        } else {
            format!("${cost:.2}")
        };
        rows.push(json!({"key":"LastUsage","value":format!("InputTokens: {}, OutputTokens: {}, CachedTokens: {}, CostUsd: {cost}",usage["InputTokens"],usage["OutputTokens"],usage["CachedTokens"])}));
    }
    if data["Capabilities"].is_object() {
        let caps = &data["Capabilities"];
        rows.push(json!({"key":"Capabilities","value":format!("Streaming: {}, Persistence: {}, DynamicModes: {}, McpServers: {}",caps["Streaming"],caps["Persistence"],caps["DynamicModes"],caps["McpServers"])}));
    }
    if !array(&data["AvailableModes"]).is_empty() {
        rows.push(json!({"key":"AvailableModes","value":array(&data["AvailableModes"]).iter().map(|m|format!("{} ({})",text(m,"id"),text(m,"label"))).collect::<Vec<_>>().join(", ")}));
    }
    let permissions = array(&data["PendingPermissions"]);
    rows.push(json!({"key":"PendingPermissions","value":if permissions.is_empty(){"[]".into()}else{permissions.iter().map(|p|format!("{} ({})",text(p,"id"),text(p,"tool"))).collect::<Vec<_>>().join(", ")}}));
    for key in ["Worktree", "ParentAgentId"] {
        rows.push(json!({"key":key,"value":data[key].as_str().unwrap_or("null")}));
    }
    let quiet = rows
        .iter()
        .map(|r| text(r, "key"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut human_options = options.clone();
    human_options.format = super::super::output::Format::Table;
    human_options.quiet = false;
    let human = Output::new(json!(rows))
        .columns(&[("KEY", "key"), ("VALUE", "value")])
        .color_column("value", |value, row| {
            if row["key"] == "Status" {
                status_color(value, row)
            } else {
                None
            }
        })
        .render(&human_options)?;
    Ok(Output::new(data).human(human).quiet_text(quiet))
}
pub(super) fn status_color(value: &Value, _row: &Value) -> Option<u8> {
    match value.as_str() {
        Some("running") => Some(32),
        Some("idle") => Some(33),
        Some("error") => Some(31),
        _ => None,
    }
}
pub(super) fn run_result(agent: &Value, status: Option<&str>) -> Output {
    Output::new(json!({"agentId":agent["id"],"status":status.unwrap_or(if agent["status"]=="running"{"running"}else{"created"}),"provider":agent["provider"],"cwd":agent["cwd"],"title":agent["title"]})).id("agentId").columns(&[("AGENT ID","agentId"),("STATUS","status"),("PROVIDER","provider"),("CWD","cwd"),("TITLE","title")]).widths(&[12,10,10,30,20])
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inspect_preserves_nulls_and_effective_model() {
        let out = inspect(
            &json!({"id":"abc","model":"default","runtimeInfo":{"model":"actual"},"labels":{}}),
            &super::super::super::output::OutputOptions::default(),
        )
        .unwrap();
        assert_eq!(out.data["Model"], "actual");
        assert!(out.data["LastUsage"].is_null());
        assert_eq!(out.data["PendingPermissions"], json!([]));
    }
}
