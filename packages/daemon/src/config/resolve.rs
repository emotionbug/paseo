use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};

const RELAY_ENDPOINT: &str = "relay.paseo.sh:443";
const APP_BASE_URL: &str = "https://app.paseo.sh";
const STT_MODEL: &str = "parakeet-tdt-0.6b-v2-int8";
const TTS_MODEL: &str = "kokoro-en-v0_19";
const CONFIG_ENV: &[&str] = &[
    "MCP_DEBUG",
    "OPENAI_STT_BASE_URL",
    "OPENAI_TTS_BASE_URL",
    "PASEO_ALLOWED_HOSTS",
    "PASEO_APP_BASE_URL",
    "PASEO_CORS_ORIGINS",
    "PASEO_DICTATION_ENABLED",
    "PASEO_DICTATION_LANGUAGE",
    "PASEO_DICTATION_LOCAL_STT_MODEL",
    "PASEO_DICTATION_STT_PROVIDER",
    "PASEO_GIT_CONCURRENCY",
    "PASEO_GIT_MAX_PROCESSES_PER_SECOND",
    "PASEO_GIT_MAX_PROCESS_CONCURRENCY",
    "PASEO_HOSTNAMES",
    "PASEO_LISTEN",
    "PASEO_LOCAL_MODELS_DIR",
    "PASEO_LOG",
    "PASEO_LOG_CONSOLE_FORMAT",
    "PASEO_LOG_CONSOLE_LEVEL",
    "PASEO_LOG_FILE_LEVEL",
    "PASEO_LOG_FILE_PATH",
    "PASEO_LOG_FILE_ROTATE_COUNT",
    "PASEO_LOG_FILE_ROTATE_SIZE",
    "PASEO_LOG_FORMAT",
    "PASEO_LOG_LEVEL",
    "PASEO_LOG_ROTATE_COUNT",
    "PASEO_LOG_ROTATE_SIZE",
    "PASEO_PASSWORD",
    "PASEO_PLUGIN_REGISTRY",
    "PASEO_RELAY_ENABLED",
    "PASEO_RELAY_ENDPOINT",
    "PASEO_RELAY_PUBLIC_ENDPOINT",
    "PASEO_RELAY_PUBLIC_USE_TLS",
    "PASEO_RELAY_USE_TLS",
    "PASEO_SERVICE_PROXY_ENABLED",
    "PASEO_SERVICE_PROXY_LISTEN",
    "PASEO_SERVICE_PROXY_PUBLIC_BASE_URL",
    "PASEO_TRUSTED_PROXIES",
    "PASEO_VOICE_LANGUAGE",
    "PASEO_VOICE_LLM_PROVIDER",
    "PASEO_VOICE_LOCAL_STT_MODEL",
    "PASEO_VOICE_LOCAL_TTS_MODEL",
    "PASEO_VOICE_LOCAL_TTS_SPEAKER_ID",
    "PASEO_VOICE_LOCAL_TTS_SPEED",
    "PASEO_VOICE_MODE_ENABLED",
    "PASEO_VOICE_STT_PROVIDER",
    "PASEO_VOICE_TTS_PROVIDER",
    "PASEO_VOICE_TURN_DETECTION_PROVIDER",
    "PORT",
    "STT_CONFIDENCE_THRESHOLD",
    "STT_MODEL",
    "TTS_MODEL",
    "TTS_VOICE",
    "PASEO_NODE_ENV",
    "PASEO_DESKTOP_MANAGED",
    "OPENAI_API_KEY",
    "OPENAI_BASE_URL",
    "OPENAI_STT_API_KEY",
    "OPENAI_TTS_API_KEY",
];

fn ev<'a>(env: &'a BTreeMap<String, String>, key: &str) -> Option<&'a str> {
    env.get(key).map(String::as_str)
}
fn present<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    value.pointer(path).filter(|v| !v.is_null())
}
fn string_at<'a>(value: &'a Value, path: &str) -> Option<&'a str> {
    present(value, path).and_then(Value::as_str)
}
fn pick(
    env: &BTreeMap<String, String>,
    key: &str,
    persisted: &Value,
    path: &str,
    fallback: Value,
) -> Value {
    ev(env, key)
        .map(|v| json!(v))
        .or_else(|| present(persisted, path).cloned())
        .unwrap_or(fallback)
}
fn defaulted(value: &Value, path: &str, fallback: Value) -> Value {
    present(value, path).cloned().unwrap_or(fallback)
}
fn boolean(value: Option<&str>) -> Option<bool> {
    match value?.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}
