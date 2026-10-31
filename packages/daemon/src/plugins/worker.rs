use crate::{
    process::{CommandSpec, ManagedProcess},
    state::AppState,
    transport::OutboundFrame,
};
use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
    sync::{Arc, Mutex, RwLock, Weak},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    sync::{Semaphore, mpsc, oneshot},
};
use tokio_util::sync::CancellationToken;

const FRAME_LIMIT: usize = 16 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
type Reply = oneshot::Sender<Result<Value>>;

pub struct PluginWorker {
    pub id: String,
    process: Arc<ManagedProcess>,
    cancel: CancellationToken,
    pending: Mutex<HashMap<String, Reply>>,
    connections: Mutex<HashMap<String, mpsc::Sender<Value>>>,
    pub metadata: RwLock<Value>,
    permits: Arc<Semaphore>,
}

impl PluginWorker {
    pub async fn start(
        app: Weak<AppState>,
        id: &str,
        directory: &Path,
        settings: &Path,
        env: BTreeMap<String, String>,
    ) -> Result<Arc<Self>> {
        let state = app.upgrade().context("Daemon is shutting down")?;
        let executable = state.executable.clone();
        let transport = state.transport.clone();
        drop(state);
        let asset = env
            .get("PASEO_PLUGIN_WORKER")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                executable
                    .parent()
                    .unwrap_or(Path::new("."))
                    .join("plugin-worker/worker.mjs")
            });
        let asset = tokio::fs::canonicalize(&asset).await.with_context(|| format!("JavaScript extension {id} requires the optional plugin-worker asset at {} (set PASEO_PLUGIN_WORKER)",asset.display()))?;
        let node = env
            .get("PASEO_PLUGIN_NODE")
            .cloned()
            .unwrap_or_else(|| "node".into());
        let (process, stdout) = ManagedProcess::spawn(CommandSpec { program: node.clone(), args: vec![asset.to_string_lossy().into_owned()], cwd: Some(directory.to_owned()), env, clear_env: true }).await.with_context(|| format!("JavaScript extension {id} requires externally installed Node.js 20+; set PASEO_PLUGIN_NODE to its executable ({node})"))?;
        let worker = Arc::new(Self {
            id: id.to_owned(),
            process,
            cancel: CancellationToken::new(),
            pending: Mutex::new(HashMap::new()),
            connections: Mutex::new(HashMap::new()),
            metadata: RwLock::new(Value::Null),
            permits: Arc::new(Semaphore::new(256)),
        });
        let (ready_tx, ready_rx) = oneshot::channel();
        worker
            .pending
            .lock()
            .unwrap()
            .insert("@ready".into(), ready_tx);
        let (incoming_tx, incoming_rx) = mpsc::channel(16);
        tokio::spawn(run_session_bridge(
            Arc::downgrade(&worker),
            transport,
            incoming_rx,
            worker.cancel.clone(),
        ));
        let weak = Arc::downgrade(&worker);
        let stop = worker.cancel.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            let mut logs = LogCapture::default();
            let result: Result<()> = async {
                loop {
                    let bytes = tokio::select! { _ = stop.cancelled() => return Ok(()), frame = read_frame(&mut reader) => frame? };
                    let message: Value = serde_json::from_slice(&bytes).context("Invalid plugin IPC JSON")?;
                    let Some(worker) = weak.upgrade() else { return Ok(()); };
                    match message["type"].as_str().unwrap_or("") {
                        "log" => {
                            let stream = message["stream"].as_str().context("Plugin log requires stream")?;
                            let bytes = STANDARD.decode(message["data"].as_str().context("Plugin log requires data")?)?;
                            if !["stdout","stderr"].contains(&stream) { bail!("Invalid plugin log stream"); }
                            if let Some(state) = app.upgrade() { logs.write(stream,&bytes,|stream,text| state.plugins.append_log(&worker.id,stream,text)); }
                        }
                        "paseo_frame" => {
                            let frame = if message["isBinary"] == true {
                                OutboundFrame::Binary(STANDARD.decode(message["data"]["$bytes"].as_str().context("Invalid plugin binary frame")?)?.into())
                            } else { OutboundFrame::Text(message["data"].as_str().context("Invalid plugin text frame")?.to_owned()) };
                            tokio::select! { _ = stop.cancelled() => return Ok(()), result = incoming_tx.send(frame) => { result.context("Plugin internal session closed")?; } }
                        }
                        "paseo_close" => { let _ = incoming_tx.send(OutboundFrame::Close { code:1000, reason:"Plugin closed".into() }).await; }
                        "ready" => { validate_ready(&message)?; *worker.metadata.write().unwrap() = message.clone(); worker.reply("@ready", Ok(message)); }
                        "result" => worker.reply(message["requestId"].as_str().unwrap_or(""), Ok(message.get("output").cloned().unwrap_or(Value::Null))),
                        "error" => worker.reply(message["requestId"].as_str().unwrap_or(""), Err(anyhow!(message["error"].as_str().unwrap_or("Plugin request failed").to_owned()))),
                        "fatal" => bail!("{}", message["error"].as_str().unwrap_or("Plugin worker failed")),
                        "hooks.changed" => { validate_hooks(&message["hooks"])?; worker.metadata.write().unwrap()["hooks"] = message["hooks"].clone(); }
                        "settings.changed" => if let Some(state) = app.upgrade() { state.plugins.settings_changed(&worker.id, message["settingsId"].as_str().unwrap_or("")).await; },
                        kind if kind.starts_with("provider.") => {
                            let id = message["connectionId"].as_str().context("Provider message missing connectionId")?;
                            let sender = worker.connections.lock().unwrap().get(id).cloned();
                            if let Some(sender) = sender {
                                // Never lose an event. A blocked consumer is a failed connection, not a dropped event.
                                if sender.try_send(message.clone()).is_err() { worker.connections.lock().unwrap().remove(id); worker.send(json!({"type":"provider.close","connectionId":id})).await?; }
                            } else if kind == "provider.connected" { worker.send(json!({"type":"provider.close","connectionId":id})).await?; }
                        }
                        other => bail!("Unknown plugin worker message: {other}"),
                    }
                }
            }.await;
            if let Some(worker) = weak.upgrade() {
                if let Err(error) = result {
                    worker.fail(error.to_string());
                    let _ = worker.process.terminate(Duration::ZERO).await;
                }
                if let Some(state) = app.upgrade() {
                    logs.flush(|stream, text| state.plugins.append_log(&worker.id, stream, text));
                    state.plugins.worker_failed(&worker).await;
                }
            }
        });
        worker.send(json!({"type":"initialize","pluginId":id,"bundle":"","appVersion":env!("CARGO_PKG_VERSION"),"pluginDirectory":directory,"settingsDirectory":settings})).await?;
        match tokio::time::timeout(REQUEST_TIMEOUT, ready_rx).await {
            Ok(Ok(Ok(_))) => Ok(worker),
            result => {
                let error = match result {
                    Ok(Ok(Err(error))) => error,
                    Ok(Err(_)) => anyhow!("Plugin worker closed during initialization"),
                    Err(_) => anyhow!("Plugin initialization timed out"),
                    _ => unreachable!(),
                };
                let _ = worker.shutdown().await;
                let diagnostics = worker.logs();
                if diagnostics.is_empty() {
                    Err(error)
                } else {
                    Err(anyhow!("{error}\n{diagnostics}"))
                }
            }
        }
    }
    pub fn is_live(&self) -> bool {
        !self.cancel.is_cancelled()
    }
    pub fn cancellation(&self) -> CancellationToken {
        self.cancel.clone()
    }
    pub fn logs(&self) -> String {
        self.process.stderr_tail()
    }
    pub async fn send(&self, message: Value) -> Result<()> {
        if self.cancel.is_cancelled() {
            bail!("Plugin is not available: {}", self.id);
        }
        let bytes = serde_json::to_vec(&message)?;
        if bytes.len() > FRAME_LIMIT {
            bail!("Plugin IPC frame exceeds limit");
        }
        let mut bytes = bytes;
        bytes.push(b'\n');
        tokio::time::timeout(REQUEST_TIMEOUT, self.process.write_bytes(&bytes))
            .await
            .context("Plugin IPC write timed out")??;
        Ok(())
    }
    pub async fn request(&self, mut message: Value, cancel: CancellationToken) -> Result<Value> {
        let _permit = tokio::select! {
            _ = cancel.cancelled() => bail!("Plugin request canceled"),
            _ = self.cancel.cancelled() => bail!("Plugin worker closed"),
            permit = self.permits.clone().acquire_owned() => permit?,
        };
        let id = uuid::Uuid::new_v4().to_string();
        message["requestId"] = json!(id);
        let is_hook = message["type"] == "hook";
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id.clone(), tx);
        let mut guard = PendingRequestGuard {
            worker: self,
            id: id.clone(),
            cancel_hook: is_hook,
        };
        let result = async {
            self.send(message).await?;
            tokio::select! {
                _ = cancel.cancelled() => bail!("Plugin request canceled"),
                result = tokio::time::timeout(REQUEST_TIMEOUT, rx) => result.context("Plugin request timed out")?.context("Plugin worker closed")?,
            }
        }.await;
        guard.cancel_hook = false;
        self.pending.lock().unwrap().remove(&id);
        if result.is_err() && is_hook && self.is_live() {
            let _ = self
                .send(json!({"type":"hook.cancel","requestId":id}))
                .await;
        }
        result
    }
    pub fn register_connection(&self, id: &str) -> Result<mpsc::Receiver<Value>> {
        let mut connections = self.connections.lock().unwrap();
        if connections.contains_key(id) {
            bail!("Duplicate plugin connection: {id}");
        }
        if connections.len() >= 1024 {
            bail!("Plugin connection limit exceeded");
        }
        let (tx, rx) = mpsc::channel(256);
        connections.insert(id.to_owned(), tx);
        Ok(rx)
    }
    pub fn remove_connection(&self, id: &str) {
        self.connections.lock().unwrap().remove(id);
    }
    fn reply(&self, id: &str, value: Result<Value>) {
        if let Some(tx) = self.pending.lock().unwrap().remove(id) {
            let _ = tx.send(value);
        }
    }
    fn fail(&self, error: String) {
        self.cancel.cancel();
        for (_, tx) in self.pending.lock().unwrap().drain() {
            let _ = tx.send(Err(anyhow!(error.clone())));
        }
        self.connections.lock().unwrap().clear();
        self.metadata.write().unwrap()["error"] = json!(error);
    }
    pub async fn shutdown(&self) -> Result<()> {
        if self.is_live() {
            let _ = self.send(json!({"type":"shutdown"})).await;
        }
        let graceful = tokio::time::timeout(Duration::from_secs(2), self.process.wait()).await;
        self.fail(format!("Plugin stopped: {}", self.id));
        if graceful.is_err() {
            self.process.terminate(Duration::ZERO).await?;
        }
        Ok(())
    }
}

