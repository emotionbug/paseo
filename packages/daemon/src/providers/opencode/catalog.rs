use std::time::Duration;

use crate::providers::registry::catalog::refresh_activity;
use anyhow::{Result, anyhow, bail};
use reqwest::Method;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::runtime::{Generation, Runtime};

pub(super) fn features(config: &Value) -> Vec<Value> {
    vec![
        json!({"type":"toggle","id":"auto_accept","label":"Auto Accept",
        "description":"Automatically approves OpenCode tool permission prompts.",
        "tooltip":"Auto accept permission prompts","icon":"shield-check",
        "value":config.pointer("/featureValues/auto_accept").and_then(Value::as_bool)==Some(true)}),
    ]
}

pub(super) fn features_v2(config: &Value) -> Vec<Value> {
    vec![
        json!({"type":"toggle","id":"auto_accept","label":"Auto-accept",
        "value":config.pointer("/featureValues/auto_accept").and_then(Value::as_bool)==Some(true)}),
    ]
}

/// Ordinary inheritance remains registry-owned; only OpenCode's unattended
/// rewrite is provider-specific. In particular, never invent a `build` agent
/// for a cross-provider unattended child when the engine owns its default.
pub(super) fn resolve_create_config(input: &Value) -> Option<Result<Value>> {
    let explicit = input["requestedMode"].as_str();
    let full_access = explicit == Some("full-access");
    let unattended = input["unattended"] == true || input["parent"]["isUnattended"] == true;
    if !full_access && !unattended {
        return None;
    }
    Some((|| {
        let inherited = if explicit.is_none() && input["parent"]["provider"] == input["provider"] {
            input["parent"]["modeId"].as_str()
        } else {
            None
        };
        let requested = if full_access {
            Some("build")
        } else {
            explicit.or(inherited)
        };
        let mut features = input["featureValues"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        if full_access || !features.contains_key("auto_accept") {
            features.insert("auto_accept".into(), json!(true));
        }
        let Some(mode) = requested else {
            return Ok(json!({"featureValues":features}));
        };
        let mut delegated = input.clone();
        delegated["requestedMode"] = json!(mode);
        delegated["featureValues"] = Value::Object(features);
        crate::providers::create_config::resolve_default(&delegated)
    })())
}

pub(super) fn location_query(cwd: &str) -> Vec<(String, String)> {
    vec![("location[directory]".into(), cwd.into())]
}

pub(super) async fn wait_ready(
    runtime: &Runtime,
    cwd: &str,
    require_bridge: bool,
    cancel: CancellationToken,
) -> Result<()> {
    let location = location_query(cwd);
    let work = async {
        loop {
            let request =
                runtime.request(Method::GET, "/api/plugin", &location, None, cancel.clone());
            match tokio::time::timeout(Duration::from_secs(5), request).await {
                Ok(result) => {
                    let result = result?;
                    let plugins = result
                        .get("data")
                        .and_then(Value::as_array)
                        .ok_or_else(|| anyhow!("OpenCode plugin inventory is malformed"))?;
                    if !require_bridge && !plugins.is_empty() {
                        return Ok(());
                    }
                    if require_bridge {
                        if let Some(bridge) = plugins.iter().find(|plugin| plugin["id"] == "paseo")
                        {
                            match bridge.pointer("/state/status").and_then(Value::as_str) {
                                Some("active") => return Ok(()),
                                Some("failed") => bail!(
                                    "OpenCode Paseo tool bridge plugin failed: {}",
                                    bridge["state"]["error"]
                                ),
                                _ => {}
                            }
                        }
                    }
                }
                Err(_) => {}
            }
            tokio::select! {
                _ = cancel.cancelled() => bail!("OpenCode location activation canceled"),
                _ = tokio::time::sleep(Duration::from_millis(50)) => {}
            }
        }
    };
    tokio::select! {
        _ = cancel.cancelled() => bail!("OpenCode location activation canceled"),
        result = tokio::time::timeout(Duration::from_secs(30),work) => result.map_err(|_|anyhow!("OpenCode location {cwd} did not activate within 30000ms"))?,
    }
}

pub(super) fn modes(generation: Generation, agents: &Value) -> Vec<Value> {
    let list = if generation == Generation::V2 {
        agents.get("data").unwrap_or(agents)
    } else {
        agents
    };
    let Some(list) = list.as_array() else {
        return vec![];
    };
    let mut result = Vec::new();
    for agent in list {
        if agent["hidden"] == true {
            continue;
        }
        if generation == Generation::V2 {
            if agent["mode"] == "subagent" {
                continue;
            }
            let mut mode = json!({"id":agent["id"],"label":agent["name"]});
            if let Some(description) = agent.get("description") {
                mode["description"] = description.clone();
            }
            result.push(mode);
        } else {
            if !matches!(agent["mode"].as_str(), Some("primary" | "all")) {
                continue;
            }
            let Some(name) = agent["name"].as_str() else {
                continue;
            };
            if name == "full-access" {
                continue;
            }
            let mut chars = name.chars();
            let label = chars
                .next()
                .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_default();
            let mut mode = json!({"id":name,"label":label,"icon":"Bot"});
            let description = agent["description"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .or(match name {
                    "build" => Some("Allows edits and tool execution for implementation work"),
                    "plan" => Some("Read-only planning mode that avoids file edits"),
                    _ => None,
                });
            if let Some(description) = description {
                mode["description"] = json!(description);
            }
            if let Some(color) = agent["color"].as_str().filter(|s| {
                s.len() == 7
                    && s.starts_with('#')
                    && s.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit)
            }) {
                mode["colorTier"] = json!(color);
            }
            result.push(mode);
        }
    }
    if generation == Generation::Legacy {
        result.sort_by(|left, right| {
            let rank = |v: &Value| match v["id"].as_str() {
                Some("build") => 0,
                Some("plan") => 1,
                _ => 2,
            };
            rank(left)
                .cmp(&rank(right))
                .then_with(|| left["label"].as_str().cmp(&right["label"].as_str()))
        });
    }
    result
}

pub(super) fn model_ref(model: &str, variant: Option<&str>) -> Result<Value> {
    let (provider, id) = model
        .split_once('/')
        .filter(|(provider, id)| !provider.is_empty() && !id.is_empty())
        .ok_or_else(|| anyhow!("OpenCode model must be provider/model"))?;
    let mut value = json!({"providerID":provider,"id":id});
    if let Some(variant) = variant.filter(|s| !s.is_empty()) {
        value["variant"] = json!(variant);
    }
    Ok(value)
}

pub(super) fn validate_config(config: &Value) -> Result<()> {
    if !config.is_object() {
        bail!("OpenCode session config must be an object");
    }
    let cwd = config["cwd"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("OpenCode session requires a working directory"))?;
    if cwd.contains('\0') {
        bail!("OpenCode working directory contains NUL");
    }
    validate_options(&config["providerOptions"])
}

pub(super) fn normalize_config(mut config: Value) -> Result<Value> {
    validate_config(&config)?;
    if config["modeId"] == "full-access" {
        config["modeId"] = json!("build");
        if !config["featureValues"].is_object() {
            config["featureValues"] = json!({});
        }
        config["featureValues"]["auto_accept"] = json!(true);
    }
    if let Some(mode) = config["modeId"].as_str() {
        let mode = mode.trim().to_owned();
        if mode.is_empty() || mode == "default" {
            config.as_object_mut().unwrap().remove("modeId");
        } else {
            config["modeId"] = json!(mode);
        }
    }
    if let Some(variant) = config["thinkingOptionId"].as_str() {
        let variant = variant.trim().to_owned();
        if variant.is_empty() || variant == "default" {
            config.as_object_mut().unwrap().remove("thinkingOptionId");
        } else {
            config["thinkingOptionId"] = json!(variant);
        }
    }
    Ok(config)
}

fn validate_options(options: &Value) -> Result<()> {
    if options.is_null() {
        return Ok(());
    }
    let options = options
        .as_object()
        .ok_or_else(|| anyhow!("OpenCode providerOptions must be an object"))?;
    for key in options.keys() {
        if key != "permission" {
            bail!("Unknown OpenCode provider option: {key}");
        }
    }
    let Some(permission) = options.get("permission") else {
        return Ok(());
    };
    if permission.is_string() {
        return validate_effect(permission);
    }
    let permissions = permission
        .as_object()
        .ok_or_else(|| anyhow!("OpenCode permission must be an action or rule object"))?;
    for (name, rule) in permissions {
        let patterns = match name.as_str() {
            "read" | "edit" | "glob" | "grep" | "list" | "bash" | "task" | "external_directory"
            | "repo_clone" | "repo_overview" | "lsp" | "skill" => true,
            "todowrite" | "question" | "webfetch" | "websearch" | "codesearch" | "doom_loop" => {
                false
            }
            _ => bail!("Unknown OpenCode permission: {name}"),
        };
        if rule.is_string() {
            validate_effect(rule)?;
        } else if patterns {
            for effect in rule
                .as_object()
                .ok_or_else(|| anyhow!("Invalid OpenCode permission rule for {name}"))?
                .values()
            {
                validate_effect(effect)?;
            }
        } else {
            bail!("OpenCode permission {name} requires an action");
        }
    }
    Ok(())
}

fn validate_effect(value: &Value) -> Result<()> {
    if !matches!(value.as_str(), Some("ask" | "allow" | "deny")) {
        bail!("OpenCode permission action must be ask, allow, or deny");
    }
    Ok(())
}

pub(super) fn permission_rules(config: &Value, generation: Generation) -> Result<Vec<Value>> {
    validate_options(&config["providerOptions"])?;
    let mut rules = Vec::new();
    let make = |permission: &str, pattern: &str, action: &str| {
        if generation == Generation::Legacy {
            json!({"permission":permission,"pattern":pattern,"action":action})
        } else {
            json!({"action":match permission {"bash"=>"shell","task"=>"subagent",_=>permission},"resource":pattern,"effect":action})
        }
    };
    if let Some(grants) = config
        .pointer("/toolPolicy/preapproved")
        .and_then(Value::as_array)
    {
        for grant in grants {
            let server = grant["server"]
                .as_str()
                .ok_or_else(|| anyhow!("OpenCode preapproval requires server"))?;
            let tool = grant["tool"]
                .as_str()
                .ok_or_else(|| anyhow!("OpenCode preapproval requires tool"))?;
            let sanitize = |s: &str| {
                s.chars()
                    .map(|c| {
                        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                            c
                        } else {
                            '_'
                        }
                    })
                    .collect::<String>()
            };
            let name = if generation == Generation::Legacy {
                format!("{server}_{tool}")
            } else {
                format!("{}_{}", sanitize(server), sanitize(tool))
            };
            rules.push(make(&name, "*", "allow"));
        }
    }
    let permission = &config["providerOptions"]["permission"];
    if let Some(action) = permission.as_str() {
        rules.push(make("*", "*", action));
    } else if let Some(entries) = permission.as_object() {
        for (name, rule) in entries {
            if let Some(action) = rule.as_str() {
                rules.push(make(name, "*", action));
            } else if let Some(patterns) = rule.as_object() {
                for (pattern, action) in patterns {
                    rules.push(make(name, pattern, action.as_str().unwrap()));
                }
            }
        }
    }
    Ok(rules)
}

