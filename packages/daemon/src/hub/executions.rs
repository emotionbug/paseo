use crate::{state::AppState, transport::RequestContext};
use anyhow::{Context, Result, ensure};
use futures_util::{
    FutureExt,
    future::{BoxFuture, Shared, join_all},
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Weak},
};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

type Outcome = std::result::Result<Value, Value>;
type Operation = Shared<BoxFuture<'static, Outcome>>;
#[derive(Default)]
struct Pending {
    creates: HashMap<String, Operation>,
    controls: HashMap<(String, String), Operation>,
    tails: HashMap<String, Operation>,
}
pub(super) struct Executions {
    app: Weak<AppState>,
    daemon_id: String,
    pub cancel: CancellationToken,
    pending: Mutex<Pending>,
}
impl Executions {
    pub fn new(app: Weak<AppState>, daemon_id: String) -> Arc<Self> {
        Arc::new(Self {
            app,
            daemon_id,
            cancel: CancellationToken::new(),
            pending: Mutex::new(Pending::default()),
        })
    }
    pub async fn invalidate(&self) {
        self.cancel.cancel();
        let pending = self.pending.lock().await;
        let work: Vec<_> = pending
            .creates
            .values()
            .chain(pending.controls.values())
            .cloned()
            .collect();
        drop(pending);
        join_all(work).await;
    }
    fn authority(&self) -> Result<()> {
        ensure!(
            !self.cancel.is_cancelled(),
            "Hub relationship authority is no longer active"
        );
        Ok(())
    }
    fn fence(&self, operation: &str) -> Result<()> {
        ensure!(
            !self.cancel.is_cancelled(),
            "Hub relationship authority ended during {operation}"
        );
        Ok(())
    }
    pub async fn create(self: &Arc<Self>, input: &Value) -> Outcome {
        if let Err(e) = self.authority() {
            return Err(create_error(e));
        }
        let id = match nonblank(input, "executionId") {
            Ok(id) => id.to_owned(),
            Err(e) => return Err(create_error(e)),
        };
        if let Err(error) = validate_create(input) {
            return Err(create_error(error));
        }
        let mut pending = self.pending.lock().await;
        if let Some(operation) = pending.creates.get(&id) {
            let operation = operation.clone();
            drop(pending);
            return operation.await;
        }
        let this = self.clone();
        let input = input.clone();
        let operation = async move { this.create_once(&input).await.map_err(create_error) }
            .boxed()
            .shared();
        pending.creates.insert(id.clone(), operation.clone());
        drop(pending);
        let this = self.clone();
        let run = operation.clone();
        tokio::spawn(async move {
            let _ = run.await;
            this.pending.lock().await.creates.remove(&id);
        });
        operation.await
    }
    async fn create_once(&self, input: &Value) -> Result<Value> {
        self.fence("agent creation")?;
        let id = nonblank(input, "executionId")?;
        let app = self.app.upgrade().context("Daemon is shutting down")?;
        if let Some(snapshot) = app.agents.find_owned_execution(&self.daemon_id, id).await? {
            require_workspace(&snapshot)?;
            self.fence("agent creation")?;
            let mut agent = crate::agent::snapshot_payload(&snapshot);
            if snapshot["_live"] == false {
                agent["status"] = "closed".into();
            }
            return Ok(agent);
        }
        self.fence("agent creation")?;
        validate_policy(input)?;
        let owner = json!({"kind":"daemon","daemonId":self.daemon_id,"executionId":id});
        let created = app
            .agents
            .hub_create(input, &owner, self.cancel.clone())
            .await?;
        let checked = (|| -> Result<&Value> {
            self.fence("agent creation")?;
            let snapshot = created
                .get("agent")
                .context("Hub agent creation returned no snapshot")?;
            require_workspace(snapshot)?;
            Ok(snapshot)
        })();
        let snapshot = match checked {
            Ok(snapshot) => snapshot,
            Err(error) => {
                app.agents.hub_cleanup_created(&created, &owner).await?;
                return Err(error);
            }
        };
        Ok(snapshot.clone())
    }
    pub async fn control(self: &Arc<Self>, input: &Value) -> Outcome {
        if let Err(e) = self.authority() {
            return Err(json!(e.to_string()));
        }
        let id = match nonblank(input, "executionId") {
            Ok(id) => id.to_owned(),
            Err(e) => return Err(json!(e.to_string())),
        };
        let action = input["action"].as_str().unwrap_or("").to_owned();
        if !matches!(action.as_str(), "interrupt" | "archive") {
            return Err(json!("Invalid Hub execution control action"));
        }
        let key = (id.clone(), action);
        let mut pending = self.pending.lock().await;
        if let Some(operation) = pending.controls.get(&key) {
            let operation = operation.clone();
            drop(pending);
            return operation.await;
        }
        let previous = pending
            .tails
            .get(&id)
            .or_else(|| pending.creates.get(&id))
            .cloned();
        let this = self.clone();
        let input = input.clone();
        let operation = async move {
            if let Some(previous) = previous {
                let _ = previous.await;
            }
            this.control_once(&input)
                .await
                .map_err(|e| json!(e.to_string()))
        }
        .boxed()
        .shared();
        pending.controls.insert(key.clone(), operation.clone());
        pending.tails.insert(id.clone(), operation.clone());
        drop(pending);
        let this = self.clone();
        let run = operation.clone();
        tokio::spawn(async move {
            let _ = run.await;
            let mut pending = this.pending.lock().await;
            pending.controls.remove(&key);
            // A later action can already own the tail; retain it until that action finishes.
            if !pending
                .controls
                .keys()
                .any(|(execution, _)| execution == &id)
            {
                pending.tails.remove(&id);
            }
        });
        operation.await
    }
    async fn control_once(&self, input: &Value) -> Result<Value> {
        self.fence("execution control")?;
        let app = self.app.upgrade().context("Daemon is shutting down")?;
        let id = nonblank(input, "executionId")?;
        let Some(snapshot) = app.agents.find_owned_execution(&self.daemon_id, id).await? else {
            return Ok(Value::Null);
        };
        self.fence("execution control")?;
        ensure!(
            self.owns(&snapshot),
            "Hub execution is owned by another relationship"
        );
        let owner = json!({"kind":"daemon","daemonId":self.daemon_id,"executionId":id});
        if input["action"] == "interrupt" {
            if snapshot.get("archivedAt").is_none_or(Value::is_null) && snapshot["_live"] == true {
                app.agents
                    .control(
                        &owner,
                        snapshot["id"].as_str().context("Missing agent ID")?,
                        "interrupt",
                        json!({}),
                    )
                    .await?;
            }
        } else {
            let workspace = require_workspace(&snapshot)?;
            self.fence("execution control")?;
            app.projects.archive_workspace(&owner, workspace).await?;
        }
        Ok(Value::Null)
    }
    fn owns(&self, snapshot: &Value) -> bool {
        snapshot["owner"]["kind"] == "daemon"
            && snapshot["owner"]["daemonId"].as_str() == Some(&self.daemon_id)
    }
    pub async fn replay(&self, context: &RequestContext) -> Result<()> {
        self.authority()?;
        let app = self.app.upgrade().context("Daemon is shutting down")?;
        let snapshots = app.agents.list_snapshots().await?;
        drop(app);
        for snapshot in snapshots {
            self.emit_snapshot(context, snapshot).await?;
        }
        Ok(())
    }
    pub async fn observe(self: &Arc<Self>, context: &RequestContext) -> Result<()> {
        self.authority()?;
        let app = self.app.upgrade().context("Daemon is shutting down")?;
        let mut events = app.agents.subscribe_events();
        let snapshots = app.agents.list_snapshots().await?;
        drop(app);
        let this = self.clone();
        let context = context.clone();
        tokio::spawn(async move {
            let cancelled = context.cancellation();
            let result: Result<()> = async {
                for snapshot in snapshots { this.emit_snapshot(&context,snapshot).await?; }
                loop {
                    tokio::select! { biased;
                        _ = cancelled.cancelled() => break,
                        _ = this.cancel.cancelled() => break,
                        event = events.recv() => {
                            let event = event.context("Hub execution event stream lost continuity")?;
                            if !context.allows("hub.execute") { continue; }
                            if event["type"] == "agent_state" {
                                this.emit_snapshot(&context,event["agent"].clone()).await?;
                            } else if event["type"] == "agent_stream" {
                                let app = this.app.upgrade().context("Daemon is shutting down")?;
                                let id = event["agentId"].as_str().context("Missing stream agent ID")?;
                                let snapshot = app.agents.agent_snapshot(id).await?;
                                drop(app);
                                if let Some(snapshot) = snapshot.filter(|s| s["_live"] == true && this.owns(s)) {
                                    if let Some(event) = crate::agent::serialize_stream_event(&event["event"]) {
                                        context.send(json!({"type":"hub.execution.agent.stream","payload":{"executionId":snapshot["owner"]["executionId"],"agentId":id,"event":event}})).await?;
                                    }
                                }
                            }
                        }
                    }
                }
                Ok(())
            }.await;
            if let Err(error) = result {
                tracing::warn!(%error,"Hub execution event delivery stopped");
                cancelled.cancel();
            }
        });
        Ok(())
    }
    async fn emit_snapshot(&self, context: &RequestContext, snapshot: Value) -> Result<()> {
        if snapshot["_live"] == true
            && self.owns(&snapshot)
            && context.allows("hub.execute")
            && !self.cancel.is_cancelled()
        {
            context.send(json!({"type":"hub.execution.agent.update","payload":{"executionId":snapshot["owner"]["executionId"],"agentId":snapshot["id"],"agent":crate::agent::snapshot_payload(&snapshot)}})).await?;
        }
        Ok(())
    }
}
fn nonblank<'a>(input: &'a Value, field: &str) -> Result<&'a str> {
    let value = input[field].as_str().unwrap_or("");
    ensure!(
        !value.trim().is_empty(),
        "Hub agent {field} cannot be blank"
    );
    Ok(value)
}
fn require_workspace(snapshot: &Value) -> Result<&str> {
    snapshot["workspaceId"]
        .as_str()
        .filter(|v| !v.is_empty())
        .with_context(|| {
            format!(
                "Hub execution agent {} has no workspaceId",
                snapshot["id"].as_str().unwrap_or("")
            )
        })
}
fn validate_policy(input: &Value) -> Result<()> {
    ensure!(
        input["mcpServers"].get("paseo").is_none(),
        "Hub execution MCP server name \"paseo\" is reserved by the daemon"
    );
    if let Some(grants) = input["toolPolicy"]["preapproved"].as_array() {
        for grant in grants {
            let server = grant["server"]
                .as_str()
                .context("Invalid tool preapproval server")?;
            let tool = grant["tool"]
                .as_str()
                .context("Invalid tool preapproval tool")?;
            ensure!(
                input["mcpServers"].get(server).is_some(),
                "Hub tool preapproval '{server}.{tool}' requires MCP server '{server}' in the same create request"
            );
        }
    }
    Ok(())
}
fn create_error(error: anyhow::Error) -> Value {
    crate::agent::creation::error_details(&error)
        .filter(|value| {
            matches!(
                value["code"].as_str(),
                Some("provider_options_invalid" | "tool_policy_unsupported")
            )
        })
        .unwrap_or_else(|| json!({"code":"create_failed","message":error.to_string()}))
}