fn nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}
// JavaScript Number accepts radix-prefixed strings as well as decimal exponents.
fn number(value: &str) -> Option<f64> {
    let value = value.trim();
    if value.is_empty() {
        return Some(0.0);
    }
    for (prefixes, radix) in [(["0x", "0X"], 16), (["0o", "0O"], 8), (["0b", "0B"], 2)] {
        if let Some(digits) = prefixes.iter().find_map(|p| value.strip_prefix(*p)) {
            if digits.is_empty() {
                return None;
            }
            return digits.chars().try_fold(0.0, |n, c| {
                c.to_digit(radix).map(|d| n * radix as f64 + d as f64)
            });
        }
    }
    value.parse().ok()
}
fn numeric_value(n: f64) -> Value {
    if n.fract() == 0.0 && n >= i64::MIN as f64 && n < -(i64::MIN as f64) {
        json!(n as i64)
    } else {
        json!(n)
    }
}
fn positive(value: Option<&str>) -> Option<Value> {
    let n = number(value?)?;
    (n.is_finite() && n > 0.0 && n.fract() == 0.0).then(|| numeric_value(n))
}
fn finite(value: Option<Value>, integer: bool, name: &str) -> Result<Option<Value>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let parsed = match &value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) if !s.trim().is_empty() => number(s),
        _ => None,
    };
    match parsed {
        Some(n) if n.is_finite() && (!integer || n.fract() == 0.0) => Ok(Some(numeric_value(n))),
        _ => bail!(
            "Invalid {name}: expected a finite {}",
            if integer { "integer" } else { "number" }
        ),
    }
}
fn log_enum(value: Option<&str>, format: bool) -> Option<Value> {
    let normalized = value?.trim().to_ascii_lowercase();
    let allowed: &[&str] = if format {
        &["pretty", "json"]
    } else {
        &["trace", "debug", "info", "warn", "error", "fatal"]
    };
    allowed
        .contains(&normalized.as_str())
        .then(|| json!(normalized))
}
fn insert_optional(target: &mut Value, key: &str, value: Option<Value>) {
    if let Some(value) = value {
        target[key] = value;
    }
}
fn copy_optional(target: &mut Value, key: &str, source: &Value, path: &str) {
    insert_optional(target, key, source.pointer(path).cloned());
}
fn lexical_absolute(base: &Path, path: &Path) -> Result<PathBuf> {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    let joined = if joined.is_absolute() {
        joined
    } else {
        std::env::current_dir()?.join(joined)
    };
    let mut normalized = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            component => normalized.push(component.as_os_str()),
        }
    }
    Ok(normalized)
}

pub fn resolve(
    home: &Path,
    user_home: &Path,
    persisted: &Value,
    env: &BTreeMap<String, String>,
) -> Result<Value> {
    resolve_with_fallback(home, user_home, persisted, env, None)
}

pub fn resolve_with_fallback(
    home: &Path,
    user_home: &Path,
    persisted: &Value,
    env: &BTreeMap<String, String>,
    relay_fallback: Option<bool>,
) -> Result<Value> {
    resolve_with_options(home, user_home, persisted, env, relay_fallback, &json!({}))
}

