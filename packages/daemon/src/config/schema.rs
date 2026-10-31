use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};

/// The on-disk defaults, rather than the resolved runtime defaults.
pub fn defaults() -> Value {
    json!({
        "version": 1,
        "daemon": {
            "listen": "127.0.0.1:6767",
            "cors": { "allowedOrigins": ["https://app.paseo.sh"] },
            "relay": { "enabled": false }
        },
        "app": { "baseUrl": "https://app.paseo.sh" }
    })
}

pub fn normalize_persisted(mut value: Value) -> Result<Value> {
    remove_at(&mut value, &["daemon"], "browserTools");
    remove_at(&mut value, &["features"], "webUi");
    remove_at(&mut value, &["providers", "local"], "autoDownload");
    remove_at(&mut value, &["providers", "openai"], "voice");
    if let Some(providers) = value.pointer_mut("/agents/providers") {
        migrate_providers(providers)?;
    }
    visit(Node::Write(&mut value), Kind::Persisted).context("Invalid persisted config")?;
    if let Some(daemon) = value.get_mut("daemon").and_then(Value::as_object_mut) {
        if let Some(allowed_hosts) = daemon.remove("allowedHosts") {
            daemon.entry("hostnames").or_insert(allowed_hosts);
        }
    }
    Ok(value)
}

pub fn normalize_patch(mut value: Value) -> Result<Value> {
    remove_at(&mut value, &[], "browserTools");
    visit(Node::Write(&mut value), Kind::Patch).context("Invalid daemon config patch")?;
    // Internal setters share this normalization path but use the full mutable
    // config's strict skills schema; the public patch schema leaves it unknown.
    if let Some(skills) = value.get_mut("skills") {
        visit(Node::Write(skills), Kind::Skills).context("skills")?;
    }
    Ok(value)
}

/// ProviderOverrideSchema alone: map-level provider-ID/extends checks belong to
/// normalize_persisted. Unknown override/model/policy properties are stripped.
pub fn normalize_provider(mut value: Value) -> Result<Value> {
    visit(Node::Write(&mut value), Kind::Provider)?;
    Ok(value)
}

pub fn validate_persisted_path(field: &str) -> Result<()> {
    let mut kind = Kind::Persisted;
    for part in field.split('.') {
        if part.is_empty() || matches!(part, "__proto__" | "prototype" | "constructor") {
            bail!("Invalid configuration path: {field}");
        }
        // A discriminated union is not an object-shaped editing boundary even
        // when each of its alternatives is an object.
        let fields = if matches!(kind, Kind::SkillSelection) {
            None
        } else {
            shape(kind, &Value::Null).map(|(_, fields)| fields)
        };
        kind = fields.and_then(|fields| fields.iter().find(|(name, _, _)| *name == part))
            .map(|(_, kind, _)| *kind)
            .with_context(|| format!("Unknown configuration path: {field}. Replace its containing object with JSON for dynamic keys."))?;
    }
    Ok(())
}

