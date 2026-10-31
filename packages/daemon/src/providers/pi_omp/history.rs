use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Component, Path, PathBuf},
    sync::LazyLock,
    time::SystemTime,
};

use anyhow::{Result, anyhow, bail};
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Value, json};
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncSeekExt},
};

use super::events::Mapper;

static TITLE_PREFIX: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^\d{4}-\d{2}-\d{2}T\d{2}[-:]\d{2}[-:]\d{2}").unwrap());
static LEGACY_TRANSCRIPT: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)(?:session|transcript)(?: file)?:\s*(/\S+\.jsonl)").unwrap()
});
static WINDOWS_PATH: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^[a-zA-Z]:[\\/]|^[/\\]{2}\?[/\\]|^\\{2}[^/\\]+[/\\][^/\\]+").unwrap()
});

pub struct History {
    pub descriptor: Value,
    pub timeline: Vec<Value>,
    pub subagent_events: Vec<Value>,
    pub messages: Vec<Value>,
    /// Stream envelopes retain replay timestamps, unlike initial timeline items.
    pub events: Vec<Value>,
}

fn nonempty(value: &Value) -> Option<&str> {
    value.as_str().map(str::trim).filter(|s| !s.is_empty())
}

fn record(line: &str) -> Option<Value> {
    serde_json::from_str::<Value>(line.trim())
        .ok()
        .filter(Value::is_object)
}

fn iso(ms: i64) -> Option<String> {
    DateTime::<Utc>::from_timestamp_millis(ms)
        .map(|d| d.to_rfc3339_opts(SecondsFormat::Millis, true))
}

fn date_ms(value: &Value) -> Option<i64> {
    if let Some(n) = value.as_f64().filter(|n| n.is_finite()) {
        return Some(n as i64);
    }
    let text = value.as_str()?;
    DateTime::parse_from_rfc3339(text)
        .map(|d| d.timestamp_millis())
        .ok()
        .or_else(|| {
            DateTime::parse_from_rfc2822(text)
                .map(|d| d.timestamp_millis())
                .ok()
        })
        .or_else(|| {
            chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d")
                .ok()?
                .and_hms_opt(0, 0, 0)
                .map(|d| d.and_utc().timestamp_millis())
        })
}

pub fn replay_timestamp(value: &Value) -> Option<String> {
    if let Some(s) = value.as_str() {
        let s = s.trim();
        return date_ms(&json!(s)).map(|_| s.to_owned());
    }
    let n = value.as_f64().filter(|n| n.is_finite())?;
    iso(if n > 1_000_000_000_000.0 {
        n
    } else {
        n * 1000.0
    } as i64)
}

