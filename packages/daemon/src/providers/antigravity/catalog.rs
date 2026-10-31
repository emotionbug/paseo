use std::{collections::HashSet, time::Duration};

use anyhow::Result;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::wire::{Error, diagnostic};
use crate::{
    process::{self, CommandSpec},
    providers::api::ProviderSpec,
};

pub fn modes() -> Vec<Value> {
    vec![
        json!({"id":"full-access","icon":"ShieldOff","colorTier":"dangerous","label":"Full access",
        "description":"Antigravity cannot ask for permission when another app drives it. Paseo starts it with --dangerously-skip-permissions.","isUnattended":true}),
    ]
}
pub fn capabilities() -> Value {
    json!({"supportsStreaming":true,"supportsSessionPersistence":true,"supportsSessionListing":false,
        "supportsDynamicModes":true,"supportsMcpServers":false,"supportsNativePaseoTools":false,
        "supportsReasoningStream":false,"supportsToolInvocations":true,"supportsRewindConversation":false,
        "supportsRewindFiles":false,"supportsRewindBoth":false})
}
pub fn command(spec: &ProviderSpec, cwd: Option<&str>) -> Result<CommandSpec> {
    let (program, args) = spec.command.split_first().ok_or_else(|| {
        Error::new(
            "SPAWN_ERROR",
            "agy not found on PATH. Install Antigravity, then run `agy` and sign in.",
        )
    })?;
    Ok(CommandSpec {
        program: program.clone(),
        args: args.to_vec(),
        cwd: cwd.map(Into::into),
        env: spec.env.clone(),
        clear_env: false,
    })
}
async fn probe(
    spec: &ProviderSpec,
    args: &[&str],
    cwd: Option<&str>,
    cancel: CancellationToken,
) -> Result<String> {
    let mut cmd = command(spec, cwd)?;
    cmd.args.extend(args.iter().map(|s| (*s).to_owned()));
    let result = process::run(cmd, 16 * 1024 * 1024, Duration::from_secs(10), cancel).await?;
    if result.status != 0 {
        let message = if result.stderr.trim().is_empty() {
            format!("agy exited with code {}", result.status)
        } else {
            result.stderr.trim().to_owned()
        };
        return Err(Error::new("PROBE_ERROR", diagnostic(&message)).into());
    }
    Ok(String::from_utf8(result.stdout)?)
}
pub async fn discover(
    spec: &ProviderSpec,
    cwd: Option<&str>,
    cancel: CancellationToken,
) -> Result<Value> {
    parse_models(&probe(spec, &["models"], cwd, cancel).await?, &spec.id)
}
pub fn parse_models(output: &str, provider: &str) -> Result<Value> {
    let mut models = Vec::new();
    let mut ids = HashSet::new();
    for line in output.trim().split('\n') {
        let fields: Vec<_> = line.trim_end_matches('\r').split('\t').collect();
        if fields.len() != 2 || fields[0].is_empty() || fields[1].is_empty() {
            return Err(Error::new("INVALID_CATALOG", "Invalid `agy models` output").into());
        }
        if !ids.insert(fields[0]) {
            return Err(Error::new(
                "INVALID_CATALOG",
                format!("Duplicate Antigravity model: {}", fields[0]),
            )
            .into());
        }
        models.push(json!({"id":fields[0],"label":fields[1],"provider":provider,"isDefault":false,"thinkingOptions":[]}));
    }
    Ok(json!({"models":models,"modes":modes(),"defaultModeId":"full-access"}))
}
pub async fn status(spec: &ProviderSpec, cancel: CancellationToken) -> Result<()> {
    let version = probe(spec, &["--version"], None, cancel.clone()).await?;
    let version = version.trim();
    let fields: Vec<_> = version.split('.').collect();
    if fields.len() != 3
        || fields
            .iter()
            .any(|v| v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(Error::new(
            "UNSUPPORTED_VERSION",
            "Could not read the Antigravity version. Update `agy` to 1.1.15 or later.",
        )
        .into());
    }
    let parsed: Vec<u64> = fields
        .iter()
        .map(|v| v.parse())
        .collect::<std::result::Result<_, _>>()?;
    if parsed.as_slice() < [1, 1, 15].as_slice() {
        return Err(Error::new(
            "UNSUPPORTED_VERSION",
            format!("Antigravity {version} is unsupported. Update `agy` to 1.1.15 or later."),
        )
        .into());
    }
    discover(spec, None, cancel).await?;
    Ok(())
}
pub fn validate(config: &Value, catalog: &Value) -> Result<()> {
    if let Some(model) = config["model"].as_str().filter(|s| !s.is_empty())
        && !catalog["models"]
            .as_array()
            .is_some_and(|models| models.iter().any(|m| m["id"] == model))
    {
        return Err(Error::new(
            "INVALID_MODEL",
            format!("Unknown Antigravity model: {model}"),
        )
        .into());
    }
    if let Some(mode) = config["modeId"].as_str().filter(|s| !s.is_empty())
        && mode != "full-access"
    {
        return Err(Error::new("INVALID_MODE", format!("Unknown Antigravity mode: {mode}")).into());
    }
    if config["thinkingOptionId"]
        .as_str()
        .is_some_and(|s| !s.is_empty())
    {
        return Err(Error::new(
            "UNSUPPORTED_THINKING",
            "Antigravity effort is selected through the model ID",
        )
        .into());
    }
    Ok(())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Persistence {
    version: u64,
    data: Conversation,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Conversation {
    conversation_id: String,
}

pub fn decode_persistence(handle: &Value) -> Result<Value> {
    let value = if let Some(value) = handle.pointer("/metadata/pluginProviderPersistence") {
        value.clone()
    } else {
        let id = handle["sessionId"]
            .as_str()
            .or_else(|| handle.as_str())
            .ok_or_else(|| {
                Error::new(
                    "INVALID_RESUME",
                    "Invalid plugin provider persistence handle",
                )
            })?;
        let data = id.strip_prefix("plugin:").ok_or_else(|| {
            Error::new(
                "INVALID_RESUME",
                "Invalid plugin provider persistence handle",
            )
        })?;
        serde_json::from_str(data)?
    };
    if !value.as_object().is_some_and(|object| {
        object.len() == 2 && object.contains_key("data") && value["version"].as_u64().is_some()
    }) {
        return Err(Error::new(
            "INVALID_RESUME",
            "Invalid plugin provider persistence handle",
        )
        .into());
    }
    Ok(value)
}
pub fn conversation(handle: &Value) -> Result<String> {
    let value = decode_persistence(handle)?;
    let persistence: Persistence = serde_json::from_value(value)?;
    if persistence.version != 1 || persistence.data.conversation_id.is_empty() {
        return Err(Error::new("INVALID_RESUME", "Invalid Antigravity persistence").into());
    }
    Ok(persistence.data.conversation_id)
}
pub fn persistence(provider: &str, id: &str) -> Value {
    let data = json!({"version":1,"data":{"conversationId":id}});
    json!({"provider":provider,"sessionId":format!("plugin:{data}"),"metadata":{"pluginProviderPersistence":data}})
}
