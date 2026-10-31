use crate::providers::api::LaunchContext;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::collections::HashMap;

pub(super) const CAPABILITIES: &[&str] = &[
    "prompt.message",
    "prompt.command",
    "prompt.image",
    "prompt.output_schema",
    "prompt.steer",
    "session.archive",
    "session.configure",
    "session.list",
    "session.persistence",
    "session.revert.both",
    "session.revert.conversation",
    "session.revert.files",
    "session.subsession",
    "session.unarchive",
    "permission",
    "permission.tool_policy",
    "timeline.plugin",
];

pub(super) fn negotiate(selected: &Value, offered: &[String]) -> Result<Vec<String>> {
    let values = selected
        .as_array()
        .context("Provider capabilities must be an array")?;
    for value in values {
        let name = value
            .as_str()
            .context("Provider capability must be a string")?;
        if CAPABILITIES.contains(&name) && !offered.iter().any(|item| item == name) {
            bail!("Provider selected an unoffered capability: {name}");
        }
    }
    Ok(CAPABILITIES
        .iter()
        .filter(|name| values.iter().any(|value| value.as_str() == Some(**name)))
        .map(|name| (*name).to_owned())
        .collect())
}

pub(super) fn agent_capabilities(capabilities: &[String]) -> Value {
    let has = |cap: &str| capabilities.iter().any(|value| value == cap);
    json!({"supportsStreaming":true,"supportsSessionPersistence":has("session.persistence"),
        "supportsSessionListing":has("session.list"),"supportsDynamicModes":has("session.configure"),
        "supportsMcpServers":true,"supportsNativePaseoTools":false,"supportsReasoningStream":true,
        "supportsToolInvocations":true,"supportsRewindConversation":has("session.revert.conversation"),
        "supportsRewindFiles":has("session.revert.files"),"supportsRewindBoth":has("session.revert.both")})
}

pub(super) fn require_capabilities(capabilities: &[String], input: &Value) -> Result<()> {
    let require = |name: &str| -> Result<()> {
        if !capabilities.iter().any(|capability| capability == name) {
            bail!("Provider does not support {name}");
        }
        Ok(())
    };
    match input["type"].as_str().unwrap_or("") {
        "sessions" => require("session.list")?,
        "session.open" => {
            if input.get("persistence").is_some() {
                require("session.persistence")?;
            }
            if input["config"].get("toolPolicy").is_some() {
                require("permission.tool_policy")?;
            }
        }
        "session.prompt" => {
            let prompt = &input["prompt"];
            if prompt["delivery"] == "steer" {
                require("prompt.steer")?;
            }
            if prompt["input"]["type"] == "command" {
                require("prompt.command")?;
            } else {
                require("prompt.message")?;
                if prompt["input"]["content"]
                    .as_array()
                    .is_some_and(|parts| parts.iter().any(|part| part["type"] == "image"))
                {
                    require("prompt.image")?;
                }
            }
            if prompt.get("outputSchema").is_some() {
                require("prompt.output_schema")?;
            }
        }
        "session.permission" => require("permission")?,
        "session.configure" => require("session.configure")?,
        "session.revert" => require(&format!(
            "session.revert.{}",
            input["scope"].as_str().unwrap_or("")
        ))?,
        "session.archive" => require("session.archive")?,
        "session.unarchive" => require("session.unarchive")?,
        _ => (),
    }
    Ok(())
}

pub(super) fn strip_parent_environment(env: &mut std::collections::BTreeMap<String, String>) {
    for key in [
        "CLAUDECODE",
        "CLAUDE_CODE_ENTRYPOINT",
        "CLAUDE_CODE_SSE_PORT",
        "CLAUDE_AGENT_SDK_VERSION",
        "PASEO_NODE_ENV",
        "PASEO_DESKTOP_MANAGED",
        "PASEO_SUPERVISED",
        "ELECTRON_RUN_AS_NODE",
        "ELECTRON_NO_ATTACH_CONSOLE",
        "ESBUILD_BINARY_PATH",
    ] {
        env.remove(key);
    }
}

pub(super) fn session_config(
    config: &Value,
    context: &LaunchContext,
    persist: bool,
) -> Result<Value> {
    let cwd = config["cwd"]
        .as_str()
        .context("Plugin provider requires cwd to open a session")?;
    let mut env = context.env.clone();
    strip_parent_environment(&mut env);
    let settings = config
        .get("featureValues")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !settings.is_object() {
        bail!("provider settings must be a JSON object");
    }
    let mut result = json!({"cwd":cwd,"env":env,"mcpServers":config.get("mcpServers").unwrap_or(&context.mcp_servers),"settings":settings,"persist":persist});
    let prompts: Vec<&str> = ["systemPrompt", "daemonAppendSystemPrompt"]
        .iter()
        .filter_map(|key| config[*key].as_str())
        .filter(|text| !text.trim().is_empty())
        .collect();
    if !prompts.is_empty() {
        result["systemPrompt"] = json!(prompts.join("\n\n"));
    }
    for (source, destination) in [
        ("model", "model"),
        ("modeId", "mode"),
        ("thinkingOptionId", "thinkingOption"),
        ("providerOptions", "providerOptions"),
        ("toolPolicy", "toolPolicy"),
    ] {
        if let Some(value) = config.get(source) {
            result[destination] = value.clone();
        }
    }
    if let Some(title) = config.get("title").filter(|title| !title.is_null()) {
        result["title"] = title.clone();
    }
    super::schema::session_config(&mut result)?;
    Ok(result)
}

