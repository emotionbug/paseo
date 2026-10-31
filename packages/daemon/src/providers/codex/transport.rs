use crate::process::{CommandSpec, ManagedProcess};
use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fmt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    sync::{mpsc, oneshot},
};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub struct RpcError {
    pub code: Value,
    pub message: String,
    pub data: Value,
}
impl fmt::Display for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for RpcError {}

impl RpcError {
    pub fn definitive_steer_rejection(&self) -> bool {
        if self.code == -32601 {
            return true;
        }
        if self.code != -32600 {
            return false;
        }
        if self.data["codexErrorInfo"]["activeTurnNotSteerable"].is_object() {
            return true;
        }
        if matches!(
            self.message.as_str(),
            "no active turn to steer" | "active turn uses a different output schema"
        ) {
            return true;
        }
        let Some(rest) = self.message.strip_prefix("expected active turn id `") else {
            return false;
        };
        let Some((expected, actual)) = rest.split_once("` but found `") else {
            return false;
        };
        !expected.is_empty()
            && !expected.contains('`')
            && actual
                .strip_suffix('`')
                .is_some_and(|s| !s.is_empty() && !s.contains('`'))
    }
    pub fn already_idle_interrupt(&self) -> bool {
        self.code == -32600 && self.message == "no active turn to interrupt"
    }
}

pub struct Transport {
    pub process: Arc<ManagedProcess>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Value>>>>,
    next: AtomicU64,
    closed: AtomicBool,
    pub cancel: CancellationToken,
}
impl Transport {
    pub async fn spawn(
        spec: CommandSpec,
        cancel: CancellationToken,
    ) -> Result<(Arc<Self>, mpsc::Receiver<Value>)> {
        let (process, stdout) = ManagedProcess::spawn(spec).await?;
        let this = Arc::new(Self {
            process,
            pending: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
            closed: AtomicBool::new(false),
            cancel: cancel.child_token(),
        });
        let (sender, receiver) = mpsc::channel(4096);
        let reader = this.clone();
        tokio::spawn(async move {
            let mut input = BufReader::new(stdout);
            let mut line = Vec::new();
            let failure = loop {
                line.clear();
                let read = tokio::select! { _ = reader.cancel.cancelled() => break "Codex app-server canceled".to_owned(), r = input.read_until(b'\n', &mut line) => r };
                match read {
                    Ok(0) => {
                        let status = tokio::select! {
                            _=reader.cancel.cancelled()=>break "Codex app-server canceled".to_owned(),
                            result=reader.process.wait()=>result,
                        };
                        break match status {
                            Ok(status) => format!(
                                "Codex app-server exited with status {status}\n{}",
                                reader.process.stderr_tail()
                            )
                            .trim()
                            .to_owned(),
                            Err(error) => format!(
                                "Codex app-server exit failed: {error}\n{}",
                                reader.process.stderr_tail()
                            )
                            .trim()
                            .to_owned(),
                        };
                    }
                    Err(error) => break format!("Codex app-server stdout: {error}"),
                    _ => {}
                }
                if line.iter().all(u8::is_ascii_whitespace) {
                    continue;
                }
                let message: Value = match serde_json::from_slice(&line) {
                    Ok(v) => v,
                    Err(e) => break format!("Invalid Codex app-server JSON: {e}"),
                };
                if message.get("method").is_none() {
                    if let Some(id) = message["id"].as_u64() {
                        let pending = reader.pending.lock().unwrap().remove(&id);
                        if let Some(pending) = pending {
                            let result = if let Some(error) = message.get("error") {
                                Err(RpcError {
                                    code: error["code"].clone(),
                                    message: error["message"]
                                        .as_str()
                                        .unwrap_or("Codex RPC error")
                                        .into(),
                                    data: error["data"].clone(),
                                }
                                .into())
                            } else {
                                Ok(message["result"].clone())
                            };
                            let _ = pending.send(result);
                        }
                    }
                } else if sender.try_send(message).is_err() {
                    break "Codex event queue exceeded 4096 pending frames or its consumer closed"
                        .to_owned();
                }
            };
            reader.closed.store(true, Ordering::Release);
            for (_, pending) in std::mem::take(&mut *reader.pending.lock().unwrap()) {
                let _ = pending.send(Err(anyhow!(failure.clone())));
            }
            let _ = sender
                .send(json!({"method":"paseo/transportClosed","params":{"message":failure}}))
                .await;
            let _ = reader.process.terminate(Duration::from_secs(2)).await;
        });
        Ok((this, receiver))
    }
    pub async fn request(&self, method: &str, params: Value) -> Result<Value> {
        self.request_timeout(method, params, Duration::from_secs(14 * 24 * 60 * 60))
            .await
    }
    pub async fn request_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value> {
        if self.closed.load(Ordering::Acquire) {
            bail!("Codex app-server is closed");
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let result = async {
            self.process.write_json(&json!({"id":id,"method":method,"params":params})).await?;
            tokio::select! {
                _ = self.cancel.cancelled() => Err(anyhow!("Codex {method} canceled; request acceptance may be ambiguous")),
                r = tokio::time::timeout(timeout,rx) => match r { Ok(Ok(v))=>v, Ok(Err(_))=>Err(anyhow!("Codex {method} disconnected")), Err(_)=>Err(anyhow!("Codex {method} timed out; request acceptance may be ambiguous")) }
            }
        }.await;
        self.pending.lock().unwrap().remove(&id);
        result
    }
    pub async fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.process
            .write_json(&json!({"method":method,"params":params}))
            .await
    }
    pub async fn reply(&self, id: Value, result: Value) -> Result<()> {
        self.process
            .write_json(&json!({"id":id,"result":result}))
            .await
    }
    pub async fn reject(&self, id: Value, message: &str) -> Result<()> {
        self.process
            .write_json(&json!({"id":id,"error":{"code":-32601,"message":message}}))
            .await
    }
    pub async fn initialize(&self) -> Result<Value> {
        let value = crate::providers::registry::catalog::refresh_activity("initialize",
            self.request_timeout("initialize",json!({"clientInfo":{"name":"codex_app_server_daemon","title":"Codex App Server Daemon","version":"0.0.0"},"capabilities":{"experimentalApi":true,"mcpServerOpenaiFormElicitation":true}}),Duration::from_secs(30))
        ).await?;
        self.notify("initialized", json!({})).await?;
        Ok(value)
    }
    pub async fn close(&self) -> Result<()> {
        self.closed.store(true, Ordering::Release);
        self.cancel.cancel();
        for (_, pending) in std::mem::take(&mut *self.pending.lock().unwrap()) {
            let _ = pending.send(Err(anyhow!("Codex app-server is closed")));
        }
        let shutdown = self.process.close_stdin().await;
        self.process.terminate(Duration::from_secs(2)).await?;
        shutdown
    }
}

