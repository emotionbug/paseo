use anyhow::{Context, Result};
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;

pub const RELOADABLE: &[(&str, &str)] = &[
    ("daemon.relay.enabled", "relay.enabled"),
    (
        "daemon.relay.requireProtectedSecurity",
        "relay.requireProtectedSecurity",
    ),
    ("daemon.mcp.enabled", "mcp.enabled"),
    ("daemon.mcp.injectIntoAgents", "mcp.injectIntoAgents"),
    ("daemon.hostnames", "hostnames"),
    ("daemon.cors.allowedOrigins", "cors.allowedOrigins"),
    ("daemon.trustedProxies", "trustedProxies"),
    (
        "daemon.git.maxProcessesPerSecond",
        "git.maxProcessesPerSecond",
    ),
    (
        "daemon.git.maxProcessConcurrency",
        "git.maxProcessConcurrency",
    ),
    ("daemon.autoArchiveAfterMerge", "autoArchiveAfterMerge"),
    (
        "daemon.enableTerminalAgentHooks",
        "enableTerminalAgentHooks",
    ),
    ("daemon.appendSystemPrompt", "appendSystemPrompt"),
    ("daemon.terminalProfiles", "terminalProfiles"),
    ("daemon.agentProfiles", "agentProfiles"),
    ("app.baseUrl", "app.baseUrl"),
    ("agents.providers", "providers"),
    ("agents.catalogRefreshTimeoutMs", "catalogRefreshTimeoutMs"),
    ("agents.metadataGeneration", "metadataGeneration"),
    ("agents.skills.selection", "skills.selection"),
    ("pluginsEnabled", "pluginsEnabled"),
];

pub fn merge(target: &mut Value, patch: &Value) {
    if let (Some(current), Some(patch)) = (target.as_object_mut(), patch.as_object()) {
        for (key, value) in patch {
            if let Some(old) = current.get_mut(key) {
                merge(old, value);
            } else {
                current.insert(key.clone(), value.clone());
            }
        }
    } else {
        *target = patch.clone();
    }
}

pub fn get<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.')
        .try_fold(value, |current, part| current.as_object()?.get(part))
}

pub fn set(value: &mut Value, path: &str, replacement: Option<&Value>) {
    let (first, rest) = path
        .split_once('.')
        .map_or((path, None), |(a, b)| (a, Some(b)));
    if !value.is_object() {
        *value = json!({});
    }
    let object = value.as_object_mut().expect("object just initialized");
    if let Some(rest) = rest {
        if replacement.is_some() || object.contains_key(first) {
            set(
                object.entry(first).or_insert_with(|| json!({})),
                rest,
                replacement,
            );
        }
    } else if let Some(replacement) = replacement {
        object.insert(first.to_owned(), replacement.clone());
    } else {
        object.remove(first);
    }
}

pub fn supported_patch(mut patch: Value, skills: bool) -> Result<Value> {
    if !skills {
        if let Some(object) = patch.as_object_mut() {
            object.remove("skills");
        }
    }
    let patch = super::schema::normalize_patch(patch)?;
    let mut result = json!({});
    for path in [
        "relay.enabled",
        "relay.requireProtectedSecurity",
        "mcp.injectIntoAgents",
        "providers",
        "removeProviders",
        "metadataGeneration.providers",
        "autoArchiveAfterMerge",
        "enableTerminalAgentHooks",
        "appendSystemPrompt",
        "terminalProfiles",
        "agentProfiles",
        "pluginsEnabled",
        "plugins",
    ] {
        if let Some(value) = get(&patch, path) {
            set(&mut result, path, Some(value));
        }
    }
    if skills {
        if let Some(value) = get(&patch, "skills.selection") {
            set(&mut result, "skills.selection", Some(value));
        }
    }
    Ok(result)
}

