use std::{
    collections::{BTreeMap, HashMap},
    fmt,
    path::PathBuf,
    sync::{
        Arc, LazyLock, Mutex, Weak,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use futures_util::{StreamExt, stream::FuturesUnordered};
use rand::RngCore;
use reqwest::{Client, Method, Response};
use serde_json::Value;
use tokio::{
    io::AsyncReadExt,
    sync::{mpsc, oneshot, watch},
};
use tokio_util::sync::CancellationToken;

use super::bridge::Bridge;
use crate::providers::registry::catalog::refresh_activity;
use crate::{
    process::{self, CommandSpec, ManagedProcess},
    providers::api::{LaunchContext, ProviderSpec},
};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
const MAX_SSE_EVENT_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Generation {
    Legacy,
    V2,
}

type StartResult = std::result::Result<Arc<Runtime>, Arc<str>>;
type Pending = watch::Receiver<Option<StartResult>>;

#[derive(Default)]
struct ManagerState {
    current: HashMap<String, Weak<Runtime>>,
    pending: HashMap<String, Pending>,
    sessions: HashMap<String, Weak<Runtime>>,
    generations: Vec<Weak<Runtime>>,
}

pub struct RuntimeManager {
    spec: ProviderSpec,
    selected: AtomicU8,
    state: Arc<Mutex<ManagerState>>,
    cancel: CancellationToken,
}

impl RuntimeManager {
    pub fn new(spec: ProviderSpec) -> Self {
        Self {
            spec,
            selected: AtomicU8::new(0),
            state: Arc::new(Mutex::new(ManagerState::default())),
            cancel: CancellationToken::new(),
        }
    }

    pub async fn generation(&self, cancel: CancellationToken) -> Result<Generation> {
        match self.selected.load(Ordering::Acquire) {
            1 => return Ok(Generation::Legacy),
            2 => return Ok(Generation::V2),
            _ => {}
        }
        let (program, prefix) = self
            .spec
            .command
            .split_first()
            .ok_or_else(|| anyhow!("OpenCode command is empty"))?;
        let mut args = prefix.to_vec();
        args.push("--version".into());
        let probe = process::run(
            CommandSpec {
                program: program.clone(),
                args,
                cwd: None,
                env: provider_environment(&self.spec, None),
                clear_env: true,
            },
            64 * 1024,
            Duration::from_secs(5),
            cancel.clone(),
        );
        let output = tokio::select! {
            biased;
            _ = cancel.cancelled() => bail!("OpenCode version discovery canceled"),
            _ = self.cancel.cancelled() => bail!("OpenCode runtime is closed"),
            result = probe => result,
        };
        // Legacy wrappers are permitted not to support --version. Discovery
        // failure and non-version output deliberately remain uncached.
        let Ok(output) = output else {
            return Ok(Generation::Legacy);
        };
        if output.status != 0 {
            return Ok(Generation::Legacy);
        }
        let text = String::from_utf8_lossy(&output.stdout);
        static VERSION_PATTERN: LazyLock<regex::Regex> = LazyLock::new(|| {
            regex::Regex::new(
                r"(?i)^(?:opencode\s+)?v?([0-9]+)\.([0-9]+)\.([0-9]+)(?:[-+][A-Za-z0-9_.-]+)?$",
            )
            .expect("constant OpenCode version pattern")
        });
        let Some(version) = VERSION_PATTERN.captures(text.trim()) else {
            return Ok(Generation::Legacy);
        };
        let generation = match &version[1] {
            "1" => Generation::Legacy,
            "2" => {
                let minor = version[2].trim_start_matches('0');
                let patch = version[3].trim_start_matches('0');
                if minor.is_empty() && patch.len() < 2 {
                    bail!(
                        "OpenCode {}.{}.{} is too old for this Paseo integration. Update OpenCode to 2.0.10 or newer, then refresh the provider in Paseo.",
                        &version[1],
                        &version[2],
                        &version[3]
                    );
                }
                Generation::V2
            }
            major => {
                bail!("Unsupported OpenCode major version {major}; supported versions are 1 and 2")
            }
        };
        self.selected.store(
            if generation == Generation::Legacy {
                1
            } else {
                2
            },
            Ordering::Release,
        );
        Ok(generation)
    }

    pub async fn acquire(
        &self,
        context: Option<&LaunchContext>,
        config: &Value,
        cancel: CancellationToken,
    ) -> Result<Arc<Runtime>> {
        self.acquire_inner(context, config, cancel, false, false)
            .await
    }

    pub async fn acquire_catalog(
        &self,
        fresh: bool,
        cancel: CancellationToken,
    ) -> Result<Arc<Runtime>> {
        self.acquire_inner(None, &serde_json::json!({}), cancel, fresh, true)
            .await
    }

    async fn acquire_inner(
        &self,
        context: Option<&LaunchContext>,
        config: &Value,
        cancel: CancellationToken,
        fresh: bool,
        catalog: bool,
    ) -> Result<Arc<Runtime>> {
        if self.cancel.is_cancelled() {
            bail!("OpenCode runtime is closed");
        }
        if cancel.is_cancelled() {
            bail!("OpenCode server acquisition canceled");
        }
        let generation = self.generation(cancel.clone()).await?;
        let acquire = self.acquire_generation(context, config, cancel, fresh, generation);
        if catalog && generation == Generation::Legacy {
            refresh_activity("server.acquire", acquire).await
        } else {
            acquire.await
        }
    }

    async fn acquire_generation(
        &self,
        context: Option<&LaunchContext>,
        config: &Value,
        cancel: CancellationToken,
        fresh: bool,
        generation: Generation,
    ) -> Result<Arc<Runtime>> {
        let home = paseo_home(&self.spec, context)?;
        let manifest = context
            .map(|c| c.tool_manifest.clone())
            .unwrap_or(Value::Null);
        let dedicated = context.is_some_and(|c| {
            c.env
                .keys()
                .any(|key| !matches!(key.as_str(), "PASEO_AGENT_ID" | "PASEO_AGENT_CWD"))
        }) || has_entries(config.get("mcpServers"))
            || context.is_some_and(|c| has_entries(Some(&c.mcp_servers)));
        // The complete manifest is fixed at plugin import. Session-scoped
        // catalogs are applied separately by the bridge, never as registration.
        let key = if dedicated {
            uuid::Uuid::new_v4().to_string()
        } else {
            serde_json::to_string(&(
                home.as_os_str().to_string_lossy(),
                generation == Generation::V2,
                &manifest,
            ))?
        };
        let mut pending = {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            if !fresh {
                if let Some(runtime) = state
                    .current
                    .get(&key)
                    .and_then(Weak::upgrade)
                    .filter(|r| !r.is_exited())
                {
                    return Ok(runtime);
                }
            }
            if let Some(pending) = state.pending.get(&key) {
                pending.clone()
            } else {
                let (sender, receiver) = watch::channel(None);
                state.pending.insert(key.clone(), receiver.clone());
                let spec = self.spec.clone();
                let environment =
                    provider_environment(&spec, if dedicated { context } else { None });
                let shared = self.state.clone();
                let stop = self.cancel.clone();
                tokio::spawn(async move {
                    let result =
                        Runtime::start(generation, spec, home, environment, manifest, stop)
                            .await
                            .map_err(|error| Arc::<str>::from(error.to_string()));
                    {
                        let mut state = shared.lock().unwrap_or_else(|p| p.into_inner());
                        state.pending.remove(&key);
                        if let Ok(runtime) = &result {
                            if !dedicated {
                                state.current.insert(key, Arc::downgrade(runtime));
                            }
                            state
                                .generations
                                .retain(|runtime| runtime.strong_count() != 0);
                            state.generations.push(Arc::downgrade(runtime));
                        }
                    }
                    // The sender and local result hold the startup lease only
                    // until delivery. Canceled acquisitions leave no owner.
                    let _ = sender.send(Some(result));
                });
                receiver
            }
        };
        loop {
            if let Some(result) = pending.borrow_and_update().clone() {
                let runtime = result.map_err(|error| anyhow!(error.to_string()))?;
                if self.cancel.is_cancelled() {
                    bail!("OpenCode runtime is closed");
                }
                if cancel.is_cancelled() {
                    bail!("OpenCode server acquisition canceled");
                }
                if runtime.is_exited() {
                    bail!("OpenCode helper server exited");
                }
                return Ok(runtime);
            }
            tokio::select! {
                biased;
                _ = cancel.cancelled() => bail!("OpenCode server acquisition canceled"),
                _ = self.cancel.cancelled() => bail!("OpenCode runtime is closed"),
                changed = pending.changed() => { changed.context("OpenCode server startup ended without a result")?; }
            }
        }
    }

    pub fn remember_session(&self, id: &str, runtime: &Arc<Runtime>) {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .sessions
            .insert(id.to_owned(), Arc::downgrade(runtime));
    }

    pub fn forget_session(&self, id: &str) {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .sessions
            .remove(id);
    }

    pub fn acquire_existing(&self, id: &str) -> Option<Arc<Runtime>> {
        if self.cancel.is_cancelled() {
            return None;
        }
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .sessions
            .get(id)
            .and_then(Weak::upgrade)
            .filter(|runtime| !runtime.is_exited())
    }

    pub async fn shutdown(&self) -> Result<()> {
        self.cancel.cancel();
        let pending = {
            let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            state.pending.values().cloned().collect::<Vec<_>>()
        };
        for mut receiver in pending {
            while receiver.borrow_and_update().is_none() {
                if receiver.changed().await.is_err() {
                    break;
                }
            }
        }
        let runtimes = {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            state.current.clear();
            state.sessions.clear();
            state
                .generations
                .drain(..)
                .filter_map(|runtime| runtime.upgrade())
                .collect::<Vec<_>>()
        };
        let mut stops = runtimes
            .iter()
            .map(|runtime| runtime.stop())
            .collect::<FuturesUnordered<_>>();
        let mut failure = None;
        while let Some(result) = stops.next().await {
            if let Err(error) = result {
                failure.get_or_insert(error);
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
        Ok(())
    }
}

impl Drop for RuntimeManager {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

pub struct Runtime {
    pub generation: Generation,
    pub environment: BTreeMap<String, String>,
    process: Arc<ManagedProcess>,
    client: Client,
    base_url: String,
    password: Option<String>,
    username: String,
    exited: CancellationToken,
    bridge: Bridge,
}

impl Runtime {
    async fn start(
        generation: Generation,
        spec: ProviderSpec,
        home: PathBuf,
        environment: BTreeMap<String, String>,
        manifest: Value,
        cancel: CancellationToken,
    ) -> Result<Arc<Self>> {
        let bridge = Bridge::start(&home, generation, manifest).await?;
        let mut process_env = environment.clone();
        bridge.decorate_env(&mut process_env, generation)?;
        let password = if generation == Generation::V2 {
            let mut secret = [0u8; 32];
            rand::rngs::OsRng.fill_bytes(&mut secret);
            let password = URL_SAFE_NO_PAD.encode(secret);
            process_env.insert("OPENCODE_PASSWORD".into(), password.clone());
            Some(password)
        } else {
            process_env
                .get("OPENCODE_SERVER_PASSWORD")
                .filter(|value| !value.is_empty())
                .cloned()
        };
        let username = if generation == Generation::V2 {
            "opencode".to_owned()
        } else {
            process_env
                .get("OPENCODE_SERVER_USERNAME")
                .cloned()
                .unwrap_or_else(|| "opencode".to_owned())
        };
        let cwd = home.join("opencode-home");
        tokio::fs::create_dir_all(&cwd).await?;
        let (program, prefix) = spec
            .command
            .split_first()
            .ok_or_else(|| anyhow!("OpenCode command is empty"))?;
        let mut args = prefix.to_vec();
        let legacy_port = if generation == Generation::Legacy {
            Some(
                tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
                    .await?
                    .local_addr()?
                    .port(),
            )
        } else {
            None
        };
        args.push("serve".to_owned());
        if generation == Generation::V2 {
            args.extend(["--hostname".to_owned(), "127.0.0.1".to_owned()]);
        }
        args.extend(["--port".to_owned(), legacy_port.unwrap_or(0).to_string()]);
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let (process, mut stdout) = ManagedProcess::spawn(CommandSpec {
            program: program.clone(),
            args,
            cwd: Some(cwd),
            env: process_env,
            clear_env: true,
        })
        .await?;
        let _ = process.close_stdin().await;
        let exited = CancellationToken::new();
        let watcher = process.clone();
        let process_exit = exited.clone();
        tokio::spawn(async move {
            let _ = watcher.wait().await;
            process_exit.cancel();
        });
        let (listening_tx, listening_rx) = oneshot::channel();
        tokio::spawn(async move {
            let mut sender = Some(listening_tx);
            let mut buffer = Vec::with_capacity(8192);
            let mut chunk = [0u8; 4096];
            loop {
                match stdout.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(count) => {
                        if sender.is_none() {
                            continue;
                        }
                        if buffer.len() + count > 8192 {
                            buffer.drain(..buffer.len().saturating_add(count).saturating_sub(8192));
                        }
                        buffer.extend_from_slice(&chunk[..count]);
                        let text = String::from_utf8_lossy(&buffer);
                        let url = if let Some(port) = legacy_port {
                            text.contains("listening on")
                                .then(|| format!("http://127.0.0.1:{port}"))
                        } else {
                            listening_url(&text)
                        };
                        if let Some(url) = url {
                            if let Some(sender) = sender.take() {
                                let _ = sender.send(url);
                            }
                        }
                    }
                }
            }
        });
        let deadline = tokio::time::Instant::now() + STARTUP_TIMEOUT;
        let started: Result<String> = tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(anyhow!("OpenCode runtime is closed")),
            _ = exited.cancelled() => Err(anyhow!("OpenCode helper server exited during startup")),
            result = tokio::time::timeout_at(deadline, listening_rx) => {
                result.context("OpenCode server startup timed out after 30s")
                    .and_then(|result| result.context("OpenCode helper server ended before listening"))
            }
        };
        let base_url = match started {
            Ok(url) => url,
            Err(error) => {
                let _ = process.terminate(SHUTDOWN_GRACE).await;
                return Err(error);
            }
        };
        let runtime = Arc::new(Self {
            generation,
            environment,
            process,
            client,
            base_url,
            username,
            password,
            exited,
            bridge,
        });
        if generation == Generation::V2 {
            let ready = tokio::time::timeout_at(
                deadline,
                runtime.request(Method::GET, "/api/info", &[], None, cancel.clone()),
            )
            .await;
            if let Err(error) = ready
                .context("OpenCode server startup timed out after 30s")
                .and_then(|result| result)
            {
                let _ = runtime.stop().await;
                return Err(error);
            }
        }
        if cancel.is_cancelled() {
            let _ = runtime.stop().await;
            bail!("OpenCode runtime is closed");
        }
        Ok(runtime)
    }

    pub fn is_exited(&self) -> bool {
        self.exited.is_cancelled()
    }

    pub fn bind_session(&self, id: &str, context: &LaunchContext) -> Result<()> {
        self.bridge.bind_session(id, context)
    }

    pub fn unbind_session(&self, id: &str, agent_id: &str) {
        self.bridge.unbind_session(id, agent_id);
    }

    pub async fn stop(&self) -> Result<()> {
        self.exited.cancel();
        let bridge = self.bridge.shutdown().await;
        let process = self.process.terminate(SHUTDOWN_GRACE).await;
        bridge?;
        process
    }

    fn builder(
        &self,
        method: Method,
        path: &str,
        query: &[(String, String)],
    ) -> Result<reqwest::RequestBuilder> {
        if !path.starts_with('/') || path.starts_with("//") {
            bail!("OpenCode request path must be absolute and local");
        }
        let url = reqwest::Url::parse(&format!("{}{path}", self.base_url))?;
        let mut request = self.client.request(method, url).query(query);
        if let Some(password) = &self.password {
            request = request.basic_auth(&self.username, Some(password));
        }
        Ok(request)
    }

    pub async fn request(
        &self,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<Value>,
        cancel: CancellationToken,
    ) -> Result<Value> {
        let mut request = self.builder(method, path, query)?;
        if let Some(body) = body {
            request = request.json(&body);
        }
        tokio::select! {
            biased;
            _ = cancel.cancelled() => bail!("OpenCode request canceled"),
            _ = self.exited.cancelled() => bail!("OpenCode helper server exited"),
            result = async {
                let response = request.send().await?;
                check_response(path, &response)?;
                if response.status() == reqwest::StatusCode::NO_CONTENT { return Ok(Value::Null); }
                let bytes = response.bytes().await?;
                if bytes.is_empty() { return Ok(Value::Null); }
                serde_json::from_slice(&bytes).with_context(|| format!("OpenCode {path} returned malformed JSON"))
            } => result,
        }
    }

    pub async fn events(
        &self,
        path: &str,
        query: &[(String, String)],
        cancel: CancellationToken,
    ) -> Result<Response> {
        let request = self
            .builder(Method::GET, path, query)?
            .header(reqwest::header::ACCEPT, "text/event-stream");
        tokio::select! {
            biased;
            _ = cancel.cancelled() => bail!("OpenCode event request canceled"),
            _ = self.exited.cancelled() => bail!("OpenCode helper server exited"),
            response = request.send() => {
                let response = response?;
                check_response(path, &response)?;
                let content_type = response.headers().get(reqwest::header::CONTENT_TYPE).and_then(|value| value.to_str().ok()).unwrap_or("");
                if !content_type.split(';').next().is_some_and(|value| value.trim().eq_ignore_ascii_case("text/event-stream")) {
                    bail!("OpenCode {path} returned an unsupported event content type");
                }
                Ok(response)
            }
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.exited.cancel();
        self.bridge.close();
        let process = self.process.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                if let Err(error) = process.terminate(SHUTDOWN_GRACE).await {
                    tracing::warn!(%error, "OpenCode helper shutdown failed");
                }
            });
        }
    }
}

#[derive(Debug)]
pub struct OpenCodeHttpError {
    pub operation: String,
    pub status: u16,
    pub html: bool,
}
impl fmt::Display for OpenCodeHttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.html {
            write!(
                f,
                "OpenCode {} returned HTML instead of JSON; incompatible OpenCode API (HTTP {})",
                self.operation, self.status
            )
        } else {
            write!(
                f,
                "OpenCode {} failed (HTTP {})",
                self.operation, self.status
            )
        }
    }
}
impl std::error::Error for OpenCodeHttpError {}

