use std::{
    collections::{BTreeMap, HashMap, HashSet},
    io,
    path::{Path, PathBuf},
    time::SystemTime,
};

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Value, json};
use tokio::{
    fs,
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
};
use uuid::Uuid;

pub struct ReplaySubagent {
    pub agent_id: String,
    pub meta: Option<Value>,
    pub entries: Vec<Value>,
}

#[derive(Default)]
pub struct ReplayHistory {
    pub parent_entries: Vec<Value>,
    pub subagents: Vec<ReplaySubagent>,
    pub workflows: Vec<Value>,
    pub workflow_entries_by_run_id: BTreeMap<String, Vec<Value>>,
}

pub fn config_dir(env: &BTreeMap<String, String>) -> Result<PathBuf> {
    if let Some(path) = env.get("CLAUDE_CONFIG_DIR") {
        return Ok(PathBuf::from(path));
    }
    Ok(dirs::home_dir()
        .ok_or_else(|| anyhow!("Cannot resolve Claude home directory"))?
        .join(".claude"))
}

/// The SDK regex has no Unicode flag: each UTF-16 code unit becomes one dash.
pub fn encode_project_path(input: &str) -> String {
    let mut encoded = String::with_capacity(input.len().min(200) + 8);
    let mut count = 0usize;
    let mut hash = 0i32;
    for unit in input.encode_utf16() {
        hash = hash.wrapping_mul(31).wrapping_add(i32::from(unit));
        if count < 200 {
            encoded.push(if unit < 128 && (unit as u8).is_ascii_alphanumeric() {
                unit as u8 as char
            } else {
                '-'
            });
        }
        count += 1;
    }
    if count > 200 {
        encoded.push('-');
        let mut value = i64::from(hash).unsigned_abs();
        let mut digits = [0u8; 7];
        let mut start = digits.len();
        loop {
            start -= 1;
            digits[start] = b"0123456789abcdefghijklmnopqrstuvwxyz"[(value % 36) as usize];
            value /= 36;
            if value == 0 {
                break;
            }
        }
        for digit in &digits[start..] {
            encoded.push(*digit as char);
        }
    }
    encoded
}

pub async fn project_dir(cwd: &str, env: &BTreeMap<String, String>) -> Result<PathBuf> {
    let canonical = fs::canonicalize(cwd)
        .await
        .unwrap_or_else(|_| PathBuf::from(cwd));
    let canonical = canonical.to_string_lossy();
    #[cfg(target_os = "macos")]
    let canonical = {
        use unicode_normalization::UnicodeNormalization;
        canonical.nfc().collect::<String>()
    };
    Ok(config_dir(env)?
        .join("projects")
        .join(encode_project_path(&canonical)))
}

pub async fn transcript_path(
    cwd: &str,
    session_id: &str,
    env: &BTreeMap<String, String>,
) -> Result<PathBuf> {
    let file_name = format!("{session_id}.jsonl");
    let expected = project_dir(cwd, env).await?.join(&file_name);
    if fs::try_exists(&expected).await.unwrap_or(false) {
        return Ok(expected);
    }
    let projects = config_dir(env)?.join("projects");
    let entries = match directory_entries(&projects).await {
        Ok(entries) => entries,
        Err(error) if missing(&error) => return Ok(expected),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let candidate = entry.path().join(&file_name);
        if fs::try_exists(&candidate).await.unwrap_or(false) {
            return Ok(candidate);
        }
    }
    Ok(expected)
}