pub fn removed(patch: &Value) -> Vec<String> {
    patch
        .get("removeProviders")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

pub fn next_mutable(current: &Value, patch: &Value, remove: &[String]) -> Value {
    let mut patch = patch.clone();
    patch
        .as_object_mut()
        .expect("validated patch")
        .remove("removeProviders");
    let mut next = current.clone();
    merge(&mut next, &patch);
    for path in ["plugins", "skills.selection"] {
        if let Some(value) = get(&patch, path) {
            set(&mut next, path, Some(value));
        }
    }
    remove_providers(&mut next, remove);
    next
}

fn remove_providers(config: &mut Value, removed: &[String]) {
    if let Some(providers) = config.get_mut("providers").and_then(Value::as_object_mut) {
        for id in removed {
            providers.remove(id);
        }
    }
    if let Some(providers) = config
        .pointer_mut("/metadataGeneration/providers")
        .and_then(Value::as_array_mut)
    {
        providers.retain(|entry| {
            !entry
                .get("provider")
                .and_then(Value::as_str)
                .is_some_and(|id| removed.iter().any(|removed| removed == id))
        });
    }
}

pub fn persist_patch(current: &Value, patch: &Value, remove: &[String]) -> Result<Value> {
    let mut result = current.clone();
    for path in [
        "relay.enabled",
        "relay.requireProtectedSecurity",
        "mcp.injectIntoAgents",
        "autoArchiveAfterMerge",
        "enableTerminalAgentHooks",
        "appendSystemPrompt",
        "terminalProfiles",
        "agentProfiles",
    ] {
        if let Some(value) = get(patch, path) {
            set(&mut result, &format!("daemon.{path}"), Some(value));
        }
    }
    for path in ["plugins", "pluginsEnabled"] {
        if let Some(value) = patch.get(path) {
            set(&mut result, path, Some(value));
        }
    }
    if patch.get("providers").is_some()
        || patch.get("metadataGeneration").is_some()
        || patch.get("skills").is_some()
        || !remove.is_empty()
    {
        let mut agents = result.get("agents").cloned().unwrap_or_else(|| json!({}));
        remove_providers(&mut agents, remove);
        if let Some(providers) = patch.get("providers").and_then(Value::as_object) {
            let previous = agents
                .as_object_mut()
                .context("agents must be an object")?
                .entry("providers")
                .or_insert_with(|| json!({}));
            let target = previous
                .as_object_mut()
                .context("providers must be an object")?;
            for (id, provider) in providers {
                let normalized = super::schema::normalize_provider(provider.clone())?;
                let mut replacement = target.get(id).cloned().unwrap_or_else(|| json!({}));
                let object = replacement
                    .as_object_mut()
                    .context("provider must be an object")?;
                for (key, value) in normalized
                    .as_object()
                    .context("provider must be an object")?
                {
                    if key == "paseoTools" {
                        merge(
                            object.entry(key.clone()).or_insert_with(|| json!({})),
                            value,
                        );
                    } else {
                        object.insert(key.clone(), value.clone());
                    }
                }
                target.insert(id.clone(), replacement);
            }
        }
        if agents
            .get("providers")
            .and_then(Value::as_object)
            .is_some_and(Map::is_empty)
        {
            agents.as_object_mut().unwrap().remove("providers");
        }
        for path in ["metadataGeneration.providers", "skills.selection"] {
            if let Some(value) = get(patch, path) {
                set(&mut agents, path, Some(value));
            }
        }
        if agents.as_object().is_some_and(Map::is_empty) {
            result.as_object_mut().unwrap().remove("agents");
        } else {
            result["agents"] = agents;
        }
    }
    super::schema::normalize_persisted(result)
}

pub fn belongs(path: &str, owner: &str) -> bool {
    path == owner
        || path
            .strip_prefix(owner)
            .is_some_and(|rest| rest.starts_with('.'))
}

pub fn diff(
    previous: Option<&Value>,
    next: Option<&Value>,
    prefix: &str,
    output: &mut Vec<String>,
) {
    if previous == next {
        return;
    }
    let a = previous.and_then(Value::as_object);
    let b = next.and_then(Value::as_object);
    if a.is_some() || b.is_some() {
        let keys: BTreeSet<&String> = a
            .into_iter()
            .flat_map(Map::keys)
            .chain(b.into_iter().flat_map(Map::keys))
            .collect();
        for key in keys {
            let path = if prefix.is_empty() {
                key.clone()
            } else {
                format!("{prefix}.{key}")
            };
            diff(
                a.and_then(|a| a.get(key)),
                b.and_then(|b| b.get(key)),
                &path,
                output,
            );
        }
    } else if !prefix.is_empty() {
        output.push(prefix.to_owned());
    }
}

pub fn compact(paths: impl IntoIterator<Item = String>, owners: &[String]) -> Vec<String> {
    paths
        .into_iter()
        .map(|path| {
            owners
                .iter()
                .find(|owner| belongs(&path, owner))
                .cloned()
                .unwrap_or(path)
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}
