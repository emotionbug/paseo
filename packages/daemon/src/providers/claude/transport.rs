use crate::process::{CommandSpec, ManagedProcess};
use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    sync::{mpsc, oneshot},
};
use tokio_util::sync::CancellationToken;

/// Claude's control channel is independent of the event consumer. Replies never
/// wait on permission resolution or on a caller holding a session-state lock.
pub(super) struct Transport {
    pub process: Arc<ManagedProcess>,
    pub generation: String,
    pending: Mutex<HashMap<String, oneshot::Sender<Result<Value>>>>,
    stop: CancellationToken,
}

impl Transport {
    pub async fn spawn(
        command: CommandSpec,
        owner: CancellationToken,
    ) -> Result<(Arc<Self>, mpsc::Receiver<Result<Value>>)> {
        let (process, stdout) = ManagedProcess::spawn(command).await?;
        let this = Arc::new(Self {
            process,
            generation: uuid::Uuid::new_v4().to_string(),
            pending: Mutex::new(HashMap::new()),
            stop: owner.child_token(),
        });
        let cleanup_stop = this.stop.clone();
        let cleanup_process = this.process.clone();
        tokio::spawn(async move {
            cleanup_stop.cancelled().await;
            let _ = cleanup_process.terminate(Duration::from_secs(2)).await;
        });
        let (tx, rx) = mpsc::channel(256);
        let weak = Arc::downgrade(&this);
        let stop = this.stop.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            let failure = loop {
                line.clear();
                let read = tokio::select! {
                    _ = stop.cancelled() => break "Claude transport closed".to_owned(),
                    read = reader.read_line(&mut line) => read,
                };
                match read {
                    Ok(0) => break "Claude process output closed".to_owned(),
                    Err(error) => break format!("Reading Claude output: {error}"),
                    Ok(_) => {}
                }
                if line.trim().is_empty() {
                    continue;
                }
                let message: Value = match serde_json::from_str(&line) {
                    Ok(value) => value,
                    Err(error) => break format!("Invalid Claude stream-json frame: {error}"),
                };
                let Some(transport) = weak.upgrade() else {
                    return;
                };
                if message["type"] == "control_response" {
                    let response = &message["response"];
                    if let Some(id) = response["request_id"].as_str() {
                        let sender = transport.pending.lock().unwrap().remove(id);
                        if let Some(sender) = sender {
                            let result = if response["subtype"] == "error" {
                                Err(anyhow!(
                                    "{}",
                                    response["error"]
                                        .as_str()
                                        .unwrap_or("Claude control request failed")
                                ))
                            } else {
                                Ok(response["response"].clone())
                            };
                            let _ = sender.send(result);
                        }
                    }
                    continue;
                }
                drop(transport);
                let sent = tokio::select! {
                    _ = stop.cancelled() => false,
                    result = tx.send(Ok(message)) => result.is_ok(),
                };
                if !sent {
                    break "Claude event consumer closed".to_owned();
                }
            };
            if let Some(transport) = weak.upgrade() {
                let pending = std::mem::take(&mut *transport.pending.lock().unwrap());
                for (_, sender) in pending {
                    let _ = sender.send(Err(anyhow!(failure.clone())));
                }
            }
            let _ = tokio::select! { _ = stop.cancelled() => Ok(()), result = tx.send(Err(anyhow!(failure))) => result.map_err(|_| ()) };
        });
        Ok((this, rx))
    }

    pub async fn request(&self, request: Value, timeout: Duration) -> Result<Value> {
        if self.stop.is_cancelled() {
            bail!("Claude transport is closed");
        }
        let id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id.clone(), tx);
        struct Pending<'a> {
            transport: &'a Transport,
            id: String,
        }
        impl Drop for Pending<'_> {
            fn drop(&mut self) {
                self.transport.pending.lock().unwrap().remove(&self.id);
            }
        }
        let _pending = Pending {
            transport: self,
            id: id.clone(),
        };
        let operation = async {
            self.process
                .write_json(&json!({"type":"control_request","request_id":id,"request":request}))
                .await?;
            rx.await
                .map_err(|_| anyhow!("Claude control reply channel closed"))?
        };
        tokio::select! {
            _ = self.stop.cancelled() => Err(anyhow!("Claude transport closed during control request")),
            result = tokio::time::timeout(timeout, operation) => {
                result.map_err(|_| anyhow!("Claude control request timed out; mutation outcome is unknown"))?
            }
        }
    }

    pub async fn reply(&self, id: &str, response: Result<Value>) -> Result<()> {
        let response = match response {
            Ok(response) => json!({"subtype":"success","request_id":id,"response":response}),
            Err(error) => json!({"subtype":"error","request_id":id,"error":error.to_string()}),
        };
        self.process
            .write_json(&json!({"type":"control_response","response":response}))
            .await
    }

    pub async fn close(&self) -> Result<()> {
        self.stop.cancel();
        let pending = std::mem::take(&mut *self.pending.lock().unwrap());
        for (_, sender) in pending {
            let _ = sender.send(Err(anyhow!("Claude session closed")));
        }
        let _ = self.process.close_stdin().await;
        self.process.terminate(Duration::from_secs(2)).await
    }
}

impl Drop for Transport {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
