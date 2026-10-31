use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, Weak};
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, OnceCell, OwnedMutexGuard, RwLock};

/// Keyed locks are weakly retained so completed request identities do not accumulate.
#[derive(Default)]
pub(super) struct KeyLocks(StdMutex<HashMap<String, Weak<Mutex<()>>>>);
impl KeyLocks {
    pub(super) async fn lock(&self, key: &str) -> OwnedMutexGuard<()> {
        let lock = {
            let mut locks = self.0.lock().unwrap_or_else(|e| e.into_inner());
            locks.retain(|_, lock| lock.strong_count() > 0);
            match locks.get(key).and_then(Weak::upgrade) {
                Some(lock) => lock,
                None => {
                    let lock = Arc::new(Mutex::new(()));
                    locks.insert(key.to_owned(), Arc::downgrade(&lock));
                    lock
                }
            }
        };
        lock.lock_owned().await
    }
}

/// Commit a private file in its own directory, then fsync the containing directory.
/// Callers only publish cache/observer changes after this succeeds.
pub(super) async fn atomic_json(path: &Path, value: &Value) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    atomic_bytes(path, &bytes).await
}
pub(super) async fn atomic_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("Persistent file has no parent")?;
    tokio::fs::create_dir_all(parent).await?;
    let temporary = parent.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let result = async {
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&temporary).await?;
        file.write_all(bytes).await?;
        file.sync_all().await?;
        drop(file);
        tokio::fs::rename(&temporary, path).await?;
        sync_directory(parent).await?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temporary).await;
    }
    result
}
pub(super) async fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    tokio::fs::File::open(path).await?.sync_all().await?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}
pub(super) async fn read_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match tokio::fs::read(path).await {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

#[derive(Default)]
struct Records {
    values: HashMap<String, Value>,
    order: Vec<String>,
    current_paths: HashMap<String, PathBuf>,
    paths: HashMap<String, HashSet<PathBuf>>,
    deleting: HashSet<String>,
    execution_owners: HashMap<(String, String), String>,
}
impl Records {
    fn insert(&mut self, id: String, value: Value, path: PathBuf) {
        self.execution_owners.retain(|_, owner| owner != &id);
        if let Some(key) = execution_key(&value["owner"]) {
            self.execution_owners.insert(key, id.clone());
        }
        if !self.values.contains_key(&id) {
            self.order.push(id.clone());
        }
        self.values.insert(id.clone(), value);
        self.paths
            .entry(id.clone())
            .or_default()
            .insert(path.clone());
        self.current_paths.insert(id, path);
    }
}

pub struct AgentStorage {
    directory: PathBuf,
    records: OnceCell<RwLock<Records>>,
    mutations: KeyLocks,
}
impl AgentStorage {
    pub fn new(home: PathBuf) -> Self {
        Self {
            directory: home.join("agents"),
            records: OnceCell::new(),
            mutations: KeyLocks::default(),
        }
    }
    async fn records(&self) -> Result<&RwLock<Records>> {
        self.records
            .get_or_try_init(|| async {
                let mut records = Records::default();
                let mut root_files = Vec::new();
                let mut directories = Vec::new();
                let mut entries = match tokio::fs::read_dir(&self.directory).await {
                    Ok(entries) => entries,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        return Ok(RwLock::new(records));
                    }
                    Err(error) => return Err(anyhow!(error)),
                };
                while let Some(entry) = entries.next_entry().await? {
                    let kind = entry.file_type().await?;
                    if kind.is_dir() {
                        directories.push(entry.path());
                    } else if kind.is_file()
                        && entry.path().extension().is_some_and(|e| e == "json")
                    {
                        root_files.push(entry.path());
                    }
                }
                root_files.sort();
                directories.sort();
                for directory in directories {
                    let mut nested = match tokio::fs::read_dir(&directory).await {
                        Ok(entries) => entries,
                        Err(error) => {
                            tracing::warn!(
                                ?error,
                                ?directory,
                                "Skipping unreadable legacy agent directory"
                            );
                            continue;
                        }
                    };
                    let mut paths = Vec::new();
                    while let Some(entry) = nested.next_entry().await? {
                        if entry.file_type().await?.is_file()
                            && entry.path().extension().is_some_and(|e| e == "json")
                        {
                            paths.push(entry.path());
                        }
                    }
                    paths.sort();
                    root_files.extend(paths);
                }
                for path in root_files {
                    let record = async {
                        let mut value: Value =
                            serde_json::from_slice(&tokio::fs::read(&path).await?)?;
                        validate_record(&mut value)?;
                        Ok::<_, anyhow::Error>(value)
                    }
                    .await;
                    match record {
                        Ok(value) => {
                            records.insert(value["id"].as_str().unwrap().to_owned(), value, path)
                        }
                        Err(error) => {
                            tracing::warn!(?error, ?path, "Skipping invalid agent record")
                        }
                    }
                }
                Ok(RwLock::new(records))
            })
            .await
    }
    pub async fn initialize(&self) -> Result<()> {
        self.records().await?;
        Ok(())
    }
    pub async fn list(&self) -> Result<Vec<Value>> {
        let records = self.records().await?.read().await;
        Ok(records
            .order
            .iter()
            .filter_map(|id| records.values.get(id).cloned())
            .collect())
    }
    pub async fn get(&self, id: &str) -> Result<Option<Value>> {
        Ok(self.records().await?.read().await.values.get(id).cloned())
    }
    pub async fn list_by_provider_session(
        &self,
        provider: &str,
        handle: &str,
    ) -> Result<Vec<Value>> {
        Ok(self
            .list()
            .await?
            .into_iter()
            .filter(|value| {
                let p = &value["persistence"];
                p["provider"].as_str() == Some(provider)
                    && (p["sessionId"].as_str() == Some(handle)
                        || p["nativeHandle"].as_str() == Some(handle))
            })
            .collect())
    }
    pub async fn find_by_daemon_execution(&self, owner: &Value) -> Result<Option<Value>> {
        let records = self.records().await?.read().await;
        Ok(execution_key(owner)
            .and_then(|key| records.execution_owners.get(&key))
            .and_then(|id| records.values.get(id))
            .cloned())
    }
    pub async fn upsert(&self, record: Value) -> Result<()> {
        let id = record["id"]
            .as_str()
            .context("Agent record id must be a string")?
            .to_owned();
        self.mutate(&id, |_| Ok(record)).await
    }
    pub async fn mutate<F>(&self, id: &str, mutation: F) -> Result<()>
    where
        F: FnOnce(Option<&Value>) -> Result<Value> + Send,
    {
        let records = self.records().await?;
        let _guard = self.mutations.lock(id).await;
        let (mut record, previous) = {
            let records = records.read().await;
            if records.deleting.contains(id) {
                return Ok(());
            }
            (
                mutation(records.values.get(id))?,
                records.current_paths.get(id).cloned(),
            )
        };
        validate_record(&mut record)?;
        if record["id"].as_str() != Some(id) {
            bail!("Agent mutation changed its identity");
        }
        let path = self
            .directory
            .join(project_directory(record["cwd"].as_str().unwrap()))
            .join(format!("{id}.json"));
        atomic_json(&path, &record).await?;
        let mut removed_previous = None;
        if let Some(previous) = previous.filter(|previous| previous != &path) {
            match tokio::fs::remove_file(&previous).await {
                Ok(()) => removed_previous = Some(previous),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    removed_previous = Some(previous)
                }
                Err(error) => {
                    tracing::warn!(?error, ?previous, "Failed to clean moved agent record")
                }
            }
        }
        let mut records = records.write().await;
        if let Some(previous) = removed_previous {
            if let Some(paths) = records.paths.get_mut(id) {
                paths.remove(&previous);
            }
        }
        records.insert(id.to_owned(), record, path);
        Ok(())
    }
    pub async fn set_title(&self, id: &str, title: &str) -> Result<()> {
        self.mutate(id, |existing| {
            let mut record = existing
                .cloned()
                .with_context(|| format!("Agent {id} not found"))?;
            record["title"] = Value::String(title.to_owned());
            Ok(record)
        })
        .await
    }
    pub async fn begin_delete(&self, id: &str) -> Result<()> {
        self.records()
            .await?
            .write()
            .await
            .deleting
            .insert(id.to_owned());
        Ok(())
    }
    pub async fn remove(&self, id: &str) -> Result<()> {
        self.begin_delete(id).await?;
        let _guard = self.mutations.lock(id).await;
        let records = self.records().await?;
        let paths = records
            .read()
            .await
            .paths
            .get(id)
            .cloned()
            .unwrap_or_default();
        let mut errors = Vec::new();
        for path in paths {
            match tokio::fs::remove_file(&path).await {
                Ok(()) => {
                    if let Some(parent) = path.parent() {
                        if let Err(error) = sync_directory(parent).await {
                            errors.push(error.to_string());
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(error) => errors.push(format!("{}: {error}", path.display())),
            }
        }
        if !errors.is_empty() {
            bail!(
                "Failed to remove durable records for agent {id}: {}",
                errors.join("; ")
            );
        }
        let mut records = records.write().await;
        records.values.remove(id);
        records.order.retain(|value| value != id);
        records.current_paths.remove(id);
        records.paths.remove(id);
        records.execution_owners.retain(|_, owner| owner != id);
        Ok(())
    }
}
fn execution_key(owner: &Value) -> Option<(String, String)> {
    if owner["kind"].as_str()? != "daemon" {
        return None;
    }
    Some((
        owner["daemonId"].as_str()?.to_owned(),
        owner["executionId"].as_str()?.to_owned(),
    ))
}
fn validate_record(record: &mut Value) -> Result<()> {
    let object = record
        .as_object_mut()
        .context("Agent record must be an object")?;
    for key in ["id", "provider", "cwd", "createdAt", "updatedAt"] {
        if !object.get(key).is_some_and(Value::is_string) {
            bail!("Agent record {key} must be a string");
        }
    }
    let id = object["id"].as_str().unwrap();
    if id.is_empty() || id == "." || id == ".." || id.contains(['/', '\\', '\0']) {
        bail!("Invalid agent record id");
    }
    object.entry("labels").or_insert_with(|| json!({}));
    if !object["labels"]
        .as_object()
        .is_some_and(|labels| labels.values().all(Value::is_string))
    {
        bail!("Agent labels must be strings");
    }
    object
        .entry("lastStatus")
        .or_insert_with(|| json!("closed"));
    if !matches!(
        object["lastStatus"].as_str(),
        Some("initializing" | "idle" | "running" | "error" | "closed")
    ) {
        bail!("Invalid agent status");
    }
    for key in ["workspaceId", "lastActivityAt"] {
        if object.get(key).is_some_and(|v| !v.is_string()) {
            bail!("Agent {key} must be a string");
        }
    }
    for key in [
        "lastUserMessageAt",
        "title",
        "lastModeId",
        "lastError",
        "attentionTimestamp",
        "archivedAt",
    ] {
        if object
            .get(key)
            .is_some_and(|v| !v.is_null() && !v.is_string())
        {
            bail!("Agent {key} must be a string or null");
        }
    }
    for key in ["requiresAttention", "internal"] {
        if object.get(key).is_some_and(|v| !v.is_boolean()) {
            bail!("Agent {key} must be a boolean");
        }
    }
    if let Some(persistence) = object.get("persistence").filter(|v| !v.is_null()) {
        if !persistence.is_object()
            || !persistence["provider"].is_string()
            || !persistence["sessionId"].is_string()
        {
            bail!("Invalid agent persistence handle");
        }
        if persistence.get("metadata").is_some_and(|v| !v.is_object()) {
            bail!("Invalid persistence metadata");
        }
    }
    for key in ["config", "runtimeInfo"] {
        if object
            .get(key)
            .is_some_and(|v| !v.is_object() && !(key == "config" && v.is_null()))
        {
            bail!("Invalid agent {key}");
        }
    }
    if object.get("attentionReason").is_some_and(|v| {
        !v.is_null() && !matches!(v.as_str(), Some("finished" | "error" | "permission"))
    }) {
        bail!("Invalid agent attention reason");
    }
    if let Some(config) = object.get("config").filter(|v| !v.is_null()) {
        for key in ["modeId", "model", "thinkingOptionId", "systemPrompt"] {
            if config
                .get(key)
                .is_some_and(|v| !v.is_null() && !v.is_string())
            {
                bail!("Invalid config {key}");
            }
        }
        for key in ["featureValues", "providerOptions", "mcpServers"] {
            if config
                .get(key)
                .is_some_and(|v| !v.is_null() && !v.is_object())
            {
                bail!("Invalid config {key}");
            }
        }
        if let Some(policy) = config.get("toolPolicy").filter(|v| !v.is_null()) {
            let policy = policy.as_object().context("Invalid tool policy")?;
            if policy.len() != 1 {
                bail!("Invalid tool policy fields");
            }
            let tools = policy
                .get("preapproved")
                .and_then(Value::as_array)
                .context("Invalid preapproved tools")?;
            for tool in tools {
                if !tool.as_object().is_some_and(|o| o.len() == 3)
                    || tool["kind"] != "mcp"
                    || !tool["server"].is_string()
                    || !tool["tool"].is_string()
                {
                    bail!("Invalid preapproved tool");
                }
            }
        }
    }
    if let Some(runtime) = object.get("runtimeInfo") {
        if !runtime["provider"].is_string()
            || !runtime
                .get("sessionId")
                .is_some_and(|v| v.is_null() || v.is_string())
        {
            bail!("Invalid runtime identity");
        }
        for key in ["model", "thinkingOptionId", "modeId"] {
            if runtime
                .get(key)
                .is_some_and(|v| !v.is_null() && !v.is_string())
            {
                bail!("Invalid runtime {key}");
            }
        }
        if runtime.get("extra").is_some_and(|v| !v.is_object()) {
            bail!("Invalid runtime extra");
        }
    }
    if let Some(owner) = object.get("owner") {
        if execution_key(owner).is_none() {
            bail!("Invalid daemon agent owner");
        }
    }
    if let Some(features) = object.get("features") {
        for feature in features
            .as_array()
            .context("Agent features must be an array")?
        {
            if !feature["id"].is_string() || !feature["label"].is_string() {
                bail!("Invalid agent feature");
            }
            for key in ["description", "tooltip", "icon"] {
                if feature.get(key).is_some_and(|v| !v.is_string()) {
                    bail!("Invalid feature {key}");
                }
            }
            match feature["type"].as_str() {
                Some("toggle") if feature["value"].is_boolean() => (),
                Some("select") => {
                    if !feature
                        .get("value")
                        .is_some_and(|v| v.is_null() || v.is_string())
                    {
                        bail!("Invalid select feature value");
                    }
                    if feature
                        .get("desktopTrigger")
                        .is_some_and(|v| !matches!(v.as_str(), Some("icon" | "label")))
                    {
                        bail!("Invalid feature desktop trigger");
                    }
                    for option in feature["options"]
                        .as_array()
                        .context("Invalid feature options")?
                    {
                        if !option["id"].is_string() || !option["label"].is_string() {
                            bail!("Invalid feature option");
                        }
                        if option.get("description").is_some_and(|v| !v.is_string())
                            || option.get("isDefault").is_some_and(|v| !v.is_boolean())
                            || option.get("metadata").is_some_and(|v| !v.is_object())
                        {
                            bail!("Invalid feature option metadata");
                        }
                    }
                }
                _ => bail!("Invalid agent feature type or value"),
            }
        }
    }
    Ok(())
}

/// Match win32.parse on Unix too: drive roots and UNC shares retain their prefix.
fn project_directory(cwd: &str) -> String {
    let is_separator = |c: char| c == '/' || c == '\\';
    let bytes = cwd.as_bytes();
    let root_end = if bytes.len() > 2
        && is_separator(bytes[0] as char)
        && is_separator(bytes[1] as char)
        && !is_separator(bytes[2] as char)
    {
        let mut position = 2;
        while position < bytes.len() && !is_separator(bytes[position] as char) {
            position += 1;
        }
        while position < bytes.len() && is_separator(bytes[position] as char) {
            position += 1;
        }
        let share = position;
        while position < bytes.len() && !is_separator(bytes[position] as char) {
            position += 1;
        }
        if position > share {
            if position < bytes.len() {
                position + 1
            } else {
                position
            }
        } else {
            1
        }
    } else if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        if bytes.len() > 2 && is_separator(bytes[2] as char) {
            3
        } else {
            2
        }
    } else if cwd.starts_with(is_separator) {
        1
    } else {
        0
    };
    let mut root = collapse_separators(&cwd[..root_end], true);
    let leading = root.len() - root.trim_start_matches('-').len();
    root.drain(..leading);
    root.truncate(root.trim_end_matches('-').len());
    let rest = collapse_separators(cwd[root_end..].trim_end_matches(is_separator), false);
    match (root.is_empty(), rest.is_empty()) {
        (true, true) => "root".to_owned(),
        (true, false) => rest,
        (false, true) => root,
        (false, false) => format!("{root}-{rest}"),
    }
}
fn collapse_separators(value: &str, colons: bool) -> String {
    let mut output = String::with_capacity(value.len());
    let mut previous_separator = false;
    for character in value.chars() {
        let separator = character == '/' || character == '\\' || (colons && character == ':');
        if separator {
            if !previous_separator {
                output.push('-');
            }
        } else {
            output.push(character);
        }
        previous_separator = separator;
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn portable_project_directory_names() {
        for (cwd, expected) in [
            ("/", "root"),
            ("/tmp/project/", "tmp-project"),
            ("C:\\work\\p", "C-work-p"),
            ("\\\\server\\share\\project", "server-share-project"),
        ] {
            assert_eq!(project_directory(cwd), expected);
        }
    }
    #[tokio::test]
    async fn legacy_duplicates_are_removed_and_opaque_fields_survive() -> Result<()> {
        let home = tempfile::tempdir()?;
        let original = json!({"id":"agent", "provider":"custom", "cwd":"/project", "createdAt":"2026-01-01", "updatedAt":"2026-01-01", "config":{"providerOptions":{"opaque":null}}, "persistence":{"provider":"custom","sessionId":"session","nativeHandle":{"nested":[null,1]},"metadata":{"opaque":null}}, "future": {"x":null}});
        atomic_json(&home.path().join("agents/agent.json"), &original).await?;
        atomic_json(&home.path().join("agents/legacy/agent.json"), &original).await?;
        let storage = AgentStorage::new(home.path().to_owned());
        storage.set_title("agent", "new").await?;
        let record = storage.get("agent").await?.unwrap();
        assert_eq!(record["persistence"], original["persistence"]);
        assert_eq!(record["future"], original["future"]);
        storage.remove("agent").await?;
        assert!(
            AgentStorage::new(home.path().to_owned())
                .list()
                .await?
                .is_empty()
        );
        Ok(())
    }
    #[tokio::test]
    async fn failed_record_write_does_not_publish_and_delete_fences_late_writes() -> Result<()> {
        let home = tempfile::tempdir()?;
        let storage = AgentStorage::new(home.path().to_owned());
        let original = json!({"id":"agent","provider":"custom","cwd":"/project","createdAt":"old","updatedAt":"now","title":"old"});
        storage.upsert(original.clone()).await?;
        let before = storage.get("agent").await?.unwrap();
        tokio::fs::write(home.path().join("agents/blocked"), b"not a directory").await?;
        let mut changed = original.clone();
        changed["cwd"] = json!("/blocked");
        changed["title"] = json!("uncommitted");
        assert!(storage.upsert(changed).await.is_err());
        assert_eq!(storage.get("agent").await?.unwrap(), before);
        storage.begin_delete("agent").await?;
        storage.upsert(original).await?;
        storage.remove("agent").await?;
        assert!(storage.get("agent").await?.is_none());
        assert!(
            AgentStorage::new(home.path().to_owned())
                .list()
                .await?
                .is_empty()
        );
        Ok(())
    }
}