pub fn resolve_with_options(
    home: &Path,
    user_home: &Path,
    persisted: &Value,
    env: &BTreeMap<String, String>,
    relay_fallback: Option<bool>,
    cli: &Value,
) -> Result<Value> {
    let fallback =
        relay_fallback.unwrap_or_else(|| persisted.pointer("/daemon/relay/enabled").is_none());
    let relay_enabled = cli
        .get("relayEnabled")
        .and_then(Value::as_bool)
        .or_else(|| boolean(ev(env, "PASEO_RELAY_ENABLED")));
    let endpoint = pick(
        env,
        "PASEO_RELAY_ENDPOINT",
        persisted,
        "/daemon/relay/endpoint",
        json!(RELAY_ENDPOINT),
    );
    let public_endpoint = pick(
        env,
        "PASEO_RELAY_PUBLIC_ENDPOINT",
        persisted,
        "/daemon/relay/publicEndpoint",
        endpoint.clone(),
    );
    let tls = cli
        .get("relayUseTls")
        .and_then(Value::as_bool)
        .or_else(|| ev(env, "PASEO_RELAY_USE_TLS").map(|v| boolean(Some(v)).unwrap_or(false)))
        .or_else(|| {
            persisted
                .pointer("/daemon/relay/useTls")
                .and_then(Value::as_bool)
        })
        .unwrap_or(endpoint.as_str() == Some(RELAY_ENDPOINT));
    let public_tls = ev(env, "PASEO_RELAY_PUBLIC_USE_TLS")
        .map(|v| boolean(Some(v)).unwrap_or(false))
        .or_else(|| {
            persisted
                .pointer("/daemon/relay/publicUseTls")
                .and_then(Value::as_bool)
        })
        .unwrap_or(tls);
    let optional_layers = boolean(ev(env, "PASEO_SERVICE_PROXY_ENABLED")).or_else(|| {
        persisted
            .pointer("/daemon/serviceProxy/enabled")
            .and_then(Value::as_bool)
    }) != Some(false);
    let mut service_proxy = json!({"publicBaseUrl": null, "standaloneListen": null});
    if optional_layers {
        let public = pick(
            env,
            "PASEO_SERVICE_PROXY_PUBLIC_BASE_URL",
            persisted,
            "/daemon/serviceProxy/publicBaseUrl",
            Value::Null,
        );
        if let Some(raw) = public.as_str() {
            let url = url::Url::parse(raw)
                .with_context(|| format!("Invalid PASEO_SERVICE_PROXY_PUBLIC_BASE_URL: {raw}"))?;
            let url = url.as_str();
            service_proxy["publicBaseUrl"] = json!(url.strip_suffix('/').unwrap_or(url));
        }
        service_proxy["standaloneListen"] = pick(
            env,
            "PASEO_SERVICE_PROXY_LISTEN",
            persisted,
            "/daemon/serviceProxy/listen",
            Value::Null,
        );
    }
    let (speech, openai) = speech_config(home, persisted, env)?;
    let voice_provider = ev(env, "PASEO_VOICE_LLM_PROVIDER")
        .or_else(|| string_at(persisted, "/features/voiceMode/llm/provider"))
        .map(|s| s.trim().to_lowercase());
    let git = json!({
        "maxProcessesPerSecond": positive(ev(env, "PASEO_GIT_MAX_PROCESSES_PER_SECOND"))
            .unwrap_or_else(|| defaulted(persisted, "/daemon/git/maxProcessesPerSecond", json!(64))),
        "maxProcessConcurrency": positive(ev(env, "PASEO_GIT_MAX_PROCESS_CONCURRENCY"))
            .or_else(|| positive(ev(env, "PASEO_GIT_CONCURRENCY")))
            .unwrap_or_else(|| defaulted(persisted, "/daemon/git/maxProcessConcurrency", json!(8))),
    });
    let mut controlled: BTreeSet<String> = override_paths_for(env, &speech["providers"])
        .into_iter()
        .collect();
    let mut retained_cli = Map::new();
    for (key, path) in [
        ("listen", "daemon.listen"),
        ("relayEnabled", "daemon.relay.enabled"),
        ("relayUseTls", "daemon.relay.useTls"),
        ("mcpEnabled", "daemon.mcp.enabled"),
        ("mcpInjectIntoAgents", "daemon.mcp.injectIntoAgents"),
        ("hostnames", ""),
    ] {
        if let Some(value) = cli.get(key) {
            retained_cli.insert(key.to_owned(), value.clone());
            if !path.is_empty() {
                controlled.insert(path.to_owned());
            }
        }
    }
    let mut resolved = json!({
        "listen": present(cli, "/listen").cloned().unwrap_or_else(|| pick(env, "PASEO_LISTEN", persisted, "/daemon/listen", json!(format!("127.0.0.1:{}", ev(env,"PORT").unwrap_or("6767"))))),
        "paseoHome": home,
        "desktopManaged": ev(env, "PASEO_DESKTOP_MANAGED") == Some("1"),
        "corsAllowedOrigins": cors(persisted, env),
        "hostnames": hostnames(persisted, env, cli),
        "trustedProxies": trusted_proxies(env).unwrap_or_else(|| defaulted(persisted, "/daemon/trustedProxies", json!(["loopback"]))),
        "mcpEnabled": present(cli, "/mcpEnabled").cloned().unwrap_or_else(|| defaulted(persisted, "/daemon/mcp/enabled", json!(true))),
        "mcpInjectIntoAgents": present(cli, "/mcpInjectIntoAgents").cloned().unwrap_or_else(|| defaulted(persisted, "/daemon/mcp/injectIntoAgents", json!(false))),
        "git": git,
        "autoArchiveAfterMerge": defaulted(persisted, "/daemon/autoArchiveAfterMerge", json!(false)),
        "enableTerminalAgentHooks": defaulted(persisted, "/daemon/enableTerminalAgentHooks", json!(false)),
        "appendSystemPrompt": defaulted(persisted, "/daemon/appendSystemPrompt", json!("")),
        "pluginsEnabled": defaulted(persisted, "/pluginsEnabled", json!(false)),
        "mcpDebug": ev(env, "MCP_DEBUG") == Some("1"),
        "isDev": ev(env, "PASEO_NODE_ENV") == Some("development"),
        "agentStoragePath": home.join("agents"),
        "staticDir": "public",
        "agentClients": {},
        "relayEnabled": relay_enabled.map(Value::Bool).unwrap_or_else(|| defaulted(persisted, "/daemon/relay/enabled", json!(fallback))),
        "relayEnabledMutable": relay_enabled.is_none(),
        "relayRequireProtectedSecurity": defaulted(persisted, "/daemon/relay/requireProtectedSecurity", json!(false)),
        "relayEndpoint": endpoint,
        "relayPublicEndpoint": public_endpoint,
        "relayUseTls": tls,
        "relayPublicUseTls": public_tls,
        "serviceProxy": service_proxy,
        "appBaseUrl": pick(env, "PASEO_APP_BASE_URL", persisted, "/app/baseUrl", json!(APP_BASE_URL)),
        "voiceLlmProvider": voice_provider,
        "voiceLlmProviderExplicit": voice_provider.is_some(),
        "voiceLlmModel": defaulted(persisted, "/features/voiceMode/llm/model", Value::Null),
        "configReload": {
            "env": env.iter().filter(|(key, _)| CONFIG_ENV.contains(&key.as_str())).collect::<BTreeMap<_, _>>(),
            "overrideControlledPaths": controlled.into_iter().collect::<Vec<_>>(),
            "relayEnabledFallback": fallback,
            "startupPersisted": persisted,
        },
        "speech": speech,
    });
    if !retained_cli.is_empty() {
        resolved["configReload"]["cli"] = Value::Object(retained_cli);
    }
    if let Some(root) = nonempty(string_at(persisted, "/worktrees/root")) {
        let expanded = if root == "~" {
            user_home.to_path_buf()
        } else if let Some(relative) = root.strip_prefix("~/") {
            user_home.join(relative)
        } else {
            PathBuf::from(root)
        };
        resolved["worktreesRoot"] = json!(lexical_absolute(home, &expanded)?);
    }
    for (key, path) in [
        ("terminalProfiles", "/daemon/terminalProfiles"),
        ("agentProfiles", "/daemon/agentProfiles"),
        ("skillSelection", "/agents/skills/selection"),
        ("plugins", "/plugins"),
        ("pluginRegistries", "/pluginRegistries"),
        (
            "providerCatalogRefreshTimeoutMs",
            "/agents/catalogRefreshTimeoutMs",
        ),
        ("metadataGeneration", "/agents/metadataGeneration"),
    ] {
        copy_optional(&mut resolved, key, persisted, path);
    }
    insert_optional(
        &mut resolved,
        "pluginRegistryUrl",
        ev(env, "PASEO_PLUGIN_REGISTRY").map(|s| json!(s)),
    );
    insert_optional(&mut resolved, "openai", openai);
    if let Some(password) = nonempty(ev(env, "PASEO_PASSWORD")) {
        // bcryptjs truncates UTF-8 password bytes at 72 bytes, including mid-codepoint.
        let bytes = password.as_bytes();
        resolved["auth"] = json!({"password": bcrypt::hash(&bytes[..bytes.len().min(72)], 12)?});
    } else if let Some(password) =
        string_at(persisted, "/daemon/auth/password").filter(|s| !s.is_empty())
    {
        resolved["auth"] = json!({"password": password});
    }
    if let Some(providers) = persisted
        .pointer("/agents/providers")
        .and_then(Value::as_object)
    {
        let mut overrides = Map::new();
        for (id, provider) in providers {
            if let Ok(provider) = super::schema::normalize_provider(provider.clone()) {
                overrides.insert(id.clone(), provider);
            }
        }
        if !overrides.is_empty() {
            let overrides = Value::Object(overrides);
            insert_optional(
                &mut resolved,
                "agentProviderSettings",
                provider_settings(&overrides),
            );
            resolved["providerOverrides"] = overrides;
        }
    }
    insert_optional(&mut resolved, "log", log_config(persisted, env));
    Ok(resolved)
}