fn check_response(path: &str, response: &Response) -> Result<()> {
    let html = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("text/html"));
    if !response.status().is_success() || html {
        return Err(OpenCodeHttpError {
            operation: path.to_owned(),
            status: response.status().as_u16(),
            html,
        }
        .into());
    }
    Ok(())
}

pub async fn read_sse(
    response: Response,
    cancel: CancellationToken,
    sender: mpsc::Sender<Value>,
) -> Result<()> {
    let v2 = response.url().path().starts_with("/api/");
    let mut stream = response.bytes_stream();
    let mut line = Vec::new();
    let mut data = Vec::new();
    let mut data_lines = 0usize;
    let mut event_bytes = 0usize;
    let mut previous_cr = false;
    let mut first_line = true;
    let mut utf16 = DeltaUtf16::default();
    loop {
        let next = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(()),
            _ = sender.closed() => return Ok(()),
            next = stream.next() => next,
        };
        let Some(chunk) = next else {
            // Only the v2 SDK delivers a final unterminated event on EOF.
            if v2 {
                if !line.is_empty() {
                    append_sse_data(&line, &mut data, &mut data_lines, first_line);
                }
                emit_sse(&mut data, &mut data_lines, &cancel, &sender, v2, &mut utf16).await?;
            }
            return Ok(());
        };
        for byte in chunk? {
            if previous_cr && byte == b'\n' {
                previous_cr = false;
                continue;
            }
            previous_cr = byte == b'\r';
            event_bytes += 1;
            if event_bytes > MAX_SSE_EVENT_BYTES {
                bail!("OpenCode SSE event exceeds 16 MiB");
            }
            if byte == b'\r' || byte == b'\n' {
                if line.is_empty() {
                    emit_sse(&mut data, &mut data_lines, &cancel, &sender, v2, &mut utf16).await?;
                    event_bytes = 0;
                } else {
                    append_sse_data(&line, &mut data, &mut data_lines, first_line);
                    line.clear();
                }
                first_line = false;
            } else {
                line.push(byte);
            }
        }
    }
}

