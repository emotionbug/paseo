//! The source's `ProjectConfigSession` with `paseo-config-file.ts` and
//! `worktree-setup-commit-status.ts`: a client's read/write surface for a known
//! project's `paseo.json`, guarded by an `{mtimeMs, size}` revision.

use super::*;
use crate::git::command;
use crate::terminals::scripts::config::valid_allocation;
use serde_json::Map;
use std::time::UNIX_EPOCH;

const FILE_NAME: &str = "paseo.json";

impl ProjectService {
    pub(super) async fn handle_project_config(
        &self,
        ctx: &RequestContext,
        m: &Value,
    ) -> Result<bool> {
        let (response, write) = match m["type"].as_str() {
            Some("read_project_config_request") => ("read_project_config_response", false),
            Some("write_project_config_request") => ("write_project_config_response", true),
            _ => return Ok(false),
        };
        let requested = m["repoRoot"].as_str().unwrap_or_default();
        let outcome = match self.known_project_root(requested).await? {
            None => Err((json!({"code":"project_not_found"}), requested.to_owned())),
            Some(root) => {
                let result = if write {
                    write_for_edit(&root, &m["config"], &m["expectedRevision"]).await
                } else {
                    read_for_edit(&root).await
                };
                match result {
                    Ok((config, revision)) => Ok((root, config, revision)),
                    Err(error) => Err((error, root.display().to_string())),
                }
            }
        };
        let payload = match outcome {
            Ok((root, config, revision)) => {
                let mut payload = json!({"requestId":m["requestId"],"repoRoot":root,"ok":true,"config":config,"revision":revision});
                if let Ok(changed) = uncommitted_setup_changes(&root, &config).await {
                    payload["hasUncommittedWorktreeSetupChanges"] = json!(changed);
                }
                payload
            }
            Err((error, root)) => {
                json!({"requestId":m["requestId"],"repoRoot":root,"ok":false,"error":error})
            }
        };
        ctx.send(json!({"type":response,"payload":payload})).await?;
        Ok(true)
    }

    /// `resolveKnownProjectRoot`: the canonical root of a non-archived project
    /// equal to the canonical requested root (trailing separators and symlinks
    /// are accepted).
    async fn known_project_root(&self, requested: &str) -> Result<Option<PathBuf>> {
        let requested = canonical_root(requested).await;
        for project in self.list_projects().await? {
            if !project["archivedAt"].is_null() {
                continue;
            }
            let Some(root) = project["rootPath"].as_str() else {
                continue;
            };
            let root = canonical_root(root).await;
            if root == requested {
                return Ok(Some(root));
            }
        }
        Ok(None)
    }
}

/// `canonicalizeConfigRoot`: resolve, realpath when possible, then strip
/// trailing separators.
async fn canonical_root(root: &str) -> PathBuf {
    let resolved = std::path::absolute(root).unwrap_or_else(|_| PathBuf::from(root));
    let real = tokio::fs::canonicalize(&resolved).await.unwrap_or(resolved);
    let mut text = real.to_string_lossy().into_owned();
    while text.len() > 1 && text.ends_with(std::path::MAIN_SEPARATOR) {
        text.pop();
    }
    PathBuf::from(text)
}

/// `statPaseoConfigPath`: `null` when the file is absent.
async fn revision(root: &Path) -> std::io::Result<Value> {
    match tokio::fs::metadata(root.join(FILE_NAME)).await {
        Ok(metadata) => {
            let modified = metadata.modified()?.duration_since(UNIX_EPOCH).unwrap_or_default();
            let mtime_ms =
                modified.as_secs() as f64 * 1e3 + f64::from(modified.subsec_nanos()) / 1e6;
            Ok(json!({"mtimeMs":mtime_ms,"size":metadata.len()}))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Value::Null),
        Err(error) => Err(error),
    }
}

/// `readPaseoConfigForEdit`: a missing (or JSON `null`) file is `{config: null,
/// revision: null}`; unreadable or schema-invalid content is
/// `invalid_project_config`.
async fn read_for_edit(root: &Path) -> std::result::Result<(Value, Value), Value> {
    let invalid = || json!({"code":"invalid_project_config"});
    let bytes = match tokio::fs::read(root.join(FILE_NAME)).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((Value::Null, Value::Null));
        }
        Err(_) => return Err(invalid()),
    };
    let json: Value = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    if json.is_null() {
        return Ok((Value::Null, Value::Null));
    }
    let config = parse_raw(&json).ok_or_else(invalid)?;
    let revision = revision(root).await.map_err(|_| invalid())?;
    Ok((config, revision))
}