fn remove_at(value: &mut Value, path: &[&str], key: &str) {
    let mut cursor = value;
    for part in path {
        let Some(child) = cursor.get_mut(*part) else {
            return;
        };
        cursor = child;
    }
    if let Some(object) = cursor.as_object_mut() {
        object.remove(key);
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Any,
    Text,
    Nonempty,
    Trimmed,
    Boolean,
    Number,
    Integer,
    Positive,
    Timeout,
    Version,
    Url,
    Bcrypt,
    SpeechProvider,
    Hosts,
    PortRange,
    Enum(&'static [&'static str]),
    Array(&'static Kind),
    NonemptyArray(&'static Kind),
    Record(&'static Kind),
    Persisted,
    Daemon,
    Mcp,
    Git,
    Cors,
    Relay,
    ServiceProxy,
    Auth,
    App,
    SpeechProviders,
    OpenAi,
    SpeechEndpoint,
    Local,
    Worktrees,
    ServicePorts,
    Agents,
    ProviderMap,
    Provider,
    Model,
    ThinkingOption,
    ToolsPolicy,
    Metadata,
    GenerationProvider,
    Skills,
    SkillSelection,
    Features,
    Dictation,
    DictationStt,
    Voice,
    Llm,
    Stt,
    TurnDetection,
    Tts,
    Log,
    ConsoleLog,
    FileLog,
    Rotate,
    AgentProfile,
    TerminalProfile,
    Plugins,
    Plugin,
    Registry,
    Patch,
    PatchRelay,
    PatchMcp,
    PatchProvider,
    PatchModel,
    PatchMetadata,
    PatchGenerationProvider,
    LegacyProvider,
    LegacyCommand,
}

#[derive(Clone, Copy)]
enum Unknown {
    Strict,
    Strip,
    Keep,
}
type Field = (&'static str, Kind, bool);

// The boolean marks required fields. Missing and explicit null never alias.
fn shape(kind: Kind, value: &Value) -> Option<(Unknown, &'static [Field])> {
    use Kind::*;
    use Unknown::*;
    Some(match kind {
        Persisted => (
            Strict,
            &[
                ("$schema", Text, false),
                ("version", Version, false),
                ("daemon", Daemon, false),
                ("app", App, false),
                ("providers", SpeechProviders, false),
                ("pluginRegistries", Record(&Registry), false),
                ("pluginsEnabled", Boolean, false),
                ("plugins", Plugins, false),
                ("worktrees", Worktrees, false),
                ("agents", Agents, false),
                ("features", Features, false),
                ("log", Log, false),
            ],
        ),
        Daemon => (
            Strict,
            &[
                ("listen", Text, false),
                ("hostnames", Hosts, false),
                ("allowedHosts", Hosts, false),
                ("trustedProxies", Hosts, false),
                ("mcp", Mcp, false),
                ("git", Git, false),
                ("autoArchiveAfterMerge", Boolean, false),
                ("enableTerminalAgentHooks", Boolean, false),
                ("appendSystemPrompt", Text, false),
                ("terminalProfiles", Array(&TerminalProfile), false),
                ("agentProfiles", Array(&AgentProfile), false),
                ("cors", Cors, false),
                ("relay", Relay, false),
                ("serviceProxy", ServiceProxy, false),
                ("auth", Auth, false),
            ],
        ),
        Mcp => (
            Keep,
            &[
                ("enabled", Boolean, false),
                ("injectIntoAgents", Boolean, false),
            ],
        ),
        Git => (
            Strict,
            &[
                ("maxProcessesPerSecond", Positive, false),
                ("maxProcessConcurrency", Positive, false),
            ],
        ),
        Cors => (Strict, &[("allowedOrigins", Array(&Text), false)]),
        Relay => (
            Strict,
            &[
                ("enabled", Boolean, false),
                ("requireProtectedSecurity", Boolean, false),
                ("endpoint", Text, false),
                ("publicEndpoint", Text, false),
                ("useTls", Boolean, false),
                ("publicUseTls", Boolean, false),
            ],
        ),
        ServiceProxy => (
            Strict,
            &[
                ("enabled", Boolean, false),
                ("listen", Text, false),
                ("publicBaseUrl", Url, false),
            ],
        ),
        Auth => (Strict, &[("password", Bcrypt, false)]),
        App => (Strict, &[("baseUrl", Text, false)]),
        SpeechProviders => (
            Strict,
            &[("openai", OpenAi, false), ("local", Local, false)],
        ),
        OpenAi => (
            Strict,
            &[
                ("apiKey", Nonempty, false),
                ("baseUrl", Trimmed, false),
                ("stt", SpeechEndpoint, false),
                ("tts", SpeechEndpoint, false),
            ],
        ),
        SpeechEndpoint => (
            Strict,
            &[("apiKey", Trimmed, false), ("baseUrl", Trimmed, false)],
        ),
        Local => (Strict, &[("modelsDir", Nonempty, false)]),
        Worktrees => (
            Strict,
            &[
                ("root", Nonempty, false),
                ("servicePorts", ServicePorts, false),
            ],
        ),
        ServicePorts => (
            Strict,
            &[("range", PortRange, false), ("portScript", Trimmed, false)],
        ),
        Agents => (
            Strict,
            &[
                ("providers", ProviderMap, false),
                ("catalogRefreshTimeoutMs", Timeout, false),
                ("metadataGeneration", Metadata, false),
                ("skills", Skills, false),
            ],
        ),
        Provider => (
            Strip,
            &[
                ("extends", Text, false),
                ("label", Text, false),
                ("description", Text, false),
                ("command", NonemptyArray(&Nonempty), false),
                ("env", Record(&Text), false),
                ("options", Record(&Any), false),
                ("params", Record(&Any), false),
                ("models", Array(&Model), false),
                ("additionalModels", Array(&Model), false),
                ("disallowedTools", Array(&Text), false),
                ("paseoTools", ToolsPolicy, false),
                ("enabled", Boolean, false),
                ("order", Number, false),
            ],
        ),
        Model => (
            Strip,
            &[
                ("id", Nonempty, true),
                ("label", Nonempty, true),
                ("description", Text, false),
                ("isDefault", Boolean, false),
                ("thinkingOptions", Array(&ThinkingOption), false),
            ],
        ),
        ThinkingOption => (
            Strip,
            &[
                ("id", Text, true),
                ("label", Text, true),
                ("description", Text, false),
                ("isDefault", Boolean, false),
            ],
        ),
        ToolsPolicy => (
            Strip,
            &[
                ("enabled", Boolean, false),
                ("disabledTools", Array(&Text), false),
            ],
        ),
        Metadata => (Strict, &[("providers", Array(&GenerationProvider), false)]),
        GenerationProvider => (
            Strict,
            &[
                ("provider", Nonempty, true),
                ("model", Nonempty, false),
                ("thinkingOptionId", Nonempty, false),
            ],
        ),
        Skills => (Strict, &[("selection", SkillSelection, false)]),
        SkillSelection if value.get("mode").and_then(Value::as_str) == Some("custom") => (
            Strict,
            &[
                ("mode", Enum(&["custom"]), true),
                ("skills", Array(&Text), true),
            ],
        ),
        SkillSelection => (Strict, &[("mode", Enum(&["all"]), true)]),
        Features => (
            Strict,
            &[("dictation", Dictation, false), ("voiceMode", Voice, false)],
        ),
        Dictation => (
            Strict,
            &[("enabled", Boolean, false), ("stt", DictationStt, false)],
        ),
        DictationStt => (
            Strict,
            &[
                ("provider", SpeechProvider, false),
                ("model", Nonempty, false),
                ("language", Trimmed, false),
                ("confidenceThreshold", Number, false),
            ],
        ),
        Voice => (
            Strict,
            &[
                ("enabled", Boolean, false),
                ("llm", Llm, false),
                ("stt", Stt, false),
                ("turnDetection", TurnDetection, false),
                ("tts", Tts, false),
            ],
        ),
        Llm => (
            Strict,
            &[("provider", Text, false), ("model", Nonempty, false)],
        ),
        Stt => (
            Strict,
            &[
                ("provider", SpeechProvider, false),
                ("model", Nonempty, false),
                ("language", Trimmed, false),
            ],
        ),
        TurnDetection => (Strict, &[("provider", SpeechProvider, false)]),
        Tts => (
            Strict,
            &[
                ("provider", SpeechProvider, false),
                ("model", Nonempty, false),
                ("voice", Trimmed, false),
                ("speakerId", Integer, false),
                ("speed", Number, false),
            ],
        ),
        Log => (
            Strict,
            &[
                (
                    "level",
                    Enum(&["trace", "debug", "info", "warn", "error", "fatal"]),
                    false,
                ),
                ("format", Enum(&["pretty", "json"]), false),
                ("console", ConsoleLog, false),
                ("file", FileLog, false),
            ],
        ),
        ConsoleLog => (
            Strict,
            &[
                (
                    "level",
                    Enum(&["trace", "debug", "info", "warn", "error", "fatal"]),
                    false,
                ),
                ("format", Enum(&["pretty", "json"]), false),
            ],
        ),
        FileLog => (
            Strict,
            &[
                (
                    "level",
                    Enum(&["trace", "debug", "info", "warn", "error", "fatal"]),
                    false,
                ),
                ("path", Nonempty, false),
                ("rotate", Rotate, false),
            ],
        ),
        Rotate => (
            Strict,
            &[("maxSize", Nonempty, false), ("maxFiles", Positive, false)],
        ),
        AgentProfile => (
            Keep,
            &[
                ("id", Text, true),
                ("name", Text, true),
                ("provider", Text, true),
                ("icon", Text, false),
                ("color", Text, false),
                ("cliDefault", Boolean, false),
                ("model", Text, false),
                ("modeId", Text, false),
                ("thinkingOptionId", Text, false),
                ("featureValues", Record(&Any), false),
                ("notes", Text, false),
            ],
        ),
        TerminalProfile => (
            Keep,
            &[
                ("id", Text, true),
                ("name", Text, true),
                ("command", Text, true),
                ("args", Array(&Text), false),
                ("icon", Text, false),
            ],
        ),
        Plugin => (
            Strict,
            &[
                ("source", Enum(&["directory"]), true),
                ("path", Nonempty, true),
                ("enabled", Boolean, false),
            ],
        ),
        Registry => (Strip, &[("authorization", Nonempty, true)]),
        Patch => (
            Keep,
            &[
                ("relay", PatchRelay, false),
                ("mcp", PatchMcp, false),
                ("providers", Record(&PatchProvider), false),
                ("removeProviders", Array(&Nonempty), false),
                ("metadataGeneration", PatchMetadata, false),
                ("autoArchiveAfterMerge", Boolean, false),
                ("enableTerminalAgentHooks", Boolean, false),
                ("appendSystemPrompt", Text, false),
                ("terminalProfiles", Array(&TerminalProfile), false),
                ("agentProfiles", Array(&AgentProfile), false),
                ("pluginsEnabled", Boolean, false),
                ("plugins", Plugins, false),
            ],
        ),
        PatchRelay => (
            Keep,
            &[
                ("enabled", Boolean, false),
                ("requireProtectedSecurity", Boolean, false),
            ],
        ),
        PatchMcp => (Keep, &[("injectIntoAgents", Boolean, false)]),
        PatchProvider => (
            Keep,
            &[
                ("paseoTools", ToolsPolicy, false),
                ("enabled", Boolean, false),
                ("additionalModels", Array(&PatchModel), false),
            ],
        ),
        PatchModel => (
            Keep,
            &[
                ("id", Nonempty, true),
                ("label", Nonempty, true),
                ("description", Text, false),
                ("isDefault", Boolean, false),
            ],
        ),
        PatchMetadata => (
            Keep,
            &[("providers", Array(&PatchGenerationProvider), false)],
        ),
        PatchGenerationProvider => (
            Keep,
            &[
                ("provider", Nonempty, true),
                ("model", Nonempty, false),
                ("thinkingOptionId", Nonempty, false),
            ],
        ),
        LegacyProvider => (
            Strip,
            &[
                ("command", LegacyCommand, false),
                ("env", Record(&Text), false),
                ("disallowedTools", Array(&Text), false),
            ],
        ),
        LegacyCommand => match value.get("mode").and_then(Value::as_str) {
            Some("append") => (
                Strip,
                &[
                    ("mode", Enum(&["append"]), true),
                    ("args", Array(&Text), false),
                ],
            ),
            Some("replace") => (
                Strip,
                &[
                    ("mode", Enum(&["replace"]), true),
                    ("argv", NonemptyArray(&Nonempty), true),
                ],
            ),
            _ => (Strip, &[("mode", Enum(&["default"]), true)]),
        },
        _ => return None,
    })
}

// A shared walker validates borrowed patches and normalizes owned JSON without
// cloning secrets or conflating absent, null, and false values.
enum Node<'a> {
    Read(&'a Value),
    Write(&'a mut Value),
}
impl Node<'_> {
    fn value(&self) -> &Value {
        match self {
            Self::Read(value) => value,
            Self::Write(value) => value,
        }
    }
}

fn visit(mut node: Node<'_>, kind: Kind) -> Result<()> {
    if let Some((unknown, fields)) = shape(kind, node.value()) {
        let object = node.value().as_object().context("Expected object")?;
        if matches!(unknown, Unknown::Strict) {
            for key in object.keys() {
                if !fields.iter().any(|(name, _, _)| key == name) {
                    bail!("Unrecognized key: {key}");
                }
            }
        }
        for (name, _, required) in fields {
            if *required && !object.contains_key(*name) {
                bail!("{name}: Required");
            }
        }
        match &mut node {
            Node::Read(value) => {
                for (name, child_kind, _) in fields {
                    if let Some(child) = value.get(*name) {
                        visit(Node::Read(child), *child_kind)
                            .with_context(|| (*name).to_owned())?;
                    }
                }
            }
            Node::Write(value) => {
                let object = value.as_object_mut().expect("checked object");
                if matches!(unknown, Unknown::Strip) {
                    object.retain(|key, _| fields.iter().any(|(name, _, _)| key == name));
                }
                for (name, child_kind, _) in fields {
                    if let Some(child) = object.get_mut(*name) {
                        visit(Node::Write(child), *child_kind)
                            .with_context(|| (*name).to_owned())?;
                    }
                }
                // Zod 4 applies defaults inside optional fields, including
                // MutableMetadataGenerationConfigSchema.partial().
                if matches!(kind, Kind::PatchMetadata) {
                    object
                        .entry("providers")
                        .or_insert_with(|| Value::Array(Vec::new()));
                }
            }
        }
        if matches!(kind, Kind::ServicePorts)
            && node.value().get("range").is_none()
            && node.value().get("portScript").is_none()
        {
            bail!("Expected range or portScript");
        }
        return Ok(());
    }
    use Kind::*;
    match kind {
        Array(child_kind) | NonemptyArray(child_kind) => {
            let array = node.value().as_array().context("Expected array")?;
            if matches!(kind, NonemptyArray(_)) && array.is_empty() {
                bail!("Array must contain at least 1 element(s)");
            }
            match node {
                Node::Read(value) => {
                    for (index, child) in
                        value.as_array().expect("checked array").iter().enumerate()
                    {
                        visit(Node::Read(child), *child_kind).with_context(|| index.to_string())?;
                    }
                }
                Node::Write(value) => {
                    for (index, child) in value
                        .as_array_mut()
                        .expect("checked array")
                        .iter_mut()
                        .enumerate()
                    {
                        visit(Node::Write(child), *child_kind)
                            .with_context(|| index.to_string())?;
                    }
                }
            }
        }
        Record(_) | ProviderMap | Plugins => {
            let child_kind = match kind {
                Record(child) => *child,
                ProviderMap => Provider,
                _ => Plugin,
            };
            let object = node.value().as_object().context("Expected object")?;
            if matches!(kind, ProviderMap | Plugins) {
                for key in object.keys() {
                    if !valid_id(key) {
                        if matches!(kind, ProviderMap) {
                            bail!("Provider ID \"{key}\" must match /^[a-z][a-z0-9-]*$/.");
                        }
                        bail!("Invalid plugin ID: {key}");
                    }
                }
            }
            match &mut node {
                Node::Read(value) => {
                    for (key, child) in value.as_object().expect("checked object") {
                        visit(Node::Read(child), child_kind).with_context(|| key.clone())?;
                    }
                }
                Node::Write(value) => {
                    for (key, child) in value.as_object_mut().expect("checked object") {
                        visit(Node::Write(child), child_kind).with_context(|| key.clone())?;
                    }
                }
            }
            if matches!(kind, ProviderMap) {
                refine_providers(node.value().as_object().expect("checked object"))?;
            }
        }
        _ => {
            validate_leaf(node.value(), kind)?;
            if matches!(kind, Trimmed | SpeechProvider | PortRange) {
                if let Node::Write(Value::String(text)) = node {
                    let trimmed = js_trim(text);
                    let lower = matches!(kind, SpeechProvider);
                    if lower {
                        *text = trimmed.to_lowercase();
                    } else if trimmed.len() != text.len() {
                        *text = trimmed.to_owned();
                    }
                }
            }
        }
    }
    Ok(())
}

fn validate_leaf(value: &Value, kind: Kind) -> Result<()> {
    use Kind::*;
    match kind {
        Any => {}
        Text | Nonempty | Trimmed | SpeechProvider | PortRange | Url | Bcrypt | Enum(_) => {
            let text = value.as_str().context("Expected string")?;
            match kind {
                Nonempty if text.is_empty() => bail!("String must contain at least 1 character(s)"),
                Trimmed if js_trim(text).is_empty() => {
                    bail!("String must contain at least 1 character(s)")
                }
                SpeechProvider
                    if !js_trim(text).eq_ignore_ascii_case("openai")
                        && !js_trim(text).eq_ignore_ascii_case("local") =>
                {
                    bail!("Expected openai or local")
                }
                Enum(values) if !values.contains(&text) => {
                    bail!("Expected one of {}", values.join(", "))
                }
                Url => {
                    url::Url::parse(text).context("Invalid URL")?;
                }
                Bcrypt if !valid_bcrypt(text) => bail!("Expected a bcrypt hash"),
                PortRange => validate_port_range(js_trim(text))?,
                _ => {}
            }
        }
        Boolean if !value.is_boolean() => bail!("Expected boolean"),
        Boolean => {}
        Number | Integer | Positive | Timeout => {
            let number = value
                .as_f64()
                .filter(|n| n.is_finite())
                .context("Expected number")?;
            if !matches!(kind, Number)
                && (number.fract() != 0.0 || number.abs() > 9_007_199_254_740_991.0)
            {
                bail!("Expected safe integer");
            }
            if matches!(kind, Positive | Timeout) && number <= 0.0 {
                bail!("Number must be greater than 0");
            }
            if matches!(kind, Timeout) && number > 2_147_483_647.0 {
                bail!("Number must be less than or equal to 2147483647");
            }
        }
        Version if value.as_f64() != Some(1.0) => bail!("Expected literal 1"),
        Version => {}
        Hosts => {
            if value == &Value::Bool(true) {
                return Ok(());
            }
            let hosts = value
                .as_array()
                .context("Expected true or an array of strings")?;
            for host in hosts {
                validate_leaf(host, Text)?;
            }
        }
        _ => unreachable!("object and collection schemas are handled by visit"),
    }
    Ok(())
}

// ECMAScript String.trim whitespace (Rust trim also removes U+0085, which JS does not).
fn js_trim(text: &str) -> &str {
    text.trim_matches(|c| {
        matches!(c,
        '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' |
        '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' |
        '\u{205f}' | '\u{3000}' | '\u{feff}')
    })
}

fn valid_id(id: &str) -> bool {
    let mut bytes = id.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_lowercase())
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn valid_bcrypt(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 60
        && &bytes[..2] == b"$2"
        && matches!(bytes[2], b'a' | b'b' | b'y')
        && bytes[3] == b'$'
        && bytes[4].is_ascii_digit()
        && bytes[5].is_ascii_digit()
        && bytes[6] == b'$'
        && bytes[7..]
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(*b, b'.' | b'/'))
}

fn validate_port_range(text: &str) -> Result<()> {
    let Some((start, end)) = text.split_once('-') else {
        bail!("Expected an inclusive TCP port range from 1-65535");
    };
    let parse = |part: &str| -> Option<u32> {
        if part.is_empty() || part.len() > 5 || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        part.parse().ok()
    };
    match (parse(start), parse(end)) {
        (Some(start), Some(end)) if start >= 1 && end <= 65_535 && start <= end => Ok(()),
        _ => bail!("Expected an inclusive TCP port range from 1-65535"),
    }
}

const BUILTINS: &[&str] = &["claude", "codex", "copilot", "opencode", "pi", "omp"];
fn refine_providers(providers: &Map<String, Value>) -> Result<()> {
    for (id, value) in providers {
        let extends = value
            .get("extends")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        if let Some(extends) = extends {
            if !BUILTINS.contains(&id.as_str())
                && value
                    .get("label")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .is_empty()
            {
                bail!("Custom provider \"{id}\" must declare label.");
            }
            if !BUILTINS.contains(&extends) && extends != "acp" {
                bail!("Provider \"{id}\" extends unknown provider \"{extends}\".");
            }
            if extends == "acp" && value.get("command").is_none() {
                bail!("Provider \"{id}\" extending \"acp\" must declare command.");
            }
        }
    }
    Ok(())
}

fn is_legacy(value: &Value) -> bool {
    value
        .get("command")
        .and_then(Value::as_object)
        .and_then(|command| command.get("mode"))
        .is_some_and(Value::is_string)
}

fn migrate_providers(value: &mut Value) -> Result<()> {
    let Some(providers) = value.as_object_mut() else {
        return Ok(());
    };
    // The TS preprocessor migrates all legacy entries only when the entire
    // legacy map parses. Otherwise the modern schema rejects the original data.
    for entry in providers.values().filter(|entry| is_legacy(entry)) {
        if visit(Node::Read(entry), Kind::LegacyProvider).is_err() {
            return Ok(());
        }
    }
    providers.retain(|_, entry| {
        if !is_legacy(entry) {
            return true;
        }
        let old = entry.as_object_mut().expect("legacy object");
        let command = old.remove("command").expect("legacy command");
        if command.get("mode").and_then(Value::as_str) == Some("append") {
            return false;
        }
        let env = old.remove("env");
        old.clear();
        if command.get("mode").and_then(Value::as_str) == Some("replace") {
            let mut command = command;
            let argv = command
                .as_object_mut()
                .expect("legacy command")
                .remove("argv")
                .expect("validated argv");
            old.insert("command".to_owned(), argv);
        }
        if let Some(env) = env {
            old.insert("env".to_owned(), env);
        }
        true
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_absence_are_distinct() {
        assert_eq!(normalize_persisted(json!({})).unwrap(), json!({}));
        assert_eq!(normalize_persisted(defaults()).unwrap(), defaults());
        assert!(normalize_persisted(json!({"daemon": null})).is_err());
        assert!(normalize_persisted(json!({"daemon": {"mcp": {"enabled": null}}})).is_err());
    }

    #[test]
    fn removed_fields_are_discarded_and_hosts_migrated() {
        let value = normalize_persisted(json!({
            "daemon": {"allowedHosts": ["localhost"], "browserTools": null},
            "features": {"webUi": "obsolete"},
            "providers": {"local": {"autoDownload": true}, "openai": {"voice": {}, "apiKey": " secret "}}
        })).unwrap();
        assert_eq!(
            value,
            json!({
                "daemon": {"hostnames": ["localhost"]}, "features": {},
                "providers": {"local": {}, "openai": {"apiKey": " secret "}}
            })
        );
        assert_eq!(
            normalize_persisted(json!({"daemon": {"hostnames": [], "allowedHosts": true}}))
                .unwrap(),
            json!({"daemon": {"hostnames": []}})
        );
    }

    #[test]
    fn legacy_append_is_dropped_and_replace_preserves_env() {
        let value = normalize_persisted(json!({"agents": {"providers": {
            "claude": {"command": {"mode": "append", "args": ["--flag"]}, "env": {"TOKEN": "secret"}},
            "codex": {"command": {"mode": "replace", "argv": ["binary", "argument"]}, "env": {"TOKEN": "secret"}, "disallowedTools": ["x"]},
            "pi": {"command": {"mode": "default"}},
            "omp": {"enabled": false, "options": {"mode": null}}
        }}})).unwrap();
        assert_eq!(
            value["agents"]["providers"],
            json!({
                "codex": {"command": ["binary", "argument"], "env": {"TOKEN": "secret"}},
                "pi": {}, "omp": {"enabled": false, "options": {"mode": null}}
            })
        );
        assert!(
            normalize_persisted(json!({"agents": {"providers": {
                "claude": {"command": {"mode": "append", "args": [2]}},
                "codex": {"command": {"mode": "default"}}
            }}}))
            .is_err()
        );
    }

    #[test]
    fn profile_passthrough_preserves_null_and_false_feature_values() {
        let profile = json!({"id": "", "name": "", "provider": "pi", "modeId": "",
            "cliDefault": false, "featureValues": {"unset": null, "off": false}, "future": null});
        let value =
            normalize_patch(json!({"agentProfiles": [profile.clone()], "future": false})).unwrap();
        assert_eq!(value["agentProfiles"][0], profile);
        assert_eq!(value["future"], false);
    }

    #[test]
    fn strict_passthrough_and_strip_are_distinct() {
        assert!(normalize_persisted(json!({"daemon": {"git": {"future": true}}})).is_err());
        assert!(normalize_persisted(json!({"future": true})).is_err());
        let value = normalize_persisted(json!({
            "daemon": {"mcp": {"future": null}},
            "pluginRegistries": {"opaque": {"authorization": "private", "future": true}},
            "agents": {"providers": {"pi": {"future": true, "paseoTools": {"enabled": false, "future": true}}}}
        })).unwrap();
        assert_eq!(value["daemon"]["mcp"]["future"], Value::Null);
        assert_eq!(
            value["pluginRegistries"]["opaque"],
            json!({"authorization": "private"})
        );
        assert_eq!(
            value["agents"]["providers"]["pi"],
            json!({"paseoTools": {"enabled": false}})
        );
        let value = normalize_patch(
            json!({"browserTools": true, "mcp": {"enabled": "passthrough"},
            "providers": {"any.key": {"command": null, "paseoTools": {"future": true}}}}),
        )
        .unwrap();
        assert_eq!(
            value,
            json!({"mcp": {"enabled": "passthrough"},
            "providers": {"any.key": {"command": null, "paseoTools": {}}}})
        );
    }

    #[test]
    fn ports_speech_and_provider_refinements_match_source() {
        let value = normalize_persisted(json!({
            "worktrees": {"servicePorts": {"range": " 00001-65535 ", "portScript": " script "}},
            "features": {"dictation": {"stt": {"provider": " LOCAL ", "language": "\u{feff}en "}}}
        }))
        .unwrap();
        assert_eq!(
            value["worktrees"]["servicePorts"],
            json!({"range": "00001-65535", "portScript": "script"})
        );
        assert_eq!(
            value["features"]["dictation"]["stt"],
            json!({"provider": "local", "language": "en"})
        );
        for range in ["0-1", "2-1", "1-65536", "1 -2", "000001-2"] {
            assert!(
                normalize_persisted(json!({"worktrees": {"servicePorts": {"range": range}}}))
                    .is_err()
            );
        }
        assert!(normalize_persisted(json!({"worktrees": {"servicePorts": {}}})).is_err());
        assert!(
            normalize_persisted(
                json!({"agents": {"providers": {"custom": {"extends": "acp", "label": "Custom"}}}})
            )
            .is_err()
        );
        assert!(
            normalize_persisted(json!({"agents": {"providers": {"custom": {"extends": "pi"}}}}))
                .is_err()
        );
        assert!(normalize_persisted(json!({"agents": {"providers": {"custom": {"extends": "custom", "label": "Custom"}}}})).is_err());
        assert!(
            normalize_persisted(json!({"agents": {"providers": {"custom": {"enabled": false}}}}))
                .is_ok()
        );
    }

    #[test]
    fn patch_metadata_default_and_internal_skill_selection() {
        assert_eq!(
            normalize_patch(json!({"metadataGeneration": {}})).unwrap(),
            json!({"metadataGeneration": {"providers": []}})
        );
        assert_eq!(normalize_patch(json!({})).unwrap(), json!({}));
        assert!(normalize_patch(json!({"skills": null})).is_err());
        assert!(
            normalize_patch(json!({"skills": {"selection": {"mode": "all", "skills": []}}}))
                .is_err()
        );
        assert_eq!(
            normalize_patch(json!({"skills": {"selection": {"mode": "custom", "skills": []}}}))
                .unwrap(),
            json!({"skills": {"selection": {"mode": "custom", "skills": []}}})
        );
    }

    #[test]
    fn plugin_auth_and_nested_provider_constraints() {
        let hash = format!("$2b$12${}", "a".repeat(53));
        let value = normalize_persisted(json!({"daemon": {"auth": {"password": hash}}})).unwrap();
        assert_eq!(value["daemon"]["auth"]["password"], hash);
        assert!(
            normalize_persisted(json!({"daemon": {"auth": {"password": "secret-not-a-hash"}}}))
                .is_err()
        );
        assert!(
            normalize_persisted(
                json!({"daemon": {"serviceProxy": {"publicBaseUrl": "relative/path"}}})
            )
            .is_err()
        );
        assert!(
            normalize_persisted(
                json!({"plugins": {"bad.id": {"source": "directory", "path": "/tmp"}}})
            )
            .is_err()
        );
        assert!(
            normalize_persisted(
                json!({"plugins": {"good-id": {"source": "directory", "path": ""}}})
            )
            .is_err()
        );
        assert!(normalize_persisted(json!({"plugins": {"good-id": {"source": "directory", "path": "/tmp", "unknown": true}}})).is_err());
        let provider = normalize_provider(json!({
            "env": {"TOKEN": "secret"}, "params": {"nullable": null, "disabled": false},
            "models": [{"id": "m", "label": "Model", "extra": true,
                "thinkingOptions": [{"id": "", "label": "", "isDefault": false, "extra": true}]}]
        }))
        .unwrap();
        assert_eq!(
            provider,
            json!({
                "env": {"TOKEN": "secret"}, "params": {"nullable": null, "disabled": false},
                "models": [{"id": "m", "label": "Model",
                    "thinkingOptions": [{"id": "", "label": "", "isDefault": false}]}]
            })
        );
        assert!(normalize_provider(json!({"env": {"TOKEN": null}})).is_err());
        assert!(
            normalize_persisted(json!({"agents": {"catalogRefreshTimeoutMs": 2147483648_u64}}))
                .is_err()
        );
    }
}
