use super::FinishSubscription;
use crate::{state::AppState, transport::hosts::HostClient};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    collections::{HashSet, VecDeque},
    sync::{Arc, Weak},
};
use tokio::sync::{Mutex, Notify};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug)]
pub(super) struct Outcome {
    pub status: String,
    pub final_snapshot: Option<Value>,
    pub last_message: Option<String>,
    pub reason: Option<&'static str>,
}
impl Outcome {
    fn from_response(value: &Value) -> Result<Self> {
        let value = value.get("payload").unwrap_or(value);
        let final_snapshot = value.get("final").filter(|v| !v.is_null()).cloned();
        if final_snapshot.is_none() {
            if let Some(error) = value["error"]
                .as_str()
                .filter(|e| !e.starts_with("Agent not found: "))
            {
                bail!("{error}");
            }
        }
        Ok(Self {
            status: value["status"]
                .as_str()
                .context("Invalid finish observation status")?
                .to_owned(),
            final_snapshot,
            last_message: value["lastMessage"].as_str().map(str::to_owned),
            reason: None,
        })
    }
}
struct BufferState {
    delivered: HashSet<String>,
    permissions: VecDeque<(String, Outcome)>,
    resolved_before_initial: HashSet<String>,
    initial_pending: bool,
    terminal: Option<Outcome>,
    failure: Option<String>,
}
impl Default for BufferState {
    fn default() -> Self {
        Self {
            delivered: HashSet::new(),
            permissions: VecDeque::new(),
            resolved_before_initial: HashSet::new(),
            initial_pending: true,
            terminal: None,
            failure: None,
        }
    }
}
impl BufferState {
    fn push(&mut self, result: Outcome) {
        if result.status == "timeout" {
            return;
        }
        if result.status == "permission" {
            for request in result
                .final_snapshot
                .as_ref()
                .and_then(|s| s["pendingPermissions"].as_array())
                .into_iter()
                .flatten()
            {
                if let Some(id) = request["id"].as_str() {
                    if self.delivered.contains(id) {
                        continue;
                    }
                    if let Some((_, prior)) = self.permissions.iter_mut().find(|(key, _)| key == id)
                    {
                        *prior = result.clone();
                    } else {
                        self.permissions.push_back((id.to_owned(), result.clone()));
                    }
                }
            }
        } else {
            self.terminal = Some(result);
        }
    }
    fn initial(&mut self, mut result: Outcome) {
        self.initial_pending = false;
        if result.status == "permission" {
            if let Some(snapshot) = &mut result.final_snapshot {
                if let Some(requests) = snapshot["pendingPermissions"].as_array_mut() {
                    requests.retain(|r| {
                        !r["id"]
                            .as_str()
                            .is_some_and(|id| self.resolved_before_initial.contains(id))
                    });
                    if !requests.is_empty() {
                        self.push(result);
                    }
                }
            }
        } else if self.terminal.is_none()
            && !result
                .final_snapshot
                .as_ref()
                .is_some_and(|s| matches!(s["status"].as_str(), Some("running" | "initializing")))
        {
            self.push(result);
        }
        self.resolved_before_initial.clear();
    }
    fn resolved(&mut self, id: &str) {
        if self.initial_pending {
            self.resolved_before_initial.insert(id.to_owned());
        }
        self.delivered.remove(id);
        self.permissions.retain(|(key, _)| key != id);
    }
    fn next(&mut self) -> Option<Outcome> {
        if let Some((_, result)) = self.permissions.pop_front() {
            for request in result
                .final_snapshot
                .as_ref()
                .and_then(|s| s["pendingPermissions"].as_array())
                .into_iter()
                .flatten()
            {
                if let Some(id) = request["id"].as_str() {
                    self.delivered.insert(id.to_owned());
                    self.permissions.retain(|(key, _)| key != id);
                }
            }
            return Some(result);
        }
        self.terminal.take()
    }
    fn observe(
        &mut self,
        snapshot: Option<Value>,
        event: Option<&Value>,
        last_message: Option<String>,
    ) {
        let Some(mut snapshot) = snapshot else {
            self.push(Outcome {
                status: "idle".into(),
                final_snapshot: None,
                last_message: None,
                reason: None,
            });
            return;
        };
        let event_type = event.and_then(|e| e["type"].as_str());
        if event_type == Some("permission_resolved") {
            if let Some(id) = event.and_then(|e| e["requestId"].as_str()) {
                self.resolved(id);
            }
            return;
        }
        if event_type == Some("turn_started") {
            return;
        }
        if event_type == Some("permission_requested") {
            snapshot["pendingPermissions"] = json!([event.unwrap()["request"]]);
            self.push(Outcome {
                status: "permission".into(),
                final_snapshot: Some(snapshot),
                last_message,
                reason: None,
            });
        } else if matches!(snapshot["status"].as_str(), Some("closed" | "error"))
            || (snapshot["status"] == "idle"
                && matches!(event_type, Some("turn_completed" | "turn_canceled")))
        {
            self.push(Outcome {
                status: if snapshot["status"] == "error" {
                    "error"
                } else {
                    "idle"
                }
                .into(),
                final_snapshot: Some(snapshot),
                last_message,
                reason: (event_type == Some("turn_canceled")).then_some("was canceled"),
            });
        }
    }
}
#[derive(Default)]
struct Buffer {
    state: Mutex<BufferState>,
    wake: Notify,
}
impl Buffer {
    async fn fail(&self, error: anyhow::Error) {
        self.state.lock().await.failure = Some(error.to_string());
        self.wake.notify_one();
    }
    async fn initial(&self, result: Outcome) {
        self.state.lock().await.initial(result);
        self.wake.notify_one();
    }
    async fn observe(&self, snapshot: Option<Value>, event: Option<&Value>, last: Option<String>) {
        self.state.lock().await.observe(snapshot, event, last);
        self.wake.notify_one();
    }
}
pub(super) struct Observation {
    buffer: Arc<Buffer>,
    cancel: CancellationToken,
    task: Option<tokio::task::JoinHandle<()>>,
}
impl Observation {
    pub fn open(
        app: Weak<AppState>,
        record: FinishSubscription,
        parent: &CancellationToken,
    ) -> Self {
        let cancel = parent.child_token();
        let buffer = Arc::new(Buffer::default());
        let task_buffer = buffer.clone();
        let task_cancel = cancel.clone();
        let task = tokio::spawn(async move {
            let result = if record.server_id.is_some() {
                remote(app, &record, task_buffer.clone(), task_cancel.clone()).await
            } else {
                local(
                    app,
                    &record.child_agent_id,
                    task_buffer.clone(),
                    task_cancel.clone(),
                )
                .await
            };
            if let Err(error) = result {
                if !task_cancel.is_cancelled() {
                    task_buffer.fail(error).await;
                }
            }
        });
        Self {
            buffer,
            cancel,
            task: Some(task),
        }
    }
    pub async fn next(&self) -> Result<Outcome> {
        loop {
            let wake = self.buffer.wake.notified();
            {
                let mut state = self.buffer.state.lock().await;
                if let Some(result) = state.next() {
                    return Ok(result);
                }
                if let Some(error) = &state.failure {
                    bail!("{error}");
                }
            }
            tokio::select! { _ = self.cancel.cancelled() => bail!("Observation canceled"), _ = wake => {} }
        }
    }
    pub async fn close(mut self) {
        self.cancel.cancel();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}
impl Drop for Observation {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
fn relevant(event: &Value) -> bool {
    matches!(
        event["type"].as_str(),
        Some(
            "permission_requested"
                | "permission_resolved"
                | "turn_started"
                | "turn_completed"
                | "turn_canceled"
                | "turn_failed"
        )
    )
}
async fn local(
    app: Weak<AppState>,
    id: &str,
    buffer: Arc<Buffer>,
    cancel: CancellationToken,
) -> Result<()> {
    let manager = app
        .upgrade()
        .context("Daemon is shutting down")?
        .agents
        .clone();
    let mut events = manager.subscribe_events();
    let initial = async {
        manager
            .message_delivery
            .wait_for_dispatch(id, cancel.clone())
            .await?;
        if manager.session(id).await?.is_none() {
            let snapshot = manager.agent_snapshot(id).await?;
            return Ok(Outcome {
                status: if snapshot.as_ref().is_some_and(|s| s["status"] == "error") {
                    "error"
                } else {
                    "idle"
                }
                .into(),
                final_snapshot: snapshot,
                last_message: manager.last_assistant_message(id).await?,
                reason: None,
            });
        }
        let result = manager
            .wait(
                &json!({"kind":"system","source":"finish-subscription"}),
                id,
                json!({"waitForActive":true}),
                cancel.clone(),
            )
            .await?;
        let snapshot = manager.agent_snapshot(id).await?;
        Ok::<_, anyhow::Error>(Outcome {
            status: if !result["permission"].is_null() && result["permission"] != false {
                "permission".into()
            } else {
                result["status"].as_str().unwrap_or("idle").into()
            },
            final_snapshot: snapshot,
            last_message: result["lastMessage"].as_str().map(str::to_owned),
            reason: None,
        })
    };
    tokio::pin!(initial);
    let mut initial_pending = true;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            result = &mut initial, if initial_pending => { initial_pending = false; buffer.initial(result?).await; }
            event = events.recv() => {
                let event = event.context("Finish observation event stream ended or lagged")?;
                let id_matches = event["agentId"] == id || event["agent"]["id"] == id;
                if !id_matches { continue; }
                if event["type"] == "agent_stream" && relevant(&event["event"]) {
                    let snapshot = manager.agent_snapshot(id).await?;
                    let last = manager.last_assistant_message(id).await?;
                    buffer.observe(snapshot, Some(&event["event"]), last).await;
                } else if event["type"] == "agent_state" && event["agent"]["lifecycle"] != "idle" {
                    let snapshot = manager.agent_snapshot(id).await?;
                    let last = manager.last_assistant_message(id).await?;
                    buffer.observe(snapshot, None, last).await;
                }
            }
        }
    }
}
async fn remote_refresh(
    client: &HostClient,
    id: &str,
    event: Option<&Value>,
    buffer: &Buffer,
) -> Result<()> {
    let response = client
        .request(
            json!({"type":"fetch_agent_request","agentId":id}),
            "fetch_agent_response",
        )
        .await?;
    let response = response.get("payload").unwrap_or(&response);
    let mut snapshot = response.get("agent").filter(|v| !v.is_null()).cloned();
    let mut last = None;
    if snapshot.as_ref().is_some_and(|s| {
        matches!(s["status"].as_str(), Some("idle" | "closed" | "error"))
            || s["pendingPermissions"]
                .as_array()
                .is_some_and(|p| !p.is_empty())
    }) {
        let response = client
            .request(
                json!({"type":"wait_for_finish_request","agentId":id,"timeoutMs":100}),
                "wait_for_finish_response",
            )
            .await?;
        let current = Outcome::from_response(&response)?;
        if current.status == "timeout" {
            let response = client
                .request(
                    json!({"type":"fetch_agent_request","agentId":id}),
                    "fetch_agent_response",
                )
                .await?;
            snapshot = response
                .get("payload")
                .unwrap_or(&response)
                .get("agent")
                .filter(|v| !v.is_null())
                .cloned();
        } else {
            snapshot = current.final_snapshot;
            last = current.last_message;
        }
    }
    buffer.observe(snapshot, event, last).await;
    Ok(())
}
async fn remote(
    app: Weak<AppState>,
    record: &FinishSubscription,
    buffer: Arc<Buffer>,
    cancel: CancellationToken,
) -> Result<()> {
    let transport = app
        .upgrade()
        .context("Daemon is shutting down")?
        .transport
        .clone();
    let client = transport
        .hosts
        .tool_client(record.server_id.as_deref().unwrap(), cancel.clone())
        .await?;
    let id = &record.child_agent_id;
    let result = async {
        client.request(json!({"type":"agent.timeline.set_subscription.request","agentIds":[id]}), "agent.timeline.set_subscription.response").await?;
        remote_refresh(&client, id, None, &buffer).await?;
        let initial = client.request(json!({"type":"wait_for_finish_request","agentId":id}), "wait_for_finish_response");
        tokio::pin!(initial);
        let mut initial_pending = true;
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return Ok(()),
                result = &mut initial, if initial_pending => { initial_pending = false; buffer.initial(Outcome::from_response(&result?)?).await; }
                event = client.next_event() => {
                    let event = event?;
                    let payload = event.get("payload").unwrap_or(&event);
                    if event["type"] == "agent_stream" && payload["agentId"] == *id && relevant(&payload["event"]) {
                        remote_refresh(&client, id, Some(&payload["event"]), &buffer).await?;
                    }
                }
            }
        }
    }.await;
    let _ = client.close().await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn permission(id: &str) -> Outcome {
        Outcome {
            status: "permission".into(),
            final_snapshot: Some(json!({"status":"running","pendingPermissions":[{"id":id}]})),
            last_message: None,
            reason: None,
        }
    }
    #[test]
    fn checkpoints_dedupe_and_terminal_survives_delivery() {
        let mut buffer = BufferState::default();
        buffer.push(permission("a"));
        buffer.push(permission("a"));
        buffer.observe(
            Some(json!({"status":"idle"})),
            Some(&json!({"type":"turn_completed"})),
            Some("done".into()),
        );
        assert_eq!(buffer.next().unwrap().status, "permission");
        buffer.push(permission("a"));
        assert_eq!(buffer.next().unwrap().last_message.as_deref(), Some("done"));
        assert!(buffer.next().is_none());
        buffer.resolved("a");
        buffer.push(permission("a"));
        assert_eq!(buffer.next().unwrap().status, "permission");
    }
    #[test]
    fn resolved_permission_cannot_return_in_initial_snapshot() {
        let mut buffer = BufferState::default();
        buffer.resolved("a");
        buffer.initial(permission("a"));
        assert!(buffer.next().is_none());
        buffer.observe(
            Some(json!({"status":"idle"})),
            Some(&json!({"type":"turn_started"})),
            None,
        );
        assert!(buffer.next().is_none());
    }
}