/// A session may replace a dead connection, never replay a request that was
/// already written. Each operation pins exactly one transport generation.
pub struct SessionTransport {
    current: Mutex<Arc<Transport>>,
    pub cancel: CancellationToken,
}
impl SessionTransport {
    pub fn new(current: Arc<Transport>, cancel: CancellationToken) -> Arc<Self> {
        Arc::new(Self {
            current: Mutex::new(current),
            cancel: cancel.child_token(),
        })
    }
    pub fn current(&self) -> Arc<Transport> {
        self.current.lock().unwrap().clone()
    }
    pub fn is_closed(&self) -> bool {
        self.current().closed.load(Ordering::Acquire)
    }
    pub fn is_current(&self, transport: &Arc<Transport>) -> bool {
        Arc::ptr_eq(&self.current(), transport)
    }
    pub fn replace(&self, transport: Arc<Transport>) {
        *self.current.lock().unwrap() = transport;
    }
    pub async fn request(&self, method: &str, params: Value) -> Result<Value> {
        self.current().request(method, params).await
    }
    pub async fn request_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value> {
        self.current()
            .request_timeout(method, params, timeout)
            .await
    }
    pub async fn reply(&self, id: Value, result: Value) -> Result<()> {
        self.current().reply(id, result).await
    }
    pub async fn reject(&self, id: Value, message: &str) -> Result<()> {
        self.current().reject(id, message).await
    }
    pub async fn close(&self) -> Result<()> {
        self.cancel.cancel();
        self.current().close().await
    }
}
