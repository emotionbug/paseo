use anyhow::{Context, Result, anyhow, bail};
use chrono::{SecondsFormat, Utc};
use rand::RngCore;
use serde_json::{Map, Value, json};
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
};
use tokio::{
    fs,
    io::AsyncWriteExt,
    sync::{Mutex, broadcast},
};

/// Errors whose codes are part of the workspace-label wire contract.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct StoreError {
    pub code: &'static str,
    pub message: &'static str,
}
fn label_error(code: &'static str, message: &'static str) -> anyhow::Error {
    StoreError { code, message }.into()
}
fn uncertain() -> anyhow::Error {
    label_error(
        "workspace_label_storage_uncertain",
        "Workspace label storage outcome is uncertain; restart the daemon before retrying",
    )
}

pub struct Store {
    root: PathBuf,
    state: Mutex<State>,
}
struct State {
    projects: Map<String, Value>,
    workspaces: Map<String, Value>,
    labels: Vec<Value>,
    blocked: bool,
    generation: String,
    seq: u64,
    changes: VecDeque<Value>,
    events: broadcast::Sender<Value>,
}

impl Store {
    pub async fn load(home: &Path) -> Result<Self> {
        let root = home.join("projects");
        let projects = read_registry(&root.join("projects.json"), false).await?;
        let workspaces = read_registry(&root.join("workspaces.json"), true).await?;
        let (events, _) = broadcast::channel(256);
        let store = Self {
            root,
            state: Mutex::new(State {
                projects,
                workspaces,
                labels: Vec::new(),
                blocked: false,
                generation: uuid::Uuid::new_v4().to_string(),
                seq: 0,
                changes: VecDeque::new(),
                events,
            }),
        };
        let mut state = store.state.lock().await;
        if let Some(transaction) = store.read_transaction().await? {
            store.recover(&mut state, &transaction).await?;
        } else {
            state.labels = read_labels(&store.root.join("workspace-labels.json")).await?;
        }
        drop(state);
        Ok(store)
    }

    pub async fn projects_exist_on_disk(&self) -> Result<bool> {
        exists(&self.root.join("projects.json")).await
    }
    pub async fn workspaces_exist_on_disk(&self) -> Result<bool> {
        exists(&self.root.join("workspaces.json")).await
    }
    pub async fn list_projects(&self) -> Vec<Value> {
        self.state.lock().await.projects.values().cloned().collect()
    }
    pub async fn list_workspaces(&self) -> Vec<Value> {
        self.state
            .lock()
            .await
            .workspaces
            .values()
            .cloned()
            .collect()
    }
    pub async fn get_project(&self, id: &str) -> Option<Value> {
        self.state.lock().await.projects.get(id).cloned()
    }
    pub async fn get_workspace(&self, id: &str) -> Option<Value> {
        self.state.lock().await.workspaces.get(id).cloned()
    }

    pub async fn upsert_project(&self, record: Value) -> Result<()> {
        self.upsert(record, false).await
    }
    pub async fn upsert_workspace(&self, record: Value) -> Result<()> {
        self.upsert(record, true).await
    }
    async fn upsert(&self, record: Value, workspace: bool) -> Result<()> {
        let record = validate_record(record, workspace)?;
        let id = text(&record, id_field(workspace))?.to_owned();
        let mut state = self.state.lock().await;
        check_workspace_mutation(&state, workspace)?;
        self.persist_change(records(&state, workspace), workspace, &id, Some(&record))
            .await?;
        records_mut(&mut state, workspace).insert(id, record);
        Ok(())
    }
    pub async fn update_project<F>(&self, id: &str, updater: F) -> Result<Option<Value>>
    where
        F: FnOnce(&Value) -> Result<Value>,
    {
        self.update(id, false, updater).await
    }
    pub async fn update_workspace<F>(&self, id: &str, updater: F) -> Result<Option<Value>>
    where
        F: FnOnce(&Value) -> Result<Value>,
    {
        self.update(id, true, updater).await
    }
    async fn update<F>(&self, id: &str, workspace: bool, updater: F) -> Result<Option<Value>>
    where
        F: FnOnce(&Value) -> Result<Value>,
    {
        let mut state = self.state.lock().await;
        check_workspace_mutation(&state, workspace)?;
        let Some(existing) = records(&state, workspace).get(id) else {
            return Ok(None);
        };
        let next = validate_record(updater(existing)?, workspace)?;
        self.persist_change(records(&state, workspace), workspace, id, Some(&next))
            .await?;
        records_mut(&mut state, workspace).insert(id.into(), next.clone());
        Ok(Some(next))
    }
    pub async fn remove_project(&self, id: &str) -> Result<Option<Value>> {
        self.remove(id, false).await
    }
    pub async fn remove_workspace(&self, id: &str) -> Result<Option<Value>> {
        self.remove(id, true).await
    }
    async fn remove(&self, id: &str, workspace: bool) -> Result<Option<Value>> {
        let mut state = self.state.lock().await;
        check_workspace_mutation(&state, workspace)?;
        if !records(&state, workspace).contains_key(id) {
            return Ok(None);
        }
        self.persist_change(records(&state, workspace), workspace, id, None)
            .await?;
        Ok(records_mut(&mut state, workspace).shift_remove(id))
    }
    pub async fn archive_workspace(
        &self,
        id: &str,
        at: &str,
        auto_url: Option<&str>,
    ) -> Result<Option<Value>> {
        self.update_workspace(id, |record| {
            let mut next = record.clone();
            next["archivedAt"] = json!(at);
            next["updatedAt"] = json!(at);
            if let Some(url) = auto_url.filter(|url| !url.is_empty()) {
                next["autoArchivedChangeRequestUrl"] = json!(url);
            }
            Ok(next)
        })
        .await
    }
    pub async fn get_or_create_active_project(
        &self,
        root: &str,
        kind: &str,
        name: &str,
        key: Option<&str>,
        timestamp: &str,
    ) -> Result<Value> {
        let mut state = self.state.lock().await;
        let active = state
            .projects
            .values()
            .filter(|p| !is_archived(p) && equivalent_paths(p["rootPath"].as_str().unwrap(), root))
            .min_by(|a, b| {
                let time = |v: &Value| {
                    chrono::DateTime::parse_from_rfc3339(v["createdAt"].as_str().unwrap()).ok()
                };
                match (time(a), time(b)) {
                    (Some(a), Some(b)) => a.timestamp_millis().cmp(&b.timestamp_millis()),
                    _ => std::cmp::Ordering::Equal,
                }
                .then_with(|| a["projectId"].as_str().cmp(&b["projectId"].as_str()))
            });
        let next = if let Some(active) = active {
            if active["kind"] == kind && active["projectKey"] == json!(key) {
                return Ok(active.clone());
            }
            let mut next = active.clone();
            next["kind"] = json!(kind);
            next["projectKey"] = json!(key);
            next["updatedAt"] = json!(timestamp);
            validate_record(next, false)?
        } else {
            let id = loop {
                let id = generate_project_id();
                if !state.projects.contains_key(&id) {
                    break id;
                }
            };
            create_project(
                json!({"projectId":id,"rootPath":root,"kind":kind,"displayName":name,"projectKey":key,"createdAt":timestamp,"updatedAt":timestamp}),
            )?
        };
        let id = text(&next, "projectId")?;
        self.persist_change(&state.projects, false, id, Some(&next))
            .await?;
        state.projects.insert(id.into(), next.clone());
        Ok(next)
    }

