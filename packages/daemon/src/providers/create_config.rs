use anyhow::{Result, anyhow, bail};
use serde_json::{Map, Value};

/// Preserves unknown modes versus an explicitly empty catalog and validates
/// only explicitly requested modes, as create-agent-mode.ts does.
pub fn resolve_default(input: &Value) -> Result<Value> {
    let provider = input
        .get("provider")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Missing target provider"))?;
    let modes = input.get("availableModes").and_then(Value::as_array);
    let requested = input.get("requestedMode").and_then(Value::as_str);
    let parent = input.get("parent").filter(|value| !value.is_null());
    let unattended = input.get("unattended").and_then(Value::as_bool) == Some(true);
    let unattended_mode = modes
        .and_then(|modes| modes.iter().find(|mode| mode["isUnattended"] == true))
        .and_then(|mode| mode.get("id"))
        .and_then(Value::as_str);
    let list_modes = || match modes {
        None => "unknown".to_owned(),
        Some(modes) if modes.is_empty() => "(none)".to_owned(),
        Some(modes) => modes
            .iter()
            .filter_map(|mode| mode.get("id").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(", "),
    };
    let mode = if let Some(requested) = requested {
        if modes.is_some_and(|modes| {
            !modes
                .iter()
                .any(|mode| mode.get("id").and_then(Value::as_str) == Some(requested))
        }) {
            bail!(
                "Invalid mode '{requested}' for provider '{provider}'. Available modes: {}",
                list_modes()
            );
        }
        Some(requested)
    } else if let Some(parent) = parent {
        if parent.get("provider").and_then(Value::as_str) == Some(provider) {
            parent.get("modeId").and_then(Value::as_str)
        } else if (unattended || parent["isUnattended"] == true) && unattended_mode.is_some() {
            unattended_mode
        } else if modes.is_some_and(Vec::is_empty) {
            None
        } else {
            let parent_mode = parent
                .get("modeId")
                .and_then(Value::as_str)
                .unwrap_or("<none>");
            let parent_provider = parent
                .get("provider")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("Missing parent provider"))?;
            bail!(
                "cannot inherit mode '{parent_mode}' from caller (provider '{parent_provider}') for new agent (provider '{provider}'). Pass an explicit mode. Available modes for '{provider}': {}",
                list_modes()
            );
        }
    } else if unattended {
        unattended_mode
    } else {
        None
    };
    let mut result = Map::new();
    if let Some(mode) = mode {
        result.insert("modeId".into(), Value::String(mode.to_owned()));
    }
    if let Some(features) = input.get("featureValues") {
        result.insert("featureValues".into(), features.clone());
    }
    Ok(Value::Object(result))
}

pub fn is_default_unattended(input: &Value) -> bool {
    let Some(mode) = input.get("modeId").and_then(Value::as_str) else {
        return false;
    };
    input
        .get("availableModes")
        .and_then(Value::as_array)
        .is_some_and(|modes| {
            modes.iter().any(|candidate| {
                candidate.get("id").and_then(Value::as_str) == Some(mode)
                    && candidate["isUnattended"] == true
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn refuses_cross_provider_mode_inheritance_without_known_safe_bridge() {
        let unknown = json!({"provider":"codex","parent":{"provider":"claude","modeId":"auto","isUnattended":false}});
        assert!(
            resolve_default(&unknown)
                .unwrap_err()
                .to_string()
                .contains("Pass an explicit mode")
        );
        let mut empty = unknown.clone();
        empty["availableModes"] = json!([]);
        assert_eq!(resolve_default(&empty).unwrap(), json!({}));
        let mut bridged = unknown;
        bridged["parent"]["isUnattended"] = json!(true);
        bridged["availableModes"] = json!([{"id":"full-access","isUnattended":true}]);
        assert_eq!(
            resolve_default(&bridged).unwrap(),
            json!({"modeId":"full-access"})
        );
    }

    #[test]
    fn explicit_mode_requires_membership_only_when_catalog_is_known() {
        let mut input = json!({"provider":"custom","requestedMode":"special","featureValues":{"auto_accept":false}});
        assert_eq!(
            resolve_default(&input).unwrap(),
            json!({"modeId":"special","featureValues":{"auto_accept":false}})
        );
        input["availableModes"] = json!([]);
        assert!(resolve_default(&input).is_err());
    }
}
