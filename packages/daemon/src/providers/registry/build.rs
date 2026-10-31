use super::{ProviderEntry, client};
use crate::providers::{
    api::{AgentProvider, ProviderSpec},
    manifest,
    muse::MuseProvider,
};
use crate::state::AppState;
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Map, Value, json};
use std::{collections::BTreeMap, sync::Arc};

struct Resolved {
    definition: Value,
    family: String,
    runtime: Value,
    options: Value,
    profile: Vec<Value>,
    additional: Vec<Value>,
    enabled: bool,
    source: &'static str,
    external: Option<String>,
}

fn override_runtime(value: &Value) -> Value {
    let mut runtime = Map::new();
    if let Some(command) = value.get("command") {
        runtime.insert("command".into(), json!({"mode":"replace","argv":command}));
    }
    for key in ["env", "disallowedTools"] {
        if let Some(value) = value.get(key) {
            runtime.insert(key.into(), value.clone());
        }
    }
    Value::Object(runtime)
}
fn merge_runtime(base: &Value, overrides: &Value) -> Value {
    let mut result = Map::new();
    if let Some(command) = overrides.get("command").or_else(|| base.get("command")) {
        result.insert("command".into(), command.clone());
    }
    if base.get("env").is_some() || overrides.get("env").is_some() {
        let mut env = base["env"].as_object().cloned().unwrap_or_default();
        if let Some(values) = overrides["env"].as_object() {
            env.extend(values.clone());
        }
        result.insert("env".into(), Value::Object(env));
    }
    if base.get("disallowedTools").is_some() || overrides.get("disallowedTools").is_some() {
        let values = base["disallowedTools"]
            .as_array()
            .into_iter()
            .flatten()
            .chain(
                overrides["disallowedTools"]
                    .as_array()
                    .into_iter()
                    .flatten(),
            )
            .cloned()
            .collect();
        result.insert("disallowedTools".into(), Value::Array(values));
    }
    Value::Object(result)
}
fn configured_options(value: &Value) -> Option<Value> {
    value
        .get("options")
        .or_else(|| value.get("params"))
        .cloned()
}
fn models(value: &Value, key: &str) -> Vec<Value> {
    value[key].as_array().cloned().unwrap_or_default()
}
fn apply_metadata(mut definition: Value, overrides: &Value) -> Value {
    for key in ["label", "description"] {
        if let Some(value) = overrides.get(key).filter(|value| !value.is_null()) {
            definition[key] = value.clone();
        }
    }
    definition
}
fn registered(
    definition: Value,
    family: String,
    runtime: &Value,
    overrides: &Value,
    source: &'static str,
    external: Option<String>,
) -> Resolved {
    let enabled = overrides["enabled"]
        .as_bool()
        .or_else(|| definition["enabledByDefault"].as_bool())
        .unwrap_or(true);
    let mut definition = apply_metadata(definition, overrides);
    definition["derivedFromProviderId"] = Value::Null;
    definition["enabled"] = json!(enabled);
    Resolved {
        definition,
        family,
        runtime: merge_runtime(runtime, &override_runtime(overrides)),
        options: configured_options(overrides).unwrap_or_else(|| json!({})),
        profile: models(overrides, "models"),
        additional: models(overrides, "additionalModels"),
        enabled,
        source,
        external,
    }
}
fn supports_exact(family: &str) -> bool {
    matches!(family, "claude" | "codex" | "opencode")
}
fn has_factory(family: &str) -> bool {
    matches!(
        family,
        "claude"
            | "codex"
            | "copilot"
            | "cursor"
            | "opencode"
            | "pi"
            | "omp"
            | "mock"
            | "mock-slow"
    )
}
fn default_command(
    family: &str,
    definition: &Value,
    env: &BTreeMap<String, String>,
) -> Vec<String> {
    if let Some(command) = definition["command"].as_array() {
        return command
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
    }
    match family {
        "pi" => vec![
            env.get("PI_COMMAND")
                .or_else(|| env.get("PI_ACP_PI_COMMAND"))
                .cloned()
                .unwrap_or_else(|| "pi".into()),
        ],
        "omp" => vec![
            env.get("OMP_COMMAND")
                .cloned()
                .unwrap_or_else(|| "omp".into()),
        ],
        "cursor" => vec!["cursor-agent".into(), "acp".into()],
        "mock" | "mock-slow" => vec![],
        _ => vec![family.to_owned()],
    }
}