    pub async fn list_labels(&self, cursor: Option<&Value>) -> Value {
        let state = self.state.lock().await;
        synchronize(&state, cursor)
    }
    /// The receiver is registered under the same lock as its initial snapshot.
    /// A lagging receiver must resynchronize with list_labels, never skip a change.
    pub async fn subscribe_labels(
        &self,
        cursor: Option<&Value>,
    ) -> (Value, broadcast::Receiver<Value>) {
        let state = self.state.lock().await;
        (synchronize(&state, cursor), state.events.subscribe())
    }
    pub async fn set_label_assignment(
        &self,
        workspace_id: &str,
        label: &Value,
        assigned: bool,
    ) -> Result<Value> {
        let label = validate_label(label)?;
        let name = require_name(text(&label, "name")?)?;
        let key = label_key(&name);
        let mut state = self.state.lock().await;
        if state.blocked {
            return Err(uncertain());
        }
        let workspace = state
            .workspaces
            .get(workspace_id)
            .filter(|w| !is_archived(w))
            .ok_or_else(|| label_error("workspace_not_found", "Workspace not found"))?;
        let existing = state
            .labels
            .iter()
            .find(|l| label_key(l["name"].as_str().unwrap()) == key);
        let catalog_changed = existing.is_none() && assigned;
        let definition = existing
            .cloned()
            .unwrap_or_else(|| json!({"name":name,"color":label["color"]}));
        let mut names = workspace
            .get("labels")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let index = names
            .iter()
            .position(|n| label_key(n.as_str().unwrap()) == key);
        let changed = if assigned && index.is_none() {
            names.push(definition["name"].clone());
            true
        } else if !assigned && index.is_some() {
            names.remove(index.unwrap());
            true
        } else {
            false
        };
        let mut updates = Vec::new();
        if changed {
            let mut next = workspace.clone();
            set_names(&mut next, &names, &now());
            updates.push(next);
        }
        let mut labels = state.labels.clone();
        if catalog_changed {
            labels.push(definition.clone());
        }
        self.commit_labels(&mut state, labels, updates).await?;
        if catalog_changed {
            publish(&mut state, json!({"kind":"upsert","label":definition}));
        }
        Ok(json!({"label":definition,"workspaceLabels":names}))
    }
    pub async fn update_label(
        &self,
        name: &str,
        new_name: Option<&str>,
        color: Option<&str>,
    ) -> Result<Value> {
        let key = label_key(&require_name(name)?);
        let new_name = new_name.map(require_name).transpose()?;
        if let Some(color) = color {
            validate_color(color)?;
        }
        let mut state = self.state.lock().await;
        if state.blocked {
            return Err(uncertain());
        }
        let index = state
            .labels
            .iter()
            .position(|l| label_key(l["name"].as_str().unwrap()) == key)
            .ok_or_else(|| label_error("label_not_found", "Label not found"))?;
        let existing = &state.labels[index];
        if let Some(name) = &new_name {
            let to_key = label_key(name);
            if to_key != key
                && state
                    .labels
                    .iter()
                    .any(|l| label_key(l["name"].as_str().unwrap()) == to_key)
            {
                return Err(label_error(
                    "label_name_taken",
                    "A label with that name already exists",
                ));
            }
        }
        let old_name = existing["name"].as_str().unwrap();
        let next_name = new_name.as_deref().unwrap_or(old_name);
        let next_color = color.unwrap_or(existing["color"].as_str().unwrap());
        let rename = next_name != old_name;
        let label = json!({"name":next_name,"color":next_color});
        if label == *existing {
            return Ok(json!({"label":label,"affectedWorkspaceCount":0}));
        }
        let mut change = json!({"kind":"upsert","label":label});
        if rename {
            change["previousName"] = json!(old_name);
        }
        let updates = if rename {
            rewrite_assignments(&state.workspaces, &key, Some(next_name))
        } else {
            Vec::new()
        };
        let count = updates.len();
        let mut labels = state.labels.clone();
        labels[index] = label.clone();
        self.commit_labels(&mut state, labels, updates).await?;
        publish(&mut state, change);
        Ok(json!({"label":label,"affectedWorkspaceCount":count}))
    }
    pub async fn delete_label(&self, name: &str) -> Result<Value> {
        let key = label_key(&require_name(name)?);
        let mut state = self.state.lock().await;
        if state.blocked {
            return Err(uncertain());
        }
        let Some(index) = state
            .labels
            .iter()
            .position(|l| label_key(l["name"].as_str().unwrap()) == key)
        else {
            return Ok(json!({"affectedWorkspaceCount":0}));
        };
        let updates = rewrite_assignments(&state.workspaces, &key, None);
        let count = updates.len();
        let mut labels = state.labels.clone();
        let removed = labels.remove(index);
        self.commit_labels(&mut state, labels, updates).await?;
        publish(&mut state, json!({"kind":"remove","name":removed["name"]}));
        Ok(json!({"affectedWorkspaceCount":count}))
    }
    pub async fn count_label_workspaces(&self, name: &str) -> Result<usize> {
        let key = label_key(&require_name(name)?);
        let state = self.state.lock().await;
        if state.blocked {
            return Err(uncertain());
        }
        Ok(state
            .workspaces
            .values()
            .filter(|w| {
                w.get("labels")
                    .and_then(Value::as_array)
                    .is_some_and(|names| {
                        names.iter().any(|n| label_key(n.as_str().unwrap()) == key)
                    })
            })
            .count())
    }
    async fn persist_change(
        &self,
        records: &Map<String, Value>,
        workspace: bool,
        id: &str,
        next: Option<&Value>,
    ) -> Result<()> {
        let values = records
            .iter()
            .filter_map(|(key, value)| if key == id { next } else { Some(value) })
            .chain(next.filter(|_| !records.contains_key(id)));
        atomic_records(
            &self.root.join(if workspace {
                "workspaces.json"
            } else {
                "projects.json"
            }),
            values,
        )
        .await
    }
    async fn persist_workspace_updates(
        &self,
        state: &State,
        updates: &Map<String, Value>,
    ) -> Result<()> {
        let values = state
            .workspaces
            .iter()
            .map(|(id, value)| updates.get(id).unwrap_or(value))
            .chain(
                updates
                    .iter()
                    .filter(|(id, _)| !state.workspaces.contains_key(*id))
                    .map(|(_, value)| value),
            );
        atomic_records(&self.root.join("workspaces.json"), values).await
    }
    async fn read_transaction(&self) -> Result<Option<Value>> {
        read_json(&self.root.join("workspace-labels.transaction.json"))
            .await?
            .map(validate_transaction)
            .transpose()
    }
    async fn commit_labels(
        &self,
        state: &mut State,
        labels: Vec<Value>,
        updates: Vec<Value>,
    ) -> Result<()> {
        if updates.is_empty() && labels == state.labels {
            return Ok(());
        }
        let mut staged = Map::new();
        let mut before = Vec::with_capacity(updates.len());
        let mut after = Vec::with_capacity(updates.len());
        for update in updates {
            let update = validate_record(update, true)?;
            let id = text(&update, "workspaceId")?;
            if let Some(current) = state.workspaces.get(id) {
                before.push(workspace_state(current));
            }
            after.push(workspace_state(&update));
            staged.insert(id.to_owned(), update);
        }
        let mut transaction = json!({"phase":"prepared","beforeLabels":state.labels,"afterLabels":labels,
            "beforeWorkspaces":before,"afterWorkspaces":after});
        let transaction_path = self.root.join("workspace-labels.transaction.json");
        // Cancellation during the multi-file protocol leaves a recoverable journal and
        // freezes this instance rather than allowing writes over an uncertain outcome.
        state.blocked = true;
        let result: Result<()> = async {
            atomic_json(&transaction_path, &transaction).await?;
            atomic_json(&self.root.join("workspace-labels.json"), &labels).await?;
            if !after.is_empty() {
                self.persist_workspace_updates(state, &staged).await?;
            }
            transaction["phase"] = json!("committed");
            atomic_json(&transaction_path, &transaction).await?;
            Ok(())
        }
        .await;
        if let Err(error) = result {
            match self.read_transaction().await {
                Ok(None) => {
                    state.blocked = false;
                    return Err(error);
                }
                Ok(Some(transaction)) if transaction["phase"] == "prepared" => {
                    if self.recover(state, &transaction).await.is_ok() {
                        state.blocked = false;
                        return Err(error);
                    }
                }
                _ => {}
            }
            return Err(uncertain());
        }
        for (id, record) in staged {
            state.workspaces.insert(id, record);
        }
        state.labels = labels;
        state.blocked = false;
        let _ = fs::remove_file(transaction_path).await;
        Ok(())
    }
    async fn recover(&self, state: &mut State, transaction: &Value) -> Result<()> {
        let path = self.root.join("workspace-labels.transaction.json");
        if transaction["phase"] == "committed" {
            // A committed marker is cleanup only. Never replay stale after-images.
            state.labels = read_labels(&self.root.join("workspace-labels.json")).await?;
            let _ = fs::remove_file(path).await;
            return Ok(());
        }
        let labels = transaction["beforeLabels"].as_array().unwrap().clone();
        let mut staged = Map::new();
        for prior in transaction["beforeWorkspaces"].as_array().unwrap() {
            if let Some(current) = state.workspaces.get(prior["workspaceId"].as_str().unwrap()) {
                let mut current = current.clone();
                if let Some(names) = prior.get("labels") {
                    current["labels"] = names.clone();
                } else {
                    current.as_object_mut().unwrap().remove("labels");
                }
                current["updatedAt"] = prior["updatedAt"].clone();
                staged.insert(prior["workspaceId"].as_str().unwrap().into(), current);
            }
        }
        atomic_json(&self.root.join("workspace-labels.json"), &labels).await?;
        if !staged.is_empty() {
            self.persist_workspace_updates(state, &staged).await?;
        }
        fs::remove_file(path).await?;
        state.labels = labels;
        for (id, record) in staged {
            state.workspaces.insert(id, record);
        }
        Ok(())
    }
}

