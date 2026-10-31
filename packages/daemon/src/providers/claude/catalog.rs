use anyhow::{Result, bail};
use regex::Regex;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::LazyLock,
};

// Kept in the same order as server/agent/providers/claude/model-manifest.ts.
struct Model {
    id: &'static str,
    label: &'static str,
    description: &'static str,
    minimum: Option<[u64; 3]>,
    priority: u8,
    context: u64,
    effort: u8,
    medium: bool,
    off: bool,
    fast: bool,
    discovery: bool,
}
macro_rules! model {
    ($id:literal,$label:literal,$description:literal,$minimum:expr,$priority:literal,$context:literal,$effort:literal,$medium:literal,$off:literal,$fast:literal,$discovery:literal) => {
        Model {
            id: $id,
            label: $label,
            description: $description,
            minimum: $minimum,
            priority: $priority,
            context: $context,
            effort: $effort,
            medium: $medium,
            off: $off,
            fast: $fast,
            discovery: $discovery,
        }
    };
}
static MODELS: &[Model] = &[
    model!(
        "claude-opus-5-5",
        "Opus 5.5",
        "Opus 5.5 · Latest release",
        Some([2, 1, 280]),
        3,
        1000000,
        5,
        true,
        false,
        true,
        false
    ),
    model!(
        "claude-opus-5",
        "Opus 5",
        "Opus 5 · Previous release",
        Some([2, 1, 219]),
        2,
        1000000,
        5,
        false,
        true,
        true,
        false
    ),
    model!(
        "claude-mythos-5-1",
        "Mythos 5.1",
        "Mythos 5.1 · Requires verified access",
        None,
        0,
        1000000,
        5,
        false,
        false,
        false,
        true
    ),
    model!(
        "claude-fable-5-1",
        "Fable 5.1",
        "Fable 5.1 · Most powerful model",
        None,
        0,
        1000000,
        5,
        false,
        false,
        false,
        false
    ),
    model!(
        "claude-fable-5",
        "Fable 5",
        "Fable 5 · Previous release",
        Some([2, 1, 169]),
        0,
        1000000,
        5,
        false,
        false,
        false,
        false
    ),
    model!(
        "claude-opus-4-8[1m]",
        "Opus 4.8 1M",
        "Opus 4.8 with 1M context window",
        None,
        0,
        1000000,
        5,
        false,
        true,
        true,
        false
    ),
    model!(
        "claude-opus-4-8",
        "Opus 4.8",
        "Opus 4.8 · Previous release",
        None,
        1,
        200000,
        5,
        false,
        true,
        true,
        false
    ),
    model!(
        "claude-sonnet-5-5",
        "Sonnet 5.5",
        "Sonnet 5.5 · Best for everyday tasks",
        Some([2, 1, 284]),
        0,
        1000000,
        5,
        true,
        false,
        false,
        false
    ),
    model!(
        "claude-sonnet-5",
        "Sonnet 5",
        "Sonnet 5 · Previous release",
        None,
        0,
        200000,
        5,
        false,
        true,
        false,
        false
    ),
    model!(
        "claude-sonnet-5[1m]",
        "Sonnet 5 1M",
        "Sonnet 5 with 1M context window",
        None,
        0,
        1000000,
        5,
        false,
        true,
        false,
        false
    ),
    model!(
        "claude-opus-4-7[1m]",
        "Opus 4.7 1M",
        "Opus 4.7 with 1M context window",
        None,
        0,
        1000000,
        5,
        false,
        true,
        true,
        false
    ),
    model!(
        "claude-opus-4-7",
        "Opus 4.7",
        "Opus 4.7 · Previous release",
        None,
        0,
        200000,
        5,
        false,
        true,
        true,
        false
    ),
    model!(
        "claude-opus-4-6[1m]",
        "Opus 4.6 1M",
        "Opus 4.6 with 1M context window",
        None,
        0,
        1000000,
        4,
        false,
        true,
        true,
        false
    ),
    model!(
        "claude-opus-4-6",
        "Opus 4.6",
        "Opus 4.6 · Most capable for complex work",
        None,
        0,
        200000,
        4,
        false,
        true,
        true,
        false
    ),
    model!(
        "claude-sonnet-4-6[1m]",
        "Sonnet 4.6 1M",
        "Sonnet 4.6 with 1M context window",
        None,
        0,
        1000000,
        4,
        false,
        true,
        false,
        false
    ),
    model!(
        "claude-sonnet-4-6",
        "Sonnet 4.6",
        "Sonnet 4.6 · Best for everyday tasks",
        None,
        0,
        200000,
        4,
        false,
        true,
        false,
        false
    ),
    model!(
        "claude-haiku-5-5",
        "Haiku 5.5",
        "Haiku 5.5 · Fastest for quick answers",
        Some([2, 1, 293]),
        0,
        1000000,
        5,
        true,
        true,
        false,
        false
    ),
    model!(
        "claude-haiku-4-5",
        "Haiku 4.5",
        "Haiku 4.5 · Previous release",
        None,
        0,
        200000,
        0,
        false,
        false,
        false,
        false
    ),
];
static VERSION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(\d+)\.(\d+)\.(\d+)\s+\(Claude Code\)").unwrap());
static ANY_VERSION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(\d+)\.(\d+)\.(\d+)\b").unwrap());
static MANIFEST_SINGLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:claude[-_ ])?(mythos|fable|opus|sonnet|haiku)[-_ ]+(\d+)(?:\[1m\])?(?:[-_ ]+\d{8})?(?:\[1m\])?$").unwrap()
});
static MANIFEST_MINOR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:claude[-_ ])?(mythos|fable|opus|sonnet|haiku)[-_ ]+(\d+)[-.](\d+)(?:\[1m\])?(?:[-_ ]+\d{8})?(?:\[1m\])?$").unwrap()
});
static RUNTIME_MINOR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)claude[-_ ](mythos|fable|opus|sonnet|haiku)[-_ ]+(\d+)[-.](\d+)(\[1m\])?")
        .unwrap()
});
static RUNTIME_SINGLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)claude[-_ ](mythos|fable|opus|sonnet|haiku)[-_ ]+(\d+)(\[1m\])?").unwrap()
});