fn provider_settings(providers: &Value) -> Option<Value> {
    let mut settings = Map::new();
    for (id, provider) in providers.as_object()? {
        let mut entry = json!({});
        if let Some(command) = provider.get("command") {
            entry["command"] = json!({"mode":"replace", "argv":command});
        }
        copy_optional(&mut entry, "env", provider, "/env");
        if !entry.as_object().unwrap().is_empty() {
            settings.insert(id.clone(), entry);
        }
    }
    (!settings.is_empty()).then_some(Value::Object(settings))
}

fn csv(value: &str) -> impl Iterator<Item = &str> {
    value.split(',').map(str::trim).filter(|s| !s.is_empty())
}
fn unique(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut result = Vec::new();
    let mut seen = BTreeSet::new();
    for value in values {
        if seen.insert(value.clone()) {
            result.push(value);
        }
    }
    result
}
fn cors(persisted: &Value, env: &BTreeMap<String, String>) -> Value {
    let configured = persisted
        .pointer("/daemon/cors/allowedOrigins")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    json!(unique(configured.chain(
        csv(ev(env, "PASEO_CORS_ORIGINS").unwrap_or("")).map(str::to_owned)
    )))
}
fn hostnames(persisted: &Value, env: &BTreeMap<String, String>, cli: &Value) -> Value {
    let configured = persisted.pointer("/daemon/hostnames");
    let from_env = ev(env, "PASEO_HOSTNAMES").or_else(|| ev(env, "PASEO_ALLOWED_HOSTS"));
    if configured.and_then(Value::as_bool) == Some(true)
        || from_env.is_some_and(|s| s.trim().eq_ignore_ascii_case("true"))
        || cli.get("hostnames").and_then(Value::as_bool) == Some(true)
    {
        return json!(true);
    }
    let configured = configured
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let cli_names = cli
        .get("hostnames")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    json!(unique(
        configured
            .chain(csv(from_env.unwrap_or("")).map(str::to_owned))
            .chain(cli_names)
    ))
}
fn trusted_proxies(env: &BTreeMap<String, String>) -> Option<Value> {
    let raw = nonempty(ev(env, "PASEO_TRUSTED_PROXIES"))?;
    Some(match boolean(Some(raw)) {
        Some(true) => json!(true),
        Some(false) => json!([]),
        None => json!(csv(raw).collect::<Vec<_>>()),
    })
}
fn log_config(persisted: &Value, env: &BTreeMap<String, String>) -> Option<Value> {
    let mut log = persisted.get("log").cloned().unwrap_or_else(|| json!({}));
    for (path, value) in log_overrides(env) {
        set_path(&mut log, &path, value);
    }
    (persisted.get("log").is_some() || log.as_object().is_some_and(|o| !o.is_empty()))
        .then_some(log)
}
fn set_path(target: &mut Value, path: &[&str], value: Value) {
    let mut cursor = target;
    for key in &path[..path.len() - 1] {
        if !cursor.get(*key).is_some_and(Value::is_object) {
            cursor[*key] = json!({});
        }
        cursor = &mut cursor[*key];
    }
    cursor[path[path.len() - 1]] = value;
}
fn log_overrides(env: &BTreeMap<String, String>) -> Vec<(Vec<&'static str>, Value)> {
    let mut result = Vec::new();
    for (key, path, format) in [
        ("PASEO_LOG_LEVEL", vec!["level"], false),
        ("PASEO_LOG_FORMAT", vec!["format"], true),
        ("PASEO_LOG_CONSOLE_LEVEL", vec!["console", "level"], false),
        ("PASEO_LOG_CONSOLE_FORMAT", vec!["console", "format"], true),
        ("PASEO_LOG_FILE_LEVEL", vec!["file", "level"], false),
    ] {
        let raw = ev(env, key).or_else(|| {
            (key == "PASEO_LOG_LEVEL")
                .then(|| ev(env, "PASEO_LOG"))
                .flatten()
        });
        if let Some(value) = log_enum(raw, format) {
            result.push((path, value));
        }
    }
    for (key, path) in [
        ("PASEO_LOG_FILE_PATH", vec!["file", "path"]),
        (
            "PASEO_LOG_FILE_ROTATE_SIZE",
            vec!["file", "rotate", "maxSize"],
        ),
    ] {
        if let Some(value) = nonempty(ev(env, key)) {
            result.push((path, json!(value)));
        }
    }
    if let Some(value) = positive(ev(env, "PASEO_LOG_FILE_ROTATE_COUNT")) {
        result.push((vec!["file", "rotate", "maxFiles"], value));
    }
    result
}

