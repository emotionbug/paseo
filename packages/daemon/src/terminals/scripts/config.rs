use anyhow::{Result, anyhow};
use serde_json::Value;
use std::path::Path;

#[derive(Clone, Debug)]
pub(crate) struct ScriptConfig {
    pub command: String,
    pub service: bool,
    pub port: Option<f64>,
}

pub(crate) async fn read(cwd: &Path) -> Result<Value> {
    let path = cwd.join("paseo.json");
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Value::Null),
        Err(error) => {
            return Err(anyhow!(
                "Failed to parse paseo.json at {}: {error}",
                path.display()
            ));
        }
    };
    let mut value: Value = serde_json::from_slice(&bytes)
        .map_err(|error| anyhow!("Failed to parse paseo.json at {}: {error}", path.display()))?;
    if !value.is_object() {
        return Ok(serde_json::json!({}));
    }
    // PaseoConfigSchema is deliberately forgiving; an invalid worktree section
    // falls back as a whole, while invalid script entries are ignored separately.
    if let Some(worktree) = value.get_mut("worktree") {
        if !worktree.is_object()
            || worktree
                .get("servicePorts")
                .is_some_and(|v| !valid_allocation(v))
        {
            *worktree = serde_json::json!({"setup":[],"teardown":[]});
        }
        if let Some(allocation) = worktree
            .get_mut("servicePorts")
            .and_then(Value::as_object_mut)
        {
            for key in ["range", "portScript"] {
                if let Some(value) = allocation
                    .get(key)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .map(str::to_owned)
                {
                    allocation.insert(key.to_owned(), value.into());
                }
            }
        }
    }
    Ok(value)
}

/// `PaseoServicePortAllocationSchema`: a strict `{range?, portScript?}` object
/// carrying at least one valid field.
pub(crate) fn valid_allocation(v: &Value) -> bool {
    let Some(o) = v.as_object() else { return false };
    if o.keys().any(|k| k != "range" && k != "portScript") || o.is_empty() {
        return false;
    }
    if let Some(p) = o.get("portScript") {
        if p.as_str().is_none_or(|s| s.trim().is_empty()) {
            return false;
        }
    }
    if let Some(r) = o.get("range") {
        let Some((a, b)) = r.as_str().and_then(|s| s.trim().split_once('-')) else {
            return false;
        };
        if a.is_empty()
            || b.is_empty()
            || a.len() > 5
            || b.len() > 5
            || !a.bytes().chain(b.bytes()).all(|c| c.is_ascii_digit())
        {
            return false;
        }
        let (Ok(a), Ok(b)) = (a.parse::<u16>(), b.parse::<u16>()) else {
            return false;
        };
        if a == 0 || a > b {
            return false;
        }
    }
    true
}

pub(crate) fn commands(config: &Value, kind: &str) -> Vec<String> {
    let value = &config["worktree"][kind];
    match value {
        Value::String(s) if !s.trim().is_empty() => vec![s.clone()],
        Value::Array(a) => a
            .iter()
            .filter_map(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

pub(crate) fn scripts(config: &Value) -> Vec<(String, ScriptConfig)> {
    let mut result: Vec<_> = config["scripts"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(name, entry)| {
            let command = entry["command"].as_str()?.trim();
            if command.is_empty() {
                return None;
            }
            let service = entry["type"] == "service";
            Some((
                name.clone(),
                ScriptConfig {
                    command: command.to_owned(),
                    service,
                    port: if service {
                        entry["port"].as_f64().filter(|p| p.is_finite())
                    } else {
                        None
                    },
                },
            ))
        })
        .collect();
    // Object.entries enumerates canonical uint32 index keys before authored keys.
    result.sort_by(
        |(left, _), (right, _)| match (array_index(left), array_index(right)) {
            (Some(a), Some(b)) => a.cmp(&b),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        },
    );
    result
}

fn array_index(value: &str) -> Option<u32> {
    if value.is_empty() || (value.len() > 1 && value.starts_with('0')) {
        return None;
    }
    value
        .parse::<u32>()
        .ok()
        .filter(|n| *n < u32::MAX && value.bytes().all(|b| b.is_ascii_digit()))
}

pub(crate) fn terminals(config: &Value) -> Vec<(Option<String>, String)> {
    config["worktree"]["terminals"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let command = entry["command"].as_str()?.trim();
            if command.is_empty() {
                return None;
            }
            let name = entry["name"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned);
            Some((name, command.to_owned()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn lifecycle_keeps_authored_whitespace_and_filters_non_commands() {
        assert_eq!(
            commands(
                &json!({"worktree":{"setup":["  echo hi  "," ",4]}}),
                "setup"
            ),
            vec!["  echo hi  "]
        );
    }
    #[test]
    fn script_type_is_opt_in() {
        let parsed = scripts(
            &json!({"scripts":{"a":{"type":"unknown","command":" x "},"b":{"type":"service","command":" y ","port":4321},"c":{"command":" "}}}),
        );
        assert_eq!(parsed.len(), 2);
        assert!(!parsed[0].1.service);
        assert_eq!(parsed[1].1.port, Some(4321.0));
    }
    #[test]
    fn service_ports_retain_finite_numbers_and_authored_script_order() {
        let value:Value=serde_json::from_str(r#"{"scripts":{"z":{"command":"z","type":"service","port":4321.0},"a":{"command":"a","type":"service","port":-3.5},"12":{"command":"twelve"},"2":{"command":"two"}}}"#).unwrap();
        let parsed = scripts(&value);
        assert_eq!(
            parsed
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            vec!["2", "12", "z", "a"]
        );
        assert_eq!(parsed[2].1.port, Some(4321.0));
        assert_eq!(parsed[3].1.port, Some(-3.5));
    }
    #[tokio::test]
    async fn missing_invalid_and_forgiving_config_are_distinct() {
        let directory = tempfile::tempdir().unwrap();
        assert_eq!(read(directory.path()).await.unwrap(), Value::Null);
        let path = directory.path().join("paseo.json");
        tokio::fs::write(&path,br#"{"worktree":{"setup":"echo ok","servicePorts":{"range":"0-1"}},"scripts":{"dev":{"command":"run"}}}"#).await.unwrap();
        let parsed = read(directory.path()).await.unwrap();
        assert!(commands(&parsed, "setup").is_empty());
        assert_eq!(scripts(&parsed)[0].1.command, "run");
        tokio::fs::write(&path, b"{invalid").await.unwrap();
        assert!(
            read(directory.path())
                .await
                .unwrap_err()
                .to_string()
                .contains("Failed to parse paseo.json")
        );
    }
}
