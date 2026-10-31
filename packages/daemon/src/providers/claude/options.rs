use super::catalog;
use crate::{
    process::CommandSpec,
    providers::api::{LaunchContext, ProviderSpec},
};
use anyhow::{Result, anyhow, bail};
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

/// Runtime-only data, including MCP bearer credentials. Never persist this structure.
/// Protocol fields/flags follow SDK 0.3.246 sdk.mjs:99,101,161 (REA artifact
/// ev_8966db2d3ba46ebf4069db894b5eaf9a2076421d3cd53b1ded3876e5642f99ec;
/// partial static graph, exact transport clauses inspected directly).
pub struct LaunchOptions {
    pub command: CommandSpec,
    pub initialize: Value,
    /// Apply once initialization completes and whenever the feature changes.
    pub fast_mode: Option<bool>,
}

pub fn provider_env(
    spec: &ProviderSpec,
    overlay: Option<&BTreeMap<String, String>>,
) -> BTreeMap<String, String> {
    let mut env: BTreeMap<_, _> = std::env::vars().collect();
    env.extend(spec.env.clone());
    if let Some(overlay) = overlay {
        env.extend(overlay.clone());
    }
    for key in [
        "CLAUDECODE",
        "CLAUDE_CODE_ENTRYPOINT",
        "CLAUDE_CODE_SSE_PORT",
        "CLAUDE_AGENT_SDK_VERSION",
    ] {
        env.remove(key);
    }
    env
}
pub fn validate_mode(mode: &str, env: &BTreeMap<String, String>) -> Result<()> {
    if !matches!(
        mode,
        "plan" | "default" | "acceptEdits" | "auto" | "bypassPermissions"
    ) {
        bail!(
            "Invalid mode '{mode}' for Claude provider. Valid modes: plan, default, acceptEdits, auto, bypassPermissions"
        );
    }
    if mode == "auto" {
        if let Some(transport) = catalog::auto_mode_transport(env) {
            bail!(
                "Claude Auto mode requires the Anthropic API and is not supported when Claude Code uses {transport}. Select another permission mode or unset the {} environment variable.",
                if transport == "Bedrock" {
                    "CLAUDE_CODE_USE_BEDROCK"
                } else {
                    "CLAUDE_CODE_USE_VERTEX"
                }
            );
        }
    }
    Ok(())
}
fn strings(value: &Value) -> bool {
    value
        .as_array()
        .is_some_and(|values| values.iter().all(Value::is_string))
}
fn strict_fields(value: &Value, path: &str, fields: &[(&str, fn(&Value) -> bool)]) -> Result<()> {
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("{path} must be an object"))?;
    for (key, value) in object {
        let Some((_, validate)) = fields.iter().find(|field| key.as_str() == field.0) else {
            bail!("Unknown Claude option: {path}.{key}");
        };
        if !validate(value) {
            bail!("Invalid Claude option: {path}.{key}");
        }
    }
    Ok(())
}
fn network(value: &Value) -> bool {
    strict_fields(
        value,
        "sandbox.network",
        &[
            ("allowedDomains", strings),
            ("deniedDomains", strings),
            ("strictAllowlist", Value::is_boolean),
            ("allowManagedDomainsOnly", Value::is_boolean),
            ("allowUnixSockets", strings),
            ("allowAllUnixSockets", Value::is_boolean),
            ("allowLocalBinding", Value::is_boolean),
            ("allowMachLookup", strings),
            ("httpProxyPort", positive_integer),
            ("socksProxyPort", positive_integer),
            ("tlsTerminate", tls),
        ],
    )
    .is_ok()
}
fn positive_integer(v: &Value) -> bool {
    v.as_u64().is_some_and(|n| n > 0)
}
fn tls(v: &Value) -> bool {
    strict_fields(
        v,
        "tlsTerminate",
        &[
            ("caCertPath", Value::is_string),
            ("caKeyPath", Value::is_string),
        ],
    )
    .is_ok()
}
fn filesystem(value: &Value) -> bool {
    strict_fields(
        value,
        "sandbox.filesystem",
        &[
            ("allowWrite", strings),
            ("denyWrite", strings),
            ("denyRead", strings),
            ("allowRead", strings),
            ("allowManagedReadPathsOnly", Value::is_boolean),
            ("disabled", Value::is_boolean),
        ],
    )
    .is_ok()
}
fn permission_rules(value: &Value) -> bool {
    strict_fields(
        value,
        "settings.permissions",
        &[("allow", strings), ("ask", strings), ("deny", strings)],
    )
    .is_ok()
}
fn ripgrep(value: &Value) -> bool {
    value.get("command").is_some_and(Value::is_string)
        && strict_fields(
            value,
            "sandbox.ripgrep",
            &[("command", Value::is_string), ("args", strings)],
        )
        .is_ok()
}
fn ignore_violations(value: &Value) -> bool {
    value
        .as_object()
        .is_some_and(|object| object.values().all(strings))
}
fn sandbox(value: &Value) -> bool {
    validate_sandbox(value, true).is_ok()
}
fn settings_sandbox(value: &Value) -> bool {
    validate_sandbox(value, false).is_ok()
}
fn validate_sandbox(value: &Value, extended: bool) -> Result<()> {
    let fields: &[(&str, fn(&Value) -> bool)] = &[
        ("enabled", Value::is_boolean),
        ("failIfUnavailable", Value::is_boolean),
        ("autoAllowBashIfSandboxed", Value::is_boolean),
        ("excludedCommands", strings),
        ("allowUnsandboxedCommands", Value::is_boolean),
        ("network", network),
        ("filesystem", filesystem),
        ("ignoreViolations", ignore_violations),
        ("enableWeakerNestedSandbox", Value::is_boolean),
        ("ripgrep", ripgrep),
    ];
    strict_fields(
        value,
        "sandbox",
        if extended { fields } else { &fields[..7] },
    )
}
fn settings(value: &Value) -> bool {
    strict_fields(
        value,
        "settings",
        &[
            ("permissions", permission_rules),
            ("sandbox", settings_sandbox),
        ],
    )
    .is_ok()
}
fn extra_args(value: &Value) -> bool {
    value
        .as_object()
        .is_some_and(|object| object.values().all(|v| v.is_string() || v.is_null()))
}
pub fn validate_provider_options(value: &Value) -> Result<()> {
    strict_fields(
        value,
        "providerOptions",
        &[
            ("allowedTools", strings),
            ("disallowedTools", strings),
            ("additionalDirectories", strings),
            ("extraArgs", extra_args),
            ("sandbox", sandbox),
            ("settings", settings),
        ],
    )
}
/// Preserve explicit allow rules, adding only the exact preapproved MCP tools.
pub fn apply_tool_policy(options: &mut Value, tool_policy: &Value) -> Result<()> {
    let Some(grants) = tool_policy.get("preapproved") else {
        return Ok(());
    };
    let grants = grants
        .as_array()
        .ok_or_else(|| anyhow!("toolPolicy.preapproved must be an array"))?;
    let object = options
        .as_object_mut()
        .ok_or_else(|| anyhow!("Claude providerOptions must be an object"))?;
    let allowed = object
        .entry("allowedTools")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or_else(|| anyhow!("allowedTools must be an array"))?;
    let mut seen = BTreeSet::new();
    allowed.retain(|value| {
        value
            .as_str()
            .is_some_and(|name| seen.insert(name.to_owned()))
    });
    for grant in grants {
        let server = grant["server"]
            .as_str()
            .ok_or_else(|| anyhow!("MCP grant is missing server"))?;
        let tool = grant["tool"]
            .as_str()
            .ok_or_else(|| anyhow!("MCP grant is missing tool"))?;
        let name = format!("mcp__{server}__{tool}");
        if seen.insert(name.clone()) {
            allowed.push(name.into());
        }
    }
    Ok(())
}
fn add_arg(args: &mut Vec<String>, name: &str, value: &str) {
    // SDK BT(): bind dash-leading values so they cannot become separate flags.
    if value.len() > 1 && value.starts_with('-') {
        args.push(format!("--{name}={value}"));
    } else {
        args.push(format!("--{name}"));
        args.push(value.to_owned());
    }
}
fn native_command(
    spec: &ProviderSpec,
    cwd: PathBuf,
    mut env: BTreeMap<String, String>,
) -> Result<CommandSpec> {
    let (program, prefix) = spec
        .command
        .split_first()
        .ok_or_else(|| anyhow!("Claude executable prefix is empty"))?;
    let mut args = prefix.to_vec();
    args.extend(
        [
            "--output-format",
            "stream-json",
            "--verbose",
            "--input-format",
            "stream-json",
        ]
        .map(str::to_owned),
    );
    env.insert("CLAUDE_CODE_ENTRYPOINT".into(), "sdk-ts".into());
    env.insert("CLAUDE_AGENT_SDK_VERSION".into(), "0.3.246".into());
    env.remove("NODE_OPTIONS");
    let debug = env.get("DEBUG_CLAUDE_AGENT_SDK").is_some_and(|value| {
        matches!(
            value.to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    });
    if debug {
        env.insert("DEBUG".into(), "1".into());
    } else {
        env.remove("DEBUG");
    }
    Ok(CommandSpec {
        program: program.clone(),
        args,
        cwd: Some(cwd),
        env,
        clear_env: true,
    })
}
pub fn normalize_mcp_servers(servers: &Value) -> Result<Value> {
    if servers.is_null() {
        return Ok(json!({}));
    }
    let servers = servers
        .as_object()
        .ok_or_else(|| anyhow!("Runtime MCP servers must be an object"))?;
    let mut result = Map::new();
    for (name, server) in servers {
        let fields: &[&str] = match server["type"].as_str() {
            Some("stdio") => &["type", "command", "args", "env"],
            Some("http" | "sse") => &["type", "url", "headers"],
            _ => bail!("Unhandled MCP server config type for '{name}'"),
        };
        let mut normalized = Map::new();
        for field in fields {
            if let Some(value) = server.get(*field) {
                normalized.insert((*field).to_owned(), value.clone());
            }
        }
        if server["alwaysLoad"] == true {
            normalized.insert("alwaysLoad".into(), true.into());
        }
        result.insert(name.clone(), Value::Object(normalized));
    }
    Ok(Value::Object(result))
}
pub fn observation_hooks() -> Value {
    json!({"PreToolUse":[{"hookCallbackIds":["hook_0"]}],"PostToolUse":[{"hookCallbackIds":["hook_1"]}],"SubagentStop":[{"hookCallbackIds":["hook_2"]}]})
}
/// Runtime must feed `input` to its subagent observer before acknowledging. These
/// hooks deliberately never modify execution or effort; they observe actual effort.
pub fn hook_response(callback_id: &str, _input: &Value, _config: &Value) -> Result<Value> {
    if !matches!(callback_id, "hook_0" | "hook_1" | "hook_2") {
        bail!("Unknown Claude hook callback: {callback_id}");
    }
    Ok(json!({}))
}

pub fn build_launch(
    spec: &ProviderSpec,
    context: &LaunchContext,
    config: &Value,
    resume: Option<&str>,
    fresh: Option<&str>,
    persist: Option<bool>,
) -> Result<LaunchOptions> {
    let mut env = provider_env(spec, Some(&context.env));
    env.insert("CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS".into(), "1".into());
    env.insert(
        "CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING".into(),
        "true".into(),
    );
    let mode = config["modeId"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or("default");
    validate_mode(mode, &env)?;
    let model = config["model"].as_str().filter(|s| !s.is_empty());
    let thinking = config["thinkingOptionId"].as_str();
    catalog::validate_thinking(model, thinking)?;
    let cwd = config["cwd"]
        .as_str()
        .ok_or_else(|| anyhow!("Claude session requires a working directory"))?;
    let mut command = native_command(spec, PathBuf::from(cwd), env)?;
    let mut provider_options = if spec.options.is_null() {
        json!({})
    } else {
        spec.options.clone()
    };
    validate_provider_options(&provider_options)?;
    if let Some(options) = config.get("providerOptions") {
        validate_provider_options(options)?;
        provider_options
            .as_object_mut()
            .unwrap()
            .extend(options.as_object().unwrap().clone());
    }
    apply_tool_policy(&mut provider_options, &config["toolPolicy"])?;
    if let Some(denied) = spec.runtime_settings.get("disallowedTools") {
        if !strings(denied) {
            bail!("Claude runtime_settings.disallowedTools must be an array of strings");
        }
        provider_options
            .as_object_mut()
            .unwrap()
            .entry("disallowedTools")
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .unwrap()
            .extend(denied.as_array().unwrap().iter().cloned());
    }
    if let Some(model) = model {
        add_arg(&mut command.args, "model", model);
    }
    match thinking {
        Some("off") => add_arg(&mut command.args, "thinking", "disabled"),
        Some(effort @ ("low" | "medium" | "high" | "xhigh" | "max" | "ultracode")) => {
            add_arg(&mut command.args, "thinking", "adaptive");
            add_arg(&mut command.args, "thinking-display", "summarized");
            add_arg(
                &mut command.args,
                "effort",
                if effort == "ultracode" {
                    "xhigh"
                } else {
                    effort
                },
            );
        }
        _ => {}
    }
    add_arg(&mut command.args, "permission-prompt-tool", "stdio");
    if let Some(fresh) = fresh {
        command.args.push(format!("--session-id={fresh}"));
    } else if let Some(resume) = resume {
        command.args.push(format!("--resume={resume}"));
    }
    for (key, flag) in [
        ("allowedTools", "allowedTools"),
        ("disallowedTools", "disallowedTools"),
    ] {
        if let Some(values) = provider_options[key].as_array().filter(|a| !a.is_empty()) {
            let value = values
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(",");
            add_arg(&mut command.args, flag, &value);
        }
    }
    let servers = normalize_mcp_servers(&context.mcp_servers)?;
    if servers.as_object().is_some_and(|s| !s.is_empty()) {
        add_arg(
            &mut command.args,
            "mcp-config",
            &serde_json::to_string(&json!({"mcpServers":servers}))?,
        );
    }
    command
        .args
        .push("--setting-sources=user,project,local".into());
    add_arg(&mut command.args, "permission-mode", mode);
    command.args.extend(
        [
            "--allow-dangerously-skip-permissions",
            "--include-partial-messages",
        ]
        .map(str::to_owned),
    );
    for directory in provider_options["additionalDirectories"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        add_arg(&mut command.args, "add-dir", directory);
    }
    if persist == Some(false) {
        command.args.push("--no-session-persistence".into());
    }
    let fast_mode =
        catalog::supports_fast(model).then_some(config["featureValues"]["fast_mode"] == true);
    let mut settings = provider_options
        .get("settings")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if let Some(fast) = fast_mode {
        settings["fastMode"] = fast.into();
    }
    if thinking == Some("ultracode") {
        settings["ultracode"] = true.into();
    }
    if config["internal"] == true {
        settings["disableAllHooks"] = true.into();
    }
    let mut extra = provider_options["extraArgs"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    if !settings.as_object().unwrap().is_empty() || provider_options.get("settings").is_some() {
        extra.insert("settings".into(), serde_json::to_string(&settings)?.into());
    }
    if let Some(sandbox) = provider_options.get("sandbox") {
        let mut sandbox = sandbox.clone();
        if sandbox["enabled"] == true && sandbox.get("failIfUnavailable").is_none() {
            sandbox["failIfUnavailable"] = true.into();
        }
        let mut merged = json!({});
        if let Some(raw) = extra
            .get("settings")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            let trimmed = raw.trim();
            if !trimmed.starts_with('{') || !trimmed.ends_with('}') {
                bail!(
                    "Cannot use both a settings file path and the sandbox option. Include the sandbox configuration in your settings file instead."
                );
            }
            if let Ok(value) = serde_json::from_str::<Value>(raw) {
                merged = value;
            }
        }
        merged["sandbox"] = sandbox;
        extra.insert("settings".into(), serde_json::to_string(&merged)?.into());
    }
    for (key, value) in extra {
        if value.is_null() {
            command.args.push(format!("--{key}"));
        } else {
            add_arg(&mut command.args, &key, value.as_str().unwrap());
        }
    }
    let prompt = [
        config["systemPrompt"].as_str(),
        config["daemonAppendSystemPrompt"].as_str(),
    ]
    .into_iter()
    .flatten()
    .map(str::trim)
    .filter(|s| !s.is_empty())
    .collect::<Vec<_>>()
    .join("\n\n");
    let mut initialize = json!({"subtype":"initialize","appendSystemPrompt":prompt,"forwardSubagentText":true,"perTaskStopAffordance":true,"hooks":observation_hooks()});
    if let Some(agents) = spec.defaults.get("agents") {
        if !agents.is_object() {
            bail!("Claude defaults.agents must be an object");
        }
        initialize["agents"] = agents.clone();
    }
    Ok(LaunchOptions {
        command,
        initialize,
        fast_mode,
    })
}
/// Caller owns the no-prompt query, five-second deadline and child cleanup. A
/// failed discovery leaves the curated catalog available, omitting gated access.
pub fn discovery_launch(spec: &ProviderSpec, home: &Path) -> Result<LaunchOptions> {
    let mut command = native_command(spec, home.to_path_buf(), provider_env(spec, None))?;
    command.args.extend(
        [
            "--no-session-persistence",
            "--tools",
            "",
            "--strict-mcp-config",
            "--setting-sources=user",
            "--settings",
            "{\"disableAllHooks\":true}",
        ]
        .map(str::to_owned),
    );
    Ok(LaunchOptions {
        command,
        initialize: json!({"subtype":"initialize","systemPrompt":[""]}),
        fast_mode: None,
    })
}