fn speech_enabled(
    env: &BTreeMap<String, String>,
    key: &str,
    persisted: &Value,
    path: &str,
) -> bool {
    if let Some(raw) = ev(env, key) {
        return match raw.trim().to_ascii_lowercase().as_str() {
            "y" => true,
            "n" => false,
            _ => boolean(Some(raw)).unwrap_or(true),
        };
    }
    persisted
        .pointer(path)
        .and_then(Value::as_bool)
        .unwrap_or(true)
}
fn requested_speech(persisted: &Value, env: &BTreeMap<String, String>) -> Result<Value> {
    let dictation = speech_enabled(
        env,
        "PASEO_DICTATION_ENABLED",
        persisted,
        "/features/dictation/enabled",
    );
    let voice = speech_enabled(
        env,
        "PASEO_VOICE_MODE_ENABLED",
        persisted,
        "/features/voiceMode/enabled",
    );
    let mut providers = json!({});
    for (name, key, path, enabled) in [
        (
            "dictationStt",
            "PASEO_DICTATION_STT_PROVIDER",
            "/features/dictation/stt/provider",
            dictation,
        ),
        (
            "voiceTurnDetection",
            "PASEO_VOICE_TURN_DETECTION_PROVIDER",
            "/features/voiceMode/turnDetection/provider",
            voice,
        ),
        (
            "voiceStt",
            "PASEO_VOICE_STT_PROVIDER",
            "/features/voiceMode/stt/provider",
            voice,
        ),
        (
            "voiceTts",
            "PASEO_VOICE_TTS_PROVIDER",
            "/features/voiceMode/tts/provider",
            voice,
        ),
    ] {
        let configured = ev(env, key).or_else(|| string_at(persisted, path));
        let provider = configured.unwrap_or("local").trim().to_lowercase();
        if !["local", "openai"].contains(&provider.as_str()) {
            bail!("Invalid speech provider for {name}: {provider}");
        }
        providers[name] =
            json!({"provider":provider, "explicit":configured.is_some(), "enabled":enabled});
    }
    Ok(providers)
}
fn active(providers: &Value, name: &str, expected: &str) -> bool {
    providers[name]["enabled"].as_bool() != Some(false)
        && providers[name]["provider"].as_str() == Some(expected)
}
fn first_nonempty<'a>(values: impl IntoIterator<Item = Option<&'a str>>) -> Option<&'a str> {
    values.into_iter().find_map(nonempty)
}
fn speech_config(
    home: &Path,
    persisted: &Value,
    env: &BTreeMap<String, String>,
) -> Result<(Value, Option<Value>)> {
    let providers = requested_speech(persisted, env)?;
    let dictation_language = first_nonempty([
        ev(env, "PASEO_DICTATION_LANGUAGE"),
        string_at(persisted, "/features/dictation/stt/language"),
    ])
    .unwrap_or("en");
    let voice_language = first_nonempty([
        ev(env, "PASEO_VOICE_LANGUAGE"),
        ev(env, "PASEO_DICTATION_LANGUAGE"),
        string_at(persisted, "/features/voiceMode/stt/language"),
        string_at(persisted, "/features/dictation/stt/language"),
    ])
    .unwrap_or("en");
    // Local schemas are parsed even when no local endpoint is selected.
    let models_dir = pick(
        env,
        "PASEO_LOCAL_MODELS_DIR",
        persisted,
        "/providers/local/modelsDir",
        json!(home.join("models/local-speech")),
    );
    let models_dir = models_dir
        .as_str()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .context("Invalid local speech modelsDir")?;
    let mut models = json!({});
    for (name, key, path, default) in [
        (
            "dictationStt",
            "PASEO_DICTATION_LOCAL_STT_MODEL",
            "/features/dictation/stt/model",
            STT_MODEL,
        ),
        (
            "voiceStt",
            "PASEO_VOICE_LOCAL_STT_MODEL",
            "/features/voiceMode/stt/model",
            STT_MODEL,
        ),
        (
            "voiceTts",
            "PASEO_VOICE_LOCAL_TTS_MODEL",
            "/features/voiceMode/tts/model",
            TTS_MODEL,
        ),
    ] {
        let model = ev(env, key)
            .or_else(|| {
                if active(&providers, name, "local") {
                    string_at(persisted, path)
                } else {
                    None
                }
            })
            .unwrap_or(default)
            .trim()
            .to_lowercase();
        let valid = if name == "voiceTts" {
            model == TTS_MODEL
        } else {
            [STT_MODEL, "parakeet-tdt-0.6b-v3-int8"].contains(&model.as_str())
        };
        if !valid {
            bail!("Invalid model id: {model}");
        }
        models[name] = json!(model);
    }
    for (field, key, path, integer) in [
        (
            "voiceTtsSpeakerId",
            "PASEO_VOICE_LOCAL_TTS_SPEAKER_ID",
            "/features/voiceMode/tts/speakerId",
            true,
        ),
        (
            "voiceTtsSpeed",
            "PASEO_VOICE_LOCAL_TTS_SPEED",
            "/features/voiceMode/tts/speed",
            false,
        ),
    ] {
        let input = ev(env, key)
            .map(|s| json!(s))
            .or_else(|| present(persisted, path).cloned());
        insert_optional(&mut models, field, finite(input, integer, key)?);
    }
    if models.get("voiceTtsSpeakerId").is_none() && models["voiceTts"] == TTS_MODEL {
        models["voiceTtsSpeakerId"] = json!(0);
    }
    let include_local = ["dictationStt", "voiceStt", "voiceTts"]
        .iter()
        .any(|name| active(&providers, name, "local"))
        || env.contains_key("PASEO_LOCAL_MODELS_DIR")
        || persisted.pointer("/providers/local/modelsDir").is_some();
    let openai = openai_config(persisted, env, &providers)?;
    let mut speech = json!({"providers":providers, "sttLanguages":{"dictation":dictation_language,"voice":voice_language}});
    if include_local {
        speech["local"] = json!({"modelsDir":models_dir,"models":models});
    }
    Ok((speech, openai))
}
fn openai_config(
    persisted: &Value,
    env: &BTreeMap<String, String>,
    providers: &Value,
) -> Result<Option<Value>> {
    let mut result = json!({});
    for (endpoint, env_key, env_base, key_path, base_path) in [
        (
            "stt",
            "OPENAI_STT_API_KEY",
            "OPENAI_STT_BASE_URL",
            "/providers/openai/stt/apiKey",
            "/providers/openai/stt/baseUrl",
        ),
        (
            "tts",
            "OPENAI_TTS_API_KEY",
            "OPENAI_TTS_BASE_URL",
            "/providers/openai/tts/apiKey",
            "/providers/openai/tts/baseUrl",
        ),
    ] {
        let key = first_nonempty([
            string_at(persisted, key_path),
            ev(env, env_key),
            string_at(persisted, "/providers/openai/apiKey"),
            ev(env, "OPENAI_API_KEY"),
        ]);
        let Some(key) = key else {
            continue;
        };
        let base = first_nonempty([
            string_at(persisted, base_path),
            ev(env, env_base),
            string_at(persisted, "/providers/openai/baseUrl"),
            ev(env, "OPENAI_BASE_URL"),
        ]);
        let mut config = json!({"apiKey":key});
        insert_optional(&mut config, "baseUrl", base.map(|s| json!(s)));
        if endpoint == "stt" {
            let threshold = nonempty(ev(env, "STT_CONFIDENCE_THRESHOLD"))
                .map(|s| json!(s))
                .or_else(|| {
                    present(persisted, "/features/dictation/stt/confidenceThreshold").cloned()
                });
            insert_optional(
                &mut config,
                "confidenceThreshold",
                finite(threshold, false, "STT_CONFIDENCE_THRESHOLD")?,
            );
            let model = first_nonempty([
                ev(env, "STT_MODEL"),
                if active(providers, "voiceStt", "openai") {
                    string_at(persisted, "/features/voiceMode/stt/model")
                } else {
                    None
                },
                if active(providers, "dictationStt", "openai") {
                    string_at(persisted, "/features/dictation/stt/model")
                } else {
                    None
                },
            ]);
            insert_optional(&mut config, "model", model.map(|s| json!(s)));
        } else {
            for (field, key, path, fallback) in [
                (
                    "voice",
                    "TTS_VOICE",
                    "/features/voiceMode/tts/voice",
                    "alloy",
                ),
                (
                    "model",
                    "TTS_MODEL",
                    "/features/voiceMode/tts/model",
                    "tts-1",
                ),
            ] {
                config[field] = json!(
                    first_nonempty([
                        ev(env, key),
                        if active(providers, "voiceTts", "openai") {
                            string_at(persisted, path)
                        } else {
                            None
                        }
                    ])
                    .unwrap_or(fallback)
                );
            }
            config["responseFormat"] = json!("pcm");
        }
        result[endpoint] = config;
    }
    Ok((!result.as_object().unwrap().is_empty()).then_some(result))
}