/// `writePaseoConfigForEdit`: validate, stage a temp file beside the target,
/// compare revisions, then rename into place.
async fn write_for_edit(
    root: &Path,
    config: &Value,
    expected: &Value,
) -> std::result::Result<(Value, Value), Value> {
    let Some(parsed) = parse_raw(config) else {
        return Err(json!({"code":"invalid_project_config"}));
    };
    let expected = if expected.is_null() {
        Value::Null
    } else {
        match (expected["mtimeMs"].as_f64(), expected["size"].as_f64()) {
            (Some(_), Some(_)) => expected.clone(),
            _ => return Err(json!({"code":"invalid_project_config"})),
        }
    };
    let failed = || json!({"code":"write_failed"});
    let temp = root.join(format!(
        ".{FILE_NAME}.{}.{}.tmp",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let content = format!(
        "{}\n",
        serde_json::to_string_pretty(&parsed).map_err(|_| failed())?
    );
    let staged = async {
        tokio::fs::write(&temp, content).await?;
        let current = revision(root).await?;
        if !same_revision(&current, &expected) {
            return Ok(Err(
                json!({"code":"stale_project_config","currentRevision":current}),
            ));
        }
        tokio::fs::rename(&temp, root.join(FILE_NAME)).await?;
        std::io::Result::Ok(Ok(()))
    }
    .await;
    match staged {
        Ok(Ok(())) => {}
        Ok(Err(stale)) => {
            let _ = tokio::fs::remove_file(&temp).await;
            return Err(stale);
        }
        Err(_) => {
            let _ = tokio::fs::remove_file(&temp).await;
            return Err(failed());
        }
    }
    match revision(root).await {
        Ok(revision) if !revision.is_null() => Ok((parsed, revision)),
        _ => Err(failed()),
    }
}

/// `paseoConfigRevisionsEqual`.
fn same_revision(left: &Value, right: &Value) -> bool {
    if left.is_null() || right.is_null() {
        return left.is_null() && right.is_null();
    }
    left["mtimeMs"].as_f64() == right["mtimeMs"].as_f64()
        && left["size"].as_f64() == right["size"].as_f64()
}

/// `hasUncommittedWorktreeSetupChanges`: the normalized `worktree.setup`
/// commands differ from those committed at `HEAD`.
async fn uncommitted_setup_changes(root: &Path, current: &Value) -> Result<bool> {
    let env = BTreeMap::new();
    let cancel = tokio_util::sync::CancellationToken::new();
    let prefix = command::text(root, &["rev-parse", "--show-prefix"], &env, cancel.clone()).await?;
    let path = format!("{prefix}{FILE_NAME}");
    command::text(root, &["rev-parse", "--verify", "HEAD"], &env, cancel.clone()).await?;
    let tracked = command::text(
        root,
        &["ls-tree", "--name-only", "HEAD", "--", &path],
        &env,
        cancel.clone(),
    )
    .await?;
    let committed = if tracked.is_empty() {
        Value::Null
    } else {
        let shown = command::text(root, &["show", &format!("HEAD:{path}")], &env, cancel).await?;
        let json: Value = serde_json::from_str(&shown)?;
        parse_raw(&json).context("Committed paseo.json is invalid")?
    };
    Ok(lifecycle_commands(&current["worktree"]["setup"])
        != lifecycle_commands(&committed["worktree"]["setup"]))
}

/// `normalizeLifecycleCommands`.
fn lifecycle_commands(value: &Value) -> Vec<&str> {
    match value {
        Value::String(command) if !command.trim().is_empty() => vec![command.as_str()],
        Value::Array(commands) => commands
            .iter()
            .filter_map(Value::as_str)
            .filter(|command| !command.trim().is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

/// `PaseoConfigRawSchema.parse`: `None` when invalid, otherwise the parsed
/// output (known keys first, unknown keys passed through, `servicePorts`
/// strings trimmed, invalid `metadataGeneration` parts caught as `{}`).
fn parse_raw(value: &Value) -> Option<Value> {
    let object = value.as_object()?;
    let mut output = Map::new();
    if let Some(worktree) = object.get("worktree") {
        output.insert("worktree".into(), parse_worktree(worktree)?);
    }
    if let Some(scripts) = object.get("scripts") {
        let scripts = scripts.as_object()?;
        let mut parsed = Map::new();
        for (name, entry) in scripts {
            let entry = entry.as_object()?;
            parsed.insert(
                name.clone(),
                Value::Object(ordered(entry, &["type", "command", "port"])),
            );
        }
        output.insert("scripts".into(), Value::Object(parsed));
    }
    if let Some(metadata) = object.get("metadataGeneration") {
        output.insert("metadataGeneration".into(), parse_metadata(metadata));
    }
    passthrough(object, &mut output, &["worktree", "scripts", "metadataGeneration"]);
    Some(Value::Object(output))
}

fn parse_worktree(value: &Value) -> Option<Value> {
    let object = value.as_object()?;
    let lifecycle = |value: &Value| match value {
        Value::String(_) => true,
        Value::Array(items) => items.iter().all(Value::is_string),
        _ => false,
    };
    let mut output = Map::new();
    for key in ["setup", "teardown"] {
        if let Some(value) = object.get(key) {
            if !lifecycle(value) {
                return None;
            }
            output.insert(key.into(), value.clone());
        }
    }
    if let Some(terminals) = object.get("terminals") {
        output.insert("terminals".into(), terminals.clone());
    }
    if let Some(allocation) = object.get("servicePorts") {
        if !valid_allocation(allocation) {
            return None;
        }
        let allocation = allocation.as_object()?;
        let mut parsed = Map::new();
        for key in ["range", "portScript"] {
            if let Some(text) = allocation.get(key).and_then(Value::as_str) {
                parsed.insert(key.into(), text.trim().into());
            }
        }
        output.insert("servicePorts".into(), Value::Object(parsed));
    }
    passthrough(
        object,
        &mut output,
        &["setup", "teardown", "terminals", "servicePorts"],
    );
    Some(Value::Object(output))
}

/// `PaseoMetadataGenerationSchema` and its entries both `.catch({})`.
fn parse_metadata(value: &Value) -> Value {
    let Some(object) = value.as_object() else {
        return json!({});
    };
    let mut output = Map::new();
    let keys = ["title", "branchName", "commitMessage", "pullRequest"];
    for key in keys {
        if let Some(entry) = object.get(key) {
            let parsed = entry
                .as_object()
                .filter(|entry| entry.get("instructions").is_none_or(Value::is_string))
                .map(|entry| Value::Object(ordered(entry, &["instructions"])))
                .unwrap_or_else(|| json!({}));
            output.insert(key.into(), parsed);
        }
    }
    passthrough(object, &mut output, &keys);
    Value::Object(output)
}

/// Known keys in schema order, then the remaining keys in authored order.
fn ordered(object: &Map<String, Value>, known: &[&str]) -> Map<String, Value> {
    let mut output = Map::new();
    for key in known {
        if let Some(value) = object.get(*key) {
            output.insert((*key).into(), value.clone());
        }
    }
    passthrough(object, &mut output, known);
    output
}

fn passthrough(object: &Map<String, Value>, output: &mut Map<String, Value>, known: &[&str]) {
    for (key, value) in object {
        if !known.contains(&key.as_str()) && key != "__proto__" {
            output.insert(key.clone(), value.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_schema_normalizes_like_the_source_parse() {
        let parsed = parse_raw(&json!({
            "extra": 1,
            "metadataGeneration": {"title": {"instructions": 5}, "branchName": {"instructions": "x", "more": true}},
            "worktree": {"servicePorts": {"range": " 3000-3010 "}, "setup": "npm i", "custom": [1]},
            "scripts": {"web": {"port": 3000, "command": "npm run dev", "type": "service"}}
        }))
        .unwrap();
        assert_eq!(
            serde_json::to_string(&parsed).unwrap(),
            r#"{"worktree":{"setup":"npm i","servicePorts":{"range":"3000-3010"},"custom":[1]},"scripts":{"web":{"type":"service","command":"npm run dev","port":3000}},"metadataGeneration":{"title":{},"branchName":{"instructions":"x","more":true}},"extra":1}"#
        );
        assert_eq!(
            parse_raw(&json!({"metadataGeneration": 3})).unwrap(),
            json!({"metadataGeneration": {}})
        );
        for invalid in [
            json!([]),
            json!({"worktree": {"setup": 1}}),
            json!({"worktree": {"setup": ["a", 2]}}),
            json!({"worktree": {"servicePorts": {"range": "9-1"}}}),
            json!({"worktree": {"servicePorts": {}}}),
            json!({"scripts": {"web": "npm start"}}),
        ] {
            assert!(parse_raw(&invalid).is_none(), "{invalid}");
        }
    }

    #[tokio::test]
    async fn writes_are_revision_guarded_and_round_trip() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        assert_eq!(read_for_edit(root).await.unwrap(), (Value::Null, Value::Null));
        let config = json!({"worktree": {"setup": ["npm ci"]}});
        let (written, revision) = write_for_edit(root, &config, &Value::Null).await.unwrap();
        assert_eq!(written, config);
        assert_eq!(read_for_edit(root).await.unwrap(), (config.clone(), revision.clone()));
        assert_eq!(
            tokio::fs::read_to_string(root.join(FILE_NAME)).await.unwrap(),
            "{\n  \"worktree\": {\n    \"setup\": [\n      \"npm ci\"\n    ]\n  }\n}\n"
        );
        let stale = write_for_edit(root, &config, &Value::Null).await.unwrap_err();
        assert_eq!(stale["code"], "stale_project_config");
        assert_eq!(stale["currentRevision"], revision);
        assert_eq!(
            write_for_edit(root, &json!({"worktree": 1}), &revision)
                .await
                .unwrap_err(),
            json!({"code":"invalid_project_config"})
        );
        let mut entries = tokio::fs::read_dir(root).await.unwrap();
        let mut names = Vec::new();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
        assert_eq!(names, [FILE_NAME]);
        tokio::fs::write(root.join(FILE_NAME), "{").await.unwrap();
        assert_eq!(
            read_for_edit(root).await.unwrap_err(),
            json!({"code":"invalid_project_config"})
        );
    }

    #[test]
    fn lifecycle_commands_drop_blank_and_non_string_entries() {
        assert_eq!(lifecycle_commands(&json!(" ")), Vec::<&str>::new());
        assert_eq!(lifecycle_commands(&json!(["a", " ", 3, "b"])), ["a", "b"]);
        assert_eq!(lifecycle_commands(&Value::Null), Vec::<&str>::new());
    }
}