fn append_sse_data(line: &[u8], data: &mut Vec<u8>, data_lines: &mut usize, first_line: bool) {
    let line = if first_line {
        line.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(line)
    } else {
        line
    };
    if let Some(value) = line.strip_prefix(b"data:") {
        let text = String::from_utf8_lossy(value);
        if *data_lines != 0 {
            data.push(b'\n');
        }
        *data_lines += 1;
        data.extend_from_slice(text.trim_start().as_bytes());
    }
}

async fn emit_sse(
    data: &mut Vec<u8>,
    data_lines: &mut usize,
    cancel: &CancellationToken,
    sender: &mpsc::Sender<Value>,
    v2: bool,
    utf16: &mut DeltaUtf16,
) -> Result<()> {
    let present = *data_lines != 0;
    *data_lines = 0;
    if !present || (v2 && data.is_empty()) {
        return Ok(());
    }
    let event = if v2 {
        utf16.parse(data)?
    } else {
        // The legacy generated SDK yields non-JSON data records as strings.
        serde_json::from_slice(data)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(data).into_owned()))
    };
    data.clear();
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Ok(()),
        result = sender.send(event) => result.map_err(|_| anyhow!("OpenCode event consumer closed")),
    }
}

type DeltaPart = (String, String, &'static str, u64);

/// JSON.parse permits UTF-16 halves in separate deltas, while Rust strings do
/// not. Keep only a trailing high surrogate until its matching low arrives;
/// ordinary JSON bypasses UTF-16 reconstruction.
#[derive(Default)]
struct DeltaUtf16 {
    pending: HashMap<DeltaPart, u16>,
}

impl DeltaUtf16 {
    fn parse(&mut self, bytes: &[u8]) -> Result<Value> {
        let error = match serde_json::from_slice::<Value>(bytes) {
            Ok(event) => {
                if !self.pending.is_empty() {
                    if let Some(key) = delta_part(&event) {
                        let event_type = event.get("type").and_then(Value::as_str).unwrap_or("");
                        if !event_type.ends_with(".delta") {
                            self.pending.remove(&key);
                        } else if self.pending.contains_key(&key)
                            && event
                                .pointer("/data/delta")
                                .and_then(Value::as_str)
                                .is_some_and(|text| !text.is_empty())
                        {
                            bail!("OpenCode text delta contains an unpaired UTF-16 surrogate");
                        }
                    }
                }
                return Ok(event);
            }
            Err(error) => error,
        };
        let mut position = 0;
        while position < bytes.len() {
            if bytes[position] != b'"' {
                position += 1;
                continue;
            }
            let Some(end) = json_string_end(bytes, position) else {
                break;
            };
            if &bytes[position..end] != b"\"delta\"" {
                position = end;
                continue;
            }
            let mut start = end;
            while bytes.get(start).is_some_and(u8::is_ascii_whitespace) {
                start += 1;
            }
            if bytes.get(start) != Some(&b':') {
                position = end;
                continue;
            }
            start += 1;
            while bytes.get(start).is_some_and(u8::is_ascii_whitespace) {
                start += 1;
            }
            if bytes.get(start) != Some(&b'"') {
                position = end;
                continue;
            }
            let Some(stop) = json_string_end(bytes, start) else {
                break;
            };
            position = stop;
            if serde_json::from_slice::<String>(&bytes[start..stop]).is_ok() {
                continue;
            }
            // Only replace the exact data.delta token for structural parsing.
            // Every other field must still be valid JSON and remain unchanged.
            let marker = uuid::Uuid::new_v4().to_string();
            let replacement = serde_json::to_vec(&marker)?;
            let mut sanitized = Vec::with_capacity(bytes.len() + replacement.len());
            sanitized.extend_from_slice(&bytes[..start]);
            sanitized.extend_from_slice(&replacement);
            sanitized.extend_from_slice(&bytes[stop..]);
            let Ok(mut event) = serde_json::from_slice::<Value>(&sanitized) else {
                continue;
            };
            if event.pointer("/data/delta").and_then(Value::as_str) != Some(marker.as_str())
                || !event
                    .get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| {
                        matches!(kind, "session.text.delta" | "session.reasoning.delta")
                    })
            {
                continue;
            }
            let Some(key) = delta_part(&event) else {
                continue;
            };
            let mut units = Vec::with_capacity(stop - start);
            if let Some(high) = self.pending.remove(&key) {
                units.push(high);
            }
            decode_json_utf16(&bytes[start..stop], &mut units)?;
            let mut decoded = char::decode_utf16(units).peekable();
            let mut text = String::new();
            while let Some(character) = decoded.next() {
                match character {
                    Ok(character) => text.push(character),
                    Err(error)
                        if (0xd800..=0xdbff).contains(&error.unpaired_surrogate())
                            && decoded.peek().is_none() =>
                    {
                        self.pending.insert(key, error.unpaired_surrogate());
                        break;
                    }
                    Err(_) => bail!("OpenCode text delta contains an unpaired UTF-16 surrogate"),
                }
            }
            event["data"]["delta"] = Value::String(text);
            return Ok(event);
        }
        Err(error).context("OpenCode event stream returned malformed JSON")
    }
}

