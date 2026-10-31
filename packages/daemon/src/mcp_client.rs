//! External MCP client used by the native provider bridges, not Paseo's MCP server.
//! Protocol/lifecycle authority: server/agent/providers/omp/mcp-bridge.ts and the
//! MCP SDK client transports. Only GET event streams resume; tool POSTs never replay.

mod http;
mod schema;
mod sse;
#[cfg(test)]
mod tests;

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    sync::{mpsc, oneshot, watch},
    time::Instant,
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

use self::http::HttpTransport;
use crate::process::{CommandSpec, ManagedProcess};

const SERVER_TIMEOUT: Duration = Duration::from_secs(10);
const CALL_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_MESSAGE: usize = 16 * 1024 * 1024;
const STDIO_LIMIT: usize = 10 * 1024 * 1024;
const MAX_PENDING: usize = 1024;
const VERSIONS: &[&str] = &[
    "2025-11-25",
    "2025-06-18",
    "2025-03-26",
    "2024-11-05",
    "2024-10-07",
];
type Reply = std::result::Result<Value, String>;
type Cleanup = Option<std::result::Result<(), Arc<str>>>;

/// One initialized external server connection. Dropping its last owner also
/// releases the owned process/HTTP session; explicit close waits for cleanup.
pub struct McpConnection {
    state: Arc<State>,
    transport: Arc<Transport>,
    cleanup: watch::Receiver<Cleanup>,
    tools: Mutex<HashMap<String, schema::ToolMetadata>>,
}

struct State {
    stop: CancellationToken,
    pending: Mutex<HashMap<u64, oneshot::Sender<Reply>>>,
    controls: mpsc::Sender<Value>,
    next_id: AtomicU64,
}

enum Transport {
    Stdio(Arc<ManagedProcess>),
    Http(HttpTransport),
}

impl McpConnection {
    pub async fn connect(
        name: &str,
        config: &Value,
        cwd: &Path,
        env: &BTreeMap<String, String>,
        cancel: CancellationToken,
    ) -> Result<Arc<Self>> {
        if cancel.is_cancelled() {
            bail!("MCP connection cancelled");
        }
        let (controls, control_rx) = mpsc::channel(64);
        let state = Arc::new(State {
            stop: cancel.child_token(),
            pending: Mutex::new(HashMap::new()),
            controls,
            next_id: AtomicU64::new(0),
        });
        let tasks = TaskTracker::new();
        let transport = match config.get("type").and_then(Value::as_str) {
            Some("stdio") => {
                let program = schema::string(config, "command")?.to_owned();
                let args = match config.get("args") {
                    None => Vec::new(),
                    Some(Value::Array(args)) => args
                        .iter()
                        .map(|arg| {
                            arg.as_str()
                                .map(str::to_owned)
                                .context("MCP command args must be strings")
                        })
                        .collect::<Result<Vec<_>>>()?,
                    _ => bail!("MCP command args must be an array"),
                };
                let mut overlay = default_environment();
                overlay.extend(env.iter().map(|(key, value)| (key.clone(), value.clone())));
                if let Some(values) = config.get("env") {
                    for (key, value) in values.as_object().context("MCP env must be an object")? {
                        overlay.insert(
                            key.clone(),
                            value
                                .as_str()
                                .context("MCP env values must be strings")?
                                .to_owned(),
                        );
                    }
                }
                let (process, stdout) = ManagedProcess::spawn(CommandSpec {
                    program,
                    args,
                    cwd: Some(cwd.to_owned()),
                    env: overlay,
                    clear_env: true,
                })
                .await
                .context("starting MCP stdio server")?;
                let reader_state = state.clone();
                tasks.spawn(async move {
                    let result = read_stdio(stdout, &reader_state).await;
                    if !reader_state.stop.is_cancelled() {
                        reader_state.fail(
                            result
                                .err()
                                .map(|e| e.to_string())
                                .unwrap_or_else(|| "MCP stdout closed".into()),
                        );
                    }
                });
                Arc::new(Transport::Stdio(process))
            }
            Some(kind @ ("http" | "sse")) => {
                Arc::new(Transport::Http(HttpTransport::new(config, kind == "sse")?))
            }
            _ => bail!("MCP transport type must be stdio, http, or sse"),
        };
        let (cleaned, cleanup) = watch::channel(None);
        let connection = Arc::new(Self {
            state: state.clone(),
            transport: transport.clone(),
            cleanup,
            tools: Mutex::new(HashMap::new()),
        });
        let control_state = state.clone();
        let control_transport = transport.clone();
        tasks.spawn(
            async move { serve_controls(control_state, control_transport, control_rx).await },
        );
        let cleanup_state = state.clone();
        let cleanup_transport = transport.clone();
        let cleanup_tasks = tasks.clone();
        tokio::spawn(async move {
            cleanup_state.stop.cancelled().await;
            cleanup_state.fail("MCP connection closed".into());
            cleanup_tasks.close();
            cleanup_tasks.wait().await;
            let result = cleanup_transport
                .close()
                .await
                .map_err(|e| Arc::<str>::from(e.to_string()));
            cleaned.send_replace(Some(result));
        });
        let deadline = Instant::now() + SERVER_TIMEOUT;
        let initialize = async {
            if let Transport::Http(http) = transport.as_ref() {
                if http.legacy {
                    start_listener(&tasks, &transport, &state);
                    http.wait_endpoint().await?;
                }
            }
            let result = connection
                .request(
                    "initialize",
                    json!({
                        "protocolVersion": VERSIONS[0], "capabilities": {},
                        "clientInfo": {"name": "paseo-omp-mcp", "version": "1.0.0"}
                    }),
                    state.stop.clone(),
                    deadline,
                )
                .await?;
            let version = schema::initialize(&result, VERSIONS)?;
            if let Transport::Http(http) = transport.as_ref() {
                http.set_protocol(version)?;
            }
            transport
                .send(
                    &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
                    &state,
                )
                .await?;
            if matches!(transport.as_ref(), Transport::Http(http) if !http.legacy) {
                start_listener(&tasks, &transport, &state);
            }
            Ok::<(), anyhow::Error>(())
        };
        let result = tokio::select! {
            biased;
            _ = state.stop.cancelled() => Err(anyhow!("MCP connection cancelled or closed")),
            result = tokio::time::timeout_at(deadline, initialize) => result.context("MCP initialization timed out").and_then(|r| r),
        };
        if let Err(error) = result {
            let _ = connection.close().await;
            return Err(error.context(format!("connecting MCP server {name}")));
        }
        Ok(connection)
    }