pub fn message_text(content: &Value) -> String {
    if let Some(s) = content.as_str() {
        return s.to_owned();
    }
    content
        .as_array()
        .map(|parts| {
            parts
                .iter()
                .filter(|p| p["type"] == "text")
                .filter_map(|p| p["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n\n")
        })
        .unwrap_or_default()
}

fn descriptor_text(content: &Value) -> Option<String> {
    let text = message_text(content);
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

fn model_id(provider: &Value, model: &Value) -> Option<String> {
    Some(format!("{}/{}", nonempty(provider)?, nonempty(model)?))
}

fn entry_model(family: &str, entry: &Value) -> Option<String> {
    match entry["type"].as_str() {
        Some("model_change") => model_id(&entry["provider"], &entry["modelId"]).or_else(|| {
            (family == "omp")
                .then(|| nonempty(&entry["model"]).map(str::to_owned))
                .flatten()
        }),
        Some("message") => model_id(&entry["message"]["provider"], &entry["message"]["model"]),
        _ => None,
    }
}

fn title(family: &str, entry: &Value) -> Option<String> {
    let value = match entry["type"].as_str() {
        Some("session_info") => &entry["name"],
        Some("title") if family == "omp" => &entry["title"],
        _ => return None,
    };
    nonempty(value).map(str::to_owned)
}

fn header(family: &str, chunk: &str) -> Option<Value> {
    let mut lines = chunk.lines();
    let valid = |e: &Value| {
        e["type"] == "session"
            && e["id"].as_str().is_some_and(|s| !s.is_empty())
            && e["cwd"].as_str().is_some_and(|s| !s.is_empty())
    };
    if family == "pi" {
        return record(lines.next()?).filter(valid);
    }
    lines.filter_map(record).find(valid)
}

async fn chunks(path: &Path) -> Result<(String, String, SystemTime)> {
    let mut file = fs::File::open(path).await?;
    let meta = file.metadata().await?;
    let mut head = vec![0; (meta.len().min(64 * 1024)) as usize];
    let count = file.read(&mut head).await?;
    head.truncate(count);
    let start = meta.len().saturating_sub(256 * 1024);
    file.seek(std::io::SeekFrom::Start(start)).await?;
    let mut tail = vec![0; (meta.len() - start) as usize];
    let count = file.read(&mut tail).await?;
    tail.truncate(count);
    Ok((
        String::from_utf8_lossy(&head).into_owned(),
        String::from_utf8_lossy(&tail).into_owned(),
        meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
    ))
}

/// Descriptor parsing deliberately uses the same bounded head/tail windows as the providers.
pub async fn describe(family: &str, path: &Path) -> Result<Option<Value>> {
    let (head, tail, mtime) = match chunks(path).await {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    let Some(header) = header(family, &head) else {
        return Ok(None);
    };
    let (mut head_title, mut first, mut head_model, mut head_thinking) = (None, None, None, None);
    for (index, line) in head.lines().enumerate() {
        let Some(entry) = record(line) else { continue };
        head_title = title(family, &entry).or(head_title);
        head_model = entry_model(family, &entry).or(head_model);
        if entry["type"] == "thinking_level_change" {
            head_thinking = nonempty(&entry["thinkingLevel"])
                .map(str::to_owned)
                .or(head_thinking);
        }
        if first.is_none() && entry["type"] == "message" && entry["message"]["role"] == "user" {
            first = descriptor_text(&entry["message"]["content"]);
        }
        if (head_title.is_some()
            && first.is_some()
            && head_model.is_some()
            && head_thinking.is_some())
            || (index + 1 >= 2000 && first.is_some())
        {
            break;
        }
    }
    let (mut tail_title, mut last, mut tail_model, mut tail_thinking) = (None, None, None, None);
    let (mut activity, mut fallback) = (None, None);
    for entry in tail.lines().rev().filter_map(record) {
        if tail_title.is_none() {
            tail_title = title(family, &entry);
        }
        if tail_model.is_none() {
            tail_model = entry_model(family, &entry);
        }
        if tail_thinking.is_none() && entry["type"] == "thinking_level_change" {
            tail_thinking = nonempty(&entry["thinkingLevel"]).map(str::to_owned);
        }
        let timestamp = date_ms(&entry["timestamp"]);
        if fallback.is_none() {
            fallback = timestamp;
        }
        if entry["type"] != "message" {
            continue;
        }
        if activity.is_none() {
            activity = timestamp;
        }
        if last.is_none() && entry["message"]["role"] == "user" {
            last = descriptor_text(&entry["message"]["content"]);
        }
    }
    let path_title = if family == "omp" {
        path.file_stem()
            .and_then(|s| s.to_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .filter(|s| !TITLE_PREFIX.is_match(s))
            .filter(|s| !(s.len() >= 8 && s.bytes().all(|c| c.is_ascii_hexdigit())))
            .map(str::to_owned)
    } else {
        None
    };
    let model = tail_model.or(head_model);
    let thinking = tail_thinking.or(head_thinking);
    let mut config = json!({});
    if let Some(model) = &model {
        config["model"] = json!(model);
    }
    if let Some(thinking) = &thinking {
        config["thinkingOptionId"] = json!(thinking);
    }
    let mtime: DateTime<Utc> = mtime.into();
    Ok(Some(json!({
        "sessionId":header["id"], "sessionFile":path, "cwd":header["cwd"],
        "title":tail_title.or(head_title).or(path_title).or_else(|| first.clone()),
        "firstUserMessage":first, "lastUserMessage":last,
        "lastActivityAt":iso(activity.or(fallback).unwrap_or(mtime.timestamp_millis())),
        "model":model, "thinkingOptionId":thinking, "importConfig":config,
        "header":header
    })))
}

fn resolve_path(value: &str, base: &Path, home: &Path) -> PathBuf {
    let path = if value == "~" {
        home.to_owned()
    } else if let Some(rest) = value.strip_prefix("~/") {
        home.join(rest)
    } else if Path::new(value).is_absolute() {
        PathBuf::from(value)
    } else {
        base.join(value)
    };
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn profile(env: &BTreeMap<String, String>) -> Result<Option<String>> {
    let value = env
        .get("OMP_PROFILE")
        .or_else(|| env.get("PI_PROFILE"))
        .map(|s| s.trim())
        .unwrap_or("");
    if value.is_empty() || value == "default" {
        return Ok(None);
    }
    let name = value.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(name.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (name.len() == 4
            && (name.starts_with("COM") || name.starts_with("LPT"))
            && name.as_bytes()[3].is_ascii_digit());
    if value.len() > 64
        || !value.as_bytes()[0].is_ascii_lowercase() && !value.as_bytes()[0].is_ascii_digit()
        || value.ends_with('.')
        || !value.bytes().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'.' | b'_' | b'-')
        })
        || reserved
    {
        bail!("Invalid OMP profile \"{value}\"");
    }
    Ok(Some(value.to_owned()))
}

async fn settings_dir(path: &Path) -> Option<String> {
    let value: Value = serde_json::from_slice(&fs::read(path).await.ok()?).ok()?;
    value["sessionDir"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .map(str::to_owned)
}

pub async fn sessions_dir(
    family: &str,
    env: &BTreeMap<String, String>,
    options: &Value,
) -> Result<PathBuf> {
    let mut env: BTreeMap<String, String> = std::env::vars()
        .chain(env.iter().map(|(k, v)| (k.clone(), v.clone())))
        .collect();
    if options["env"].is_object() {
        env.clear();
    }
    for source in [options.get("env"), options.pointer("/runtimeSettings/env")] {
        if let Some(source) = source.and_then(Value::as_object) {
            for (key, value) in source {
                if let Some(value) = value.as_str() {
                    env.insert(key.clone(), value.to_owned());
                }
            }
        }
    }
    let cwd = std::env::current_dir()?;
    let base = options["cwd"].as_str().map(Path::new).unwrap_or(&cwd);
    let home = options["homeDir"]
        .as_str()
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .ok_or_else(|| anyhow!("Home directory is unavailable"))?;
    if let Some(dir) = options
        .get("sessionDir")
        .or_else(|| options.pointer("/providerOptions/sessionDir"))
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
    {
        return Ok(resolve_path(dir, base, &home));
    }
    let profile = if family == "omp" {
        profile(&env)?
    } else {
        None
    };
    let agent = if profile.is_none() {
        env.get("PI_CODING_AGENT_DIR")
            .filter(|s| !s.trim().is_empty())
            .map(|s| resolve_path(s, &cwd, &home))
    } else {
        None
    };
    let agent = agent.unwrap_or_else(|| {
        let config = if family == "omp" {
            env.get("PI_CONFIG_DIR")
                .filter(|s| !s.is_empty())
                .map(String::as_str)
                .unwrap_or(".omp")
        } else {
            ".pi"
        };
        // Node path.join keeps an absolute-looking config suffix beneath home.
        let mut root = home.join(config.trim_start_matches('/'));
        if let Some(profile) = &profile {
            root = root.join("profiles").join(profile);
        }
        root.join("agent")
    });
    if let Some(dir) = env
        .get("PI_CODING_AGENT_SESSION_DIR")
        .filter(|s| !s.trim().is_empty())
    {
        return Ok(resolve_path(dir, base, &home));
    }
    let global = settings_dir(&agent.join("settings.json")).await;
    let local = if let Some(cwd) = options["cwd"].as_str() {
        settings_dir(
            &Path::new(cwd)
                .join(if family == "omp" { ".omp" } else { ".pi" })
                .join("settings.json"),
        )
        .await
    } else {
        None
    };
    if let Some(dir) = local.or(global) {
        return Ok(resolve_path(&dir, base, &home));
    }
    if family == "omp"
        && cfg!(any(target_os = "linux", target_os = "macos"))
        && (profile.is_some()
            || !env
                .get("PI_CODING_AGENT_DIR")
                .is_some_and(|s| !s.trim().is_empty()))
    {
        if let Some(xdg) = env.get("XDG_DATA_HOME").filter(|s| !s.is_empty()) {
            let mut root = Path::new(xdg).join("omp");
            if let Some(profile) = &profile {
                root = root.join("profiles").join(profile);
            }
            if fs::try_exists(&root).await.unwrap_or(false) {
                return Ok(root.join("sessions"));
            }
        }
    }
    Ok(agent.join("sessions"))
}

async fn files(root: &Path) -> Vec<PathBuf> {
    let mut pending = vec![root.to_owned()];
    let mut out = Vec::new();
    while let Some(path) = pending.pop() {
        let Ok(mut dir) = fs::read_dir(&path).await else {
            continue;
        };
        let mut entries = Vec::new();
        while let Ok(Some(entry)) = dir.next_entry().await {
            entries.push(entry);
        }
        entries.sort_by_key(|e| e.file_name());
        // Reverse stack traversal then sort the final paths to match recursive readdir order.
        for entry in entries {
            let Ok(kind) = entry.file_type().await else {
                continue;
            };
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() && entry.file_name().to_string_lossy().ends_with(".jsonl") {
                out.push(entry.path());
            }
        }
    }
    out.sort();
    out
}

fn preview(value: &Value) -> Value {
    let text = value
        .as_str()
        .unwrap_or("")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if text.is_empty() {
        return Value::Null;
    }
    // JavaScript previews are bounded in UTF-16 code units, not UTF-8 bytes.
    let units: Vec<_> = text.encode_utf16().take(160).collect();
    json!(String::from_utf16_lossy(&units))
}

pub async fn discover(
    family: &str,
    env: &BTreeMap<String, String>,
    options: &Value,
) -> Result<Vec<Value>> {
    let root = sessions_dir(family, env, options).await?;
    let mut ranked = Vec::new();
    for file in files(&root).await {
        if let Ok(meta) = fs::metadata(&file).await {
            if let Ok(mtime) = meta.modified() {
                ranked.push((file, mtime));
            }
        }
    }
    ranked.sort_by(|a, b| b.1.cmp(&a.1));
    let limit = options["limit"].as_u64().unwrap_or(20) as usize;
    let scan = options["scanLimit"]
        .as_u64()
        .map(|n| n as usize)
        .unwrap_or_else(|| limit.saturating_mul(40).max(400))
        .min(500);
    let cwd = options["cwd"].as_str().filter(|s| !s.is_empty());
    let canonical_cwd = if let Some(cwd) = cwd {
        fs::canonicalize(cwd).await.ok()
    } else {
        None
    };
    if options.get("scanLimit").is_some() || cwd.is_none() {
        ranked.truncate(scan);
    }
    let mut out = Vec::new();
    for (file, _) in ranked {
        let Some(desc) = describe(family, &file).await? else {
            continue;
        };
        if let Some(cwd) = cwd {
            let candidate = desc["cwd"].as_str().unwrap_or("");
            let same = equivalent_paths(candidate, cwd);
            if !same {
                let canonical_candidate = fs::canonicalize(candidate).await.ok();
                let canonical_match = canonical_candidate
                    .as_ref()
                    .is_some_and(|p| equivalent_paths(&p.to_string_lossy(), cwd))
                    || canonical_cwd
                        .as_ref()
                        .is_some_and(|p| equivalent_paths(candidate, &p.to_string_lossy()))
                    || (canonical_cwd.is_some() && canonical_candidate == canonical_cwd);
                if !canonical_match {
                    continue;
                }
            }
        }
        out.push(json!({"providerHandleId":file,"cwd":desc["cwd"],"title":desc["title"],"firstPromptPreview":preview(&desc["firstUserMessage"]),"lastPromptPreview":preview(if desc["lastUserMessage"].is_null() { &desc["firstUserMessage"] } else { &desc["lastUserMessage"] }),"lastActivityAt":desc["lastActivityAt"]}));
        if out.len() >= limit {
            break;
        }
    }
    out.sort_by(|a, b| {
        b["lastActivityAt"]
            .as_str()
            .cmp(&a["lastActivityAt"].as_str())
    });
    Ok(out)
}

pub async fn resolve_session_file(
    family: &str,
    handle: &str,
    env: &BTreeMap<String, String>,
    options: &Value,
) -> Result<Option<PathBuf>> {
    let handle = if family == "omp" {
        handle.trim()
    } else {
        handle
    };
    if handle.is_empty() {
        return Ok(None);
    }
    if fs::metadata(handle).await.is_ok_and(|m| m.is_file()) {
        return Ok(Some(PathBuf::from(handle)));
    }
    if family != "omp" {
        return Ok(None);
    }
    let root = sessions_dir(family, env, options).await?;
    let mut files = files(&root).await;
    let suffix = format!("_{handle}.jsonl");
    files.sort_by_key(|p| {
        !p.file_name()
            .is_some_and(|s| s.to_string_lossy().ends_with(&suffix))
    });
    for file in files {
        if let Ok((head, _, _)) = chunks(&file).await {
            if header(family, &head).is_some_and(|h| h["id"] == handle) {
                return Ok(Some(file));
            }
        }
    }
    Ok(None)
}

pub fn import_persistence(provider: &str, handle: &str, cwd: &str, stored_config: &Value) -> Value {
    let mut metadata = stored_config.as_object().cloned().unwrap_or_default();
    metadata.insert("provider".into(), json!(provider));
    metadata.insert("cwd".into(), json!(cwd));
    json!({"provider":provider,"sessionId":handle,"nativeHandle":handle,"metadata":metadata})
}

pub async fn active_entries(path: &Path, active_entry_id: Option<&str>) -> Result<Vec<Value>> {
    let content = fs::read(path).await?;
    let entries: Vec<_> = String::from_utf8_lossy(&content)
        .lines()
        .filter_map(record)
        .filter(|e| e["id"].is_string())
        .collect();
    let by_id: HashMap<_, _> = entries
        .iter()
        .enumerate()
        .filter_map(|(i, e)| e["id"].as_str().map(|id| (id, i)))
        .collect();
    let parents: HashSet<_> = entries
        .iter()
        .filter_map(|e| e["parentId"].as_str())
        .filter(|s| !s.is_empty())
        .collect();
    let mut current = active_entry_id
        .filter(|s| !s.is_empty())
        .and_then(|id| by_id.get(id).copied())
        .or_else(|| {
            entries
                .iter()
                .rposition(|e| !parents.contains(e["id"].as_str().unwrap_or("")))
        })
        .or_else(|| entries.len().checked_sub(1));
    let mut seen = HashSet::new();
    let mut chain = Vec::new();
    while let Some(index) = current {
        let entry = &entries[index];
        let id = entry["id"].as_str().unwrap_or("");
        if id.is_empty() || !seen.insert(id) {
            break;
        }
        chain.push(entry.clone());
        current = entry["parentId"]
            .as_str()
            .filter(|s| !s.is_empty())
            .and_then(|p| by_id.get(p).copied());
    }
    chain.reverse();
    Ok(chain)
}

fn fallback_message(role: &str, value: &Value) -> Value {
    let text = if let Some(text) = value["content"].as_str() {
        text.to_owned()
    } else if let Some(parts) = value["content"].as_array() {
        let text = parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if text.is_empty() {
            "Unsupported history record".into()
        } else {
            text
        }
    } else {
        value["text"]
            .as_str()
            .unwrap_or("Unsupported history record")
            .to_owned()
    };
    json!({"role":"custom","content":format!("[{role}] {text}")})
}

fn entry_message(family: &str, entry: &Value) -> Option<Value> {
    if let Some(role) = entry["message"]["role"].as_str() {
        let message = &entry["message"];
        if matches!(
            role,
            "user" | "assistant" | "toolResult" | "custom" | "bashExecution"
        ) {
            return Some(message.clone());
        }
        if family == "pi"
            || matches!(
                role,
                "system"
                    | "pythonExecution"
                    | "hookMessage"
                    | "branchSummary"
                    | "compactionSummary"
                    | "fileMention"
            )
        {
            return None;
        }
        if role == "developer" {
            if let Some(content) = message["content"].as_array().filter(|c| c.len() == 1) {
                if content[0]["type"] == "text" {
                    if let Some(text) = content[0]["text"].as_str() {
                        let text = text.trim();
                        if text.ends_with("</system-reminder>")
                            && text.strip_prefix("<system-reminder").is_some_and(|s| {
                                s.starts_with('>')
                                    || s.chars().next().is_some_and(char::is_whitespace)
                            })
                        {
                            return None;
                        }
                    }
                }
            }
        }
        return Some(fallback_message(role, message));
    }
    if entry["type"] == "custom_message" {
        if !entry["content"].is_string() && !entry["content"].is_array() {
            return None;
        }
        let mut message = json!({"role":"custom","content":entry["content"]});
        for key in ["id", "customType", "display", "details", "attribution"] {
            if let Some(value) = entry.get(key) {
                message[key] = value.clone();
            }
        }
        return Some(message);
    }
    let kind = entry["type"].as_str()?;
    if family == "pi"
        || matches!(
            kind,
            "session"
                | "session_init"
                | "system"
                | "title"
                | "title_change"
                | "custom"
                | "system_prompt"
                | "model_change"
                | "thinking_level_change"
                | "ttsr_injection"
                | "tool_execution"
        )
        || kind.starts_with("tool_execution_")
    {
        return None;
    }
    Some(fallback_message(kind, entry))
}

pub async fn load_with_metadata(
    family: &str,
    provider: &str,
    path: &Path,
    active_entry_id: Option<&str>,
    metadata: &Value,
) -> Result<History> {
    let mut visited = HashSet::new();
    load_inner(
        family,
        provider,
        path,
        active_entry_id,
        &mut visited,
        true,
        metadata.get("bridgedTools"),
    )
    .await
}

pub async fn load_live(
    family: &str,
    provider: &str,
    path: &Path,
    active_entry_id: Option<&str>,
    metadata: &Value,
) -> Result<History> {
    let mut visited = HashSet::new();
    load_inner(
        family,
        provider,
        path,
        active_entry_id,
        &mut visited,
        false,
        metadata.get("bridgedTools"),
    )
    .await
}

fn push_event(history: &mut History, event: Value) {
    if event["type"] == "timeline" {
        history.timeline.push(event["item"].clone());
    }
    if event["type"] == "provider_subagent" {
        history.subagent_events.push(event.clone());
    }
    history.events.push(event);
}

async fn load_inner(
    family: &str,
    provider: &str,
    path: &Path,
    active_entry_id: Option<&str>,
    visited: &mut HashSet<PathBuf>,
    append_todo: bool,
    bridged_tools: Option<&Value>,
) -> Result<History> {
    let mut history = History {
        descriptor: describe(family, path).await?.unwrap_or(Value::Null),
        timeline: Vec::new(),
        subagent_events: Vec::new(),
        messages: Vec::new(),
        events: Vec::new(),
    };
    if !visited.insert(path.to_owned()) {
        return Ok(history);
    }
    let entries = match active_entries(path, active_entry_id).await {
        Ok(entries) => entries,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            return Ok(history);
        }
        Err(error) => return Err(error),
    };
    if history.descriptor.is_object() {
        history.descriptor["activeBranchEntryId"] = entries
            .last()
            .map(|e| e["id"].clone())
            .unwrap_or(Value::Null);
    }
    let mut mapper = Mapper::new(family, provider);
    if let Some(tools) = bridged_tools {
        mapper.set_bridged_tools(tools);
    }
    for entry in &entries {
        let timestamp = if family == "omp" {
            replay_timestamp(&entry["timestamp"])
        } else {
            None
        };
        if family == "omp" && entry["type"] == "compaction" {
            let mut item = json!({"type":"compaction","status":"completed"});
            if entry["tokensBefore"].is_number() {
                item["preTokens"] = entry["tokensBefore"].clone();
            }
            let mut event = json!({"type":"timeline","provider":provider,"item":item});
            if let Some(timestamp) = timestamp {
                event["timestamp"] = json!(timestamp);
            }
            push_event(&mut history, event);
            continue;
        }
        let Some(message) = entry_message(family, entry) else {
            continue;
        };
        let user_id = (message["role"] == "user")
            .then(|| entry["id"].as_str())
            .flatten();
        for mut event in mapper.replay_message(&message, user_id) {
            if event["type"] == "timeline" {
                if let Some(timestamp) = &timestamp {
                    event["timestamp"] = json!(timestamp);
                } else if let Some(object) = event.as_object_mut() {
                    object.remove("timestamp");
                }
            }
            push_event(&mut history, event);
        }
        history.messages.push(message);
    }
    if family == "omp" {
        let transcripts = subagent_transcripts(&history.messages, path);
        for transcript in transcripts {
            let child_entries = match active_entries(&transcript.path, None).await {
                Ok(entries) => entries,
                Err(error)
                    if error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
                {
                    Vec::new()
                }
                Err(error) => return Err(error),
            };
            let model = child_entries
                .iter()
                .filter_map(|e| {
                    if e["type"] == "model_change" {
                        model_id(&e["provider"], &e["modelId"])
                    } else if e["type"] == "message" && e["message"]["role"] == "assistant" {
                        model_id(
                            &e["message"]["provider"],
                            e["message"]
                                .get("responseModel")
                                .filter(|v| !v.is_null())
                                .unwrap_or(&e["message"]["model"]),
                        )
                    } else {
                        None
                    }
                })
                .last();
            let title = subagent_title(&transcript.title, model.as_deref());
            push_event(
                &mut history,
                transcript.upsert(
                    provider,
                    "running",
                    &title,
                    child_entries
                        .first()
                        .and_then(|e| replay_timestamp(&e["timestamp"])),
                ),
            );
            let child = Box::pin(load_inner(
                family,
                provider,
                &transcript.path,
                None,
                visited,
                false,
                None,
            ))
            .await?;
            for event in child.events {
                if event["type"] == "timeline" {
                    let mut body =
                        json!({"type":"timeline","id":transcript.id,"item":event["item"]});
                    if let Some(timestamp) = event.get("timestamp") {
                        body["timestamp"] = timestamp.clone();
                    }
                    push_event(
                        &mut history,
                        json!({"type":"provider_subagent","provider":provider,"event":body}),
                    );
                } else if event["type"] == "provider_subagent" {
                    push_event(&mut history, event);
                }
            }
            push_event(
                &mut history,
                transcript.upsert(
                    provider,
                    transcript.status,
                    &title,
                    child_entries
                        .last()
                        .and_then(|e| replay_timestamp(&e["timestamp"])),
                ),
            );
        }
        if append_todo {
            if let Some(todo) = todo_state(&entries) {
                push_event(
                    &mut history,
                    json!({"type":"timeline","provider":provider,"item":todo}),
                );
            }
        }
    } else {
        let mut remaining = 16 * 1024 * 1024;
        for child in mapper.drain_child_sessions() {
            let (Some(id), Some(file)) = (child["id"].as_str(), child["file"].as_str()) else {
                continue;
            };
            if remaining == 0 {
                break;
            }
            let budget = remaining.min(2 * 1024 * 1024);
            remaining -= budget;
            for event in pi_child_history(id, Path::new(file), budget).await {
                push_event(
                    &mut history,
                    json!({"type":"provider_subagent","provider":provider,"event":event}),
                );
            }
        }
    }
    Ok(history)
}

struct Transcript {
    id: String,
    title: String,
    tool_call: String,
    path: PathBuf,
    status: &'static str,
}
impl Transcript {
    fn upsert(
        &self,
        provider: &str,
        status: &str,
        title: &str,
        timestamp: Option<String>,
    ) -> Value {
        let mut event = json!({"type":"upsert","id":self.id,"title":title,"status":status,"toolCallId":self.tool_call});
        if let Some(timestamp) = timestamp {
            event["timestamp"] = json!(timestamp);
        }
        json!({"type":"provider_subagent","provider":provider,"event":event})
    }
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.0),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

fn subagent_transcripts(messages: &[Value], parent: &Path) -> Vec<Transcript> {
    let mut calls = HashMap::new();
    for message in messages.iter().filter(|m| m["role"] == "assistant") {
        for block in message["content"].as_array().into_iter().flatten() {
            if block["type"] == "toolCall" && block["name"] == "task" {
                if let Some(id) = block["id"].as_str() {
                    calls.insert(
                        id,
                        block["arguments"]["agent"]
                            .as_str()
                            .unwrap_or("OMP subagent"),
                    );
                }
            }
        }
    }
    let mut out = Vec::new();
    for message in messages
        .iter()
        .filter(|m| m["role"] == "toolResult" && m["toolName"] == "task")
    {
        let id = message["toolCallId"].as_str().unwrap_or("");
        let Some(title) = calls.get(id) else { continue };
        let details = message
            .get("details")
            .filter(|v| !v.is_null())
            .unwrap_or(&message["content"]["details"]);
        let mut found = false;
        for result in details["results"].as_array().into_iter().flatten() {
            let Some(child_id) = result["id"].as_str().filter(|s| !s.is_empty()) else {
                continue;
            };
            found = true;
            let status = if message["isError"] == true
                || truthy(&result["error"])
                || result["exitCode"].as_f64().is_some_and(|n| n != 0.0)
            {
                "failed"
            } else if result["aborted"] == true {
                "canceled"
            } else {
                "completed"
            };
            let basename = Path::new(child_id)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy();
            out.push(Transcript {
                id: child_id.to_owned(),
                title: result["agent"].as_str().unwrap_or(title).to_owned(),
                tool_call: id.to_owned(),
                path: parent.with_extension("").join(format!("{basename}.jsonl")),
                status,
            });
        }
        if found {
            continue;
        }
        let text = message["content"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter(|p| p["type"] == "text")
                    .filter_map(|p| p["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        if let Some(capture) = LEGACY_TRANSCRIPT.captures(&text) {
            let path = PathBuf::from(&capture[1]);
            out.push(Transcript {
                id: path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                title: (*title).to_owned(),
                tool_call: id.to_owned(),
                path,
                status: if message["isError"] == true {
                    "failed"
                } else {
                    "completed"
                },
            });
        }
    }
    out
}

fn subagent_title(title: &str, model: Option<&str>) -> String {
    let title = if title.trim().is_empty() {
        "OMP subagent"
    } else {
        title.trim()
    };
    match model.map(str::trim).filter(|m| !m.is_empty()) {
        None => title.to_owned(),
        Some(model) => match model
            .split_once('/')
            .filter(|(a, b)| !a.is_empty() && !b.is_empty())
        {
            Some((provider, name)) => format!("{title} · {name} ({provider})"),
            None => format!("{title} · {model}"),
        },
    }
}

pub fn todo_state(entries: &[Value]) -> Option<Value> {
    let message = entries
        .iter()
        .rev()
        .find(|e| e["message"]["role"] == "toolResult" && e["message"]["toolName"] == "todo")?
        .get("message")?;
    let phases = message["details"]["phases"].as_array()?;
    let mut tasks = Vec::new();
    for phase in phases {
        phase["name"].as_str()?;
        for task in phase["tasks"].as_array()? {
            let content = task["content"].as_str()?;
            let status = task["status"].as_str()?;
            if !matches!(
                status,
                "pending" | "in_progress" | "completed" | "blocked" | "abandoned"
            ) {
                return None;
            }
            if task.get("blocker").is_some_and(|v| !v.is_string()) {
                return None;
            }
            tasks.push((content, status, task["blocker"].as_str()));
        }
    }
    if tasks.is_empty() {
        return None;
    }
    Some(
        json!({"type":"todo","items":tasks.into_iter().filter(|(_,status,_)| *status != "abandoned").map(|(content,status,blocker)| {
        let text = if status == "blocked" { format!("{content} ({})", blocker.filter(|s| !s.is_empty()).map(|s| format!("blocked: {s}")).unwrap_or_else(|| "blocked".into())) } else { content.to_owned() };
        json!({"text":text,"status":if matches!(status,"in_progress" | "completed") {status} else {"pending"},"completed":status == "completed"})
    }).collect::<Vec<_>>()}),
    )
}

pub(super) async fn pi_child_history(id: &str, path: &Path, budget: usize) -> Vec<Value> {
    let Ok(file) = fs::File::open(path).await else {
        return Vec::new();
    };
    let mut data = Vec::new();
    if file
        .take(budget as u64)
        .read_to_end(&mut data)
        .await
        .is_err()
    {
        return Vec::new();
    }
    let Some(last_newline) = data.iter().rposition(|b| *b == b'\n') else {
        return Vec::new();
    };
    let mut mapper = Mapper::new("pi", "pi");
    let mut out = Vec::new();
    for entry in String::from_utf8_lossy(&data[..last_newline])
        .lines()
        .filter_map(record)
    {
        if !entry["message"]["role"].is_string() {
            continue;
        }
        for event in mapper.replay_message(&entry["message"], None) {
            if event["type"] != "timeline" {
                continue;
            }
            let mut body =
                json!({"type":"timeline","id":id,"item":limit_child_item(event["item"].clone())});
            if truthy(&entry["timestamp"]) {
                body["timestamp"] = entry["timestamp"].clone();
            }
            out.push(body);
            if out.len() >= 200 {
                return out;
            }
        }
    }
    out
}

pub(super) fn limit_child_item(mut item: Value) -> Value {
    if item["type"] != "tool_call" {
        return item;
    }
    let shell = item["detail"]["type"] == "shell";
    let plain = item["detail"]["type"] == "plain_text";
    let failed = item["status"] == "failed";
    for (pointer, enabled, notice) in [
        ("/error/content", shell && failed, ""),
        ("/detail/output", shell, ""),
        ("/detail/text", plain, "\n\n[Content truncated]"),
    ] {
        if !enabled {
            continue;
        }
        let Some(value) = item.pointer_mut(pointer) else {
            continue;
        };
        let Some(text) = value.as_str() else { continue };
        if text.encode_utf16().count() <= 64 * 1024 {
            continue;
        }
        let prefix: Vec<_> = text.encode_utf16().take(64 * 1024 - notice.len()).collect();
        *value = json!(format!("{}{notice}", String::from_utf16_lossy(&prefix)));
    }
    item
}

fn equivalent_paths(left: &str, right: &str) -> bool {
    let windows = WINDOWS_PATH.is_match(left) || WINDOWS_PATH.is_match(right);
    normalize_comparison_path(left, windows) == normalize_comparison_path(right, windows)
}

fn normalize_comparison_path(value: &str, windows: bool) -> String {
    let mut value = if windows {
        value.replace('\\', "/").to_lowercase()
    } else {
        value.to_owned()
    };
    if windows && value.starts_with("//?/unc/") {
        value = format!("//{}", &value[8..]);
    } else if windows && value.starts_with("//?/") {
        value.drain(..4);
    }
    let (root, rest) = if windows && value.as_bytes().get(1) == Some(&b':') {
        if value.as_bytes().get(2) == Some(&b'/') {
            (value[..3].to_owned(), &value[3..])
        } else {
            (value[..2].to_owned(), &value[2..])
        }
    } else if windows && value.starts_with("//") {
        let mut split = value[2..].splitn(3, '/');
        let server = split.next().unwrap_or("");
        let share = split.next().unwrap_or("");
        (format!("//{server}/{share}/"), split.next().unwrap_or(""))
    } else if let Some(rest) = value.strip_prefix('/') {
        ("/".to_owned(), rest)
    } else {
        (String::new(), value.as_str())
    };
    let absolute = root.ends_with('/');
    let mut parts = Vec::new();
    for part in rest.split('/') {
        match part {
            "" | "." => {}
            ".." if parts.last().is_some_and(|p| *p != "..") => {
                parts.pop();
            }
            ".." if absolute => {}
            _ => parts.push(part),
        }
    }
    if parts.is_empty() {
        if root.is_empty() { ".".into() } else { root }
    } else {
        format!("{root}{}", parts.join("/"))
    }
}