fn missing(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

fn expected_import_error(error: &io::Error) -> bool {
    missing(error) || error.kind() == io::ErrorKind::PermissionDenied
}

async fn directory_entries(path: &Path) -> io::Result<Vec<fs::DirEntry>> {
    let mut directory = fs::read_dir(path).await?;
    let mut entries = Vec::new();
    while let Some(entry) = directory.next_entry().await? {
        entries.push(entry);
    }
    // Node readdir returns name order; traversal and equal-timestamp records retain it.
    entries.sort_by_key(|entry| entry.file_name());
    Ok(entries)
}

async fn visit_records(path: &Path, mut visit: impl FnMut(Value)) -> Result<()> {
    let file = fs::File::open(path)
        .await
        .with_context(|| format!("Cannot read Claude transcript {}", path.display()))?;
    let metadata = file.metadata().await?;
    if metadata.is_dir() {
        bail!("Claude transcript is a directory: {}", path.display());
    }
    // Read the opened inode's initial extent, not newly appended messages.
    let mut reader = BufReader::with_capacity(64 * 1024, file.take(metadata.len()));
    let mut line = Vec::new();
    while reader.read_until(b'\n', &mut line).await? != 0 {
        if let Ok(entry @ Value::Object(_)) = serde_json::from_slice::<Value>(&line) {
            visit(entry);
        }
        line.clear();
    }
    Ok(())
}

async fn optional_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&fs::read(path).await.ok()?).ok()
}

fn filtered_fields(value: &Value, strings: &[&str], numbers: &[&str]) -> Option<Value> {
    value.as_object()?;
    let mut result = serde_json::Map::new();
    for key in strings {
        if let Some(value @ Value::String(_)) = value.get(*key) {
            result.insert((*key).into(), value.clone());
        }
    }
    for key in numbers {
        if let Some(value @ Value::Number(_)) = value.get(*key) {
            result.insert((*key).into(), value.clone());
        }
    }
    Some(Value::Object(result))
}

pub async fn read_history(path: &Path) -> Result<ReplayHistory> {
    let session_directory = path.with_extension("");
    let sidechain_directory = session_directory.join("subagents");
    let mut sidechain_files = vec![path.to_path_buf()];
    let mut workflow_files: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    let mut meta = HashMap::new();
    let mut directories = vec![sidechain_directory.clone()];
    while let Some(directory) = directories.pop() {
        let entries = match directory_entries(&directory).await {
            Ok(entries) => entries,
            Err(error) if missing(&error) => continue,
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let kind = entry.file_type().await?;
            if kind.is_dir() {
                directories.push(entry.path());
                continue;
            }
            if !kind.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let file = entry.path();
            if name.ends_with(".jsonl") {
                let parts: Vec<_> = file
                    .strip_prefix(&sidechain_directory)?
                    .components()
                    .collect();
                if parts.len() >= 3 && parts[0].as_os_str() == "workflows" {
                    workflow_files
                        .entry(parts[1].as_os_str().to_string_lossy().into_owned())
                        .or_default()
                        .push(file);
                } else {
                    sidechain_files.push(file);
                }
            } else if let Some(agent_id) = name
                .strip_prefix("agent-")
                .and_then(|name| name.strip_suffix(".meta.json"))
                .filter(|id| !id.is_empty())
            {
                if let Some(value) = optional_json(&file).await.and_then(|value| {
                    filtered_fields(
                        &value,
                        &["agentType", "description", "toolUseId"],
                        &["spawnDepth"],
                    )
                }) {
                    if value.as_object().is_some_and(|object| !object.is_empty()) {
                        meta.insert(agent_id.to_owned(), value);
                    }
                }
            }
        }
    }
    let mut history = ReplayHistory::default();
    let mut indices: HashMap<String, usize> = HashMap::new();
    for file in sidechain_files {
        visit_records(&file, |entry| {
            if entry.get("isSidechain") != Some(&Value::Bool(true)) {
                if file == path {
                    history.parent_entries.push(entry);
                }
                return;
            }
            let Some(agent_id) = entry.get("agentId").and_then(Value::as_str) else {
                return;
            };
            let index = *indices.entry(agent_id.to_owned()).or_insert_with(|| {
                let index = history.subagents.len();
                history.subagents.push(ReplaySubagent {
                    agent_id: agent_id.to_owned(),
                    meta: meta.remove(agent_id),
                    entries: Vec::new(),
                });
                index
            });
            history.subagents[index].entries.push(entry);
        })
        .await?;
    }
    for (run_id, files) in workflow_files {
        let mut entries = Vec::new();
        for file in files {
            visit_records(&file, |entry| {
                if entry.get("type").and_then(Value::as_str) != Some("user")
                    || has_tool_result(&entry)
                {
                    entries.push(entry);
                }
            })
            .await?;
        }
        entries.sort_by_key(|entry| (timestamp(entry).is_none(), timestamp(entry)));
        history.workflow_entries_by_run_id.insert(run_id, entries);
    }
    let workflow_directory = session_directory.join("workflows");
    let entries = match directory_entries(&workflow_directory).await {
        Ok(entries) => entries,
        Err(error) if missing(&error) => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        if !entry.file_type().await?.is_file()
            || entry.path().extension().and_then(|s| s.to_str()) != Some("json")
        {
            continue;
        }
        if let Some(raw) = optional_json(&entry.path()).await {
            if raw.get("runId").and_then(Value::as_str).is_none() {
                continue;
            }
            if let Some(mut workflow) = filtered_fields(
                &raw,
                &[
                    "runId",
                    "timestamp",
                    "summary",
                    "workflowName",
                    "status",
                    "defaultModel",
                ],
                &["startTime", "totalTokens"],
            ) {
                if let Some(result) = raw.get("result") {
                    workflow["result"] = result.clone();
                }
                history.workflows.push(workflow);
            }
        }
    }
    Ok(history)
}