pub fn parse_version(value: &str) -> Option<[u64; 3]> {
    let caps = VERSION
        .captures(value)
        .or_else(|| ANY_VERSION.captures(value))?;
    Some([
        caps[1].parse().ok()?,
        caps[2].parse().ok()?,
        caps[3].parse().ok()?,
    ])
}
/// Executable wrappers may print their own version; catalog probes must require this marker.
pub fn executable_version(value: &str) -> Option<[u64; 3]> {
    VERSION
        .is_match(value)
        .then(|| parse_version(value))
        .flatten()
}
fn normalize_match(value: &str, regex: &Regex, minor: bool) -> Option<&'static str> {
    let caps = regex.captures(value)?;
    let base = if minor {
        format!(
            "claude-{}-{}-{}",
            caps[1].to_lowercase(),
            &caps[2],
            &caps[3]
        )
    } else {
        format!("claude-{}-{}", caps[1].to_lowercase(), &caps[2])
    };
    if value.to_ascii_lowercase().contains("[1m]") {
        let suffixed = format!("{base}[1m]");
        if let Some(model) = MODELS.iter().find(|m| m.id == suffixed) {
            return Some(model.id);
        }
    }
    MODELS.iter().find(|m| m.id == base).map(|m| m.id)
}
pub fn normalize_manifest_model_id(value: Option<&str>) -> Option<&'static str> {
    let value = value?.trim();
    MODELS
        .iter()
        .find(|m| m.id == value)
        .map(|m| m.id)
        .or_else(|| normalize_match(value, &MANIFEST_SINGLE, false))
        .or_else(|| normalize_match(value, &MANIFEST_MINOR, true))
}
pub fn normalize_runtime_model_id(value: Option<&str>) -> Option<&'static str> {
    normalize_manifest_model_id(value).or_else(|| {
        let value = value?.trim();
        normalize_match(value, &RUNTIME_MINOR, true)
            .or_else(|| normalize_match(value, &RUNTIME_SINGLE, false))
    })
}
pub fn observed_model_id(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    if value.is_empty() || value == "<synthetic>" {
        return None;
    }
    Some(
        normalize_runtime_model_id(Some(value))
            .unwrap_or(value)
            .to_owned(),
    )
}
pub fn context_window(value: Option<&str>) -> Option<u64> {
    if value?.trim().to_ascii_lowercase().ends_with("[1m]") {
        return Some(1000000);
    }
    let id = normalize_runtime_model_id(value)?;
    MODELS.iter().find(|m| m.id == id).map(|m| m.context)
}
pub fn model_label(value: Option<&str>) -> Option<&'static str> {
    let id = normalize_runtime_model_id(value)?;
    MODELS
        .iter()
        .find(|model| model.id == id)
        .map(|model| model.label)
}
fn manifest_model(value: Option<&str>) -> Option<&'static Model> {
    let id = normalize_manifest_model_id(value)?;
    MODELS.iter().find(|m| m.id == id)
}
pub fn supports_fast(model: Option<&str>) -> bool {
    manifest_model(model).is_some_and(|m| m.fast)
}
pub fn disabled_thinking_fallback(model: Option<&str>) -> Option<&'static str> {
    manifest_model(model)
        .filter(|m| m.effort > 0)
        .map(|m| if m.medium { "medium" } else { "high" })
}
pub fn validate_thinking(model: Option<&str>, option: Option<&str>) -> Result<()> {
    match option {
        None | Some("" | "default" | "low" | "medium" | "high" | "xhigh" | "max" | "ultracode") => {
            Ok(())
        }
        Some("off") if manifest_model(model).is_some_and(|m| m.off) => Ok(()),
        Some("off") => bail!(
            "Thinking option 'off' is not available for model '{}'",
            model.unwrap_or("default")
        ),
        Some(option) => bail!("Unknown thinking option: {option}"),
    }
}
fn thinking_options(effort: u8, off: bool, medium: bool) -> Vec<Value> {
    let mut result = Vec::new();
    if off {
        result.push(json!({"id":"off","label":"Off"}));
    }
    for (id, label) in [
        ("low", "Low"),
        ("medium", "Medium"),
        ("high", "High"),
        ("xhigh", "Extra High"),
        ("max", "Max"),
    ] {
        if id == "xhigh" && effort != 5 {
            continue;
        }
        let mut value = json!({"id":id,"label":label});
        if id == if medium { "medium" } else { "high" } {
            value["isDefault"] = true.into();
        }
        result.push(value);
    }
    if effort == 5 {
        result.push(json!({"id":"ultracode","label":"Ultra Code"}));
    }
    result
}
pub fn manifest_models(version: Option<&str>, discovered: Option<&BTreeSet<String>>) -> Vec<Value> {
    let available: Vec<_> = MODELS
        .iter()
        .filter(|m| {
            (version.is_none()
                || m.minimum.is_none()
                || parse_version(version.unwrap())
                    .zip(m.minimum)
                    .is_some_and(|(v, min)| v >= min))
                && (!m.discovery || discovered.is_none_or(|ids| ids.contains(m.id)))
        })
        .collect();
    let default = available
        .iter()
        .filter(|m| m.priority > 0)
        .max_by_key(|m| m.priority)
        .map(|m| m.id);
    let mut result = Vec::new();
    for model in available {
        let mut value = json!({"provider":"claude","id":model.id,"label":model.label,"description":model.description,"contextWindowMaxTokens":model.context});
        if Some(model.id) == default {
            value["isDefault"] = true.into();
        }
        if model.effort > 0 {
            value["thinkingOptions"] =
                thinking_options(model.effort, model.off, model.medium).into();
            value["defaultThinkingOptionId"] = if model.medium { "medium" } else { "high" }.into();
        }
        if model.id == "claude-fable-5" {
            value["aliases"] = json!(["claude-fable-5[1m]"]);
            result.push(value.clone());
            value["id"] = "claude-fable-5[1m]".into();
            value.as_object_mut().unwrap().remove("aliases");
            value.as_object_mut().unwrap().remove("isDefault");
            value["isSelectable"] = false.into();
        }
        result.push(value);
    }
    result
}
pub fn resolve_configured_model(mut model: Value) -> Value {
    if model.get("thinkingOptions").is_some() {
        return model;
    }
    if let Some(found) = manifest_model(model["id"].as_str()) {
        if found.effort > 0 {
            model["thinkingOptions"] =
                thinking_options(found.effort, found.off, found.medium).into();
        }
    } else {
        model["thinkingOptions"] = thinking_options(4, false, false).into();
    }
    model
}
pub fn merge_settings_models(
    mut models: Vec<Value>,
    settings: &Value,
    version: Option<&str>,
) -> Vec<Value> {
    let mut additions = Vec::new();
    let mut add = |value: &Value, key: &str| {
        let Some(id) = value.as_str().map(str::trim).filter(|s| !s.is_empty()) else {
            return;
        };
        if additions.iter().any(|m: &Value| m["id"] == id) {
            return;
        }
        additions.push(json!({"provider":"claude","id":id,"label":id,"description":format!("From Claude settings.json {key}")}));
    };
    add(&settings["model"], "model");
    for key in [
        "ANTHROPIC_MODEL",
        "ANTHROPIC_SMALL_FAST_MODEL",
        "ANTHROPIC_DEFAULT_FABLE_MODEL",
        "ANTHROPIC_DEFAULT_OPUS_MODEL",
        "ANTHROPIC_DEFAULT_SONNET_MODEL",
        "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    ] {
        add(&settings["env"][key], &format!("env.{key}"));
    }
    let all = manifest_models(version, None);
    for addition in additions {
        if let Some(existing) = models.iter_mut().find(|m| m["id"] == addition["id"]) {
            if existing["isSelectable"] == false {
                existing
                    .as_object_mut()
                    .unwrap()
                    .extend(addition.as_object().unwrap().clone());
                existing["isSelectable"] = true.into();
            }
        } else {
            let mut value = all
                .iter()
                .find(|m| m["id"] == addition["id"])
                .cloned()
                .unwrap_or_else(|| json!({}));
            value
                .as_object_mut()
                .unwrap()
                .extend(addition.as_object().unwrap().clone());
            models.push(value);
        }
    }
    models
}
pub async fn catalog(
    config_dir: &Path,
    version: Option<&str>,
    initialize: &Value,
    env: &BTreeMap<String, String>,
) -> Value {
    let discovered = initialize["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|model| {
            normalize_runtime_model_id(
                model
                    .get("resolvedModel")
                    .filter(|v| !v.is_null())
                    .unwrap_or(&model["value"])
                    .as_str(),
            )
            .map(str::to_owned)
        })
        .collect();
    let models = manifest_models(version, Some(&discovered));
    let settings = match tokio::fs::read(config_dir.join("settings.json")).await {
        Ok(bytes) => serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null),
        Err(_) => Value::Null,
    };
    let mut result = mode_catalog(env);
    result["models"] = merge_settings_models(models, &settings, version).into();
    result
}
pub fn auto_mode_transport(env: &BTreeMap<String, String>) -> Option<&'static str> {
    for (key, label) in [
        ("CLAUDE_CODE_USE_BEDROCK", "Bedrock"),
        ("CLAUDE_CODE_USE_VERTEX", "Vertex"),
    ] {
        if env.get(key).is_some_and(|v| {
            !matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "" | "0" | "false" | "no" | "off"
            )
        }) {
            return Some(label);
        }
    }
    None
}
pub fn mode_catalog(env: &BTreeMap<String, String>) -> Value {
    let restricted = auto_mode_transport(env).is_some();
    let modes: Vec<Value> = [
        (
            "plan",
            "Plan Mode",
            "Analyze the codebase without executing tools or edits",
        ),
        (
            "default",
            "Always Ask",
            "Prompts for permission the first time a tool is used",
        ),
        (
            "acceptEdits",
            "Accept File Edits",
            "Automatically approves edit-focused tools without prompting",
        ),
        (
            "auto",
            "Auto mode",
            "Uses a model classifier to review permission prompts automatically",
        ),
        (
            "bypassPermissions",
            "Bypass",
            "Skip all permission prompts (use with caution)",
        ),
    ]
    .into_iter()
    .filter(|(id, _, _)| !restricted || *id != "auto")
    .map(|(id, label, description)| json!({"id":id,"label":label,"description":description}))
    .collect();
    json!({"modes":modes,"defaultModeId":if restricted {"default"} else {"auto"}})
}
pub fn features(config: &Value) -> Vec<Value> {
    if !supports_fast(config["model"].as_str()) {
        return vec![];
    }
    vec![
        json!({"type":"toggle","id":"fast_mode","label":"Fast","description":"Lower latency Opus responses at higher token cost","tooltip":"Toggle fast mode","icon":"zap","value":config["featureValues"]["fast_mode"] == true}),
    ]
}
pub fn commands(initialize: &Value) -> Vec<Value> {
    let mut commands = BTreeMap::new();
    for command in initialize["commands"].as_array().into_iter().flatten() {
        let Some(name) = command["name"].as_str() else {
            continue;
        };
        commands.entry(name.to_owned()).or_insert_with(|| {
            let root = matches!(
                name,
                "clear"
                    | "compact"
                    | "context"
                    | "debug"
                    | "extra-usage"
                    | "heapdump"
                    | "init"
                    | "loop"
                    | "schedule"
                    | "usage"
            );
            let mut value = json!({"name":name,"kind":if root {"command"} else {"skill"}});
            for key in ["description", "argumentHint"] {
                if let Some(field) = command.get(key) {
                    value[key] = field.clone();
                }
            }
            value
        });
    }
    commands.entry("rewind".into()).or_insert_with(|| json!({"name":"rewind","description":"Rewind tracked files to a previous user message","argumentHint":"[user_message_uuid]"}));
    commands.into_values().collect()
}