pub(super) fn entries(
    app: &AppState,
    config: &Value,
    previous: &[Arc<ProviderEntry>],
) -> Result<(Vec<Arc<ProviderEntry>>, Vec<Arc<MuseProvider>>)> {
    let overrides = &config["providerOverrides"];
    let settings = &config["agentProviderSettings"];
    let environment = app.config.environment();
    let development = environment
        .get("PASEO_NODE_ENV")
        .is_some_and(|value| value == "development");
    let mut resolved = Vec::<Resolved>::new();
    for definition in manifest::builtins().iter().chain(if development {
        manifest::development().iter()
    } else {
        [].iter()
    }) {
        let id = definition["id"]
            .as_str()
            .context("Builtin provider id is missing")?;
        let mut definition = definition.clone();
        definition["supportsExactMcpPreapproval"] = json!(supports_exact(id));
        resolved.push(registered(
            definition,
            id.to_owned(),
            &settings[id],
            &overrides[id],
            "builtin",
            None,
        ));
    }
    let native = manifest::native_plugin_definitions();
    let external = app.plugins.registered_providers();
    for (definition, is_external) in native
        .into_iter()
        .map(|value| (value, false))
        .chain(external.into_iter().map(|value| (value, true)))
    {
        let id = definition["id"]
            .as_str()
            .context("Plugin provider id is missing")?
            .to_owned();
        if resolved.iter().any(|entry| entry.definition["id"] == id) {
            bail!("Plugin provider '{id}' conflicts with a built-in provider");
        }
        let configured = overrides.get(&id);
        if definition["requiresConfiguration"] == true && configured.is_none() {
            continue;
        }
        if configured
            .and_then(|value| value["extends"].as_str())
            .is_some_and(|extends| !extends.is_empty())
        {
            continue;
        }
        let family = if is_external {
            id.clone()
        } else {
            definition["nativeFamily"]
                .as_str()
                .context("Native plugin family is missing")?
                .to_owned()
        };
        resolved.push(registered(
            definition,
            family,
            &settings[&id],
            &overrides[&id],
            "custom",
            is_external.then_some(id),
        ));
    }
    if let Some(overrides) = overrides.as_object() {
        for (id, override_value) in overrides {
            if resolved.iter().any(|entry| entry.definition["id"] == *id)
                || manifest::builtins()
                    .iter()
                    .any(|definition| definition["id"] == *id)
            {
                continue;
            }
            let Some(base_id) = override_value["extends"]
                .as_str()
                .filter(|extends| !extends.is_empty())
            else {
                continue;
            };
            // createDerivedDefinition validates the label after the extends-specific
            // command/base/factory checks, so those errors win when both apply.
            let label = || {
                override_value["label"]
                    .as_str()
                    .filter(|label| !label.is_empty())
                    .ok_or_else(|| anyhow!("Custom provider '{id}' requires a label"))
            };
            let mut entry = if base_id == "acp" {
                if !override_value["command"]
                    .as_array()
                    .is_some_and(|command| !command.is_empty())
                {
                    bail!("ACP provider '{id}' requires a command");
                }
                let label = label()?;
                let family = if matches!(id.as_str(), "cursor" | "kimi" | "kiro" | "traecli") {
                    id.clone()
                } else {
                    "acp".into()
                };
                registered(
                    json!({"id":id,"label":label,"description":"Custom ACP provider","defaultModeId":null,"modes":[],"supportsExactMcpPreapproval":id=="hub-e2e"}),
                    family,
                    &Value::Null,
                    override_value,
                    "custom",
                    None,
                )
            } else {
                let base = resolved
                    .iter()
                    .find(|entry| entry.definition["id"] == base_id)
                    .ok_or_else(|| {
                        anyhow!("Custom provider '{id}' extends unknown provider '{base_id}'")
                    })?;
                if !has_factory(base_id) {
                    bail!("No provider client factory registered for '{base_id}'");
                }
                let label = label()?;
                let mut definition = base.definition.clone();
                definition["id"] = json!(id);
                definition["label"] = json!(label);
                let mut entry = registered(
                    definition,
                    base_id.to_owned(),
                    &base.runtime,
                    override_value,
                    "custom",
                    None,
                );
                entry.options =
                    configured_options(override_value).unwrap_or_else(|| base.options.clone());
                entry.definition["derivedFromProviderId"] = json!(base_id);
                entry
            };
            entry.enabled = override_value["enabled"] != false;
            entry.definition["enabled"] = json!(entry.enabled);
            resolved.push(entry);
        }
    }
    let mut entries = Vec::with_capacity(resolved.len());
    let mut muse = Vec::new();
    for resolved in resolved {
        let id = resolved.definition["id"]
            .as_str()
            .context("Provider id is missing")?
            .to_owned();
        let command = if resolved.runtime["command"]["mode"] == "replace" {
            resolved.runtime["command"]["argv"]
                .as_array()
                .context("Provider command argv must be an array")?
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_owned)
                        .context("Provider command arguments must be strings")
                })
                .collect::<Result<Vec<_>>>()?
        } else {
            default_command(&resolved.family, &resolved.definition, &environment)
        };
        let env = resolved.runtime["env"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(key, value)| {
                Ok((
                    key.clone(),
                    value
                        .as_str()
                        .context("Provider environment values must be strings")?
                        .to_owned(),
                ))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        let identity = json!({"definition":resolved.definition,"family":resolved.family,"runtime":resolved.runtime,"options":resolved.options,"profile":resolved.profile,"additional":resolved.additional,"command":command,"external":resolved.external});
        if let Some(old) = previous
            .iter()
            .find(|entry| entry.spec.id == id && entry.identity == identity)
        {
            entries.push(old.clone());
            continue;
        }
        let spec = ProviderSpec {
            id: id.clone(),
            family: resolved.family,
            command,
            env,
            options: resolved.options.clone(),
            defaults: json!({}),
            runtime_settings: resolved.runtime,
            configured_models: resolved
                .profile
                .iter()
                .chain(&resolved.additional)
                .cloned()
                .collect(),
        };
        let raw: Arc<dyn AgentProvider> = if let Some(external) = resolved.external {
            app.plugins.create_provider(&external, spec.clone())?
        } else {
            match spec.family.as_str() {
                "claude" => Arc::new(crate::providers::claude::ClaudeProvider::new(spec.clone())),
                "codex" => Arc::new(crate::providers::codex::CodexProvider::new(spec.clone())),
                "opencode" => Arc::new(crate::providers::opencode::OpenCodeProvider::new(
                    spec.clone(),
                )),
                "pi" | "omp" => {
                    Arc::new(crate::providers::pi_omp::JsonlProvider::new(spec.clone()))
                }
                "copilot" | "cursor" | "kimi" | "kiro" | "traecli" | "acp" => {
                    Arc::new(crate::providers::acp::AcpProvider::new(spec.clone()))
                }
                "antigravity" => Arc::new(crate::providers::antigravity::AntigravityProvider::new(
                    spec.clone(),
                )),
                "hoplite" => Arc::new(crate::providers::hoplite::HopliteProvider::new(
                    spec.clone(),
                )),
                "muse" => {
                    let provider = Arc::new(MuseProvider::new(spec.clone()));
                    muse.push(provider.clone());
                    provider
                }
                "mock" | "mock-slow" => Arc::new(
                    crate::providers::development::DevelopmentProvider::new(&spec.family)?,
                ),
                family => bail!("No provider client factory registered for '{family}'"),
            }
        };
        let wrapped = client::wrap_client(
            raw,
            id,
            resolved.profile.clone(),
            resolved.additional.clone(),
            false,
            resolved.options,
        )?;
        entries.push(Arc::new(ProviderEntry {
            definition: Arc::new(resolved.definition),
            spec,
            client: wrapped,
            enabled: resolved.enabled,
            source: resolved.source,
            profile_models: resolved.profile,
            additional_models: resolved.additional,
            profile_models_are_additive: false,
            identity,
        }));
    }
    Ok((entries, muse))
}