fn records(state: &State, workspace: bool) -> &Map<String, Value> {
    if workspace {
        &state.workspaces
    } else {
        &state.projects
    }
}
fn records_mut(state: &mut State, workspace: bool) -> &mut Map<String, Value> {
    if workspace {
        &mut state.workspaces
    } else {
        &mut state.projects
    }
}
fn id_field(workspace: bool) -> &'static str {
    if workspace {
        "workspaceId"
    } else {
        "projectId"
    }
}
fn check_workspace_mutation(state: &State, workspace: bool) -> Result<()> {
    if workspace && state.blocked {
        bail!("Workspace registry mutations are blocked until daemon restart")
    }
    Ok(())
}
pub fn is_archived(record: &Value) -> bool {
    record["archivedAt"].as_str().is_some_and(|s| !s.is_empty())
}
fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}
fn text<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("{field}: expected string"))
}
fn validate_record(value: Value, workspace: bool) -> Result<Value> {
    let input = value
        .as_object()
        .ok_or_else(|| anyhow!("Registry record: expected object"))?;
    let required: &[&str] = if workspace {
        &[
            "workspaceId",
            "projectId",
            "cwd",
            "kind",
            "displayName",
            "createdAt",
            "updatedAt",
        ]
    } else {
        &[
            "projectId",
            "rootPath",
            "kind",
            "displayName",
            "createdAt",
            "updatedAt",
        ]
    };
    let nullable: &[&str] = if workspace {
        &[
            "title",
            "branch",
            "worktreeRoot",
            "baseBranch",
            "mainRepoRoot",
            "autoArchivedChangeRequestUrl",
            "pinnedAt",
        ]
    } else {
        &["projectKey", "customName", "customIconRevision"]
    };
    let mut out = Map::new();
    for field in required {
        out.insert((*field).into(), json!(text(&value, field)?));
    }
    let kinds: &[&str] = if workspace {
        &["local_checkout", "worktree", "directory"]
    } else {
        &["git", "non_git"]
    };
    if !kinds.contains(&text(&value, "kind")?) {
        bail!("kind: invalid registry kind")
    }
    let archived = input
        .get("archivedAt")
        .ok_or_else(|| anyhow!("archivedAt: required"))?;
    if !archived.is_null() && !archived.is_string() {
        bail!("archivedAt: expected string or null")
    }
    out.insert("archivedAt".into(), archived.clone());
    for field in nullable {
        let val = input.get(*field).unwrap_or(&Value::Null);
        if !val.is_null() && !val.is_string() {
            bail!("{field}: expected string or null")
        }
        out.insert((*field).into(), val.clone());
    }
    if workspace {
        for field in ["isPaseoOwnedWorktree", "background"] {
            let val = input.get(field).cloned().unwrap_or(json!(false));
            if !val.is_boolean() {
                bail!("{field}: expected boolean")
            }
            out.insert(field.into(), val);
        }
        if let Some(labels) = input.get("labels") {
            validate_names(labels)?;
            out.insert("labels".into(), labels.clone());
        }
        if let Some(source) = input.get("untrustedSource") {
            if source["kind"] != "change_request" {
                bail!("untrustedSource.kind: expected change_request")
            }
            let number = source["number"]
                .as_f64()
                .filter(|n| n.is_finite() && *n > 0.0 && n.fract() == 0.0)
                .ok_or_else(|| anyhow!("untrustedSource.number: expected positive integer"))?;
            let _ = number;
            out.insert(
                "untrustedSource".into(),
                json!({"kind":"change_request","forge":text(source,"forge")?,
                "number":source["number"],"headRepository":text(source,"headRepository")?}),
            );
        }
    }
    Ok(Value::Object(out))
}
pub fn create_project(mut input: Value) -> Result<Value> {
    input
        .as_object_mut()
        .ok_or_else(|| anyhow!("Project: expected object"))?
        .entry("archivedAt")
        .or_insert(Value::Null);
    validate_record(input, false)
}
pub fn create_workspace(mut input: Value) -> Result<Value> {
    let object = input
        .as_object_mut()
        .ok_or_else(|| anyhow!("Workspace: expected object"))?;
    object.entry("archivedAt").or_insert(Value::Null);
    for field in ["isPaseoOwnedWorktree", "background"] {
        if object.get(field).is_some_and(Value::is_null) {
            object.insert(field.into(), json!(false));
        }
    }
    validate_record(input, true)
}
pub fn generate_project_id() -> String {
    random_id("prj_")
}
pub fn generate_workspace_id() -> String {
    random_id("wks_")
}
fn random_id(prefix: &str) -> String {
    let mut bytes = [0u8; 8];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    format!("{prefix}{}", hex::encode(bytes))
}