    pub async fn list_tools(&self, cancel: CancellationToken) -> Result<Vec<Value>> {
        let deadline = Instant::now() + SERVER_TIMEOUT;
        let mut tools = Vec::new();
        let mut metadata = HashMap::new();
        let mut cursor = None;
        let mut seen = HashSet::new();
        loop {
            let params = cursor
                .take()
                .map(|cursor| json!({"cursor":cursor}))
                .unwrap_or_else(|| json!({}));
            let mut result = self
                .request("tools/list", params, cancel.clone(), deadline)
                .await?;
            schema::result(&result)?;
            let page = result
                .get_mut("tools")
                .context("MCP tools/list result has no tools")?
                .take();
            let Value::Array(page) = page else {
                bail!("MCP tools/list tools must be an array")
            };
            if tools.len().saturating_add(page.len()) > 65_536 {
                bail!("MCP tool inventory exceeds 65536 tools");
            }
            for tool in page {
                let item = schema::tool(&tool)?;
                metadata.insert(schema::string(&tool, "name")?.to_owned(), item);
                tools.push(tool);
            }
            match result.get("nextCursor") {
                None => break,
                Some(Value::String(next)) => {
                    if seen.len() >= 1024 || !seen.insert(next.clone()) {
                        bail!("MCP tools/list pagination did not terminate");
                    }
                    cursor = Some(next.clone());
                }
                _ => bail!("MCP tools/list nextCursor must be a string"),
            }
        }
        *self.tools.lock().unwrap_or_else(|e| e.into_inner()) = metadata;
        Ok(tools)
    }

    /// Returns the MCP result without projecting content into any provider's
    /// tool format. `isError` is a tool result, not a JSON-RPC transport error.
    pub async fn call_tool(
        &self,
        name: &str,
        args: Value,
        cancel: CancellationToken,
    ) -> Result<Value> {
        if !args.is_object() {
            bail!("MCP tool arguments must be an object");
        }
        let metadata = self
            .tools
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .cloned();
        if metadata.as_ref().is_some_and(|m| m.task_required) {
            bail!(
                "MCP tool requires task-based execution, which this synchronous bridge does not advertise"
            );
        }
        let mut result = self
            .request(
                "tools/call",
                json!({"name":name,"arguments":args}),
                cancel,
                Instant::now() + CALL_TIMEOUT,
            )
            .await?;
        schema::call_result(&mut result)?;
        if let Some(validator) = metadata.and_then(|m| m.output) {
            match result.get("structuredContent") {
                Some(content) => {
                    if !validator.is_valid(content) {
                        bail!("MCP structured content does not match the tool output schema");
                    }
                }
                None if result.get("isError") != Some(&Value::Bool(true)) => {
                    bail!("MCP tool has an output schema but returned no structured content")
                }
                None => {}
            }
        }
        Ok(result)
    }

