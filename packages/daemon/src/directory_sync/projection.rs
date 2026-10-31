use serde_json::Value;

/// Equality of JSON.stringify output for JSON-valued source projections.
/// Numeric object keys are emitted in index order, all other keys in insertion
/// order. Numbers have ECMAScript Number semantics; -0 and 0 serialize alike.
/// Comparing structurally avoids serializing/copying every unchanged snapshot.
pub fn same_json_fingerprint(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(left), Value::Bool(right)) => left == right,
        (Value::String(left), Value::String(right)) => left == right,
        (Value::Number(left), Value::Number(right)) => left.as_f64() == right.as_f64(),
        (Value::Array(left), Value::Array(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| same_json_fingerprint(left, right))
        }
        (Value::Object(left), Value::Object(right)) => {
            if left.len() != right.len() {
                return false;
            }
            for (key, value) in left.iter().filter(|(key, _)| is_array_index(key)) {
                if !right
                    .get(key)
                    .is_some_and(|other| same_json_fingerprint(value, other))
                {
                    return false;
                }
            }
            let mut left = left.iter().filter(|(key, _)| !is_array_index(key));
            let mut right = right.iter().filter(|(key, _)| !is_array_index(key));
            loop {
                match (left.next(), right.next()) {
                    (None, None) => return true,
                    (Some((lk, lv)), Some((rk, rv)))
                        if lk == rk && same_json_fingerprint(lv, rv) => {}
                    _ => return false,
                }
            }
        }
        _ => false,
    }
}

fn is_array_index(key: &str) -> bool {
    if key == "0" {
        return true;
    }
    if key.starts_with('0') || key.is_empty() {
        return false;
    }
    key.bytes().all(|byte| byte.is_ascii_digit())
        && key.parse::<u32>().is_ok_and(|number| number != u32::MAX)
}

fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(value)) => *value,
        Some(Value::String(value)) => !value.is_empty(),
        Some(Value::Number(value)) => value.as_f64().is_some_and(|value| value != 0.0),
        _ => true,
    }
}

fn js_trim(value: &str) -> &str {
    value.trim_matches(|character: char| {
        matches!(character,
        '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' |
        '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' |
        '\u{205f}' | '\u{3000}' | '\u{feff}')
    })
}

pub fn resolve_effective_thinking_option_id<'a>(
    runtime_info: Option<&'a Value>,
    configured: Option<&'a Value>,
) -> Option<&'a str> {
    let value = runtime_info
        .and_then(|runtime| runtime.get("thinkingOptionId"))
        .or(configured);
    value
        .and_then(Value::as_str)
        .map(js_trim)
        .filter(|value| !value.is_empty())
}

/// Workspace ownership is supplied by the caller; a missing registry entry does
/// not hide the agent. Internal agents are never public directory entries.
pub fn agent_directory_includes(
    agent: &Value,
    workspace: Option<&Value>,
    include_background: bool,
) -> bool {
    !truthy(agent.get("internal"))
        && (include_background
            || !truthy(agent.get("workspaceId"))
            || !workspace.is_some_and(|workspace| truthy(workspace.get("background"))))
}

pub fn workspace_directory_includes(workspace: &Value, include_background: bool) -> bool {
    include_background || !truthy(workspace.get("background"))
}

pub fn matches_workspace_filter(workspace: &Value, filter: Option<&Value>) -> bool {
    let include_background = filter
        .and_then(|filter| filter.get("includeBackground"))
        .and_then(Value::as_bool)
        == Some(true);
    let exact_query = filter
        .and_then(|filter| filter.get("query"))
        .and_then(Value::as_str);
    if truthy(workspace.get("background"))
        && !include_background
        && exact_query != workspace.get("id").and_then(Value::as_str)
    {
        return false;
    }
    let Some(filter) = filter else {
        return true;
    };
    if let Some(project_id) = filter
        .get("projectId")
        .and_then(Value::as_str)
        .map(js_trim)
        .filter(|id| !id.is_empty())
    {
        if workspace.get("projectId").and_then(Value::as_str) != Some(project_id) {
            return false;
        }
    }
    if let Some(query) = filter
        .get("query")
        .and_then(Value::as_str)
        .map(js_trim)
        .filter(|query| !query.is_empty())
    {
        let query = crate::json::locale_lowercase(query);
        if !["name", "projectId", "id"].iter().any(|key| {
            workspace
                .get(*key)
                .and_then(Value::as_str)
                .is_some_and(|value| crate::json::locale_lowercase(value).contains(&query))
        }) {
            return false;
        }
    }
    true
}

pub fn matches_agent_updates_filter(
    agent: &Value,
    project: &Value,
    filter: Option<&Value>,
) -> bool {
    let include_archived = filter
        .and_then(|filter| filter.get("includeArchived"))
        .and_then(Value::as_bool)
        == Some(true);
    if !include_archived && truthy(agent.get("archivedAt")) {
        return false;
    }
    if truthy(agent.get("internal"))
        && filter
            .and_then(|filter| filter.get("includeInternal"))
            .and_then(Value::as_bool)
            != Some(true)
    {
        return false;
    }
    let Some(filter) = filter else {
        return true;
    };
    if let Some(labels) = filter.get("labels").and_then(Value::as_object) {
        if !labels.iter().all(|(key, value)| {
            agent.get("labels").and_then(|labels| labels.get(key)) == Some(value)
        }) {
            return false;
        }
    }
    if let Some(expected) = filter.get("thinkingOptionId") {
        let expected = resolve_effective_thinking_option_id(None, Some(expected));
        let effective = agent
            .get("effectiveThinkingOptionId")
            .filter(|value| !value.is_null());
        let actual = match effective {
            Some(value) => value.as_str(),
            None => resolve_effective_thinking_option_id(
                agent.get("runtimeInfo"),
                agent.get("thinkingOptionId"),
            ),
        };
        if expected != actual {
            return false;
        }
    }
    if let Some(statuses) = filter.get("statuses").and_then(Value::as_array) {
        if !statuses.is_empty()
            && !agent
                .get("status")
                .is_some_and(|status| statuses.contains(status))
        {
            return false;
        }
    }
    if let Some(expected) = filter.get("requiresAttention").and_then(Value::as_bool) {
        if agent
            .get("requiresAttention")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            != expected
        {
            return false;
        }
    }
    if let Some(keys) = filter.get("projectKeys").and_then(Value::as_array) {
        let mut meaningful = keys
            .iter()
            .filter_map(Value::as_str)
            .filter(|key| !js_trim(key).is_empty())
            .peekable();
        if meaningful.peek().is_some()
            && !meaningful.any(|key| project.get("projectKey").and_then(Value::as_str) == Some(key))
        {
            return false;
        }
    }
    true
}