fn timestamp(value: &Value) -> Option<i64> {
    let value = value.get("timestamp")?;
    if let Some(number) = value.as_f64() {
        let millis = if number > 1_000_000_000_000.0 {
            number
        } else {
            number * 1000.0
        };
        return (millis.is_finite() && millis.abs() <= 8_640_000_000_000_000.0)
            .then_some(millis as i64);
    }
    let text = value.as_str()?.trim();
    DateTime::parse_from_rfc3339(text)
        .or_else(|_| DateTime::parse_from_rfc2822(text))
        .ok()
        .map(|time| time.timestamp_millis())
}

fn has_tool_result(entry: &Value) -> bool {
    entry
        .pointer("/message/content")
        .and_then(Value::as_array)
        .is_some_and(|blocks| {
            blocks
                .iter()
                .any(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
        })
}

fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(value)) => *value,
        Some(Value::String(value)) => !value.is_empty(),
        Some(Value::Number(value)) => value.as_f64() != Some(0.0),
        _ => true,
    }
}

fn trimmed(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn tag<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let start = text.find(&format!("<{name}>"))? + name.len() + 2;
    let end = text[start..].find(&format!("</{name}>"))? + start;
    Some(&text[start..end])
}

fn noise(text: &str) -> bool {
    text == "No response requested."
        || (text.starts_with("[Request interrupted by user")
            && text.ends_with(']')
            && !text[..text.len() - 1].contains(']'))
        || (text.starts_with("<local-command-stdout>") && text.ends_with("</local-command-stdout>"))
}

fn user_text(entry: &Value) -> Option<String> {
    let message = entry.get("message")?;
    let content = message.get("content");
    let text = if let Some(text) = content.and_then(Value::as_str) {
        Some(text)
    } else if let Some(text) = message.get("text").and_then(Value::as_str) {
        Some(text)
    } else {
        content.and_then(Value::as_array).and_then(|blocks| {
            blocks
                .iter()
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .find(|text| !text.trim().is_empty() && !noise(text.trim()))
        })
    }?;
    let text = text.trim();
    if text.is_empty() || noise(text) {
        return None;
    }
    if let Some(command_message) = tag(text, "command-message") {
        let command = tag(text, "command-name")
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(command_message.trim());
        if command.is_empty() {
            return None;
        }
        let mut command = if command.starts_with('/') {
            command.to_owned()
        } else {
            format!("/{command}")
        };
        if let Some(args) = tag(text, "command-args")
            .map(str::trim)
            .filter(|args| !args.is_empty())
        {
            command.push(' ');
            command.push_str(args);
        }
        Some(command)
    } else {
        Some(text.to_owned())
    }
}

fn preview(text: &str) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_utf16(&normalized, 160)
}