struct PendingRequestGuard<'a> {
    worker: &'a PluginWorker,
    id: String,
    cancel_hook: bool,
}
impl Drop for PendingRequestGuard<'_> {
    fn drop(&mut self) {
        self.worker.pending.lock().unwrap().remove(&self.id);
        if self.cancel_hook && self.worker.is_live() {
            let process = self.worker.process.clone();
            let message = json!({"type":"hook.cancel","requestId":self.id});
            tokio::spawn(async move {
                let _ = tokio::time::timeout(REQUEST_TIMEOUT, process.write_json(&message)).await;
            });
        }
    }
}

pub async fn validate(
    directory: &Path,
    executable: &Path,
    env: BTreeMap<String, String>,
    cancel: CancellationToken,
) -> Result<()> {
    let asset = env
        .get("PASEO_PLUGIN_WORKER")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            executable
                .parent()
                .unwrap_or(Path::new("."))
                .join("plugin-worker/worker.mjs")
        });
    let asset = tokio::fs::canonicalize(&asset).await.with_context(|| format!("JavaScript extension compilation requires optional plugin-worker asset at {} (set PASEO_PLUGIN_WORKER)",asset.display()))?;
    let node = env
        .get("PASEO_PLUGIN_NODE")
        .cloned()
        .unwrap_or_else(|| "node".into());
    let result = crate::process::run(CommandSpec {
        program: node, args: vec![asset.to_string_lossy().into_owned(), "--validate".into(), directory.to_string_lossy().into_owned(), env!("CARGO_PKG_VERSION").into()],
        cwd: Some(directory.to_owned()), env, clear_env: true,
    }, 1024*1024, Duration::from_secs(120), cancel).await.context("Plugin compilation requires externally installed Node.js 20+ (PASEO_PLUGIN_NODE) and optional esbuild/TypeScript worker dependencies")?;
    if result.status != 0 {
        bail!("Plugin compilation failed: {}", result.stderr);
    }
    Ok(())
}