fn override_paths_for(env: &BTreeMap<String, String>, providers: &Value) -> Vec<String> {
    let mut paths = BTreeSet::new();
    let mut add = |condition: bool, path: &str| {
        if condition {
            paths.insert(path.to_owned());
        }
    };
    for (key, path) in [
        ("PASEO_LISTEN", "daemon.listen"),
        ("PASEO_APP_BASE_URL", "app.baseUrl"),
        ("PASEO_RELAY_ENDPOINT", "daemon.relay.endpoint"),
        ("PASEO_RELAY_PUBLIC_ENDPOINT", "daemon.relay.publicEndpoint"),
        ("PASEO_RELAY_USE_TLS", "daemon.relay.useTls"),
        ("PASEO_RELAY_PUBLIC_USE_TLS", "daemon.relay.publicUseTls"),
        ("PASEO_DICTATION_ENABLED", "features.dictation.enabled"),
        (
            "PASEO_DICTATION_STT_PROVIDER",
            "features.dictation.stt.provider",
        ),
        (
            "PASEO_DICTATION_LANGUAGE",
            "features.dictation.stt.language",
        ),
        ("PASEO_VOICE_MODE_ENABLED", "features.voiceMode.enabled"),
        (
            "PASEO_VOICE_LLM_PROVIDER",
            "features.voiceMode.llm.provider",
        ),
        (
            "PASEO_VOICE_STT_PROVIDER",
            "features.voiceMode.stt.provider",
        ),
        ("PASEO_VOICE_LANGUAGE", "features.voiceMode.stt.language"),
        (
            "PASEO_VOICE_TURN_DETECTION_PROVIDER",
            "features.voiceMode.turnDetection.provider",
        ),
        (
            "PASEO_VOICE_TTS_PROVIDER",
            "features.voiceMode.tts.provider",
        ),
        (
            "PASEO_VOICE_LOCAL_TTS_SPEAKER_ID",
            "features.voiceMode.tts.speakerId",
        ),
        (
            "PASEO_VOICE_LOCAL_TTS_SPEED",
            "features.voiceMode.tts.speed",
        ),
        ("PASEO_LOCAL_MODELS_DIR", "providers.local.modelsDir"),
    ] {
        add(env.contains_key(key), path);
    }
    add(trusted_proxies(env).is_some(), "daemon.trustedProxies");
    add(
        positive(ev(env, "PASEO_GIT_MAX_PROCESSES_PER_SECOND")).is_some(),
        "daemon.git.maxProcessesPerSecond",
    );
    // Source ownership chooses the legacy variable only when the new variable is absent.
    add(
        positive(
            ev(env, "PASEO_GIT_MAX_PROCESS_CONCURRENCY")
                .or_else(|| ev(env, "PASEO_GIT_CONCURRENCY")),
        )
        .is_some(),
        "daemon.git.maxProcessConcurrency",
    );
    add(
        nonempty(ev(env, "PASEO_PASSWORD")).is_some(),
        "daemon.auth.password",
    );
    add(
        boolean(ev(env, "PASEO_RELAY_ENABLED")).is_some(),
        "daemon.relay.enabled",
    );
    let proxy = boolean(ev(env, "PASEO_SERVICE_PROXY_ENABLED"));
    add(proxy.is_some(), "daemon.serviceProxy.enabled");
    add(
        env.contains_key("PASEO_SERVICE_PROXY_LISTEN") || proxy == Some(false),
        "daemon.serviceProxy.listen",
    );
    add(
        env.contains_key("PASEO_SERVICE_PROXY_PUBLIC_BASE_URL") || proxy == Some(false),
        "daemon.serviceProxy.publicBaseUrl",
    );
    for (key, name, path) in [
        (
            "PASEO_DICTATION_LOCAL_STT_MODEL",
            "dictationStt",
            "features.dictation.stt.model",
        ),
        (
            "PASEO_VOICE_LOCAL_STT_MODEL",
            "voiceStt",
            "features.voiceMode.stt.model",
        ),
        (
            "PASEO_VOICE_LOCAL_TTS_MODEL",
            "voiceTts",
            "features.voiceMode.tts.model",
        ),
    ] {
        add(
            env.contains_key(key) && active(providers, name, "local"),
            path,
        );
    }
    let dictation = active(providers, "dictationStt", "openai");
    let voice = active(providers, "voiceStt", "openai");
    add(
        env.contains_key("STT_CONFIDENCE_THRESHOLD") && (dictation || voice),
        "features.dictation.stt.confidenceThreshold",
    );
    add(
        env.contains_key("STT_MODEL") && dictation,
        "features.dictation.stt.model",
    );
    add(
        env.contains_key("STT_MODEL") && voice,
        "features.voiceMode.stt.model",
    );
    add(
        env.contains_key("TTS_MODEL") && active(providers, "voiceTts", "openai"),
        "features.voiceMode.tts.model",
    );
    add(
        env.contains_key("TTS_VOICE") && active(providers, "voiceTts", "openai"),
        "features.voiceMode.tts.voice",
    );
    add(
        env.contains_key("PASEO_DICTATION_LANGUAGE") && !env.contains_key("PASEO_VOICE_LANGUAGE"),
        "features.voiceMode.stt.language",
    );
    for (path, _) in log_overrides(env) {
        add(true, &format!("log.{}", path.join(".")));
    }
    paths.into_iter().collect()
}

