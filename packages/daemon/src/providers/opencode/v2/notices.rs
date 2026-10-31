use anyhow::{Result, bail};
use serde_json::{Value, json};

pub(super) fn initialize(config: &mut Value) -> Result<Vec<Value>> {
    let value = config
        .get("openCodeRuntimeNotices")
        .cloned()
        .unwrap_or_else(|| json!([]));
    let Some(mut notices) = value.as_array().cloned() else {
        bail!("Invalid OpenCode runtime notices");
    };
    for notice in &notices {
        if !matches!(notice["major"].as_i64(), Some(1 | 2)) {
            bail!("Invalid OpenCode runtime notice major");
        }
        let Some(time) = notice["timestamp"].as_str() else {
            bail!("Invalid OpenCode runtime notice timestamp");
        };
        if !time.ends_with('Z') || chrono::DateTime::parse_from_rfc3339(time).is_err() {
            bail!("Invalid OpenCode runtime notice timestamp");
        }
    }
    let added = notices.last().is_none_or(|notice| notice["major"] != 2);
    if added {
        notices.push(json!({"major":2,"timestamp":super::timeline::timestamp(&Value::Null)}));
    }
    let initial = if added {
        vec![entry(notices.last().unwrap())]
    } else {
        Vec::new()
    };
    config["openCodeRuntimeNotices"] = Value::Array(notices);
    Ok(initial)
}
fn entry(notice: &Value) -> Value {
    json!({"timestamp":notice["timestamp"],"item":{"type":"notification","level":"info","message":format!("This chat uses OpenCode v{}.",notice["major"])}})
}
pub(super) fn merge(config: &Value, history: Vec<Value>) -> Vec<Value> {
    let mut notices = super::array(&config["openCodeRuntimeNotices"])
        .iter()
        .peekable();
    let mut result = Vec::with_capacity(history.len() + notices.len());
    let event = |notice: &Value| {
        let mut row = entry(notice);
        row["type"] = json!("timeline");
        row["provider"] = json!("opencode");
        row
    };
    for row in history {
        if let Some(time) = row["timestamp"].as_str() {
            while notices
                .peek()
                .is_some_and(|notice| notice["timestamp"].as_str().is_some_and(|t| t <= time))
            {
                result.push(event(notices.next().unwrap()));
            }
        }
        result.push(row);
    }
    result.extend(notices.map(event));
    result
}