async fn read_json(path: &Path) -> Result<Option<Value>> {
    match fs::read(path).await {
        Ok(bytes) => {
            Ok(Some(serde_json::from_slice(&bytes).with_context(|| {
                format!("Invalid JSON in {}", path.display())
            })?))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("Reading {}", path.display())),
    }
}
async fn exists(path: &Path) -> Result<bool> {
    match fs::metadata(path).await {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}
async fn read_registry(path: &Path, workspace: bool) -> Result<Map<String, Value>> {
    let mut records = Map::new();
    if let Some(value) = read_json(path).await? {
        let array = value
            .as_array()
            .ok_or_else(|| anyhow!("{}: expected registry array", path.display()))?;
        for record in array {
            let record = validate_record(record.clone(), workspace)
                .with_context(|| path.display().to_string())?;
            records.insert(text(&record, id_field(workspace))?.into(), record);
        }
    }
    Ok(records)
}
async fn atomic_records<'a>(path: &Path, records: impl Iterator<Item = &'a Value>) -> Result<()> {
    use serde::ser::{SerializeSeq, Serializer};
    let mut bytes = Vec::new();
    let mut serializer = serde_json::Serializer::pretty(&mut bytes);
    let mut sequence = serializer.serialize_seq(None)?;
    for record in records {
        sequence.serialize_element(record)?;
    }
    sequence.end()?;
    atomic_bytes(path, &bytes).await
}
async fn atomic_json<T: serde::Serialize + ?Sized>(path: &Path, value: &T) -> Result<()> {
    atomic_bytes(path, &serde_json::to_vec_pretty(value)?).await
}
async fn atomic_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("Missing storage parent")?;
    fs::create_dir_all(parent).await?;
    let temporary = parent.join(format!(
        ".{}.{}.{}.tmp",
        path.file_name().unwrap().to_string_lossy(),
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let result: Result<()> = async {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .await?;
        file.write_all(bytes).await?;
        file.sync_all().await?;
        drop(file);
        fs::rename(&temporary, path).await?;
        Ok(())
    }
    .await;
    if result.is_err() {
        let _ = fs::remove_file(&temporary).await;
    }
    result.with_context(|| format!("Writing {}", path.display()))
}
fn validate_names(value: &Value) -> Result<()> {
    if !value
        .as_array()
        .is_some_and(|a| a.iter().all(Value::is_string))
    {
        bail!("labels: expected string array")
    }
    Ok(())
}
fn validate_color(color: &str) -> Result<()> {
    if ![
        "violet", "sky", "emerald", "orange", "pink", "indigo", "teal", "red", "amber", "blue",
    ]
    .contains(&color)
    {
        bail!("color: invalid workspace label color")
    }
    Ok(())
}
fn validate_label(value: &Value) -> Result<Value> {
    let name = text(value, "name")?;
    let color = text(value, "color")?;
    validate_color(color)?;
    Ok(json!({"name":name,"color":color}))
}
fn validate_labels(value: &Value) -> Result<Vec<Value>> {
    value
        .as_array()
        .ok_or_else(|| anyhow!("Expected workspace label array"))?
        .iter()
        .map(validate_label)
        .collect()
}
async fn read_labels(path: &Path) -> Result<Vec<Value>> {
    match read_json(path).await? {
        Some(value) => validate_labels(&value),
        None => Ok(Vec::new()),
    }
}
fn validate_transaction(value: Value) -> Result<Value> {
    if value["phase"] != "prepared" && value["phase"] != "committed" {
        bail!("Invalid workspace label transaction phase")
    }
    let mut out = json!({"phase":value["phase"],"beforeLabels":validate_labels(&value["beforeLabels"])?,"afterLabels":validate_labels(&value["afterLabels"])?});
    for field in ["beforeWorkspaces", "afterWorkspaces"] {
        let states = value[field]
            .as_array()
            .ok_or_else(|| anyhow!("{field}: expected array"))?;
        let mut parsed = Vec::with_capacity(states.len());
        for state in states {
            let mut item = json!({"workspaceId":text(state,"workspaceId")?,"updatedAt":text(state,"updatedAt")?});
            if let Some(labels) = state.get("labels") {
                validate_names(labels)?;
                item["labels"] = labels.clone();
            }
            parsed.push(item);
        }
        out[field] = json!(parsed);
    }
    Ok(out)
}
fn workspace_state(workspace: &Value) -> Value {
    let mut state =
        json!({"workspaceId":workspace["workspaceId"],"updatedAt":workspace["updatedAt"]});
    if let Some(labels) = workspace.get("labels") {
        state["labels"] = labels.clone();
    }
    state
}
fn js_whitespace(c: char) -> bool {
    matches!(c, '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}')
}
pub fn normalize_label_name(name: &str) -> String {
    name.split(js_whitespace)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}
pub fn label_key(name: &str) -> String {
    normalize_label_name(name).to_lowercase()
}
fn require_name(raw: &str) -> Result<String> {
    let name = normalize_label_name(raw);
    if name.is_empty() {
        return Err(label_error(
            "label_name_empty",
            "Label name cannot be empty",
        ));
    }
    Ok(name)
}
fn set_names(workspace: &mut Value, names: &[Value], timestamp: &str) {
    if names.is_empty() {
        workspace.as_object_mut().unwrap().remove("labels");
    } else {
        workspace["labels"] = json!(names);
    }
    workspace["updatedAt"] = json!(timestamp);
}
fn rewrite_assignments(workspaces: &Map<String, Value>, key: &str, to: Option<&str>) -> Vec<Value> {
    let timestamp = now();
    workspaces
        .values()
        .filter_map(|workspace| {
            let labels = workspace.get("labels")?.as_array()?;
            if !labels
                .iter()
                .any(|label| label_key(label.as_str().unwrap()) == key)
            {
                return None;
            }
            let names: Vec<Value> = labels
                .iter()
                .filter_map(|label| {
                    if label_key(label.as_str().unwrap()) == key {
                        to.map(|name| json!(name))
                    } else {
                        Some(label.clone())
                    }
                })
                .collect();
            let mut next = workspace.clone();
            set_names(&mut next, &names, &timestamp);
            Some(next)
        })
        .collect()
}
fn publish(state: &mut State, mut change: Value) {
    state.seq += 1;
    change["generation"] = json!(state.generation);
    change["seq"] = json!(state.seq);
    state.changes.push_back(change.clone());
    if state.changes.len() > 256 {
        state.changes.pop_front();
    }
    let _ = state.events.send(change);
}
fn synchronize(state: &State, cursor: Option<&Value>) -> Value {
    let after = cursor.and_then(|c| c["afterSeq"].as_u64());
    let oldest = state
        .changes
        .front()
        .and_then(|c| c["seq"].as_u64())
        .unwrap_or(state.seq + 1);
    let catch_up = cursor
        .is_some_and(|c| c["generation"].as_str() == Some(state.generation.as_str()))
        && after.is_some_and(|seq| seq <= state.seq && seq >= oldest - 1);
    if !catch_up {
        return json!({"labels":state.labels,"sync":{"mode":"snapshot","generation":state.generation,"headSeq":state.seq,"removals":[]}});
    }
    let mut upserts = Map::new();
    let mut removals = Map::new();
    for change in state
        .changes
        .iter()
        .filter(|c| c["seq"].as_u64().unwrap() > after.unwrap())
    {
        let removed = if change["kind"] == "remove" {
            change["name"].as_str()
        } else {
            change["previousName"].as_str().filter(|s| !s.is_empty())
        };
        if let Some(name) = removed {
            let key = name.to_lowercase();
            if upserts.shift_remove(&key).is_none() {
                removals.insert(key, json!({"name":name,"seq":change["seq"]}));
            }
        }
        if change["kind"] == "upsert" {
            let key = change["label"]["name"].as_str().unwrap().to_lowercase();
            removals.shift_remove(&key);
            upserts.insert(key, change["label"].clone());
        }
    }
    json!({"labels":upserts.values().collect::<Vec<_>>(),"sync":{"mode":"changes","generation":state.generation,"headSeq":state.seq,"removals":removals.values().collect::<Vec<_>>()}})
}

pub fn derive_project_kind(checkout: &Value) -> &'static str {
    if checkout["isGit"] == true {
        "git"
    } else {
        "non_git"
    }
}
pub fn derive_workspace_kind(checkout: &Value) -> &'static str {
    if checkout["isGit"] != true {
        "directory"
    } else if checkout["mainRepoRoot"]
        .as_str()
        .is_some_and(|s| !s.is_empty())
    {
        "worktree"
    } else {
        "local_checkout"
    }
}
fn normalized_branch(checkout: &Value) -> Option<&str> {
    checkout["currentBranch"]
        .as_str()
        .map(|s| s.trim_matches(js_whitespace))
        .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("HEAD"))
}
fn last_segment(path: &str) -> &str {
    path.rsplit(['/', '\\'])
        .find(|s| !s.is_empty())
        .unwrap_or(path)
}
pub fn derive_workspace_display_name(cwd: &str, checkout: &Value) -> String {
    normalized_branch(checkout)
        .unwrap_or_else(|| last_segment(cwd))
        .into()
}
pub fn initial_workspace_placement(input: &Value) -> Value {
    let cwd = input["cwd"].as_str().unwrap_or("");
    if input["source"] == "created_worktree" {
        return json!({"cwd":cwd,"kind":"worktree",
            "displayName":input["branch"].as_str().filter(|s| !s.is_empty()).unwrap_or(cwd),
            "branch":input["branch"],"worktreeRoot":input["worktreeRoot"],"baseBranch":input["baseBranch"],
            "isPaseoOwnedWorktree":true,"mainRepoRoot":input["mainRepoRoot"]});
    }
    let checkout = &input["checkout"];
    let git = checkout["isGit"] == true;
    json!({"cwd":cwd,"kind":derive_workspace_kind(checkout),"displayName":derive_workspace_display_name(cwd,checkout),
        "branch":normalized_branch(checkout),"worktreeRoot":if git { json!(checkout["worktreeRoot"].as_str().unwrap_or(cwd)) } else { Value::Null },
        "baseBranch":null,"isPaseoOwnedWorktree":git && checkout["isPaseoOwnedWorktree"] == true,
        "mainRepoRoot":if git { checkout["mainRepoRoot"].clone() } else { Value::Null }})
}
pub fn reconcile_workspace_placement(
    workspace: &Value,
    checkout: Option<&Value>,
    updated_at: &str,
) -> Option<Value> {
    let checkout = checkout.filter(|v| !v.is_null())?;
    let observed = initial_workspace_placement(
        &json!({"source":"checkout","cwd":workspace["cwd"],"checkout":checkout}),
    );
    let mut fields = Map::new();
    let mut next = workspace.clone();
    for key in [
        "kind",
        "branch",
        "worktreeRoot",
        "isPaseoOwnedWorktree",
        "mainRepoRoot",
    ] {
        if workspace[key] != observed[key] {
            fields.insert(key.into(), observed[key].clone());
            next[key] = observed[key].clone();
        }
    }
    if fields.is_empty() {
        return None;
    }
    next["updatedAt"] = json!(updated_at);
    Some(json!({"workspace":next,"fields":fields}))
}