pub(super) fn prompt_input(prompt: Value, commands: &[Value], steer: bool) -> Result<Value> {
    if !steer {
        if let Some(text) = prompt.as_str().and_then(|text| text.strip_prefix('/')) {
            let (name, arguments) = text.split_once(' ').unwrap_or((text, ""));
            if commands.iter().any(|command| command["name"] == name) {
                return Ok(json!({"type":"command","name":name,"arguments":arguments}));
            }
        }
    }
    let mut content = match prompt {
        Value::String(text) => json!([{"type":"text","text":text}]),
        Value::Array(parts) => Value::Array(parts),
        _ => bail!("prompt content must be a string or array"),
    };
    super::schema::content(&mut content)?;
    Ok(json!({"type":"message","content":content}))
}

pub(super) fn persistence_id(persistence: &Value) -> String {
    // ProviderPersistenceSchema defines version before data, independently of the
    // input object's insertion order. Data retains JS enumeration/number semantics.
    format!(
        "plugin:{{\"version\":{},\"data\":{}}}",
        crate::json::stringify(&persistence["version"]).expect("JSON value"),
        crate::json::stringify(&persistence["data"]).expect("JSON value")
    )
}
pub(super) fn persistence_handle(provider: &str, persistence: &Value) -> Value {
    json!({"provider":provider,"sessionId":persistence_id(persistence),"metadata":{"pluginProviderPersistence":persistence}})
}
pub(super) fn decode_persistence(handle: &Value) -> Result<Value> {
    if let Some(value) = handle["metadata"].get("pluginProviderPersistence") {
        validate_persistence(value)?;
        return Ok(value.clone());
    }
    decode_persistence_id(
        handle["sessionId"]
            .as_str()
            .context("Invalid plugin provider persistence handle")?,
    )
}
pub(super) fn decode_persistence_id(id: &str) -> Result<Value> {
    let raw = id
        .strip_prefix("plugin:")
        .context("Invalid plugin provider persistence handle")?;
    let value: Value = serde_json::from_str(raw)?;
    validate_persistence(&value)?;
    Ok(value)
}
fn validate_persistence(value: &Value) -> Result<()> {
    let object = value
        .as_object()
        .context("Invalid plugin provider persistence handle")?;
    if object.len() != 2
        || !object.contains_key("data")
        || !value["version"]
            .as_f64()
            .is_some_and(|number| number >= 0.0 && number.fract() == 0.0)
    {
        bail!("Invalid plugin provider persistence handle");
    }
    Ok(())
}

pub(super) fn catalog(provider: &str, catalog: &Value) -> Value {
    let models: Vec<Value> = catalog["models"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|model| {
            let mut result = model.clone();
            result["provider"] = json!(provider);
            if result.get("isDefault").is_none() {
                result["isDefault"] = json!(model.get("id") == catalog.get("defaultModel"));
            }
            if result.get("thinkingOptions").is_none() {
                copy(catalog, "thinkingOptions", &mut result, "thinkingOptions");
            }
            if result.get("defaultThinkingOptionId").is_none() {
                copy(
                    catalog,
                    "defaultThinkingOption",
                    &mut result,
                    "defaultThinkingOptionId",
                );
            }
            result
        })
        .collect();
    let mut result = json!({"models":models,"modes":catalog["modes"]});
    copy(catalog, "defaultMode", &mut result, "defaultModeId");
    result
}

pub(super) fn copy(source: &Value, key: &str, target: &mut Value, name: &str) {
    if let Some(value) = source.get(key) {
        target[name] = value.clone();
    }
}

pub(super) fn timeline_item(
    item: Value,
    snapshots: &mut HashMap<String, Value>,
    mut tokens: Option<&mut HashMap<String, Value>>,
) -> Option<Value> {
    let id = item["id"].as_str()?.to_owned();
    let kind = item["type"].as_str()?;
    if let Some(token) = item.get("revertToken") {
        if let Some(tokens) = tokens.as_mut() {
            tokens.insert(id.clone(), token.clone());
            if matches!(kind, "user_message" | "assistant_message") {
                tokens.insert(
                    item["messageId"].as_str().unwrap_or(&id).to_owned(),
                    token.clone(),
                );
            }
        }
    }
    let previous = snapshots.insert(id.clone(), item.clone());
    let mut result = match kind {
        "assistant_message" | "reasoning" => {
            let previous_text = previous
                .as_ref()
                .filter(|previous| previous["type"] == kind)
                .and_then(|previous| previous["text"].as_str())
                .unwrap_or("");
            let text = item["text"].as_str()?;
            let delta = text.strip_prefix(previous_text).unwrap_or(text);
            if delta.is_empty() {
                return None;
            }
            let mut result = json!({"type":kind,"text":delta});
            if kind == "assistant_message" {
                result["messageId"] = json!(item["messageId"].as_str().unwrap_or(&id));
            }
            result
        }
        "user_message" => {
            if previous.as_ref().is_some_and(|previous| {
                previous["type"] == "user_message" && previous["text"] == item["text"]
            }) {
                return None;
            }
            let mut result = json!({"type":kind,"text":item["text"],"messageId":item["messageId"].as_str().unwrap_or(&id)});
            copy(&item, "clientMessageId", &mut result, "clientMessageId");
            result
        }
        _ => item,
    };
    if result["type"] != "plugin" {
        result.as_object_mut()?.remove("id");
    }
    result.as_object_mut()?.remove("revertToken");
    Some(result)
}
