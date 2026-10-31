use super::storage::{KeyLocks, atomic_json, read_optional, sync_directory};
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, broadcast, watch};

use crate::json::fingerprint as digest;

pub struct MessageReceipts {
    directory: PathBuf,
    operations: KeyLocks,
}
impl MessageReceipts {
    pub fn new(home: PathBuf) -> Self {
        Self {
            directory: home.join("agent-requests"),
            operations: KeyLocks::default(),
        }
    }
    /// Local preparation happens before the pending receipt. Once delivery starts,
    /// every failure (including cancellation) deliberately leaves pending on disk.
    pub async fn send<T, P, PF, S, SF>(
        &self,
        agent_id: &str,
        message_id: &str,
        request: &Value,
        prepare: P,
        send: S,
    ) -> Result<()>
    where
        P: FnOnce() -> PF,
        PF: Future<Output = Result<T>>,
        S: FnOnce(T) -> SF,
        SF: Future<Output = Result<()>>,
    {
        let key = digest(&json!(["send", agent_id, message_id]))?;
        let _guard = self.operations.lock(&key).await;
        let path = self.directory.join(format!("{key}.json"));
        let fingerprint = digest(request)?;
        if let Some(existing) = read_send_receipt(&path).await? {
            if existing["fingerprint"].as_str() != Some(&fingerprint) {
                bail!("agent_request_key_conflict");
            }
            if existing["state"] == "completed" {
                return Ok(());
            }
            bail!("agent_request_outcome_unknown");
        }
        let prepared = prepare().await?;
        let mut receipt = json!({"fingerprint":fingerprint,"agentId":agent_id,"state":"pending"});
        atomic_json(&path, &receipt).await?;
        send(prepared).await?;
        receipt["state"] = json!("completed");
        atomic_json(&path, &receipt).await
    }
}
async fn read_send_receipt(path: &Path) -> Result<Option<Value>> {
    let Some(bytes) = read_optional(path).await? else {
        return Ok(None);
    };
    let value: Value = serde_json::from_slice(&bytes)?;
    if !value["fingerprint"].is_string()
        || !value["agentId"].is_string()
        || !matches!(value["state"].as_str(), Some("pending" | "completed"))
    {
        bail!("Invalid message receipt at {}", path.display());
    }
    Ok(Some(value))
}

