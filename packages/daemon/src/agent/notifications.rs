mod attention;
mod delivery;
mod envelope;
mod observation;
mod push;

use crate::state::AppState;
use anyhow::{Context, Result, bail};
pub use delivery::{AgentMessageDelivery, MessageReceipt};
pub use envelope::{agent_message_command, prepare_agent_message, project_agent_messages};
use observation::{Observation, Outcome};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::{Mutex, Notify, watch};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FinishSubscription {
    id: String,
    caller_agent_id: String,
    child_agent_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    server_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    require_parent_ownership: Option<bool>,
}
type Pair = (String, String, Option<String>);
impl FinishSubscription {
    fn key(&self) -> Pair {
        (
            self.caller_agent_id.clone(),
            self.child_agent_id.clone(),
            self.server_id.clone(),
        )
    }
    fn validate(&self) -> Result<()> {
        if self.id.is_empty()
            || self.caller_agent_id.is_empty()
            || self.child_agent_id.is_empty()
            || self.server_id.as_ref().is_some_and(String::is_empty)
        {
            bail!("Invalid finish subscription record");
        }
        Ok(())
    }
}
struct Watch {
    records: Vec<FinishSubscription>,
    cancel: CancellationToken,
    pending: AtomicBool,
    decision: watch::Sender<Option<bool>>,
}
impl Watch {
    fn key(&self) -> Pair {
        self.records[0].key()
    }
    fn record(&self) -> FinishSubscription {
        let mut record = self.records[0].clone();
        record.require_parent_ownership = Some(
            self.records
                .iter()
                .all(|r| r.require_parent_ownership == Some(true)),
        );
        record
    }
    fn stop(&self) {
        self.pending.store(false, Ordering::Release);
        self.cancel.cancel();
        self.decision.send_replace(Some(false));
    }
}
#[derive(Default)]
struct State {
    initialized: bool,
    closed: bool,
    path: PathBuf,
    server_id: String,
    records: Vec<FinishSubscription>,
    watches: BTreeMap<Pair, Vec<Arc<Watch>>>,
    tasks: usize,
}
struct Inner {
    app: Weak<AppState>,
    state: Mutex<State>,
    changed: Notify,
    drained: Notify,
}
#[derive(Clone)]
pub struct FinishSubscriptions {
    inner: Arc<Inner>,
    push: push::PushNotifications,
}
#[derive(Clone)]
pub struct FinishRegistration {
    inner: Weak<Inner>,
    watch: Arc<Watch>,
    predecessors: Vec<Arc<Watch>>,
}
impl FinishSubscriptions {
    pub fn new(app: Weak<AppState>) -> Self {
        Self {
            push: push::PushNotifications::new(app.clone()),
            inner: Arc::new(Inner {
                app,
                state: Mutex::new(State::default()),
                changed: Notify::new(),
                drained: Notify::new(),
            }),
        }
    }
    pub async fn initialize(&self) -> Result<()> {
        let mut state = self.inner.state.lock().await;
        if state.initialized {
            return Ok(());
        }
        if state.closed {
            bail!("Finish subscriptions are closed");
        }
        self.push.initialize().await?;
        let (path, server_id) = {
            let app = self
                .inner
                .app
                .upgrade()
                .context("Daemon is shutting down")?;
            (
                app.home.join("finish-subscriptions.json"),
                app.transport.identity()["serverId"]
                    .as_str()
                    .context("Daemon server identity unavailable")?
                    .to_owned(),
            )
        };
        let records: Vec<FinishSubscription> = match super::storage::read_optional(&path).await? {
            Some(bytes) => {
                serde_json::from_slice(&bytes).context("Invalid finish subscription store")?
            }
            None => Vec::new(),
        };
        for record in &records {
            record.validate()?;
        }
        let mut groups: BTreeMap<Pair, Vec<FinishSubscription>> = BTreeMap::new();
        for record in &records {
            groups.entry(record.key()).or_default().push(record.clone());
        }
        state.records = records;
        state.path = path;
        state.server_id = server_id;
        state.initialized = true;
        for records in groups.into_values() {
            attach(&self.inner, &mut state, records, false);
        }
        Ok(())
    }
    pub async fn register(&self, mut input: Value, pending: bool) -> Result<FinishRegistration> {
        self.initialize().await?;
        input
            .as_object_mut()
            .context("Finish subscription must be an object")?
            .insert("id".into(), json!(uuid::Uuid::new_v4().to_string()));
        let mut record: FinishSubscription = serde_json::from_value(input)?;
        record.validate()?;
        let mut state = self.inner.state.lock().await;
        if state.closed {
            bail!("Finish subscriptions are closed");
        }
        if record.server_id.as_deref() == Some(state.server_id.as_str()) {
            record.server_id = None;
        }
        let mut records = state.records.clone();
        records.push(record.clone());
        super::storage::atomic_json(&state.path, &serde_json::to_value(&records)?).await?;
        state.records = records;
        let predecessors = state
            .watches
            .get(&record.key())
            .cloned()
            .unwrap_or_default();
        let watch = attach(&self.inner, &mut state, vec![record], true);
        let registration = FinishRegistration {
            inner: Arc::downgrade(&self.inner),
            watch,
            predecessors,
        };
        drop(state);
        if !pending {
            registration.accept().await?;
        }
        Ok(registration)
    }
    pub async fn shutdown(&self) -> Result<()> {
        {
            let mut state = self.inner.state.lock().await;
            state.closed = true;
            for watches in state.watches.values() {
                for watch in watches {
                    watch.stop();
                }
            }
            self.inner.changed.notify_waiters();
        }
        loop {
            let drained = self.inner.drained.notified();
            tokio::pin!(drained);
            drained.as_mut().enable();
            if self.inner.state.lock().await.tasks == 0 {
                break;
            }
            drained.await;
        }
        self.push.shutdown().await;
        Ok(())
    }
}
impl FinishRegistration {
    pub async fn accept(&self) -> Result<()> {
        let Some(inner) = self.inner.upgrade() else {
            return Ok(());
        };
        let mut state = inner.state.lock().await;
        if state.closed || !self.watch.pending.swap(false, Ordering::AcqRel) {
            return Ok(());
        }
        for prior in &self.predecessors {
            stop(&mut state, prior);
        }
        self.watch.decision.send_replace(Some(true));
        inner.changed.notify_waiters();
        let ids: HashSet<&str> = self
            .predecessors
            .iter()
            .flat_map(|w| w.records.iter().map(|r| r.id.as_str()))
            .collect();
        // Acceptance is irrevocable even if retiring an older durable row fails. Recovery
        // coalesces those rows, and an older observer cannot remove this new identity.
        if let Err(error) = remove_ids(&mut state, &ids).await {
            tracing::error!(?error, "Failed to remove superseded finish subscriptions");
        }
        Ok(())
    }
    pub async fn reject(&self) -> Result<()> {
        let Some(inner) = self.inner.upgrade() else {
            return Ok(());
        };
        let mut state = inner.state.lock().await;
        stop(&mut state, &self.watch);
        inner.changed.notify_waiters();
        let ids = self.watch.records.iter().map(|r| r.id.as_str()).collect();
        remove_ids(&mut state, &ids).await
    }
}
fn attach(
    inner: &Arc<Inner>,
    state: &mut State,
    records: Vec<FinishSubscription>,
    pending: bool,
) -> Arc<Watch> {
    let (decision, _) = watch::channel(if pending { None } else { Some(true) });
    let watch = Arc::new(Watch {
        records,
        cancel: CancellationToken::new(),
        pending: AtomicBool::new(pending),
        decision,
    });
    state
        .watches
        .entry(watch.key())
        .or_default()
        .push(watch.clone());
    state.tasks += 1;
    let task_watch = watch.clone();
    let task_inner = inner.clone();
    tokio::spawn(async move {
        run(&task_inner, &task_watch).await;
        let mut state = task_inner.state.lock().await;
        state.tasks -= 1;
        task_inner.drained.notify_waiters();
    });
    watch
}
fn stop(state: &mut State, watch: &Arc<Watch>) {
    watch.stop();
    let key = watch.key();
    if let Some(group) = state.watches.get_mut(&key) {
        group.retain(|other| !Arc::ptr_eq(other, watch));
        if group.is_empty() {
            state.watches.remove(&key);
        }
    }
}
async fn remove_ids(state: &mut State, ids: &HashSet<&str>) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let records: Vec<_> = state
        .records
        .iter()
        .filter(|r| !ids.contains(r.id.as_str()))
        .cloned()
        .collect();
    super::storage::atomic_json(&state.path, &serde_json::to_value(&records)?).await?;
    state.records = records;
    Ok(())
}
async fn remove(inner: &Inner, watch: &Arc<Watch>) -> Result<()> {
    let mut state = inner.state.lock().await;
    let ids = watch.records.iter().map(|r| r.id.as_str()).collect();
    remove_ids(&mut state, &ids).await?;
    stop(&mut state, watch);
    inner.changed.notify_waiters();
    Ok(())
}
async fn decisions(inner: &Inner, watch: &Watch) -> Result<()> {
    loop {
        let changed = inner.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        {
            let state = inner.state.lock().await;
            if state.closed || watch.cancel.is_cancelled() {
                bail!("Finish subscriptions are closed");
            }
            if !state
                .watches
                .get(&watch.key())
                .is_some_and(|group| group.iter().any(|w| w.pending.load(Ordering::Acquire)))
            {
                return Ok(());
            }
        }
        tokio::select! { _ = watch.cancel.cancelled() => bail!("Finish subscription canceled"), _ = changed => {} }
    }
}
async fn suppressed(
    inner: &Inner,
    record: &FinishSubscription,
    outcome: Option<&Outcome>,
) -> Result<bool> {
    let manager = inner
        .app
        .upgrade()
        .context("Daemon is shutting down")?
        .agents
        .clone();
    let caller = manager.agent_snapshot(&record.caller_agent_id).await?;
    // bootstrap.ts callerIsActive: a caller is active while it exists and is not archived.
    if caller.as_ref().is_none_or(|s| s["archivedAt"].is_string()) {
        return Ok(true);
    }
    if record.require_parent_ownership != Some(true) {
        return Ok(false);
    }
    let Some(snapshot) = outcome.and_then(|o| o.final_snapshot.as_ref()) else {
        return Ok(false);
    };
    let local_server = inner.state.lock().await.server_id.clone();
    let parent_id = snapshot["labels"]["paseo.parent-agent-id"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let parent_server = snapshot["labels"]["paseo.parent-server-id"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(record.server_id.as_deref().unwrap_or(&local_server));
    Ok(parent_id != Some(record.caller_agent_id.as_str()) || parent_server != local_server)
}
fn reason(outcome: &Outcome) -> &'static str {
    if let Some(reason @ ("host unreachable" | "host removed")) = outcome.reason {
        return reason;
    }
    let Some(snapshot) = &outcome.final_snapshot else {
        return "is unavailable";
    };
    if snapshot["status"] == "closed" {
        "was closed"
    } else if outcome.status == "error" {
        "errored"
    } else if outcome.status == "permission" {
        "needs permission"
    } else {
        outcome.reason.unwrap_or("finished")
    }
}
fn notification_body(
    record: &FinishSubscription,
    server: &str,
    title: &str,
    outcome: &Outcome,
    permission: Option<&Value>,
) -> String {
    let reason = reason(outcome);
    if matches!(reason, "host unreachable" | "host removed") {
        let host = if reason == "host removed" {
            format!("Host {server} was removed or is unavailable.")
        } else {
            format!(
                "Could not observe child agent {} on host {server} after three failed attempts.",
                record.child_agent_id
            )
        };
        return format!(
            "{host}\n\nStatus of child agent {} ({title}) is unknown.\n\nServer: {server}\n\nThis finish subscription has ended. Reassess the delegated work; reconnecting will not resume notifications.",
            record.child_agent_id
        );
    }
    let mut sections = vec![
        format!("Agent {} ({title}) {reason}.", record.child_agent_id),
        format!("Server: {server}"),
    ];
    if reason == "needs permission" {
        if let Some(permission) = permission {
            sections.push(
                "Respond with `respond_to_permission` using the `agentId` and `requestId` below."
                    .into(),
            );
            let request = json!({"agentId":record.child_agent_id,"serverId":server,"requestId":permission["id"],"request":permission});
            sections.push(format!(
                "<permission-request>\n{}\n</permission-request>",
                serde_json::to_string_pretty(&request).expect("JSON value serialization")
            ));
        }
    }
    if let Some(text) = outcome
        .last_message
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let utf16_length = text.encode_utf16().count();
        let text = if utf16_length > 4000 {
            let prefix: Vec<u16> = text.encode_utf16().take(4000).collect();
            format!(
                "{}\n[truncated {} chars; use get_agent_activity for the full response]",
                String::from_utf16_lossy(&prefix),
                utf16_length - 4000
            )
        } else {
            text.to_owned()
        };
        sections.push(format!("<agent-response>\n{text}\n</agent-response>"));
    }
    sections.join("\n\n")
}
async fn deliver(
    inner: &Inner,
    record: &FinishSubscription,
    outcome: &Outcome,
    permissions: &mut HashSet<String>,
    cancel: &CancellationToken,
) -> Result<bool> {
    let requests = outcome
        .final_snapshot
        .as_ref()
        .and_then(|s| s["pendingPermissions"].as_array())
        .map(Vec::as_slice)
        .unwrap_or_default();
    permissions.retain(|id| {
        requests
            .iter()
            .any(|r| r["id"].as_str() == Some(id.as_str()))
    });
    let reason = reason(outcome);
    let local_server = inner.state.lock().await.server_id.clone();
    let server = record.server_id.as_deref().unwrap_or(&local_server);
    let title = outcome
        .final_snapshot
        .as_ref()
        .and_then(|s| s["title"].as_str())
        .unwrap_or(&record.child_agent_id);
    let event = match reason {
        "needs permission" => "permission-required",
        "was closed" | "is unavailable" => "closed",
        "finished" => "finished",
        _ => "errored",
    };
    let queue = inner
        .app
        .upgrade()
        .context("Daemon is shutting down")?
        .agents
        .message_delivery
        .clone();
    let messages: Vec<Option<&Value>> = if reason == "needs permission" {
        requests.iter().map(Some).collect()
    } else {
        vec![None]
    };
    let mut receipts = Vec::new();
    for request in messages {
        if cancel.is_cancelled() {
            bail!("Finish subscription canceled");
        }
        let request_id = request.and_then(|r| r["id"].as_str());
        if request_id.is_some_and(|id| permissions.contains(id)) {
            continue;
        }
        let input = json!({"recipientId":record.caller_agent_id,"prompt":notification_body(record, server, title, outcome, request),
            "source":{"kind":"agent-notification","agentId":record.child_agent_id,"serverId":server,"title":title,"event":event},
            "activeTurnBehavior":"steer","unarchive":false});
        let receipt = tokio::select! { _ = cancel.cancelled() => bail!("Finish subscription canceled"), receipt = queue.enqueue(input) => receipt? };
        receipts.push((request_id, receipt));
    }
    // A successful permission is remembered even when another request in this checkpoint fails.
    let mut failure = None;
    for (id, receipt) in receipts {
        match receipt.dispatched(cancel.clone()).await {
            Ok(_) => {
                if let Some(id) = id {
                    permissions.insert(id.to_owned());
                }
            }
            Err(error) => {
                failure.get_or_insert(error);
            }
        }
    }
    if let Some(error) = failure {
        return Err(error);
    }
    if cancel.is_cancelled() {
        bail!("Finish subscription canceled");
    }
    Ok(reason != "needs permission")
}
async fn remote_failure(inner: &Inner, server_id: &str) -> Result<&'static str> {
    let transport = inner
        .app
        .upgrade()
        .context("Daemon is shutting down")?
        .transport
        .clone();
    let directory = transport.hosts.list().await?;
    let row = directory["hosts"]
        .as_array()
        .and_then(|rows| rows.iter().find(|r| r["serverId"] == server_id));
    Ok(match row {
        None => "removed",
        Some(row) if row["status"] == "connecting" => "connecting",
        _ => "unreachable",
    })
}
async fn run(inner: &Arc<Inner>, watch: &Arc<Watch>) {
    let mut decision = watch.decision.subscribe();
    loop {
        match *decision.borrow_and_update() {
            Some(true) => break,
            Some(false) => return,
            None => {}
        }
        tokio::select! { _ = watch.cancel.cancelled() => return, changed = decision.changed() => if changed.is_err() { return; } }
    }
    let record = watch.record();
    let mut observation: Option<Observation> = None;
    let mut pending: Option<Outcome> = None;
    let mut acknowledged = false;
    let mut permissions = HashSet::new();
    let mut remote_failures = 0;
    let mut retries = 0u32;
    let mut warned = false;
    while !watch.cancel.is_cancelled() {
        let attempt = async {
            if suppressed(inner, &record, None).await? {
                remove(inner, watch).await?;
                return Ok(true);
            }
            if pending.is_none() {
                if observation.is_none() {
                    observation = Some(Observation::open(
                        inner.app.clone(),
                        record.clone(),
                        &watch.cancel,
                    ));
                }
                match observation.as_ref().unwrap().next().await {
                    Ok(result) => {
                        pending = Some(result);
                        remote_failures = 0;
                    }
                    Err(error) => {
                        if let Some(observation) = observation.take() {
                            observation.close().await;
                        }
                        if watch.cancel.is_cancelled() {
                            return Err(error);
                        }
                        let Some(server) = &record.server_id else {
                            return Err(error);
                        };
                        let reason = remote_failure(inner, server).await?;
                        if reason == "connecting" {
                            return Err(error);
                        }
                        if reason == "unreachable" {
                            remote_failures += 1;
                            if remote_failures < 3 {
                                return Err(error);
                            }
                        }
                        pending = Some(Outcome {
                            status: "error".into(),
                            final_snapshot: None,
                            last_message: None,
                            reason: Some(if reason == "removed" {
                                "host removed"
                            } else {
                                "host unreachable"
                            }),
                        });
                    }
                }
            }
            decisions(inner, watch).await?;
            if suppressed(inner, &record, pending.as_ref()).await? {
                remove(inner, watch).await?;
                return Ok(true);
            }
            if !acknowledged {
                acknowledged = deliver(
                    inner,
                    &record,
                    pending.as_ref().unwrap(),
                    &mut permissions,
                    &watch.cancel,
                )
                .await?;
            }
            if acknowledged {
                remove(inner, watch).await?;
                return Ok(true);
            }
            pending = None;
            Ok::<_, anyhow::Error>(false)
        }
        .await;
        match attempt {
            Ok(true) => break,
            Ok(false) => {
                retries = 0;
                warned = false;
            }
            Err(error) => {
                if watch.cancel.is_cancelled() {
                    break;
                }
                if !warned {
                    tracing::warn!(
                        ?error,
                        subscription = record.id,
                        "Retrying finish subscription"
                    );
                    warned = true;
                }
                let delay = std::time::Duration::from_millis((1000u64 << retries.min(3)).min(5000));
                retries = retries.saturating_add(1);
                tokio::select! { _ = watch.cancel.cancelled() => break, _ = tokio::time::sleep(delay) => {} }
            }
        }
    }
    if let Some(observation) = observation {
        observation.close().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn record() -> FinishSubscription {
        FinishSubscription {
            id: "intent".into(),
            caller_agent_id: "parent".into(),
            child_agent_id: "child".into(),
            server_id: None,
            require_parent_ownership: Some(true),
        }
    }
    #[tokio::test]
    async fn pending_rejection_preserves_prior_intent_and_acceptance_supersedes_only_captured_ids()
    {
        let home = tempfile::tempdir().unwrap();
        let service = FinishSubscriptions::new(Weak::new());
        let path = home.path().join("finish-subscriptions.json");
        {
            let mut state = service.inner.state.lock().await;
            state.initialized = true;
            state.path = path.clone();
            state.server_id = "local".into();
        }
        let input = json!({"callerAgentId":"parent","childAgentId":"child","serverId":"local"});
        let first = service.register(input.clone(), true).await.unwrap();
        let candidate = service.register(input.clone(), true).await.unwrap();
        let stored: Vec<FinishSubscription> =
            serde_json::from_slice(&tokio::fs::read(&path).await.unwrap()).unwrap();
        assert_eq!(stored.len(), 2);
        assert!(stored.iter().all(|r| r.server_id.is_none()));
        assert_eq!(*candidate.watch.decision.borrow(), None);
        candidate.reject().await.unwrap();
        assert!(!first.watch.cancel.is_cancelled());
        assert_eq!(service.inner.state.lock().await.records.len(), 1);
        let accepted = service.register(input, true).await.unwrap();
        accepted.accept().await.unwrap();
        assert!(first.watch.cancel.is_cancelled());
        assert_eq!(*accepted.watch.decision.borrow(), Some(true));
        remove(&service.inner, &first.watch).await.unwrap();
        assert_eq!(
            service.inner.state.lock().await.records[0].id,
            accepted.watch.records[0].id
        );
        service.shutdown().await.unwrap();
        let stored: Vec<FinishSubscription> =
            serde_json::from_slice(&tokio::fs::read(&path).await.unwrap()).unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].id, accepted.watch.records[0].id);
    }
    #[test]
    fn recovery_coalesces_conditional_and_unconditional_intents() {
        let first = record();
        let mut next = first.clone();
        next.id = "next".into();
        next.require_parent_ownership = None;
        let (decision, _) = watch::channel(Some(true));
        let watch = Watch {
            records: vec![first, next],
            cancel: CancellationToken::new(),
            pending: AtomicBool::new(false),
            decision,
        };
        assert_eq!(watch.record().require_parent_ownership, Some(false));
    }
    #[test]
    fn finish_notices_preserve_failure_meaning_and_limit_response() {
        let mut outcome = Outcome {
            status: "error".into(),
            final_snapshot: None,
            last_message: None,
            reason: Some("host unreachable"),
        };
        let body = notification_body(&record(), "remote", "Worker", &outcome, None);
        assert!(body.contains("after three failed attempts"));
        assert!(body.contains("is unknown"));
        assert!(!body.contains("finished."));
        outcome.reason = None;
        assert_eq!(reason(&outcome), "is unavailable");
        outcome.final_snapshot = Some(json!({"status":"idle"}));
        outcome.status = "idle".into();
        outcome.last_message = Some("x".repeat(4100));
        let body = notification_body(&record(), "local", "Worker", &outcome, None);
        assert!(body.contains("truncated 100 chars"));
        assert!(!body.contains(&"x".repeat(4001)));
    }
}