fn validate_create(input: &Value) -> Result<()> {
    nonblank(input, "prompt")?;
    let cwd = nonblank(input, "cwd")?;
    ensure!(
        std::path::Path::new(cwd).is_absolute(),
        "Hub agent cwd must be absolute"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relationship_scope_never_falls_back_to_execution_or_directory_identity() {
        let executions = Executions::new(Weak::new(), "relationship-a".into());
        assert!(executions.owns(
            &json!({"owner":{"kind":"daemon","daemonId":"relationship-a","executionId":"same-id"}})
        ));
        assert!(!executions.owns(&json!({"owner":{"kind":"daemon","daemonId":"relationship-b","executionId":"same-id"},"cwd":"/same/directory"})));
        assert!(!executions.owns(&json!({"owner":{"kind":"user"},"cwd":"/same/directory"})));
    }

    #[test]
    fn tool_policy_cannot_approve_foreign_or_daemon_mcp_servers() {
        assert!(validate_policy(&json!({"mcpServers":{"paseo":{}}})).is_err());
        assert!(validate_policy(&json!({"toolPolicy":{"preapproved":[{"server":"foreign","tool":"execute"}]},"mcpServers":{"allowed":{}}})).is_err());
        assert!(validate_policy(&json!({"toolPolicy":{"preapproved":[{"server":"allowed","tool":"execute"}]},"mcpServers":{"allowed":{}}})).is_ok());
    }

    #[tokio::test]
    async fn retired_authority_rejects_new_effects_before_manager_access() {
        let executions = Executions::new(Weak::new(), "relationship-a".into());
        executions.invalidate().await;
        let error = executions
            .create(&json!({"executionId":"e","prompt":"prompt","cwd":"/tmp"}))
            .await
            .unwrap_err();
        assert_eq!(
            error["message"],
            "Hub relationship authority is no longer active"
        );
        let error = executions
            .control(&json!({"executionId":"e","action":"archive"}))
            .await
            .unwrap_err();
        assert_eq!(error, "Hub relationship authority is no longer active");
    }

    #[tokio::test]
    async fn authority_retirement_drains_effects_before_returning() {
        let executions = Executions::new(Weak::new(), "relationship-a".into());
        let (finish, finished) = tokio::sync::oneshot::channel::<()>();
        let operation = async move {
            finished.await.unwrap();
            Ok(Value::Null)
        }
        .boxed()
        .shared();
        executions
            .pending
            .lock()
            .await
            .creates
            .insert("execution-a".into(), operation);
        let retiring = executions.clone();
        let retired = tokio::spawn(async move {
            retiring.invalidate().await;
        });
        executions.cancel.cancelled().await;
        assert!(!retired.is_finished());
        assert_eq!(
            executions.fence("agent creation").unwrap_err().to_string(),
            "Hub relationship authority ended during agent creation"
        );
        finish.send(()).unwrap();
        retired.await.unwrap();
    }

    #[test]
    fn missing_workspace_does_not_authorize_directory_based_archival() {
        assert!(require_workspace(&json!({"id":"a","cwd":"/same/directory"})).is_err());
        assert_eq!(
            require_workspace(&json!({"id":"a","workspaceId":"workspace-exact"})).unwrap(),
            "workspace-exact"
        );
    }
}