pub(super) async fn fetch(
    runtime: &Runtime,
    cwd: &str,
    provider: &str,
    cancel: CancellationToken,
) -> Result<Value> {
    let v2 = runtime.generation == Generation::V2;
    if v2 {
        refresh_activity(
            "plugin.list",
            wait_ready(runtime, cwd, false, cancel.clone()),
        )
        .await?;
    }
    let query = if v2 {
        location_query(cwd)
    } else {
        vec![("directory".into(), cwd.into())]
    };
    let (models, agents) = if v2 {
        let (models, agents, providers) = tokio::join!(
            refresh_activity(
                "model.list",
                runtime.request(Method::GET, "/api/model", &query, None, cancel.clone())
            ),
            refresh_activity(
                "agent.list",
                runtime.request(Method::GET, "/api/agent", &query, None, cancel.clone())
            ),
            refresh_activity(
                "provider.list",
                runtime.request(Method::GET, "/api/provider", &query, None, cancel.clone())
            ),
        );
        let providers = providers?;
        let agents = agents?;
        if providers["data"].as_array().is_none_or(Vec::is_empty) {
            bail!("OpenCode has no connected providers. Authenticate using opencode auth login.");
        }
        (v2_models(&models?, provider)?, agents)
    } else {
        let (providers, agents) = tokio::join!(
            refresh_activity(
                "provider.list",
                runtime.request(Method::GET, "/provider", &query, None, cancel.clone())
            ),
            refresh_activity(
                "app.agents",
                runtime.request(Method::GET, "/agent", &query, None, cancel)
            ),
        );
        (
            legacy_models(&providers?, provider)?,
            agents.unwrap_or_else(|_| json!([])),
        )
    };
    Ok(json!({"models":models,"modes":modes(runtime.generation,&agents)}))
}