#[derive(Clone, Debug)]
pub struct CreationInput {
    pub kind: String,
    pub key: String,
    pub request: Value,
    pub workspace_id: Option<String>,
    pub agent_id: Option<String>,
    pub has_agent: bool,
    pub has_prompt: bool,
}
#[async_trait]
pub trait CreationHooks: Send + Sync {
    async fn exists(&self, kind: &str, id: &str) -> Result<bool>;
    async fn read_agent(&self, id: &str) -> Result<Option<Value>>;
    async fn validate_completed(&self, snapshot: &Value) -> Result<()>;
}
pub enum CreationAdmission {
    Run(CreationGuard),
    Replay(Value),
    Running(CreationSubscription),
}
struct Active {
    generation: uuid::Uuid,
    sender: watch::Sender<Value>,
}
struct CreationInner {
    directory: PathBuf,
    legacy_directory: PathBuf,
    admission: Mutex<()>,
    accesses: KeyLocks,
    active: StdMutex<HashMap<String, Active>>,
    events: broadcast::Sender<Value>,
}
#[derive(Clone)]
pub struct CreationReceipts {
    inner: Arc<CreationInner>,
}
impl CreationReceipts {
    pub fn new(home: PathBuf) -> Self {
        let (events, _) = broadcast::channel(256);
        Self {
            inner: Arc::new(CreationInner {
                directory: home.join("creations"),
                legacy_directory: home.join("agent-requests"),
                admission: Mutex::new(()),
                accesses: KeyLocks::default(),
                active: StdMutex::new(HashMap::new()),
                events,
            }),
        }
    }
    pub fn subscribe_events(&self) -> broadcast::Receiver<Value> {
        self.inner.events.subscribe()
    }
    pub async fn begin(
        &self,
        input: &CreationInput,
        hooks: &dyn CreationHooks,
    ) -> Result<CreationAdmission> {
        validate_kind(&input.kind)?;
        let identity = identity_for(&input.kind, &input.key)?;
        let _admission = self.inner.admission.lock().await;
        tokio::fs::create_dir_all(&self.inner.directory).await?;
        let fingerprint = digest(&input.request)?;
        let (mut record, receiver) = self.inner.read_with_active(&identity).await?;
        if record.is_none() {
            record = self.import_legacy(input, &fingerprint, hooks).await?;
        }
        if record
            .as_ref()
            .is_some_and(|r| r["fingerprint"].as_str() != Some(&fingerprint))
        {
            bail!("{}_request_key_conflict", input.kind);
        }
        if let Some(record) = &record {
            if let Some(receiver) = receiver {
                return Ok(CreationAdmission::Running(CreationSubscription {
                    snapshot: Some(record["snapshot"].clone()),
                    receiver: Some(receiver),
                }));
            }
        }
        if let Some(record) = &mut record {
            if record["snapshot"]["phase"] == "completed" {
                hooks.validate_completed(&record["snapshot"]).await?;
            }
            if record["snapshot"]["phase"] == "completed"
                || record["snapshot"]["outcomeUnknown"] == true
            {
                return Ok(CreationAdmission::Replay(record["snapshot"].clone()));
            }
            if let Some(stage) = record["inFlight"].as_str().map(str::to_owned) {
                self.inner.publish(&identity, record, json!({"phase":"failed","error":format!("{}_request_outcome_unknown",input.kind),"failedStage":stage,"outcomeUnknown":true}), None, &[]).await?;
                return Ok(CreationAdmission::Replay(record["snapshot"].clone()));
            }
        }
        let mut record = match record {
            Some(record) => record,
            None => {
                let record = initial_record(input, &fingerprint);
                self.inner.write(&identity, &record).await?;
                record
            }
        };
        if input.kind == "workspace" {
            if let Some(id) = record["snapshot"]["workspaceId"].as_str() {
                self.claim(&identity, "workspace", id, hooks).await?;
            }
        }
        if let Some(id) = record["snapshot"]["agentId"].as_str() {
            self.claim(&identity, "agent", id, hooks).await?;
        }
        if record["snapshot"]["phase"] == "failed" {
            let phase = if record["snapshot"].get("workspace").is_some() {
                "workspace_ready"
            } else {
                "accepted"
            };
            self.inner
                .publish(
                    &identity,
                    &mut record,
                    json!({"phase":phase,"error":null}),
                    None,
                    &["errorCode", "failedStage", "outcomeUnknown"],
                )
                .await?;
        }
        let generation = uuid::Uuid::new_v4();
        let (sender, _) = watch::channel(record["snapshot"].clone());
        self.inner
            .active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(identity.clone(), Active { generation, sender });
        Ok(CreationAdmission::Run(CreationGuard {
            inner: self.inner.clone(),
            identity,
            record,
            generation,
        }))
    }
    /// Recovery reads serialize with admission and receipt writes, not with provider work.
    pub async fn subscribe(
        &self,
        kind: &str,
        key: &str,
        hooks: &dyn CreationHooks,
    ) -> Result<CreationSubscription> {
        validate_kind(kind)?;
        let identity = identity_for(kind, key)?;
        let _admission = self.inner.admission.lock().await;
        let (mut record, receiver) = self.inner.read_with_active(&identity).await?;
        if let Some(record) = &mut record {
            if receiver.is_none() {
                if let Some(stage) = record["inFlight"].as_str().map(str::to_owned) {
                    self.inner.publish(&identity, record, json!({"phase":"failed","failedStage":stage,"outcomeUnknown":true,"error":format!("{kind}_request_outcome_unknown")}), None, &[]).await?;
                }
            }
            if record["snapshot"]["phase"] == "completed" {
                hooks.validate_completed(&record["snapshot"]).await?;
            }
        }
        Ok(CreationSubscription {
            snapshot: record.map(|r| r["snapshot"].clone()),
            receiver,
        })
    }
    async fn claim(
        &self,
        identity: &str,
        kind: &str,
        id: &str,
        hooks: &dyn CreationHooks,
    ) -> Result<()> {
        let path = self
            .inner
            .directory
            .join(format!("{}.claim", digest(&json!([kind, id]))?));
        if let Some(owner) = read_optional(&path).await? {
            if owner != identity.as_bytes() {
                bail!("{kind}_id_conflict");
            }
            return Ok(());
        }
        if hooks.exists(kind, id).await? {
            bail!("{kind}_id_conflict");
        }
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = match options.open(&path).await {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if read_optional(&path).await?.as_deref() == Some(identity.as_bytes()) {
                    return Ok(());
                }
                bail!("{kind}_id_conflict");
            }
            Err(error) => return Err(error.into()),
        };
        file.write_all(identity.as_bytes()).await?;
        file.sync_all().await?;
        sync_directory(&self.inner.directory).await
    }
    async fn import_legacy(
        &self,
        input: &CreationInput,
        fingerprint: &str,
        hooks: &dyn CreationHooks,
    ) -> Result<Option<Value>> {
        if input.kind != "agent" {
            return Ok(None);
        }
        let path = self
            .inner
            .legacy_directory
            .join(format!("{}.json", digest(&json!(["create", input.key]))?));
        let Some(receipt) = read_send_receipt(&path).await? else {
            return Ok(None);
        };
        let mut legacy_request = json!({"type":"create_agent_request"});
        for (key, value) in input
            .request
            .as_object()
            .context("Creation request must be an object")?
        {
            legacy_request[key] = value.clone();
        }
        if receipt["fingerprint"].as_str() != Some(&digest(&legacy_request)?) {
            bail!("agent_request_key_conflict");
        }
        let agent_id = receipt["agentId"].as_str().unwrap();
        let agent = hooks.read_agent(agent_id).await?;
        if agent.is_none() && receipt["state"] == "completed" {
            bail!("Previously created agent no longer exists");
        }
        let mut imported = input.clone();
        imported.agent_id = Some(agent_id.to_owned());
        let mut record = initial_record(&imported, fingerprint);
        if let Some(agent) = agent {
            record["snapshot"]["phase"] = json!("completed");
            if let Some(workspace_id) = agent.get("workspaceId").filter(|v| !v.is_null()) {
                record["snapshot"]["workspaceId"] = workspace_id.clone();
            }
            record["snapshot"]["agent"] = agent;
        } else {
            record["snapshot"]["phase"] = json!("failed");
            record["snapshot"]["error"] = json!("agent_request_outcome_unknown");
            record["snapshot"]["failedStage"] = json!("agent");
            record["snapshot"]["outcomeUnknown"] = json!(true);
        }
        self.inner
            .write(&identity_for(&input.kind, &input.key)?, &record)
            .await?;
        Ok(Some(record))
    }
}
impl CreationInner {
    fn active_receiver(&self, identity: &str) -> Option<watch::Receiver<Value>> {
        self.active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(identity)
            .map(|active| active.sender.subscribe())
    }
    async fn read_with_active(
        &self,
        identity: &str,
    ) -> Result<(Option<Value>, Option<watch::Receiver<Value>>)> {
        // Register the watcher while replacement is excluded: otherwise a final
        // milestone can land between reading the old snapshot and subscribing.
        let _access = self.accesses.lock(identity).await;
        let Some(bytes) = read_optional(&self.directory.join(format!("{identity}.json"))).await?
        else {
            return Ok((None, None));
        };
        let record: Value = serde_json::from_slice(&bytes)?;
        validate_creation_record(&record)?;
        Ok((Some(record), self.active_receiver(identity)))
    }
    async fn write(&self, identity: &str, record: &Value) -> Result<()> {
        let _access = self.accesses.lock(identity).await;
        atomic_json(&self.directory.join(format!("{identity}.json")), record).await
    }
    /// None leaves inFlight unchanged; Some(None) clears it.
    async fn publish(
        &self,
        identity: &str,
        record: &mut Value,
        update: Value,
        in_flight: Option<Option<&str>>,
        remove: &[&str],
    ) -> Result<()> {
        let mut next = record.clone();
        if let Some(stage) = in_flight {
            next["inFlight"] = json!(stage);
        }
        let snapshot = next["snapshot"]
            .as_object_mut()
            .context("Invalid creation snapshot")?;
        let revision = snapshot["revision"]
            .as_u64()
            .context("Invalid creation revision")?;
        for key in remove {
            snapshot.remove(*key);
        }
        for (key, value) in update.as_object().context("Invalid creation update")? {
            snapshot.insert(key.clone(), value.clone());
        }
        snapshot.insert(
            "revision".into(),
            json!(
                revision
                    .checked_add(1)
                    .context("Creation revision overflow")?
            ),
        );
        self.write(identity, &next).await?;
        *record = next;
        let snapshot = &record["snapshot"];
        if let Some(active) = self
            .active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(identity)
        {
            active.sender.send_replace(snapshot.clone());
        }
        let _ = self.events.send(snapshot.clone());
        Ok(())
    }
}