fn truncate_utf16(text: &str, limit: usize) -> String {
    let mut length = 0;
    let end = text
        .char_indices()
        .find_map(|(index, c)| {
            length += c.len_utf16();
            (length > limit).then_some(index)
        })
        .unwrap_or(text.len());
    text[..end].to_owned()
}

async fn descriptor(path: &Path, modified: SystemTime) -> Result<Option<Value>> {
    let mut session_id = None;
    let mut cwd = None;
    let mut custom_title = None;
    let mut ai_title = None;
    let mut first_prompt = None;
    let mut first_preview = None;
    let mut last_preview = None;
    visit_records(path, |entry| {
        if truthy(entry.get("isSidechain")) {
            return;
        }
        let kind = entry.get("type").and_then(Value::as_str);
        if kind == Some("user")
            && (entry.get("isSynthetic") == Some(&Value::Bool(true))
                || entry.get("isMeta") == Some(&Value::Bool(true))
                || truthy(entry.get("toolUseResult")))
        {
            return;
        }
        if session_id.as_deref().is_none_or(str::is_empty) {
            session_id = entry
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
        if cwd.as_deref().is_none_or(str::is_empty) {
            cwd = entry.get("cwd").and_then(Value::as_str).map(str::to_owned);
        }
        match kind {
            Some("custom-title") if entry.get("customTitle").is_some_and(Value::is_string) => {
                custom_title = trimmed(entry.get("customTitle")).map(str::to_owned)
            }
            Some("ai-title") if entry.get("aiTitle").is_some_and(Value::is_string) => {
                ai_title = trimmed(entry.get("aiTitle")).map(str::to_owned)
            }
            Some("user") => {
                if let Some(text) = user_text(&entry) {
                    let value = preview(&text);
                    if first_prompt.is_none() {
                        first_prompt = Some(text);
                        first_preview = Some(value.clone());
                    }
                    last_preview = Some(value);
                }
            }
            _ => {}
        }
    })
    .await?;
    let Some(session_id) = session_id.filter(|id| !id.is_empty()) else {
        return Ok(None);
    };
    let Some(cwd) = cwd.filter(|cwd| !cwd.is_empty()) else {
        return Ok(None);
    };
    let title = custom_title
        .or(ai_title)
        .or(first_prompt)
        .unwrap_or_else(|| format!("Claude session {}", truncate_utf16(&session_id, 8)));
    Ok(Some(
        json!({"providerHandleId":session_id,"cwd":cwd,"title":title,"firstPromptPreview":first_preview,"lastPromptPreview":last_preview,"lastActivityAt":DateTime::<Utc>::from(modified).to_rfc3339_opts(SecondsFormat::Millis,true)}),
    ))
}

pub async fn list_importable_sessions(
    env: &BTreeMap<String, String>,
    options: &Value,
) -> Result<Vec<Value>> {
    tokio::time::timeout(
        std::time::Duration::from_secs(45),
        list_importable_inner(env, options),
    )
    .await
    .map_err(|_| anyhow!("Claude session discovery timed out after 45 seconds"))?
}

async fn list_importable_inner(
    env: &BTreeMap<String, String>,
    options: &Value,
) -> Result<Vec<Value>> {
    let cwd = options
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|cwd| !cwd.is_empty());
    let root = match cwd {
        Some(cwd) => project_dir(cwd, env).await?,
        None => config_dir(env)?.join("projects"),
    };
    let root_entries = match directory_entries(&root).await {
        Ok(entries) => entries,
        Err(error) if missing(&error) => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let limit = options
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(20)
        .min(usize::MAX as u64) as usize;
    let scan_limit = options
        .get("scanLimit")
        .and_then(Value::as_u64)
        .unwrap_or((limit as u64).saturating_mul(3))
        .min(500) as usize;
    let mut files = Vec::new();
    let mut denied = Vec::new();
    if cwd.is_some() {
        files.extend(
            root_entries
                .into_iter()
                .map(|entry| entry.path())
                .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")),
        );
    } else {
        for entry in root_entries {
            let path = entry.path();
            match fs::metadata(&path).await {
                Ok(metadata) if metadata.is_dir() => {}
                Ok(_) => continue,
                Err(error) if expected_import_error(&error) => {
                    if !missing(&error) {
                        denied.push(error);
                    }
                    continue;
                }
                Err(error) => return Err(error.into()),
            }
            match directory_entries(&path).await {
                Ok(entries) => files.extend(
                    entries
                        .into_iter()
                        .map(|entry| entry.path())
                        .filter(|path| {
                            path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
                        }),
                ),
                Err(error) if expected_import_error(&error) => {
                    if !missing(&error) {
                        denied.push(error);
                    }
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
    let mut candidates = Vec::new();
    for file in files {
        match fs::metadata(&file).await {
            Ok(metadata) if metadata.is_file() => candidates.push((file, metadata.modified()?)),
            Ok(_) => {}
            Err(error) if expected_import_error(&error) => {
                if !missing(&error) {
                    denied.push(error);
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    if candidates.is_empty() && !denied.is_empty() {
        bail!(
            "Cannot discover Claude sessions: access was denied to {} paths and no transcripts could be discovered. {}",
            denied.len(),
            denied[0]
        );
    }
    candidates.sort_by(|a, b| b.1.cmp(&a.1));
    candidates.truncate(scan_limit);
    let count = candidates.len();
    let mut unreadable = Vec::new();
    let mut result = Vec::new();
    for (path, modified) in candidates {
        match descriptor(&path, modified).await {
            Ok(Some(value)) => result.push(value),
            Ok(None) => {}
            Err(error)
                if error
                    .downcast_ref::<io::Error>()
                    .is_some_and(expected_import_error) =>
            {
                unreadable.push(error)
            }
            Err(error) => return Err(error),
        }
    }
    if !unreadable.is_empty() && unreadable.len() == count {
        bail!(
            "Cannot list Claude sessions: all {count} candidate transcripts have unreadable import metadata. {}",
            unreadable[0]
        );
    }
    result.truncate(limit);
    Ok(result)
}

pub async fn renamed_title(
    cwd: &str,
    session_id: &str,
    env: &BTreeMap<String, String>,
) -> Result<Option<String>> {
    let path = transcript_path(cwd, session_id, env).await?;
    let mut title = None;
    let result = visit_records(&path, |entry| {
        if !truthy(entry.get("isSidechain"))
            && entry.get("type").and_then(Value::as_str) == Some("custom-title")
        {
            if let Some(value) = trimmed(entry.get("customTitle")) {
                title = Some(value.to_owned());
            }
        }
    })
    .await;
    match result {
        Err(error)
            if error
                .downcast_ref::<io::Error>()
                .is_some_and(|error| error.kind() == io::ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
        Ok(()) => Ok(title),
    }
}

fn valid_sdk_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn sdk_fork_title(entries: &[Value]) -> String {
    let mut custom = None;
    let mut ai = None;
    for entry in entries {
        if let Some(title) = entry
            .get("customTitle")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            custom = Some(title);
        }
        if let Some(title) = entry
            .get("aiTitle")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            ai = Some(title);
        }
    }
    if let Some(title) = custom.or(ai) {
        return format!("{title} (fork)");
    }
    let mut command = String::new();
    for entry in entries {
        if entry.get("type").and_then(Value::as_str) != Some("user")
            || entry.get("isMeta") == Some(&Value::Bool(true))
            || entry.get("isCompactSummary") == Some(&Value::Bool(true))
            || has_tool_result(entry)
        {
            continue;
        }
        let content = entry.pointer("/message/content");
        let parts: Vec<&str> = match content {
            Some(Value::String(text)) => vec![text],
            Some(Value::Array(blocks)) => blocks
                .iter()
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect(),
            _ => continue,
        };
        for part in parts {
            let text = part.replace('\n', " ");
            let text = text.trim();
            if text.is_empty() {
                continue;
            }
            if let Some(name) = tag(text, "command-name") {
                if command.is_empty() {
                    command = name.to_owned();
                }
                continue;
            }
            if let Some(input) = tag(text, "bash-input") {
                return format!("! {} (fork)", input.trim());
            }
            // SDK's aee excludes markup/system scaffolding and interruption messages.
            let bytes = text.as_bytes();
            let markup = bytes.first() == Some(&b'<')
                && bytes.get(1).is_some_and(u8::is_ascii_lowercase)
                && {
                    let end = bytes[2..]
                        .iter()
                        .position(|byte| {
                            !byte.is_ascii_alphanumeric() && *byte != b'_' && *byte != b'-'
                        })
                        .map(|index| index + 2);
                    end.is_some_and(|index| {
                        bytes[index] == b'>' || bytes[index].is_ascii_whitespace()
                    })
                };
            if markup || (text.starts_with("[Request interrupted by user") && text.contains(']')) {
                continue;
            }
            let title = if text.encode_utf16().count() > 200 {
                format!("{}…", truncate_utf16(text, 200).trim())
            } else {
                text.to_owned()
            };
            return format!("{title} (fork)");
        }
    }
    format!(
        "{} (fork)",
        if command.is_empty() {
            "Forked session"
        } else {
            &command
        }
    )
}

/// Port of SDK 0.3.246 J8e -> $4 -> Vze/F4 -> z4 for Paseo's project-folder
/// SessionStore. This is deliberately not the SDK's default filesystem fork:
/// the configured store only has load/append, so it does not copy sidecars.
pub async fn fork_session(
    transcript_path: &Path,
    session_id: &str,
    up_to_message_id: Option<&str>,
) -> Result<String> {
    if !valid_sdk_uuid(session_id) {
        bail!("Invalid sessionId: {session_id}");
    }
    let up_to_message_id = up_to_message_id.filter(|id| !id.is_empty());
    if let Some(id) = up_to_message_id {
        if !valid_sdk_uuid(id) {
            bail!("Invalid upToMessageId: {id}");
        }
    }
    let directory = transcript_path
        .parent()
        .ok_or_else(|| anyhow!("Claude transcript has no project directory"))?;
    let source = directory.join(format!("{session_id}.jsonl"));
    let mut entries = Vec::new();
    if let Err(error) = visit_records(&source, |entry| {
        if entry.get("type").is_some_and(Value::is_string) {
            entries.push(entry);
        }
    })
    .await
    {
        if error
            .downcast_ref::<io::Error>()
            .is_some_and(|error| error.kind() == io::ErrorKind::NotFound)
        {
            bail!("Session {session_id} not found");
        }
        return Err(error);
    }
    if entries.is_empty() {
        bail!("Session {session_id} not found");
    }
    let title = sdk_fork_title(&entries);
    let mut transcript = Vec::new();
    let mut replacements = Vec::new();
    let mut suppressed = false;
    let mut atis = None;
    let mut relocated = None;
    for entry in entries {
        match entry.get("type").and_then(Value::as_str) {
            Some("user" | "assistant" | "attachment" | "system" | "progress")
                if entry.get("uuid").is_some_and(Value::is_string) =>
            {
                if !truthy(entry.get("isSidechain")) {
                    transcript.push(entry);
                }
            }
            Some("history-suppression") => suppressed = true,
            Some("atis-latch")
                if entry.get("sessionId").and_then(Value::as_str) == Some(session_id) =>
            {
                if let Some(value) = entry
                    .get("atis")
                    .and_then(Value::as_str)
                    .filter(|value| value.bytes().all(|b| (0x21..=0x7e).contains(&b)))
                {
                    atis = Some(value.to_owned());
                }
            }
            Some("content-replacement")
                if entry.get("sessionId").and_then(Value::as_str) == Some(session_id) =>
            {
                if let Some(values) = entry.get("replacements").and_then(Value::as_array) {
                    replacements.extend(values.iter().cloned());
                }
            }
            Some("relocated")
                if entry.get("sessionId").and_then(Value::as_str) == Some(session_id) =>
            {
                if let Some(value) = entry
                    .get("relocatedCwd")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                {
                    relocated = Some(value.to_owned());
                }
            }
            _ => {}
        }
    }
    if transcript.is_empty() {
        bail!("Session {session_id} has no messages to fork");
    }
    if let Some(id) = up_to_message_id {
        let index = transcript
            .iter()
            .position(|entry| entry.get("uuid").and_then(Value::as_str) == Some(id))
            .ok_or_else(|| anyhow!("Message {id} not found in session {session_id}"))?;
        transcript.truncate(index + 1);
    }
    let mut ids = HashMap::new();
    let mut positions = HashMap::new();
    for (index, entry) in transcript.iter().enumerate() {
        let id = entry["uuid"].as_str().expect("filtered UUID");
        ids.insert(id.to_owned(), Uuid::new_v4().to_string());
        positions.insert(id.to_owned(), index);
    }
    let last = transcript
        .iter()
        .rposition(|entry| entry.get("type").and_then(Value::as_str) != Some("progress"))
        .ok_or_else(|| anyhow!("Session {session_id} has no messages to fork"))?;
    let fork_id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let mut output = Vec::new();
    if suppressed {
        output.push(json!({"type":"history-suppression","sessionId":fork_id,"cause":"fork_inherit","ts":now}));
    }
    for (index, entry) in transcript.iter().enumerate() {
        if entry.get("type").and_then(Value::as_str) == Some("progress") {
            continue;
        }
        let original_id = entry["uuid"].as_str().expect("filtered UUID");
        let mut parent = entry.get("parentUuid").and_then(Value::as_str);
        let mut mapped_parent = None;
        let mut visited = HashSet::new();
        while let Some(id) = parent.filter(|id| !id.is_empty()) {
            // Malformed cycles must not hang the daemon while replaying untrusted disk data.
            if !visited.insert(id) {
                bail!("Claude transcript contains a progress-parent cycle");
            }
            let Some(position) = positions.get(id) else {
                break;
            };
            let ancestor = &transcript[*position];
            if ancestor.get("type").and_then(Value::as_str) != Some("progress") {
                mapped_parent = ids.get(id);
                break;
            }
            parent = ancestor.get("parentUuid").and_then(Value::as_str);
        }
        let mut value = entry.clone();
        value["uuid"] = json!(ids[original_id]);
        value["parentUuid"] = json!(mapped_parent);
        if let Some(logical) = entry.get("logicalParentUuid") {
            value["logicalParentUuid"] = if logical.is_null() {
                Value::Null
            } else {
                json!(logical.as_str().and_then(|id| ids.get(id)))
            };
        }
        value["sessionId"] = json!(fork_id);
        value["isSidechain"] = Value::Bool(false);
        value["forkedFrom"] = json!({"sessionId":session_id,"messageUuid":original_id});
        if index == last {
            value["timestamp"] = json!(now);
        }
        if entry.get("type").and_then(Value::as_str) == Some("system")
            && entry.get("subtype").and_then(Value::as_str) == Some("model_refusal_fallback")
        {
            value["neutralizedByFork"] = Value::Bool(true);
        }
        let object = value.as_object_mut().expect("transcript object");
        for field in [
            "teamName",
            "agentName",
            "sessionKind",
            "slug",
            "sourceToolAssistantUUID",
        ] {
            object.remove(field);
        }
        output.push(value);
    }
    if !replacements.is_empty() {
        output.push(json!({"type":"content-replacement","sessionId":fork_id,"replacements":replacements,"uuid":Uuid::new_v4().to_string(),"timestamp":now}));
    }
    if let Some(atis) = atis {
        output.push(json!({"type":"atis-latch","sessionId":fork_id,"atis":atis}));
    }
    if let Some(cwd) = relocated {
        output.push(json!({"type":"relocated","sessionId":fork_id,"relocatedCwd":cwd}));
    }
    output.push(json!({"type":"custom-title","sessionId":fork_id,"customTitle":title,"uuid":Uuid::new_v4().to_string(),"timestamp":now}));
    let path = directory.join(format!("{fork_id}.jsonl"));
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .await?;
    let mut line = Vec::new();
    for entry in output {
        serde_json::to_writer(&mut line, &entry)?;
        line.push(b'\n');
        file.write_all(&line).await?;
        line.clear();
    }
    file.flush().await?;
    Ok(fork_id)
}