fn v2_models(response: &Value, provider: &str) -> Result<Vec<Value>> {
    let mut result = Vec::new();
    for model in response["data"]
        .as_array()
        .ok_or_else(|| anyhow!("OpenCode model inventory is malformed"))?
    {
        if model["enabled"] != true {
            continue;
        }
        let provider_id = model["providerID"]
            .as_str()
            .ok_or_else(|| anyhow!("OpenCode model has no providerID"))?;
        let model_id = model["id"]
            .as_str()
            .ok_or_else(|| anyhow!("OpenCode model has no id"))?;
        let variants = model["variants"]
            .as_array()
            .map(|variants| {
                variants
                    .iter()
                    .map(|variant| json!({"id":variant["id"],"label":variant["id"]}))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        result.push(json!({"provider":provider,"id":format!("{provider_id}/{model_id}"),"label":model["name"],
            "contextWindowMaxTokens":model["limit"]["context"],"metadata":{"providerId":provider_id,"modelId":model_id,
            "supportsAttachments":model["capabilities"]["input"].as_array().is_some_and(|items|items.iter().any(|item|item=="image")),
            "supportsToolCall":model["capabilities"]["tools"],"contextWindowMaxTokens":model["limit"]["context"]},"thinkingOptions":variants}));
    }
    Ok(result)
}

fn legacy_models(response: &Value, provider: &str) -> Result<Vec<Value>> {
    let all = response["all"]
        .as_array()
        .ok_or_else(|| anyhow!("OpenCode provider inventory is malformed"))?;
    let connected = response["connected"].as_array();
    let accessible = |entry: &Value| {
        entry["source"] == "api" || connected.is_some_and(|ids| ids.contains(&entry["id"]))
    };
    if !all.iter().any(accessible) {
        bail!(
            "OpenCode has no connected providers. Please authenticate with at least one provider (e.g., openai, anthropic), set appropriate environment variables (e.g., OPENAI_API_KEY), or log in to OpenCode Go via the console."
        );
    }
    let mut result = Vec::new();
    for entry in all.iter().filter(|entry| accessible(entry)) {
        let provider_id = entry["id"]
            .as_str()
            .ok_or_else(|| anyhow!("OpenCode provider has no id"))?;
        for (id, model) in entry["models"]
            .as_object()
            .ok_or_else(|| anyhow!("OpenCode provider has no model inventory"))?
        {
            let mut metadata =
                json!({"providerId":provider_id,"providerName":entry["name"],"modelId":id});
            for (target, source) in [
                ("family", "family"),
                ("releaseDate", "release_date"),
                ("supportsAttachments", "attachment"),
                ("supportsReasoning", "reasoning"),
                ("supportsToolCall", "tool_call"),
                ("cost", "cost"),
                ("limit", "limit"),
            ] {
                if let Some(value) = model.get(source) {
                    metadata[target] = value.clone();
                }
            }
            if let Some(context) = model
                .pointer("/limit/context")
                .and_then(Value::as_f64)
                .filter(|v| v.is_finite() && *v > 0.0)
            {
                metadata["contextWindowMaxTokens"] = json!(context);
            }
            let mut definition = json!({"provider":provider,"id":format!("{provider_id}/{id}"),"label":model["name"],
                "description":format!("{} - {}",entry["name"].as_str().unwrap_or_default(),model["family"].as_str().unwrap_or_default()).trim(),"metadata":metadata});
            if let Some(variants) = model["variants"]
                .as_object()
                .filter(|variants| !variants.is_empty())
            {
                let mut options = vec![json!({"id":"default","label":"Default","isDefault":true})];
                options.extend(
                    variants
                        .keys()
                        .filter(|id| id.as_str() != "default")
                        .map(|id| json!({"id":id,"label":id})),
                );
                definition["thinkingOptions"] = json!(options);
                definition["defaultThinkingOptionId"] = json!("default");
            }
            result.push(definition);
        }
    }
    Ok(result)
}

pub(super) async fn commands(
    runtime: &Runtime,
    cwd: &str,
    cancel: CancellationToken,
) -> Result<Vec<Value>> {
    let mut commands = vec![
        json!({"name":"compact","description":"Compact the current session","argumentHint":"","kind":"command"}),
        json!({"name":"summarize","description":"Compact the current session","argumentHint":"","kind":"command"}),
    ];
    let v2 = runtime.generation == Generation::V2;
    if v2 {
        wait_ready(runtime, cwd, false, cancel.clone()).await?;
    }
    let query = if v2 {
        location_query(cwd)
    } else {
        vec![("directory".into(), cwd.into())]
    };
    let path = if v2 { "/api/command" } else { "/command" };
    let result = runtime
        .request(Method::GET, path, &query, None, cancel.clone())
        .await;
    let result = if v2 {
        result?
    } else {
        match result {
            Ok(value) => value,
            Err(_) => return Ok(commands),
        }
    };
    let items = if v2 { &result["data"] } else { &result };
    for command in items
        .as_array()
        .ok_or_else(|| anyhow!("OpenCode command inventory is malformed"))?
    {
        let Some(name) = command["name"].as_str() else {
            continue;
        };
        let hint = if v2 {
            String::new()
        } else {
            command["hints"]
                .as_array()
                .map(|hints| {
                    hints
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default()
        };
        let value = json!({"name":name,"description":command["description"].as_str().unwrap_or_default(),"argumentHint":hint,"kind":if !v2&&command["source"]=="skill" {"skill"}else{"command"}});
        if let Some(index) = commands.iter().position(|item| item["name"] == name) {
            commands[index] = value;
        } else {
            commands.push(value);
        }
    }
    if v2 {
        let skills = runtime
            .request(Method::GET, "/api/skill", &query, None, cancel)
            .await?;
        for skill in skills["data"]
            .as_array()
            .ok_or_else(|| anyhow!("OpenCode skill inventory is malformed"))?
        {
            let Some(name) = skill["id"].as_str() else {
                continue;
            };
            if commands.iter().any(|item| item["name"] == name) {
                continue;
            }
            commands.push(json!({"name":name,"description":skill["description"].as_str().unwrap_or_default(),"argumentHint":"","kind":"skill"}));
        }
    }
    Ok(commands)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_permission_generations_without_widening_exact_grants() {
        let config = json!({
            "providerOptions":{"permission":{"bash":{"pwd":"allow"},"task":"ask"}},
            "toolPolicy":{"preapproved":[{"server":"paseo.host","tool":"read/info"}]}
        });
        assert_eq!(
            permission_rules(&config, Generation::V2).unwrap(),
            vec![
                json!({"action":"paseo_host_read_info","resource":"*","effect":"allow"}),
                json!({"action":"shell","resource":"pwd","effect":"allow"}),
                json!({"action":"subagent","resource":"*","effect":"ask"}),
            ]
        );
        assert_eq!(
            permission_rules(&config, Generation::Legacy).unwrap(),
            vec![
                json!({"permission":"paseo.host_read/info","pattern":"*","action":"allow"}),
                json!({"permission":"bash","pattern":"pwd","action":"allow"}),
                json!({"permission":"task","pattern":"*","action":"ask"}),
            ]
        );
    }

    #[test]
    fn v2_catalog_filters_disabled_models_and_hidden_subagents() {
        let enabled = json!({"providerID":"provider","id":"model","name":"Model","enabled":true,
            "limit":{"context":200000},"capabilities":{"input":["text","image"],"tools":true},
            "variants":[{"id":"high"}]});
        let mut disabled = enabled.clone();
        disabled["enabled"] = json!(false);
        disabled["id"] = json!("disabled");
        let models = v2_models(&json!({"data":[enabled,disabled]}), "custom-opencode").unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0]["provider"], "custom-opencode");
        assert_eq!(models[0]["id"], "provider/model");
        assert_eq!(models[0]["metadata"]["supportsAttachments"], true);
        assert_eq!(
            models[0]["thinkingOptions"],
            json!([{"id":"high","label":"high"}])
        );
        assert_eq!(
            modes(
                Generation::V2,
                &json!({"data":[
                    {"id":"build","name":"Build","mode":"primary"},
                    {"id":"secret","name":"Secret","mode":"primary","hidden":true},
                    {"id":"child","name":"Child","mode":"subagent"}
                ]})
            ),
            vec![json!({"id":"build","label":"Build"})]
        );
    }

    #[test]
    fn legacy_catalog_accepts_console_models_and_keeps_unknown_variants() {
        let models = legacy_models(&json!({"connected":[],"all":[
            {"id":"console","name":"Console","source":"api","models":{
                "native":{"name":"Native","limit":{"context":4096},"variants":{"default":{},"novel":{}}}
            }},
            {"id":"offline","name":"Offline","source":"config","models":{"hidden":{"name":"Hidden"}}}
        ]}), "opencode").unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0]["id"], "console/native");
        assert_eq!(
            models[0]["thinkingOptions"],
            json!([
                {"id":"default","label":"Default","isDefault":true},{"id":"novel","label":"novel"}
            ])
        );
        assert!(legacy_models(&json!({"connected":[],"all":[]}), "opencode").is_err());
    }

    #[test]
    fn normalizes_legacy_unattended_without_discarding_opaque_metadata() {
        let config = normalize_config(json!({"cwd":"/work","modeId":"full-access",
            "thinkingOptionId":"default","featureValues":{"custom":1},"opaque":{"native":true}}))
        .unwrap();
        assert_eq!(config["modeId"], "build");
        assert_eq!(
            config["featureValues"],
            json!({"custom":1,"auto_accept":true})
        );
        assert_eq!(config["opaque"], json!({"native":true}));
        assert!(config.get("thinkingOptionId").is_none());
        assert!(
            permission_rules(
                &json!({"providerOptions":{"permission":{"bash":"unexpected"}}}),
                Generation::V2
            )
            .is_err()
        );
        assert!(model_ref("no-provider", None).is_err());
        assert_eq!(
            model_ref("provider/model/with/slashes", Some("high")).unwrap(),
            json!({"providerID":"provider","id":"model/with/slashes","variant":"high"})
        );
    }

    #[test]
    fn unattended_creation_keeps_engine_default_and_explicit_opt_out() {
        assert_eq!(resolve_create_config(&json!({
            "provider":"opencode","unattended":true,"parent":{"provider":"codex","modeId":"full-access","isUnattended":true},
            "availableModes":[{"id":"custom"}]
        })).unwrap().unwrap(),json!({"featureValues":{"auto_accept":true}}));
        assert_eq!(
            resolve_create_config(&json!({
                "provider":"opencode","unattended":true,"requestedMode":"custom",
                "featureValues":{"auto_accept":false},"availableModes":[{"id":"custom"}]
            }))
            .unwrap()
            .unwrap(),
            json!({"modeId":"custom","featureValues":{"auto_accept":false}})
        );
        assert_eq!(
            resolve_create_config(&json!({
                "provider":"opencode","requestedMode":"full-access",
                "featureValues":{"auto_accept":false},"availableModes":[{"id":"build"}]
            }))
            .unwrap()
            .unwrap(),
            json!({"modeId":"build","featureValues":{"auto_accept":true}})
        );
        assert!(
            resolve_create_config(
                &json!({"provider":"opencode","requestedMode":"full-access","availableModes":[]})
            )
            .unwrap()
            .is_err()
        );
        assert!(
            resolve_create_config(&json!({"provider":"opencode","requestedMode":"custom"}))
                .is_none()
        );
    }
}