fn delta_part(event: &Value) -> Option<DeltaPart> {
    let family = match event.get("type")?.as_str()? {
        "session.text.delta" | "session.text.started" | "session.text.ended" => "text",
        "session.reasoning.delta" | "session.reasoning.started" | "session.reasoning.ended" => {
            "reasoning"
        }
        _ => return None,
    };
    let data = event.get("data")?;
    Some((
        data.get("sessionID")?.as_str()?.to_owned(),
        data.get("assistantMessageID")?.as_str()?.to_owned(),
        family,
        data.get("ordinal")?.as_u64()?,
    ))
}

fn json_string_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut position = start + 1;
    while position < bytes.len() {
        match bytes[position] {
            b'\\' => position += 2,
            b'"' => return Some(position + 1),
            _ => position += 1,
        }
    }
    None
}

fn decode_json_utf16(raw: &[u8], units: &mut Vec<u16>) -> Result<()> {
    let mut position = 1;
    while position < raw.len() - 1 {
        if raw[position] != b'\\' {
            let start = position;
            while position < raw.len() - 1 && raw[position] != b'\\' {
                if raw[position] < 0x20 {
                    bail!("OpenCode delta contains an invalid JSON control character");
                }
                position += 1;
            }
            units.extend(std::str::from_utf8(&raw[start..position])?.encode_utf16());
            continue;
        }
        position += 1;
        let escape = *raw
            .get(position)
            .ok_or_else(|| anyhow!("Incomplete OpenCode JSON escape"))?;
        position += 1;
        let unit = match escape {
            b'"' => b'"' as u16,
            b'\\' => b'\\' as u16,
            b'/' => b'/' as u16,
            b'b' => 8,
            b'f' => 12,
            b'n' => 10,
            b'r' => 13,
            b't' => 9,
            b'u' => {
                let digits = raw
                    .get(position..position + 4)
                    .ok_or_else(|| anyhow!("Incomplete OpenCode Unicode escape"))?;
                position += 4;
                u16::from_str_radix(std::str::from_utf8(digits)?, 16)
                    .context("Invalid OpenCode Unicode escape")?
            }
            _ => bail!("Invalid OpenCode JSON escape"),
        };
        units.push(unit);
    }
    Ok(())
}

