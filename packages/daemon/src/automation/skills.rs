mod files;
#[cfg(test)]
mod tests;
mod transaction;

use anyhow::{Context, Result};
use files::{Status, Targets};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, Weak},
};
use tokio::sync::Mutex;

use crate::{state::AppState, transport::RequestContext};

/// One writer covers startup maintenance, reads, recovery and selection commits.
/// The queue is owned by each spawned operation so disconnecting a caller cannot
/// release serialization while its filesystem transaction is still in flight.
pub struct SkillsService {
    app: Weak<AppState>,
    queue: Arc<Mutex<()>>,
}

impl SkillsService {
    pub fn new(app: Weak<AppState>) -> Self {
        Self {
            app,
            queue: Arc::new(Mutex::new(())),
        }
    }

    pub async fn initialize(&self) -> Result<()> {
        // The source treats startup maintenance as best-effort. An invalid owned
        // recovery journal remains in place and subsequent requests report it.
        if let Err(error) = self.execute(Operation::AutoUpdate).await {
            tracing::error!(error = %error, "Failed to maintain orchestration skills at startup");
        }
        Ok(())
    }

    pub async fn handle(&self, context: &RequestContext, message: &Value) -> Result<bool> {
        let Some(kind) = message["type"].as_str() else {
            return Ok(false);
        };
        let operation = match kind {
            "agent.skills.get_status.request" => Operation::Status,
            "agent.skills.reconcile.request" => Operation::Reconcile,
            "agent.skills.uninstall.request" => Operation::Uninstall,
            "agent.skills.save_selection.request" => Operation::Save {
                selection: files::selection(&message["selection"]),
                confirmed: files::names(&message["confirmedRemovals"]),
            },
            "agent.skills.import_legacy_selection.request" => {
                Operation::Import(files::selection(&message["selection"]))
            }
            _ => return Ok(false),
        };
        let mut payload = self.execute(operation).await?;
        payload["requestId"] = message["requestId"].clone();
        context.send(json!({"type":format!("{}.response", kind.trim_end_matches(".request")), "payload":payload})).await?;
        Ok(true)
    }

    async fn execute(&self, operation: Operation) -> Result<Value> {
        let app = self.app.upgrade().context("Daemon is shutting down")?;
        let queue = self.queue.clone();
        tokio::spawn(async move {
            let _guard = queue.lock().await;
            let configured = app.config.mutable_snapshot();
            let previous = files::selection(&configured["skills"]["selection"]);
            if let Operation::Import(next) = operation {
                if configured
                    .get("skills")
                    .and_then(|v| v.get("selection"))
                    .is_some()
                {
                    return Ok(json!({"imported":false,"selection":previous}));
                }
                app.config.set_agent_skill_selection(next.clone()).await?;
                return Ok(json!({"imported":true,"selection":next}));
            }
            let targets = targets(&app);
            let recovery_targets = targets.clone();
            let committed = previous.clone();
            blocking(move || transaction::recover(&recovery_targets, &committed)).await?;
            match operation {
                Operation::Save {
                    selection: next,
                    confirmed,
                } => save(&app, targets, previous, next, confirmed).await,
                operation => {
                    let selected = previous.clone();
                    let status = blocking(move || {
                        if matches!(operation, Operation::Uninstall) {
                            return files::uninstall(&targets, &selected);
                        }
                        let status = files::status(&targets, &selected)?;
                        let apply = matches!(operation, Operation::Reconcile)
                            || (matches!(operation, Operation::AutoUpdate)
                                && status.state == "drift");
                        if !apply {
                            return Ok(status);
                        }
                        let ops: Vec<_> = status
                            .ops
                            .into_iter()
                            .filter(|op| op.kind != "delete")
                            .collect();
                        files::apply(&targets, &selected, &ops)
                    })
                    .await?;
                    snapshot(status, previous)
                }
            }
        })
        .await
        .context("Skills operation task failed")?
    }
}

enum Operation {
    Status,
    Reconcile,
    Uninstall,
    AutoUpdate,
    Save {
        selection: Value,
        confirmed: Vec<String>,
    },
    Import(Value),
}

async fn blocking<T: Send + 'static>(
    run: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(run)
        .await
        .context("Skills filesystem operation failed")?
}

fn snapshot(status: Status, selection: Value) -> Result<Value> {
    let mut result = serde_json::to_value(status)?;
    result["selection"] = selection;
    Ok(result)
}

fn targets(app: &AppState) -> Targets {
    let executable_directory = app.executable.parent().unwrap_or(std::path::Path::new("."));
    let candidates = [
        executable_directory.join("skills"),
        executable_directory.join("../share/paseo/skills"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../skills"),
    ];
    let source = candidates
        .iter()
        .find(|path| path.is_dir())
        .cloned()
        .unwrap_or_else(|| candidates[0].clone());
    Targets {
        source,
        roots: [
            app.user_home.join(".agents/skills"),
            app.user_home.join(".claude/skills"),
            app.user_home.join(".codex/skills"),
        ],
    }
}

async fn scan(targets: Targets, selection: Value) -> Result<Status> {
    blocking(move || files::status(&targets, &selection)).await
}

async fn save(
    app: &AppState,
    targets: Targets,
    previous: Value,
    next: Value,
    confirmed: Vec<String>,
) -> Result<Value> {
    let mut plan = scan(targets.clone(), next.clone()).await?;
    for _ in 0..3 {
        let removals: Vec<_> = plan
            .ops
            .iter()
            .filter(|op| op.kind == "delete")
            .map(|op| op.name.clone())
            .collect();
        if removals.iter().any(|name| !confirmed.contains(name)) {
            let mut result = snapshot(scan(targets, previous.clone()).await?, previous)?;
            result["confirmationRequired"] = json!({"removals":removals});
            return Ok(result);
        }
        let stage_targets = targets.clone();
        let stage_previous = previous.clone();
        let stage_next = next.clone();
        let (transaction, status) = blocking(move || {
            let transaction = transaction::Transaction::begin(
                &stage_targets,
                stage_previous,
                stage_next.clone(),
                &plan.ops,
            )?;
            let ops: Vec<_> = plan
                .ops
                .into_iter()
                .filter(|op| op.kind != "delete")
                .collect();
            match files::apply(&stage_targets, &stage_next, &ops) {
                Ok(status) => Ok((transaction, status)),
                Err(error) => {
                    transaction.rollback(&stage_targets)?;
                    Err(error)
                }
            }
        })
        .await?;
        if status.ops.is_empty() {
            if let Err(error) = app.config.set_agent_skill_selection(next.clone()).await {
                let rollback_targets = targets.clone();
                blocking(move || transaction.rollback(&rollback_targets)).await?;
                return Err(error);
            }
            // Once config persistence succeeds, cleanup failure must not turn a
            // committed save into a failed response. Recovery retries cleanup.
            if let Err(error) = blocking(move || transaction.commit()).await {
                tracing::warn!(error = %error, "Skills transaction cleanup deferred to recovery");
            }
            let mut result = snapshot(status, next)?;
            result["confirmationRequired"] = Value::Null;
            return Ok(result);
        }
        let rollback_targets = targets.clone();
        blocking(move || transaction.rollback(&rollback_targets)).await?;
        plan = scan(targets.clone(), next.clone()).await?;
    }
    anyhow::bail!("Skills changed repeatedly while the selection was being saved")
}