    pub async fn close(&self) -> Result<()> {
        self.state.stop.cancel();
        let mut cleanup = self.cleanup.clone();
        loop {
            if let Some(result) = cleanup.borrow_and_update().clone() {
                return result.map_err(|e| anyhow!(e.to_string()));
            }
            cleanup
                .changed()
                .await
                .context("MCP cleanup supervisor stopped")?;
        }
    }

    async fn request(
        &self,
        method: &str,
        params: Value,
        cancel: CancellationToken,
        deadline: Instant,
    ) -> Result<Value> {
        if cancel.is_cancelled() || self.state.stop.is_cancelled() {
            bail!("MCP request cancelled or connection closed");
        }
        let id = self
            .state
            .next_id
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| {
                // JSON-RPC IDs must remain exactly representable to JS servers.
                (id < 9_007_199_254_740_991).then_some(id + 1)
            })
            .map_err(|_| anyhow!("MCP request ID range exhausted"))?;
        let (sender, mut receiver) = oneshot::channel();
        {
            let mut pending = self.state.pending.lock().unwrap_or_else(|e| e.into_inner());
            if self.state.stop.is_cancelled() {
                bail!("MCP connection closed")
            }
            if pending.len() >= MAX_PENDING {
                bail!("MCP request concurrency exceeds 1024")
            }
            pending.insert(id, sender);
        }
        let mut guard = PendingRequest {
            state: self.state.clone(),
            id,
            reason: "Request cancelled",
        };
        let message = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        let exchange = async {
            let send = async {
                tokio::select! {
                    biased;
                    _ = self.state.stop.cancelled() => Err(anyhow!("MCP connection closed")),
                    result = self.transport.send(&message, &self.state) => result,
                }
            };
            tokio::pin!(send);
            let reply = tokio::select! {
                biased;
                reply = &mut receiver => {
                    // A reply may arrive on the independent GET stream before
                    // its POST finishes. Stdio still finishes its atomic frame.
                    if !self.state.stop.is_cancelled() && matches!(self.transport.as_ref(), Transport::Stdio(_)) { send.await?; }
                    reply
                }
                result = &mut send => {
                    result?;
                    receiver.await
                }
            };
            reply
                .context("MCP response channel closed")?
                .map_err(|e| anyhow!(e))
        };
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(anyhow!("MCP request cancelled")),
            _ = tokio::time::sleep_until(deadline) => {
                guard.reason = "Request timed out";
                Err(anyhow!("MCP {method} request timed out"))
            }
            result = exchange => result,
        }
    }
}

impl Drop for McpConnection {
    fn drop(&mut self) {
        self.state.stop.cancel();
    }
}

struct PendingRequest {
    state: Arc<State>,
    id: u64,
    reason: &'static str,
}

impl Drop for PendingRequest {
    fn drop(&mut self) {
        let removed = self
            .state
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id)
            .is_some();
        if removed && !self.state.stop.is_cancelled() {
            // Synchronous Drop also covers cancellation by dropping the caller's
            // future. No request/HTTP mutation is automatically retried.
            if self
                .state
                .controls
                .try_send(json!({"jsonrpc":"2.0","method":"notifications/cancelled",
                "params":{"requestId":self.id,"reason":self.reason}}))
                .is_err()
            {
                self.state.fail("MCP cancellation queue overflow".into());
            }
        }
    }
}

impl State {
    fn fail(&self, message: String) {
        self.stop.cancel();
        let pending = std::mem::take(&mut *self.pending.lock().unwrap_or_else(|e| e.into_inner()));
        for (_, sender) in pending {
            let _ = sender.send(Err(message.clone()));
        }
    }

    fn has_pending(&self, id: u64) -> bool {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&id)
    }

    fn receive(&self, value: Value) -> Result<()> {
        if let Value::Array(messages) = value {
            if messages.is_empty() {
                bail!("MCP empty JSON-RPC batch")
            }
            for message in messages {
                self.receive_one(message)?;
            }
            Ok(())
        } else {
            self.receive_one(value)
        }
    }

    fn receive_one(&self, mut message: Value) -> Result<()> {
        schema::rpc(&message)?;
        if let Some(method) = message.get("method").and_then(Value::as_str) {
            if let Some(id) = message.get("id") {
                let response = if method == "ping" {
                    json!({"jsonrpc":"2.0","id":id,"result":{}})
                } else {
                    // Like the source Client's empty capabilities: no roots,
                    // sampling, elicitation or experimental task handlers.
                    json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Method not found"}})
                };
                self.controls
                    .try_send(response)
                    .map_err(|_| anyhow!("MCP server request queue overflow"))?;
            }
            return Ok(());
        }
        // A stale/cancelled response must not complete a subsequent request.
        if let Some(id) = message.get("id").and_then(Value::as_u64) {
            let sender = self
                .pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
            if let Some(sender) = sender {
                let result = if let Some(error) = message.get("error") {
                    Err(format!(
                        "MCP JSON-RPC error {}: {}",
                        error["code"],
                        error["message"].as_str().unwrap_or("")
                    ))
                } else {
                    Ok(message
                        .get_mut("result")
                        .expect("validated JSON-RPC result")
                        .take())
                };
                let _ = sender.send(result);
            }
        }
        Ok(())
    }
}

