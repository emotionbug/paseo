use crate::process::{CommandSpec, ManagedProcess};
use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    sync::{Notify, mpsc, oneshot},
};
use tokio_util::sync::CancellationToken;

const FRAME_LIMIT: usize = 32 * 1024 * 1024;
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Value,
}

fn error_message(error: &Value) -> String {
    let base = error["message"].as_str().unwrap_or("ACP request failed");
    let mut data = &error["data"];
    while data.is_object() {
        for key in ["details", "errorMessage", "message", "detail", "title"] {
            if let Some(detail) = data[key].as_str().map(str::trim).filter(|v| !v.is_empty()) {
                return if detail == base {
                    base.to_owned()
                } else {
                    format!("{base}: {detail}")
                };
            }
        }
        data = &data["error"];
    }
    base.to_owned()
}

pub struct Rpc {
    pub process: Arc<ManagedProcess>,
    pending: Mutex<HashMap<u64, oneshot::Sender<(Result<Value>, u64)>>>,
    next: AtomicU64,
    pub cancel: CancellationToken,
    failure: Mutex<Option<String>>,
    processed: AtomicU64,
    progress: Notify,
}
impl Rpc {
    pub async fn spawn(
        spec: CommandSpec,
        owner: CancellationToken,
    ) -> Result<(Arc<Self>, mpsc::Receiver<(u64, Value)>)> {
        let (process, stdout) = ManagedProcess::spawn(spec).await?;
        let rpc = Arc::new(Self {
            process,
            pending: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
            cancel: owner.child_token(),
            failure: Mutex::new(None),
            processed: AtomicU64::new(0),
            progress: Notify::new(),
        });
        let (tx, rx) = mpsc::channel(1024);
        let reader_rpc = rpc.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            let mut sequence = 0;
            let result: Result<()> = async {
                loop {
                    let mut bytes = Vec::new();
                    let mut bounded = (&mut reader).take((FRAME_LIMIT + 1) as u64);
                    let read = tokio::select! {
                        _ = reader_rpc.cancel.cancelled() => break,
                        result = bounded.read_until(b'\n', &mut bytes) => result?,
                    };
                    if read == 0 { bail!("ACP process closed stdout: {}", reader_rpc.process.stderr_tail()); }
                    if bytes.len() > FRAME_LIMIT { bail!("ACP stdout frame exceeds {FRAME_LIMIT} bytes"); }
                    if bytes.iter().all(u8::is_ascii_whitespace) { continue; }
                    let frame: Value = match serde_json::from_slice(&bytes) {
                        Ok(frame) => frame,
                        Err(error) => {
                            tracing::warn!(error=%error, "ACP provider emitted non-JSON stdout; ignoring line");
                            continue;
                        }
                    };
                    if frame.get("method").is_none() {
                        let response_id = frame["id"].as_u64().or_else(|| frame["id"].as_str().filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())).and_then(|s| s.parse::<u64>().ok()).filter(|id| *id <= 9_007_199_254_740_991));
                        if let Some(id) = response_id {
                            if let Some(reply) = reader_rpc.pending.lock().unwrap().remove(&id) {
                                let result = if let Some(error) = frame.get("error") {
                                    Err(RpcError { code: error["code"].as_i64().unwrap_or(-32603), message: error_message(error), data: error["data"].clone() }.into())
                                } else { Ok(frame.get("result").cloned().unwrap_or_else(|| json!({}))) };
                                let _ = reply.send((result, sequence));
                            }
                        }
                    } else {
                        // Never block control replies behind event delivery or a permission dialog.
                        sequence += 1;
                        tx.try_send((sequence, frame)).map_err(|e| anyhow!("ACP notification consumer cannot keep up: {e}"))?;
                    }
                }
                Ok(())
            }.await;
            let message = result
                .err()
                .map(|e| e.to_string())
                .unwrap_or_else(|| "ACP owner closed".into());
            *reader_rpc.failure.lock().unwrap() = Some(message.clone());
            for (_, reply) in reader_rpc.pending.lock().unwrap().drain() {
                let _ = reply.send((Err(anyhow!(message.clone())), sequence));
            }
            reader_rpc.cancel.cancel();
            let _ = reader_rpc.process.terminate(Duration::from_secs(2)).await;
        });
        Ok((rpc, rx))
    }
    pub async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Option<Duration>,
    ) -> Result<Value> {
        if let Some(error) = self.failure.lock().unwrap().clone() {
            bail!("{error}");
        }
        let deadline = timeout.map(|duration| tokio::time::Instant::now() + duration);
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        if let Err(error) = self
            .write_frame(
                json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params}),
                timeout.unwrap_or(Duration::from_secs(30)),
            )
            .await
        {
            self.pending.lock().unwrap().remove(&id);
            return Err(error);
        }
        let result: Result<(Result<Value>, u64)> = tokio::select! {
            biased;
            response = async {
                match deadline {
                    Some(deadline) => tokio::time::timeout_at(deadline, rx).await.map_err(|_| anyhow!("ACP request {method} timed out; acceptance is unknown and it must not be replayed"))?.map_err(|_|anyhow!("ACP response channel closed")),
                    None => rx.await.map_err(|_|anyhow!("ACP response channel closed")),
                }
            } => response,
            _ = self.cancel.cancelled() => Err(anyhow!("ACP request {method} cancelled; acceptance is unknown and it must not be replayed")),
        };
        self.pending.lock().unwrap().remove(&id);
        let (value, sequence) = result?;
        // session/load and prompt completion must not overtake earlier updates.
        while self.processed.load(Ordering::Acquire) < sequence {
            let notified = self.progress.notified();
            if self.processed.load(Ordering::Acquire) >= sequence {
                break;
            }
            tokio::select! { _ = notified => {}, _ = self.cancel.cancelled() => bail!("ACP event owner closed") }
        }
        value
    }
    pub fn acknowledge(&self, sequence: u64) {
        self.processed.store(sequence, Ordering::Release);
        self.progress.notify_waiters();
    }
    async fn write_frame(&self, frame: Value, timeout: Duration) -> Result<()> {
        let result = tokio::select! {
            biased;
            written=tokio::time::timeout(timeout,self.process.write_json(&frame))=>match written{Ok(result)=>result,Err(_)=>Err(anyhow!("ACP write timed out; acceptance is unknown and it must not be replayed"))},
            _=self.cancel.cancelled()=>Err(anyhow!("ACP connection closed during write")),
        };
        if result.is_err() {
            self.cancel.cancel();
        }
        result
    }
    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.request(method, params, Some(Duration::from_secs(30)))
            .await
    }
    pub async fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.write_frame(
            json!({"jsonrpc":"2.0","method":method,"params":params}),
            Duration::from_secs(30),
        )
        .await
    }
    pub async fn reply(&self, id: Value, result: Result<Value>) -> Result<()> {
        let frame = match result {
            Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
            Err(error) => {
                json!({"jsonrpc":"2.0","id":id,"error":{"code":error.downcast_ref::<RpcError>().map(|e| e.code).unwrap_or(-32603),"message":error.to_string()}})
            }
        };
        self.write_frame(frame, Duration::from_secs(30)).await
    }
    pub async fn close(&self) -> Result<()> {
        self.cancel.cancel();
        self.process.terminate(Duration::from_secs(2)).await
    }
}