pub struct CreationGuard {
    inner: Arc<CreationInner>,
    identity: String,
    record: Value,
    generation: uuid::Uuid,
}
impl CreationGuard {
    pub fn snapshot(&self) -> &Value {
        &self.record["snapshot"]
    }
    pub async fn begin_stage(&mut self, stage: &str) -> Result<()> {
        if !matches!(stage, "workspace" | "agent" | "prompt") {
            bail!("Invalid creation stage");
        }
        let mut next = self.record.clone();
        next["inFlight"] = json!(stage);
        self.inner.write(&self.identity, &next).await?;
        self.record = next;
        Ok(())
    }
    pub async fn workspace_ready(
        &mut self,
        workspace: Value,
        setup_skipped_reason: Option<String>,
    ) -> Result<()> {
        let id = workspace["id"].as_str().context("Workspace id missing")?;
        let mut update =
            json!({"phase":"workspace_ready", "workspaceId":id, "workspace":workspace});
        if let Some(reason) = setup_skipped_reason {
            update["setupSkippedReason"] = json!(reason);
        }
        self.inner
            .publish(&self.identity, &mut self.record, update, Some(None), &[])
            .await
    }
    pub async fn agent_ready(&mut self, agent: Value, has_prompt: bool) -> Result<()> {
        let mut update = json!({"phase":"agent_ready"});
        if let Some(id) = agent.get("workspaceId").filter(|v| !v.is_null()) {
            update["workspaceId"] = id.clone();
        }
        update["agent"] = agent;
        self.inner
            .publish(
                &self.identity,
                &mut self.record,
                update,
                Some(if has_prompt { Some("prompt") } else { None }),
                &[],
            )
            .await
    }
    pub async fn prompt_started(&mut self, agent: Value) -> Result<()> {
        self.inner
            .publish(
                &self.identity,
                &mut self.record,
                json!({"phase":"prompt_started","agent":agent}),
                Some(None),
                &[],
            )
            .await
    }
    pub async fn complete(&mut self, agent: Option<Value>) -> Result<()> {
        let mut update = json!({"phase":"completed"});
        if let Some(agent) = agent {
            update["agent"] = agent;
        }
        self.inner
            .publish(&self.identity, &mut self.record, update, Some(None), &[])
            .await
    }
    pub async fn fail(
        &mut self,
        error: &str,
        code: Option<&str>,
        hooks: &dyn CreationHooks,
    ) -> Result<()> {
        let stage = self.record["inFlight"].as_str().unwrap_or("agent");
        let id = self.snapshot()[if stage == "workspace" {
            "workspaceId"
        } else {
            "agentId"
        }]
        .as_str();
        let rejected = matches!(code, Some("directory_not_found" | "source_required"));
        let unknown = stage == "prompt"
            || (stage == "workspace" && !rejected)
            || match id {
                Some(id) => hooks.exists(stage, id).await?,
                None => false,
            };
        let mut update =
            json!({"phase":"failed","error":error,"failedStage":stage,"outcomeUnknown":unknown});
        if let Some(code) = code {
            update["errorCode"] = json!(code);
        }
        self.inner
            .publish(
                &self.identity,
                &mut self.record,
                update,
                if unknown { None } else { Some(None) },
                if code.is_none() { &["errorCode"] } else { &[] },
            )
            .await
    }
}
impl Drop for CreationGuard {
    fn drop(&mut self) {
        let mut active = self.inner.active.lock().unwrap_or_else(|e| e.into_inner());
        if active
            .get(&self.identity)
            .is_some_and(|entry| entry.generation == self.generation)
        {
            active.remove(&self.identity);
        }
    }
}
pub struct CreationSubscription {
    pub snapshot: Option<Value>,
    receiver: Option<watch::Receiver<Value>>,
}
impl CreationSubscription {
    pub async fn changed(&mut self) -> Result<Option<Value>> {
        let Some(receiver) = &mut self.receiver else {
            return Ok(None);
        };
        if receiver.changed().await.is_err() {
            self.receiver = None;
            return Ok(None);
        }
        let snapshot = receiver.borrow_and_update().clone();
        self.snapshot = Some(snapshot.clone());
        Ok(Some(snapshot))
    }
    pub async fn completion(&mut self) -> Result<Value> {
        loop {
            if let Some(snapshot) = &self.snapshot {
                if matches!(snapshot["phase"].as_str(), Some("completed" | "failed")) {
                    return Ok(snapshot.clone());
                }
            }
            if self.changed().await?.is_none() {
                bail!("Creation operation ended before committing its outcome");
            }
        }
    }
}
fn initial_record(input: &CreationInput, fingerprint: &str) -> Value {
    let workspace_id = input.workspace_id.clone().or_else(|| {
        (input.kind == "workspace")
            .then(|| format!("wks_{}", hex::encode(rand::random::<[u8; 8]>())))
    });
    let agent_id = input.has_agent.then(|| {
        input
            .agent_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
    });
    json!({"fingerprint":fingerprint, "inFlight":null, "snapshot":{
        "kind":input.kind,"idempotencyKey":input.key,"revision":0,"phase":"accepted","error":null,"workspaceId":workspace_id,"agentId":agent_id
    }})
}
fn validate_kind(kind: &str) -> Result<()> {
    if !matches!(kind, "agent" | "workspace") {
        bail!("Invalid creation kind");
    }
    Ok(())
}
fn identity_for(kind: &str, key: &str) -> Result<String> {
    digest(&json!([kind, key]))
}
fn validate_creation_record(record: &Value) -> Result<()> {
    if !record["fingerprint"].is_string()
        || !record.get("inFlight").is_some_and(|v| {
            v.is_null() || matches!(v.as_str(), Some("workspace" | "agent" | "prompt"))
        })
    {
        bail!("Invalid creation receipt");
    }
    let snapshot = &record["snapshot"];
    validate_kind(snapshot["kind"].as_str().context("Creation kind missing")?)?;
    if !snapshot["idempotencyKey"].is_string()
        || snapshot["revision"].as_u64().is_none()
        || !matches!(
            snapshot["phase"].as_str(),
            Some(
                "accepted"
                    | "workspace_ready"
                    | "agent_ready"
                    | "prompt_started"
                    | "completed"
                    | "failed"
            )
        )
    {
        bail!("Invalid creation snapshot");
    }
    for field in ["workspaceId", "agentId", "error"] {
        if !snapshot
            .get(field)
            .is_some_and(|v| v.is_null() || v.is_string())
        {
            bail!("Invalid creation {field}");
        }
    }
    for field in ["setupSkippedReason", "errorCode"] {
        if snapshot.get(field).is_some_and(|v| !v.is_string()) {
            bail!("Invalid creation {field}");
        }
    }
    for field in ["workspace", "agent"] {
        if snapshot
            .get(field)
            .is_some_and(|v| !v.is_object() || !v["id"].is_string())
        {
            bail!("Invalid creation {field} snapshot");
        }
    }
    if snapshot
        .get("failedStage")
        .is_some_and(|v| !matches!(v.as_str(), Some("workspace" | "agent" | "prompt")))
        || snapshot
            .get("outcomeUnknown")
            .is_some_and(|v| !v.is_boolean())
    {
        bail!("Invalid creation failure outcome");
    }
    Ok(())
}

#[cfg(test)]
#[path = "receipts/tests.rs"]
mod tests;