pub fn mutable(resolved: &Value) -> Value {
    let mut value = json!({
        "relay": {"enabled":defaulted(resolved,"/relayEnabled",json!(true)),"requireProtectedSecurity":defaulted(resolved,"/relayRequireProtectedSecurity",json!(false))},
        "mcp": {"enabled":defaulted(resolved,"/mcpEnabled",json!(true)),"injectIntoAgents":defaulted(resolved,"/mcpInjectIntoAgents",json!(true))},
        "cors": {},
        "trustedProxies":defaulted(resolved,"/trustedProxies",json!(["loopback"])),
        "git":defaulted(resolved,"/git",json!({"maxProcessesPerSecond":64,"maxProcessConcurrency":8})),
        "app":{"baseUrl":defaulted(resolved,"/appBaseUrl",json!(APP_BASE_URL))},
        "providers":defaulted(resolved,"/providerOverrides",json!({})),
        "metadataGeneration":{"providers":defaulted(resolved,"/metadataGeneration/providers",json!([]))},
        "autoArchiveAfterMerge":defaulted(resolved,"/autoArchiveAfterMerge",json!(false)),
        "enableTerminalAgentHooks":defaulted(resolved,"/enableTerminalAgentHooks",json!(false)),
        "appendSystemPrompt":defaulted(resolved,"/appendSystemPrompt",json!("")),
        "pluginsEnabled":defaulted(resolved,"/pluginsEnabled",json!(false)),
        "plugins":defaulted(resolved,"/plugins",json!({})),
        "skills":{},
    });
    copy_optional(
        &mut value["cors"],
        "allowedOrigins",
        resolved,
        "/corsAllowedOrigins",
    );
    copy_optional(
        &mut value["skills"],
        "selection",
        resolved,
        "/skillSelection",
    );
    for (key, path) in [
        ("hostnames", "/hostnames"),
        (
            "catalogRefreshTimeoutMs",
            "/providerCatalogRefreshTimeoutMs",
        ),
        ("terminalProfiles", "/terminalProfiles"),
        ("agentProfiles", "/agentProfiles"),
    ] {
        copy_optional(&mut value, key, resolved, path);
    }
    value
}