fn listening_url(text: &str) -> Option<String> {
    let (_, after) = text.rsplit_once("server listening on http://127.0.0.1:")?;
    let end = after.find('\n')?;
    let port = after[..end].trim_end_matches('\r').parse::<u16>().ok()?;
    (port != 0).then(|| format!("http://127.0.0.1:{port}"))
}

fn has_entries(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Object(map)) => !map.is_empty(),
        Some(Value::Array(list)) => !list.is_empty(),
        _ => false,
    }
}

fn provider_environment(
    spec: &ProviderSpec,
    context: Option<&LaunchContext>,
) -> BTreeMap<String, String> {
    let mut environment: BTreeMap<String, String> = std::env::vars().collect();
    environment.extend(spec.env.clone());
    if let Some(context) = context {
        environment.extend(context.env.clone());
    }
    for key in [
        "CLAUDECODE",
        "CLAUDE_CODE_ENTRYPOINT",
        "CLAUDE_CODE_SSE_PORT",
        "CLAUDE_AGENT_SDK_VERSION",
        "PASEO_NODE_ENV",
        "PASEO_DESKTOP_MANAGED",
        "PASEO_SUPERVISED",
        "ELECTRON_RUN_AS_NODE",
        "ELECTRON_NO_ATTACH_CONSOLE",
        "ESBUILD_BINARY_PATH",
    ] {
        environment.remove(key);
    }
    environment
}

fn paseo_home(spec: &ProviderSpec, context: Option<&LaunchContext>) -> Result<PathBuf> {
    if let Some(context) = context {
        return Ok(context.home.clone());
    }
    let configured = spec
        .env
        .get("PASEO_HOME")
        .cloned()
        .or_else(|| std::env::var("PASEO_HOME").ok());
    let home = dirs::home_dir().ok_or_else(|| anyhow!("Unable to resolve Paseo home directory"))?;
    let path = match configured.as_deref() {
        None | Some("~") => {
            if configured.is_none() {
                home.join(".paseo")
            } else {
                home
            }
        }
        Some(path) if path.starts_with("~/") => home.join(&path[2..]),
        Some(path) => PathBuf::from(path),
    };
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}