async fn read_frame<R: tokio::io::AsyncBufRead + Unpin>(reader: &mut R) -> Result<Vec<u8>> {
    let mut frame = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            bail!("Plugin IPC closed");
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let count = newline.map_or(available.len(), |index| index + 1);
        if frame.len() + count > FRAME_LIMIT {
            bail!("Plugin IPC frame exceeds limit");
        }
        frame.extend_from_slice(&available[..count]);
        reader.consume(count);
        if newline.is_some() {
            return Ok(frame);
        }
    }
}

#[derive(Default)]
struct LogLine {
    bytes: Vec<u8>,
    overflowed: bool,
    activity: u64,
}
#[derive(Default)]
struct LogCapture {
    stdout: LogLine,
    stderr: LogLine,
    sequence: u64,
}
impl LogCapture {
    fn write(&mut self, stream: &str, bytes: &[u8], mut emit: impl FnMut(&str, &str)) {
        let line = if stream == "stdout" {
            &mut self.stdout
        } else {
            &mut self.stderr
        };
        for part in bytes.split_inclusive(|byte| *byte == b'\n') {
            let newline = part.last() == Some(&b'\n');
            let chunk = if newline {
                &part[..part.len() - 1]
            } else {
                part
            };
            if !chunk.is_empty() {
                self.sequence += 1;
                line.activity = self.sequence;
            }
            let remaining = (16 * 1024_usize).saturating_sub(line.bytes.len());
            if chunk.len() > remaining {
                line.overflowed = true;
            }
            line.bytes
                .extend_from_slice(&chunk[..chunk.len().min(remaining)]);
            if newline {
                Self::emit(stream, line, &mut emit);
            }
        }
    }
    fn emit(stream: &str, line: &mut LogLine, emit: &mut impl FnMut(&str, &str)) {
        if !line.overflowed && line.bytes.last() == Some(&b'\r') {
            line.bytes.pop();
        }
        emit(stream, &String::from_utf8_lossy(&line.bytes));
        line.bytes.clear();
        line.overflowed = false;
        line.activity = 0;
    }
    fn flush(&mut self, mut emit: impl FnMut(&str, &str)) {
        let streams = if self.stdout.activity <= self.stderr.activity {
            [("stdout", &mut self.stdout), ("stderr", &mut self.stderr)]
        } else {
            [("stderr", &mut self.stderr), ("stdout", &mut self.stdout)]
        };
        for (stream, line) in streams {
            if !line.bytes.is_empty() || line.overflowed {
                Self::emit(stream, line, &mut emit);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn log_capture_preserves_utf8_across_chunks_and_flush_activity_order() {
        let mut capture = LogCapture::default();
        let mut lines = Vec::new();
        let mut emit = |stream: &str, text: &str| lines.push((stream.to_owned(), text.to_owned()));
        capture.write("stdout", &[0xe2, 0x82], &mut emit);
        capture.write("stdout", &[0xac, b'\r', b'\n'], &mut emit);
        capture.write("stderr", b"first", &mut emit);
        capture.write("stdout", b"second", &mut emit);
        capture.flush(&mut emit);
        assert_eq!(
            lines,
            vec![
                ("stdout".into(), "€".into()),
                ("stderr".into(), "first".into()),
                ("stdout".into(), "second".into())
            ]
        );
    }
    #[tokio::test]
    async fn bounded_frames_reject_oversized_protocol_without_newline() {
        let mut input = std::io::Cursor::new(vec![b'x'; FRAME_LIMIT + 1]);
        assert!(
            read_frame(&mut input)
                .await
                .unwrap_err()
                .to_string()
                .contains("exceeds limit")
        );
        let mut input = std::io::Cursor::new(b"{\"type\":\"ready\"}\n{\"type\":\"result\"}\n");
        assert_eq!(
            read_frame(&mut input).await.unwrap(),
            b"{\"type\":\"ready\"}\n"
        );
        assert_eq!(
            read_frame(&mut input).await.unwrap(),
            b"{\"type\":\"result\"}\n"
        );
    }
}

fn run_session_bridge(
    weak: Weak<PluginWorker>,
    transport: Arc<crate::transport::TransportService>,
    mut incoming: mpsc::Receiver<OutboundFrame>,
    stop: CancellationToken,
) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send>> {
    Box::pin(async move {
        loop {
            let hello = loop {
                let frame = tokio::select! { _ = stop.cancelled() => return, frame = incoming.recv() => match frame { Some(frame) => frame, None => return } };
                let is_hello = match &frame {
                    OutboundFrame::Text(text) => serde_json::from_str::<Value>(text)
                        .is_ok_and(|value| value["type"] == "hello"),
                    _ => false,
                };
                if is_hello {
                    break frame;
                }
                // An ended physical source cannot replay pending requests into its replacement.
            };
            let Some(worker) = weak.upgrade() else {
                return;
            };
            let plugin_id = worker.id.clone();
            drop(worker);
            let (source_tx, source_rx) = mpsc::channel(16);
            let (output_tx, mut output_rx) = mpsc::channel(1);
            let source_cancel = stop.child_token();
            let service = transport.clone();
            let cancel = source_cancel.clone();
            let mut task = tokio::spawn(async move {
                service
                    .open_plugin_session(&plugin_id, source_rx, output_tx, cancel)
                    .await
            });
            if source_tx.send(hello).await.is_err() {
                source_cancel.cancel();
                let _ = task.await;
                continue;
            }
            let mut joined = false;
            loop {
                tokio::select! {
                    _ = stop.cancelled() => break,
                    result = &mut task => {
                        joined = true;
                        if let Ok(Err(error)) = result { tracing::debug!(%error,"Plugin internal source closed"); }
                        break;
                    }
                    frame = incoming.recv() => {
                        let Some(frame) = frame else { source_cancel.cancel(); if !joined { let _ = task.await; } return; };
                        if source_tx.try_send(frame).is_err() { break; }
                    }
                    frame = output_rx.recv() => {
                        let Some(frame) = frame else { break; };
                        let Some(worker) = weak.upgrade() else { source_cancel.cancel(); if !joined { let _ = task.await; } return; };
                        let message = match frame {
                            OutboundFrame::Text(data) => json!({"type":"paseo_frame","data":data,"isBinary":false}),
                            OutboundFrame::Binary(data) => json!({"type":"paseo_frame","data":{"$bytes":STANDARD.encode(data)},"isBinary":true}),
                            OutboundFrame::Flush(ack) => { let _ = ack.send(Ok(())); continue; }
                            OutboundFrame::Close { .. } => break,
                            OutboundFrame::Ping(_) | OutboundFrame::Pong(_) => continue,
                        };
                        if let Err(error) = worker.send(message).await { worker.fail(error.to_string()); break; }
                    }
                }
            }
            source_cancel.cancel();
            if !joined {
                let _ = task.await;
            }
            drop(source_tx);
            drop(output_rx);
            while incoming.try_recv().is_ok() {}
            if stop.is_cancelled() {
                return;
            }
            let Some(worker) = weak.upgrade() else {
                return;
            };
            if let Err(error) = worker.send(json!({"type":"paseo_close"})).await {
                worker.fail(error.to_string());
                return;
            }
        }
    })
}

fn validate_hooks(hooks: &Value) -> Result<()> {
    if !hooks.is_object()
        || !["events", "before"].iter().all(|field| {
            hooks[*field]
                .as_array()
                .is_some_and(|values| values.iter().all(Value::is_string))
        })
    {
        bail!("Invalid plugin hook metadata");
    }
    Ok(())
}
fn validate_ready(message: &Value) -> Result<()> {
    if !message["methods"]
        .as_array()
        .is_some_and(|methods| methods.iter().all(Value::is_string))
    {
        bail!("Invalid plugin RPC metadata");
    }
    for provider in message["providers"]
        .as_array()
        .context("Invalid plugin provider metadata")?
    {
        for field in ["id", "label"] {
            if !provider[field]
                .as_str()
                .is_some_and(|value| !value.is_empty())
            {
                bail!("Invalid plugin provider {field}");
            }
        }
        for field in ["description", "iconPath"] {
            if provider.get(field).is_some_and(|value| !value.is_string()) {
                bail!("Invalid plugin provider {field}");
            }
        }
        for field in ["requiresConfiguration", "hasCatalogCacheKey", "hasStatus"] {
            if provider.get(field).is_some_and(|value| !value.is_boolean()) {
                bail!("Invalid plugin provider {field}");
            }
        }
        if let Some(command) = provider.get("command") {
            if !command.as_array().is_some_and(|args| {
                !args.is_empty()
                    && args
                        .iter()
                        .all(|arg| arg.as_str().is_some_and(|arg| !arg.is_empty()))
            }) {
                bail!("Invalid plugin provider command");
            }
        }
    }
    if let Some(sources) = message.get("usageSources") {
        for source in sources
            .as_array()
            .context("Invalid plugin usage metadata")?
        {
            for field in ["id", "label"] {
                if !source[field]
                    .as_str()
                    .is_some_and(|value| !value.is_empty())
                {
                    bail!("Invalid plugin usage {field}");
                }
            }
            if source.get("icon").is_some_and(|value| !value.is_string()) {
                bail!("Invalid plugin usage icon");
            }
        }
    }
    if let Some(hooks) = message.get("hooks") {
        validate_hooks(hooks)?;
    }
    Ok(())
}