fn windows_path(value: &str) -> bool {
    let b = value.as_bytes();
    (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'/' | b'\\'))
        || value.starts_with("\\\\")
        || value.starts_with("//?/")
}
fn lexical_path(value: &str, windows: bool, fold: bool) -> String {
    let converted = if windows {
        value.replace('\\', "/")
    } else {
        value.to_owned()
    };
    let mut value = converted.as_str();
    let namespace_unc;
    if windows && value.starts_with("//?/") {
        value = &value[4..];
        if value
            .get(..4)
            .is_some_and(|s| s.eq_ignore_ascii_case("UNC/"))
        {
            namespace_unc = format!("//{}", &value[4..]);
            value = &namespace_unc;
        }
    }
    let drive = windows && value.as_bytes().get(1) == Some(&b':');
    let unc = windows && value.starts_with("//");
    let absolute = value.starts_with('/') || (drive && value.as_bytes().get(2) == Some(&b'/'));
    let (prefix, rest) = if drive {
        (&value[..2], &value[2..])
    } else {
        ("", value)
    };
    let mut parts = Vec::new();
    let protected = if unc { 2 } else { 0 };
    for part in rest.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            if parts.len() > protected && parts.last() != Some(&"..") {
                parts.pop();
            } else if !absolute {
                parts.push(part);
            }
        } else {
            parts.push(part);
        }
    }
    let mut result = String::from(prefix);
    if unc {
        result.push_str("//");
    } else if absolute {
        result.push('/');
    }
    result.push_str(&parts.join("/"));
    if result.is_empty() {
        result.push('.');
    }
    if unc && parts.len() == 2 {
        result.push('/');
    }
    if windows {
        result = result.replace('/', "\\");
    }
    if fold {
        result = result.to_lowercase();
    }
    result
}
pub fn equivalent_paths(left: &str, right: &str) -> bool {
    let windows = windows_path(left) || windows_path(right);
    lexical_path(left, windows, windows) == lexical_path(right, windows, windows)
}
async fn path_variants(path: &str) -> Vec<String> {
    let mut variants = vec![path.to_owned()];
    if let Ok(real) = fs::canonicalize(path).await {
        let real = real.to_string_lossy().into_owned();
        if real != path {
            variants.push(real);
        }
    }
    variants
}
pub(super) async fn relative_inside(root: &str, candidate: &str) -> Option<String> {
    let roots = path_variants(root).await;
    let candidates = path_variants(candidate).await;
    for root in &roots {
        for candidate in &candidates {
            let windows = windows_path(root) || windows_path(candidate);
            let normalized_root = lexical_path(root, windows, false);
            let normalized_candidate = lexical_path(candidate, windows, false);
            let root = if windows {
                normalized_root.to_lowercase()
            } else {
                normalized_root.clone()
            };
            let candidate = if windows {
                normalized_candidate.to_lowercase()
            } else {
                normalized_candidate.clone()
            };
            if root == candidate {
                return Some(String::new());
            }
            let separator = if windows { '\\' } else { '/' };
            let prefix = format!("{}{separator}", root.trim_end_matches(separator));
            if candidate.starts_with(&prefix) {
                // Count components rather than lowercase bytes (Unicode case folding changes length).
                let count = normalized_root
                    .trim_end_matches(separator)
                    .split(separator)
                    .count();
                return Some(
                    normalized_candidate
                        .split(separator)
                        .skip(count)
                        .collect::<Vec<_>>()
                        .join(if windows { "\\" } else { "/" }),
                );
            }
        }
    }
    None
}
fn remote_location(remote: &str) -> Option<(String, Option<String>, String)> {
    let remote = remote.trim_matches(js_whitespace);
    let (host, port, path) = if !remote.contains("://") {
        let (user, rest) = remote.split_once('@')?;
        if user.is_empty() {
            return None;
        }
        let (host, path) = rest.split_once(':')?;
        (host.to_owned(), None, path.to_owned())
    } else {
        let url = url::Url::parse(remote).ok()?;
        if !matches!(url.scheme(), "http" | "https" | "ssh") {
            return None;
        }
        let default = match url.scheme() {
            "http" => 80,
            "https" => 443,
            _ => 22,
        };
        let port = url.port().filter(|p| *p != default).map(|p| p.to_string());
        let encoded = url.path().as_bytes();
        for (index, byte) in encoded.iter().enumerate() {
            if *byte == b'%'
                && !(encoded.get(index + 1).is_some_and(u8::is_ascii_hexdigit)
                    && encoded.get(index + 2).is_some_and(u8::is_ascii_hexdigit))
            {
                return None;
            }
        }
        (
            url.host_str()?.to_owned(),
            port,
            percent_encoding::percent_decode_str(url.path())
                .decode_utf8()
                .ok()?
                .into_owned(),
        )
    };
    let host = host
        .trim_matches(js_whitespace)
        .trim_end_matches('.')
        .to_lowercase();
    let bytes = host.as_bytes();
    if !bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        || !bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        || !bytes.iter().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(*b, b'.' | b'_' | b'-')
        })
    {
        return None;
    }
    let path = path.trim_matches(js_whitespace).trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    if path.is_empty() {
        return None;
    }
    Some((host, port, path.into()))
}
/// The source's `parseGitHubRepoNameFromRemote`: the repository name of a
/// two-segment `owner/name` path on a GitHub cloud host.
pub(crate) fn github_repo_name(remote: &str) -> Option<String> {
    let (host, _, path) = remote_location(remote)?;
    if host != "github.com" && host != "ssh.github.com" {
        return None;
    }
    let segments: Vec<_> = path.split('/').filter(|s| !s.is_empty()).collect();
    match segments.as_slice() {
        [_, name] => Some((*name).to_owned()),
        _ => None,
    }
}
pub async fn derive_project_key(
    root: &str,
    remote: Option<&str>,
    worktree_root: Option<&str>,
    main_root: Option<&str>,
    server_id: Option<&str>,
) -> Result<String> {
    let selected = match worktree_root.filter(|s| !s.is_empty()) {
        Some(worktree) => relative_inside(worktree, root)
            .await
            .filter(|s| !s.is_empty()),
        None => None,
    };
    if let Some((host, port, path)) = remote.and_then(remote_location) {
        let path = if host == "github.com" || host == "ssh.github.com" {
            path.to_lowercase()
        } else {
            path
        };
        let host = port.map(|port| format!("{host}:{port}")).unwrap_or(host);
        let key = format!("remote:{host}/{path}");
        return Ok(selected
            .map(|suffix| format!("{key}#subdir:{}", suffix.replace('\\', "/")))
            .unwrap_or(key));
    }
    let local = match (selected.as_deref(), main_root.filter(|s| !s.is_empty())) {
        (Some(suffix), Some(main)) => format!("{main}/{suffix}"),
        _ => root.to_owned(),
    };
    // project-key.ts chooses win32.resolve for forward-slash UNC as well.
    let unc = local.starts_with("//")
        && local[2..]
            .split(['/', '\\'])
            .filter(|s| !s.is_empty())
            .count()
            >= 2;
    let windows = windows_path(&local) || unc;
    let local = if windows {
        lexical_path(&local, true, false)
    } else if Path::new(&local).is_absolute() {
        lexical_path(&local, false, false)
    } else {
        lexical_path(
            &std::env::current_dir()?.join(local).to_string_lossy(),
            false,
            false,
        )
    };
    let canonical = fs::canonicalize(&local)
        .await
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or(local);
    let path = lexical_path(
        &canonical,
        windows_path(&canonical),
        windows_path(&canonical),
    );
    Ok(server_id
        .filter(|s| !s.is_empty())
        .map(|id| format!("host:{id}:{path}"))
        .unwrap_or(path))
}
pub async fn derive_project_grouping_display_name(
    root: &str,
    remote: Option<&str>,
    worktree_root: Option<&str>,
) -> String {
    if let Some(worktree) = worktree_root.filter(|s| !s.is_empty()) {
        if relative_inside(worktree, root)
            .await
            .is_some_and(|s| !s.is_empty())
        {
            return last_segment(root).into();
        }
    }
    match remote.and_then(remote_location) {
        Some((_, _, path)) => {
            let segments: Vec<_> = path.split('/').filter(|s| !s.is_empty()).collect();
            let display = segments[segments.len().saturating_sub(2)..].join("/");
            if display.is_empty() {
                root.into()
            } else {
                display
            }
        }
        None => last_segment(root).into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn project(id: &str) -> Value {
        create_project(
            json!({"projectId":id,"rootPath":"/repo","kind":"git","displayName":"repo",
            "createdAt":"2026-01-01T00:00:00.000Z","updatedAt":"2026-01-01T00:00:00.000Z"}),
        )
        .unwrap()
    }
    fn workspace(id: &str) -> Value {
        create_workspace(json!({"workspaceId":id,"projectId":"prj_test","cwd":"/repo","kind":"local_checkout",
            "displayName":"main","createdAt":"2026-01-01T00:00:00.000Z","updatedAt":"2026-01-01T00:00:00.000Z"})).unwrap()
    }
    #[tokio::test]
    async fn defaults_archived_identity_and_serialized_updates_survive_reload() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::load(dir.path()).await?;
        let mut old = project("prj_old");
        old["archivedAt"] = json!("2026-02-01T00:00:00.000Z");
        store.upsert_project(old).await?;
        let new = store
            .get_or_create_active_project("/repo/", "git", "repo", None, "2026-03-01T00:00:00.000Z")
            .await?;
        assert_ne!(new["projectId"], "prj_old");
        store.upsert_workspace(workspace("wks_test")).await?;
        let (left, right) = tokio::join!(
            store.update_workspace("wks_test", |v| {
                let mut v = v.clone();
                v["title"] = json!("Review");
                Ok(v)
            }),
            store.update_workspace("wks_test", |v| {
                let mut v = v.clone();
                v["pinnedAt"] = json!("now");
                Ok(v)
            }),
        );
        left?;
        right?;
        let reloaded = Store::load(dir.path()).await?;
        let w = reloaded.get_workspace("wks_test").await.unwrap();
        assert_eq!(w["title"], "Review");
        assert_eq!(w["pinnedAt"], "now");
        assert_eq!(w["background"], false);
        assert_eq!(w["baseBranch"], Value::Null);
        assert!(w.get("labels").is_none());
        assert_eq!(reloaded.list_projects().await.len(), 2);
        Ok(())
    }
    #[tokio::test]
    async fn failed_registry_write_does_not_publish_cache() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::load(dir.path()).await?;
        store.upsert_workspace(workspace("wks_test")).await?;
        let path = dir.path().join("projects/workspaces.json");
        fs::remove_file(&path).await?;
        fs::create_dir(&path).await?;
        assert!(
            store
                .update_workspace("wks_test", |v| {
                    let mut v = v.clone();
                    v["title"] = json!("uncommitted");
                    Ok(v)
                })
                .await
                .is_err()
        );
        assert_eq!(
            store.get_workspace("wks_test").await.unwrap()["title"],
            Value::Null
        );
        Ok(())
    }
    #[tokio::test]
    async fn labels_rename_archived_assignments_and_delete_durably() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::load(dir.path()).await?;
        store.upsert_workspace(workspace("wks_test")).await?;
        let snapshot = store.list_labels(None).await;
        let cursor = json!({"generation":snapshot["sync"]["generation"],"afterSeq":0});
        store
            .set_label_assignment(
                "wks_test",
                &json!({"name":" Needs  review ","color":"sky"}),
                true,
            )
            .await?;
        store
            .archive_workspace("wks_test", "2026-02-01T00:00:00.000Z", None)
            .await?;
        let edited = store
            .update_label("needs review", Some("READY"), Some("red"))
            .await?;
        assert_eq!(edited["affectedWorkspaceCount"], 1);
        assert_eq!(
            store.get_workspace("wks_test").await.unwrap()["labels"],
            json!(["READY"])
        );
        assert_eq!(
            store.list_labels(Some(&cursor)).await["labels"],
            json!([{"name":"READY","color":"red"}])
        );
        assert!(
            store
                .set_label_assignment("wks_test", &json!({"name":"other","color":"sky"}), true)
                .await
                .is_err()
        );
        assert_eq!(
            store.delete_label("ready").await?["affectedWorkspaceCount"],
            1
        );
        let reloaded = Store::load(dir.path()).await?;
        assert!(
            reloaded
                .get_workspace("wks_test")
                .await
                .unwrap()
                .get("labels")
                .is_none()
        );
        assert_eq!(reloaded.list_labels(None).await["labels"], json!([]));
        Ok(())
    }
    async fn journal_fixture(dir: &Path, phase: &str) -> Result<()> {
        let mut w = workspace("wks_test");
        w["title"] = json!("newer unrelated title");
        w["labels"] = json!(["After"]);
        w["updatedAt"] = json!("after");
        atomic_json(&dir.join("projects/workspaces.json"), &vec![w]).await?;
        atomic_json(
            &dir.join("projects/workspace-labels.json"),
            &json!([{"name":"After","color":"red"}]),
        )
        .await?;
        atomic_json(&dir.join("projects/workspace-labels.transaction.json"), &json!({
            "phase":phase,"beforeLabels":[{"name":"Before","color":"sky"}],"afterLabels":[{"name":"After","color":"red"}],
            "beforeWorkspaces":[{"workspaceId":"wks_test","labels":["Before"],"updatedAt":"before"}],
            "afterWorkspaces":[{"workspaceId":"wks_test","labels":["After"],"updatedAt":"stale"}]
        })).await
    }
    #[tokio::test]
    async fn prepared_journal_rolls_back_only_label_fields() -> Result<()> {
        let dir = tempfile::tempdir()?;
        journal_fixture(dir.path(), "prepared").await?;
        let store = Store::load(dir.path()).await?;
        let w = store.get_workspace("wks_test").await.unwrap();
        assert_eq!(w["labels"], json!(["Before"]));
        assert_eq!(w["updatedAt"], "before");
        assert_eq!(w["title"], "newer unrelated title");
        assert_eq!(
            store.list_labels(None).await["labels"],
            json!([{"name":"Before","color":"sky"}])
        );
        assert!(
            !exists(
                &dir.path()
                    .join("projects/workspace-labels.transaction.json")
            )
            .await?
        );
        assert_eq!(
            Store::load(dir.path())
                .await?
                .get_workspace("wks_test")
                .await,
            Some(w)
        );
        Ok(())
    }
    #[tokio::test]
    async fn committed_journal_never_replays_stale_after_images() -> Result<()> {
        let dir = tempfile::tempdir()?;
        journal_fixture(dir.path(), "committed").await?;
        let store = Store::load(dir.path()).await?;
        let w = store.get_workspace("wks_test").await.unwrap();
        assert_eq!(w["updatedAt"], "after");
        assert_eq!(w["title"], "newer unrelated title");
        assert_eq!(
            store.list_labels(None).await["labels"],
            json!([{"name":"After","color":"red"}])
        );
        Ok(())
    }
    #[tokio::test]
    async fn failed_compound_rollback_freezes_workspace_but_not_project_mutations() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::load(dir.path()).await?;
        store.upsert_workspace(workspace("wks_test")).await?;
        let catalog = dir.path().join("projects/workspace-labels.json");
        fs::create_dir(&catalog).await?;
        let error = store
            .set_label_assignment("wks_test", &json!({"name":"Review","color":"sky"}), true)
            .await
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<StoreError>().unwrap().code,
            "workspace_label_storage_uncertain"
        );
        assert!(
            store
                .upsert_workspace(workspace("wks_other"))
                .await
                .is_err()
        );
        store.upsert_project(project("prj_other")).await?;
        assert!(
            store
                .get_workspace("wks_test")
                .await
                .unwrap()
                .get("labels")
                .is_none()
        );
        fs::remove_dir(&catalog).await?;
        let restarted = Store::load(dir.path()).await?;
        restarted.upsert_workspace(workspace("wks_other")).await?;
        assert!(
            restarted
                .get_workspace("wks_test")
                .await
                .unwrap()
                .get("labels")
                .is_none()
        );
        Ok(())
    }
    #[tokio::test]
    async fn malformed_existing_storage_is_not_an_empty_registry() -> Result<()> {
        let dir = tempfile::tempdir()?;
        atomic_json(
            &dir.path().join("projects/workspaces.json"),
            &json!([{"workspaceId":"broken"}]),
        )
        .await?;
        assert!(Store::load(dir.path()).await.is_err());
        atomic_json(&dir.path().join("projects/workspaces.json"), &json!([])).await?;
        atomic_json(
            &dir.path()
                .join("projects/workspace-labels.transaction.json"),
            &json!({"phase":"prepared"}),
        )
        .await?;
        assert!(Store::load(dir.path()).await.is_err());
        Ok(())
    }
    #[tokio::test]
    async fn rename_collision_is_atomic_and_sequence_resets_after_restart() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::load(dir.path()).await?;
        store.upsert_workspace(workspace("wks_test")).await?;
        store
            .set_label_assignment("wks_test", &json!({"name":"Review","color":"sky"}), true)
            .await?;
        store
            .set_label_assignment("wks_test", &json!({"name":"Ready","color":"red"}), true)
            .await?;
        let before = store.list_labels(None).await;
        let error = store
            .update_label("Review", Some("READY"), Some("pink"))
            .await
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<StoreError>().unwrap().code,
            "label_name_taken"
        );
        assert_eq!(store.list_labels(None).await, before);
        assert_eq!(
            store.get_workspace("wks_test").await.unwrap()["labels"],
            json!(["Review", "Ready"])
        );
        let cursor =
            json!({"generation":before["sync"]["generation"],"afterSeq":before["sync"]["headSeq"]});
        let (snapshot, mut receiver) = store.subscribe_labels(Some(&cursor)).await;
        assert_eq!(snapshot["sync"]["mode"], "changes");
        assert_eq!(snapshot["labels"], json!([]));
        store.update_label("Review", Some("REVIEW"), None).await?;
        let change = receiver.recv().await?;
        assert_eq!(change["previousName"], "Review");
        assert_eq!(change["label"]["name"], "REVIEW");
        let delta = store.list_labels(Some(&cursor)).await;
        assert_eq!(delta["sync"]["removals"], json!([]));
        assert_eq!(delta["labels"], json!([{"name":"REVIEW","color":"sky"}]));
        let reloaded = Store::load(dir.path()).await?;
        let restarted = reloaded.list_labels(Some(&cursor)).await;
        assert_eq!(restarted["sync"]["mode"], "snapshot");
        assert_ne!(
            restarted["sync"]["generation"],
            before["sync"]["generation"]
        );
        assert_eq!(
            reloaded.get_workspace("wks_test").await.unwrap()["labels"],
            json!(["REVIEW", "Ready"])
        );
        Ok(())
    }
    #[tokio::test]
    async fn remote_project_keys_preserve_ports_and_selected_subdirectories() -> Result<()> {
        assert_eq!(
            derive_project_key(
                "/repo/src",
                Some("ssh://git@github.com:22/Owner/Repo.git"),
                Some("/repo"),
                None,
                None
            )
            .await?,
            "remote:github.com/owner/repo#subdir:src"
        );
        assert_eq!(
            derive_project_key(
                "/repo",
                Some("https://forge.example:8443/Owner/Repo.git"),
                None,
                None,
                None
            )
            .await?,
            "remote:forge.example:8443/Owner/Repo"
        );
        assert!(equivalent_paths(r"\\?\C:\Repo\..\Repo\", "c:/repo"));
        Ok(())
    }
}