impl Transport {
    async fn send(&self, message: &Value, state: &Arc<State>) -> Result<()> {
        match self {
            Self::Stdio(process) => {
                if let Err(error) = process.write_json(message).await {
                    state.fail("MCP stdin write failed; command admission may be ambiguous".into());
                    return Err(error.context("writing MCP request; not replayed"));
                }
                Ok(())
            }
            Self::Http(http) => http.send(message, state).await,
        }
    }

    async fn close(&self) -> Result<()> {
        match self {
            Self::Stdio(process) => {
                // SDK teardown offers EOF for two seconds, then SIGTERM for
                // two more. ManagedProcess supplies the final tree kill.
                let eof = async {
                    process.close_stdin().await?;
                    process.wait().await
                };
                if matches!(
                    tokio::time::timeout(Duration::from_secs(2), eof).await,
                    Ok(Ok(_))
                ) {
                    Ok(())
                } else {
                    process.terminate(Duration::from_secs(2)).await
                }
            }
            Self::Http(http) => http.close().await,
        }
    }
}

fn start_listener(tasks: &TaskTracker, transport: &Arc<Transport>, state: &Arc<State>) {
    let transport = transport.clone();
    let state = state.clone();
    tasks.spawn(async move {
        if let Transport::Http(http) = transport.as_ref() {
            http.listen(&state).await;
        }
    });
}

async fn serve_controls(
    state: Arc<State>,
    transport: Arc<Transport>,
    mut receiver: mpsc::Receiver<Value>,
) {
    loop {
        let message = tokio::select! {
            biased;
            _ = state.stop.cancelled() => break,
            message = receiver.recv() => match message { Some(message) => message, None => break },
        };
        let result = tokio::select! {
            biased;
            _ = state.stop.cancelled() => break,
            result = tokio::time::timeout(SERVER_TIMEOUT, transport.send(&message, &state)) => result,
        };
        if !matches!(result, Ok(Ok(()))) {
            state.fail("MCP control reply could not be delivered".into());
            break;
        }
    }
}

fn default_environment() -> BTreeMap<String, String> {
    #[cfg(windows)]
    const KEYS: &[&str] = &[
        "APPDATA",
        "HOMEDRIVE",
        "HOMEPATH",
        "LOCALAPPDATA",
        "PATH",
        "PROCESSOR_ARCHITECTURE",
        "SYSTEMDRIVE",
        "SYSTEMROOT",
        "TEMP",
        "USERNAME",
        "USERPROFILE",
        "PROGRAMFILES",
    ];
    #[cfg(not(windows))]
    const KEYS: &[&str] = &["HOME", "LOGNAME", "PATH", "SHELL", "TERM", "USER"];
    KEYS.iter()
        .filter_map(|key| {
            std::env::var(key)
                .ok()
                .filter(|value| !value.starts_with("()"))
                .map(|value| ((*key).to_owned(), value))
        })
        .collect()
}

async fn read_stdio(stdout: tokio::process::ChildStdout, state: &Arc<State>) -> Result<()> {
    let mut reader = BufReader::new(stdout);
    let mut line = Vec::new();
    loop {
        let bytes = tokio::select! {
            biased;
            _ = state.stop.cancelled() => return Ok(()),
            result = reader.fill_buf() => result.context("reading MCP stdout")?,
        };
        if bytes.is_empty() {
            if !line.is_empty() {
                bail!("MCP stdout ended with an incomplete JSON frame")
            }
            return Ok(());
        }
        let newline = bytes.iter().position(|byte| *byte == b'\n');
        let take = newline.map_or(bytes.len(), |index| index + 1);
        if line.len().saturating_add(take) > STDIO_LIMIT {
            bail!("MCP stdio frame exceeds 10 MiB")
        }
        line.extend_from_slice(&bytes[..take]);
        reader.consume(take);
        if newline.is_some() {
            let message = serde_json::from_slice(&line).context("invalid MCP stdout JSON frame")?;
            state.receive(message)?;
            line.clear();
        }
    }
}