/// Apply the mutable service view without re-resolving startup-only listeners,
/// credentials, speech endpoints, environment, or plugin registry selection.
pub fn apply_mutable(resolved: &Value, mutable: &Value) -> Value {
    let mut result = resolved.clone();
    for (key, path) in [
        ("relayEnabled", "/relay/enabled"),
        (
            "relayRequireProtectedSecurity",
            "/relay/requireProtectedSecurity",
        ),
        ("mcpEnabled", "/mcp/enabled"),
        ("mcpInjectIntoAgents", "/mcp/injectIntoAgents"),
        ("hostnames", "/hostnames"),
        ("corsAllowedOrigins", "/cors/allowedOrigins"),
        ("trustedProxies", "/trustedProxies"),
        ("git", "/git"),
        ("appBaseUrl", "/app/baseUrl"),
        (
            "providerCatalogRefreshTimeoutMs",
            "/catalogRefreshTimeoutMs",
        ),
        ("providerOverrides", "/providers"),
        ("metadataGeneration", "/metadataGeneration"),
        ("autoArchiveAfterMerge", "/autoArchiveAfterMerge"),
        ("enableTerminalAgentHooks", "/enableTerminalAgentHooks"),
        ("appendSystemPrompt", "/appendSystemPrompt"),
        ("pluginsEnabled", "/pluginsEnabled"),
        ("plugins", "/plugins"),
        ("skillSelection", "/skills/selection"),
        ("terminalProfiles", "/terminalProfiles"),
        ("agentProfiles", "/agentProfiles"),
    ] {
        if let Some(value) = mutable.pointer(path) {
            result[key] = value.clone();
        } else if let Some(object) = result.as_object_mut() {
            object.remove(key);
        }
    }
    result
        .as_object_mut()
        .unwrap()
        .remove("agentProviderSettings");
    insert_optional(
        &mut result,
        "agentProviderSettings",
        provider_settings(&mutable["providers"]),
    );
    result
}
