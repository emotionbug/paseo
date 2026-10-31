use std::{
    collections::BTreeMap,
    path::PathBuf,
    process::ExitStatus,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::Result;
use rand::RngCore;
use serde::ser::{SerializeMap, Serializer};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    sync::{mpsc, oneshot, watch},
};
use tokio_util::sync::CancellationToken;

use crate::{
    process::{CommandSpec, ManagedProcess},
    providers::api::ProviderSpec,
};

const FINGERPRINT: &str = "sha256:e0e163db6ccf00dbe68402ce55d6319b3edc33c421f31e9583b587b2de8a118f";

#[derive(Clone, Debug, thiserror::Error)]
#[error("{message}")]
pub struct MuseError {
    pub kind: String,
    pub message: String,
    pub diagnostic: Option<String>,
    pub reason: Option<String>,
}

impl MuseError {
    pub fn new(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            message: message.into(),
            diagnostic: None,
            reason: None,
        }
    }

    pub fn provider_error(&self) -> Value {
        let mut error = json!({"code": self.kind, "message": self.message});
        if let Some(diagnostic) = &self.diagnostic {
            error["diagnostic"] = diagnostic.clone().into();
        }
        error
    }
}

pub fn actionable_error(error: &anyhow::Error, env: &BTreeMap<String, String>) -> Value {
    let Some(error) = error.downcast_ref::<MuseError>() else {
        return json!({"code": "muse", "message": error.to_string()});
    };
    if error.kind == "internal" && error.message.contains("automated reviewer is unavailable") {
        let config_home = env
            .get("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env.get("HOME").map(String::as_str).unwrap_or("~")).join(".config")
            });
        return json!({
            "code": "defaultProfileUnavailable",
            "message": format!("Change permissions.default_profile in {}: the automated reviewer is unavailable on this host.", config_home.join("muse/settings.json").display()),
        });
    }
    if error.reason.as_deref() == Some("compaction_unavailable") {
        return json!({
            "code": error.kind,
            "message": "Muse cannot compact this session with its current model configuration. Ensure Muse has a context limit for the selected model (context_compaction.provider_context_limit_tokens).",
            "diagnostic": error.diagnostic.as_deref().unwrap_or(&error.message),
        });
    }
    if error.kind == "sessionNotFound" {
        return json!({"code": error.kind, "message": "This Muse session no longer exists. Create a new agent."});
    }
    if error.kind == "authRequired" {
        return json!({"code": error.kind, "message": "Run `muse login` or set META_API_KEY."});
    }
    if error.kind == "internal" && error.message.contains("UnsafePath") {
        let data_home = env
            .get("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env.get("HOME").map(String::as_str).unwrap_or("~"))
                    .join(".local/share")
            });
        return json!({
            "code": "unsafePath",
            "message": format!("Muse data directory {} requires mode 0700. Set its permissions before retrying.", data_home.join("muse").display()),
        });
    }
    error.provider_error()
}

pub fn actionable(error: &anyhow::Error, env: &BTreeMap<String, String>) -> anyhow::Error {
    let presented = actionable_error(error, env);
    let mut mapped = error
        .downcast_ref::<MuseError>()
        .cloned()
        .unwrap_or_else(|| MuseError::new("muse", error.to_string()));
    mapped.kind = presented["code"].as_str().unwrap_or("muse").into();
    mapped.message = presented["message"].as_str().unwrap_or_default().into();
    if let Some(diagnostic) = presented.get("diagnostic").and_then(Value::as_str) {
        mapped.diagnostic = Some(diagnostic.into());
    }
    mapped.into()
}

pub fn command_id() -> String {
    let mut bytes = [0_u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    bytes[..6].copy_from_slice(&millis.to_be_bytes()[2..]);
    bytes[6] = 0x70 | (bytes[6] & 0x0f);
    bytes[8] = 0x80 | (bytes[8] & 0x3f);
    uuid::Uuid::from_bytes(bytes).to_string()
}

pub fn serve_args(options: &Value) -> Result<Vec<String>> {
    let mut errors = Vec::new();
    let Some(object) = options.as_object() else {
        return Err(MuseError::new(
            "invalidProviderOptions",
            "Invalid Muse providerOptions: providerOptions: expected object",
        )
        .into());
    };
    for key in object.keys() {
        if !matches!(key.as_str(), "sandbox" | "trustWorkspace") {
            errors.push(format!("providerOptions.{key}: Unrecognized key"));
        }
    }
    let mut enabled = false;
    let mut network = "proxy-only";
    if let Some(sandbox) = object.get("sandbox") {
        if let Some(sandbox) = sandbox.as_object() {
            for key in sandbox.keys() {
                if !matches!(key.as_str(), "enabled" | "network") {
                    errors.push(format!("providerOptions.sandbox.{key}: Unrecognized key"));
                }
            }
            if let Some(value) = sandbox.get("enabled") {
                match value.as_bool() {
                    Some(value) => enabled = value,
                    None => errors.push("providerOptions.sandbox.enabled: expected boolean".into()),
                }
            }
            if let Some(value) = sandbox.get("network") {
                match value.as_str() {
                    Some(value @ ("proxy-only" | "restricted" | "enabled")) => network = value,
                    _ => errors.push("providerOptions.sandbox.network: expected proxy-only, restricted, or enabled".into()),
                }
            }
        } else {
            errors.push("providerOptions.sandbox: expected object".into());
        }
    }
    let trust_workspace = match object.get("trustWorkspace") {
        None => true,
        Some(value) => match value.as_bool() {
            Some(value) => value,
            None => {
                errors.push("providerOptions.trustWorkspace: expected boolean".into());
                true
            }
        },
    };
    if !errors.is_empty() {
        return Err(MuseError::new(
            "invalidProviderOptions",
            format!("Invalid Muse providerOptions: {}", errors.join("; ")),
        )
        .into());
    }
    let mut args = vec!["--sandbox-network".into(), network.into()];
    if !enabled {
        args.push("--disable-sandbox".into());
    }
    if trust_workspace {
        args.push("--trust-workspace".into());
    }
    Ok(args)
}

type Reply = std::result::Result<Value, MuseError>;
type CloseResult = std::result::Result<(), MuseError>;

#[derive(Default)]
struct State {
    sequence: u64,
    pending: BTreeMap<u64, oneshot::Sender<Reply>>,
    failure: Option<MuseError>,
    closing: bool,
}

struct Inner {
    process: Arc<ManagedProcess>,
    error_env: BTreeMap<String, String>,
    state: Arc<Mutex<State>>,
    notifications: mpsc::UnboundedSender<Value>,
    request_timeout: Duration,
    cancel: CancellationToken,
    close_requested: CancellationToken,
    reader_stop: CancellationToken,
    reader_done: CancellationToken,
    exited: CancellationToken,
    close_result: watch::Sender<Option<CloseResult>>,
    runtime: tokio::runtime::Handle,
}

pub struct Connection {
    inner: Arc<Inner>,
}

impl Connection {
    /// Transport controls are separate from strict Muse providerOptions. Omit
    /// providerOptions for maintenance hosts; pass {} for default session flags.
    pub async fn spawn(
        spec: &ProviderSpec,
        env: BTreeMap<String, String>,
        cwd: Option<PathBuf>,
        options: &Value,
        cancel: CancellationToken,
    ) -> Result<(Arc<Self>, mpsc::UnboundedReceiver<Value>)> {
        if cancel.is_cancelled() {
            return Err(MuseError::new("closed", "Muse host is closing").into());
        }
        let (program, prefix) = spec
            .command
            .split_first()
            .filter(|(program, _)| !program.is_empty())
            .ok_or_else(|| {
                MuseError::new(
                    "missingLaunch",
                    "Muse requires a daemon-resolved executable",
                )
            })?;
        let mut args = prefix.to_vec();
        args.push("serve".into());
        if let Some(provider_options) = options.get("providerOptions") {
            args.extend(serve_args(provider_options)?);
        }
        let mut launch_env = spec.env.clone();
        launch_env.extend(env);
        let error_env = ["HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME"]
            .into_iter()
            .filter_map(|key| {
                launch_env
                    .get(key)
                    .map(|value| (key.to_owned(), value.clone()))
            })
            .collect();
        let (process, stdout) = ManagedProcess::spawn(CommandSpec {
            program: program.clone(),
            args,
            cwd,
            env: launch_env,
            clear_env: true,
        })
        .await
        .map_err(|error| {
            let not_found = error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound);
            MuseError::new(
                "spawn",
                format!(
                    "spawn {program}{}: {error:#}",
                    if not_found { " ENOENT" } else { "" }
                ),
            )
        })?;
        let (notifications, receiver) = mpsc::unbounded_channel();
        let (close_result, _) = watch::channel(None);
        let inner = Arc::new(Inner {
            process,
            error_env,
            state: Arc::new(Mutex::new(State::default())),
            notifications,
            request_timeout: Duration::from_millis(
                options
                    .get("timeoutMs")
                    .and_then(Value::as_u64)
                    .unwrap_or(10_000),
            ),
            cancel,
            close_requested: CancellationToken::new(),
            reader_stop: CancellationToken::new(),
            reader_done: CancellationToken::new(),
            exited: CancellationToken::new(),
            close_result,
            runtime: tokio::runtime::Handle::current(),
        });
        let reader = inner.clone();
        inner.runtime.spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            loop {
                let line = tokio::select! {
                    biased;
                    _ = reader.reader_stop.cancelled() => break,
                    line = lines.next_line() => line,
                };
                match line {
                    Ok(Some(line)) => {
                        let frame = serde_json::from_str::<Value>(&line).map_err(|error| {
                            MuseError::new("invalidFrame", format!("Invalid MSP frame: {error}"))
                        });
                        let result = match frame {
                            Ok(frame) => reader.receive(frame).await,
                            Err(error) => Err(error),
                        };
                        if let Err(error) = result {
                            reader.fail(error);
                            reader.begin_close();
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        reader.fail(MuseError::new(
                            "invalidFrame",
                            format!("Invalid MSP frame: {error}"),
                        ));
                        reader.begin_close();
                        break;
                    }
                }
            }
            reader.reader_done.cancel();
        });
        let watcher = inner.clone();
        inner.runtime.spawn(async move {
            let status = watcher.process.wait_status().await;
            // Child exit can race the last buffered response. Drain stdout first,
            // unless an explicit close owns teardown of inherited open pipes.
            tokio::select! {
                _ = watcher.reader_done.cancelled() => {},
                _ = watcher.close_requested.cancelled() => {},
            }
            let error = match status {
                Ok(status) => classify_exit(status, &watcher.process.stderr_tail()),
                Err(error) => MuseError::new("crash", format!("Muse host exit failed: {error:#}")),
            };
            watcher.fail(error);
            watcher.exited.cancel();
            watcher.reader_stop.cancel();
        });
        let cancellation = inner.clone();
        inner.runtime.spawn(async move {
            tokio::select! {
                _ = cancellation.cancel.cancelled() => {
                    cancellation.fail(MuseError::new("closed", "Muse host is closing"));
                    cancellation.begin_close();
                },
                _ = cancellation.close_requested.cancelled() => {},
                _ = cancellation.exited.cancelled() => {},
            }
        });
        let connection = Arc::new(Self { inner });
        let startup_timeout = Duration::from_millis(
            options
                .get("startupTimeoutMs")
                .and_then(Value::as_u64)
                .unwrap_or(30_000),
        );
        let initialized = connection.request_with_timeout("initialize", json!({
            "clientInfo": {"name": "paseo", "version": "1"},
            "capabilities": {"experimentalApi": true, "requestedCapabilities": ["sessionMcp", "sessionListStream"]},
        }), startup_timeout).await;
        let initialized = match initialized {
            Ok(value) => value,
            Err(error) => {
                let _ = connection.close().await;
                return Err(error);
            }
        };
        let Some(fingerprint) = initialized
            .get("schema")
            .and_then(|schema| schema.get("fingerprint"))
            .and_then(Value::as_str)
        else {
            let _ = connection.close().await;
            return Err(MuseError::new(
                "invalidFrame",
                "Invalid MSP initialize result: schema.fingerprint must be a string",
            )
            .into());
        };
        if fingerprint != FINGERPRINT {
            tracing::warn!(
                expected = FINGERPRINT,
                received = fingerprint,
                "Muse MSP fingerprint mismatch"
            );
        }
        if let Err(error) = connection
            .inner
            .process
            .write_json(&json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}))
            .await
        {
            let _ = connection.close().await;
            return Err(connection
                .inner
                .failure()
                .unwrap_or_else(|| MuseError::new("closed", error.to_string()))
                .into());
        }
        Ok((connection, receiver))
    }

    pub async fn request(&self, method: &str, params: Value) -> Result<Value> {
        self.request_with_timeout(method, params, self.inner.request_timeout)
            .await
    }

    pub async fn request_with_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value> {
        self.request_params(method, &params, timeout).await
    }

    async fn request_params(
        &self,
        method: &str,
        params: &Value,
        timeout: Duration,
    ) -> Result<Value> {
        let result = self
            .request_raw(method, params, timeout)
            .await
            .map_err(|error| actionable(&error, &self.inner.error_env))?;
        if !result.is_object() {
            return Err(MuseError::new(
                "invalidFrame",
                format!("Invalid MSP {method} result: expected object"),
            )
            .into());
        }
        Ok(result)
    }

    async fn request_raw(&self, method: &str, params: &Value, timeout: Duration) -> Result<Value> {
        let (sender, receiver) = oneshot::channel();
        let id = {
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let Some(error) = &state.failure {
                return Err(error.clone().into());
            }
            if state.closing || self.inner.cancel.is_cancelled() {
                return Err(MuseError::new("closed", "Muse host is closing").into());
            }
            state.sequence += 1;
            let id = state.sequence;
            state.pending.insert(id, sender);
            id
        };
        let _pending = PendingGuard {
            state: self.inner.state.clone(),
            id,
        };
        let mut bytes = Vec::new();
        let mut serializer = serde_json::Serializer::new(&mut bytes);
        let mut frame = serializer.serialize_map(Some(4))?;
        frame.serialize_entry("jsonrpc", "2.0")?;
        frame.serialize_entry("id", &id)?;
        frame.serialize_entry("method", method)?;
        frame.serialize_entry("params", params)?;
        frame.end()?;
        bytes.push(b'\n');
        let response = tokio::time::timeout(timeout, async {
            // As in the MSP client, process exit owns broken-pipe diagnostics.
            // A write failure is never authority to replay an admitted command.
            let _ = self.inner.process.write_bytes(&bytes).await;
            receiver.await.unwrap_or_else(|_| {
                Err(self
                    .inner
                    .failure()
                    .unwrap_or_else(|| MuseError::new("closed", "Muse host is closing")))
            })
        })
        .await;
        match response {
            Ok(response) => response.map_err(Into::into),
            Err(_) => Err(MuseError::new("timeout", format!("Muse {method} timed out")).into()),
        }
    }

    pub async fn command(&self, method: &str, mut params: Value) -> Result<Value> {
        let input = params.as_object_mut().ok_or_else(|| {
            MuseError::new("invalidParams", "Muse command params must be an object")
        })?;
        if !input.get("commandId").is_some_and(Value::is_string) {
            input.insert("commandId".into(), command_id().into());
        }
        for attempt in 0..3 {
            match self
                .request_params(method, &params, self.inner.request_timeout)
                .await
            {
                Ok(result) => return Ok(result),
                Err(error) => {
                    let retryable = error.downcast_ref::<MuseError>().is_some_and(|error| {
                        matches!(error.kind.as_str(), "overloaded" | "backpressured")
                    });
                    if !retryable || attempt == 2 {
                        return Err(error);
                    }
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_millis(100 * (attempt + 1))) => {},
                        _ = self.inner.close_requested.cancelled() => {
                            let error = self.inner.failure().unwrap_or_else(|| MuseError::new("closed", "Muse host is closing"));
                            return Err(actionable(&error.into(), &self.inner.error_env));
                        },
                        _ = self.inner.cancel.cancelled() => return Err(MuseError::new("closed", "Muse host is closing").into()),
                    }
                }
            }
        }
        unreachable!("the last attempt always returns")
    }

    pub async fn close(&self) -> Result<()> {
        let mut completion = self.inner.close_result.subscribe();
        self.inner.begin_close();
        loop {
            if let Some(result) = completion.borrow_and_update().clone() {
                return result.map_err(Into::into);
            }
            if completion.changed().await.is_err() {
                return Err(MuseError::new("closed", "Muse close supervisor stopped").into());
            }
        }
    }

    pub fn failure(&self) -> Option<MuseError> {
        self.inner.failure()
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.inner.begin_close();
    }
}

struct PendingGuard {
    state: Arc<Mutex<State>>,
    id: u64,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .pending
            .remove(&self.id);
    }
}

impl Inner {
    fn failure(&self) -> Option<MuseError> {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .failure
            .clone()
    }

    fn fail(&self, error: MuseError) {
        let (pending, notify) = {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            if state.failure.is_some() {
                return;
            }
            state.failure = Some(error.clone());
            (std::mem::take(&mut state.pending), !state.closing)
        };
        for sender in pending.into_values() {
            let _ = sender.send(Err(error.clone()));
        }
        if notify {
            let _ = self
                .notifications
                .send(json!({"method": "$error", "params": {"message": error.message}}));
        }
    }

    fn begin_close(self: &Arc<Self>) {
        {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            if state.closing {
                return;
            }
            state.closing = true;
        }
        self.close_requested.cancel();
        let inner = self.clone();
        self.runtime.spawn(async move {
            let result = inner.finish_close().await;
            if let Err(error) = &result {
                inner.fail(error.clone());
            }
            inner.reader_stop.cancel();
            inner.close_result.send_replace(Some(result));
        });
    }

    async fn finish_close(&self) -> CloseResult {
        let drained = tokio::time::timeout(Duration::from_secs(1), async {
            let _ = self.process.close_stdin().await;
            self.exited.cancelled().await;
        })
        .await
        .is_ok();
        if drained {
            return Ok(());
        }
        match tokio::time::timeout(
            Duration::from_secs(5),
            self.process.terminate(Duration::ZERO),
        )
        .await
        {
            Ok(Ok(())) => {
                self.exited.cancelled().await;
                Ok(())
            }
            Ok(Err(error)) => Err(MuseError::new(
                "timeout",
                format!("Muse host did not close after termination: {error:#}"),
            )),
            Err(_) => Err(MuseError::new(
                "timeout",
                "Muse host did not close after termination",
            )),
        }
    }

    async fn receive(self: &Arc<Self>, mut frame: Value) -> std::result::Result<(), MuseError> {
        validate_frame(&frame)?;
        if let Some(method) = frame
            .get("method")
            .and_then(Value::as_str)
            .filter(|method| !method.is_empty())
        {
            if let Some(id) = frame.get("id") {
                let response = if matches!(method, "approval/request" | "userInput/request") {
                    json!({"jsonrpc": "2.0", "id": id, "result": {}})
                } else {
                    json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "Unsupported server request"}})
                };
                let _ = self.process.write_json(&response).await;
            } else {
                let method = method.to_owned();
                let params = frame
                    .get_mut("params")
                    .map(Value::take)
                    .unwrap_or(Value::Null);
                let mut notification = json!({"method": method});
                notification["params"] = params;
                let _ = self.notifications.send(notification);
            }
            return Ok(());
        }
        let Some(id) = frame.get("id") else {
            return Err(MuseError::new(
                "invalidFrame",
                "Invalid MSP frame: MSP response has no id",
            ));
        };
        if id.is_null() {
            if let Some(error) = frame.get("error") {
                self.fail(rpc_error(error, false));
                self.begin_close();
                return Ok(());
            }
            return Err(MuseError::new(
                "invalidFrame",
                "Invalid MSP frame: MSP result has a null id",
            ));
        }
        let Some(id) = id.as_u64().or_else(|| {
            let number = id.as_f64()?;
            (number >= 0.0 && number.fract() == 0.0 && number <= u64::MAX as f64)
                .then_some(number as u64)
        }) else {
            return Ok(());
        };
        let sender = self
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .pending
            .remove(&id);
        if let Some(sender) = sender {
            let result = if let Some(error) = frame.get("error") {
                Err(rpc_error(error, true))
            } else {
                Ok(frame
                    .get_mut("result")
                    .map(Value::take)
                    .unwrap_or(Value::Null))
            };
            let _ = sender.send(result);
        }
        Ok(())
    }
}

fn validate_frame(frame: &Value) -> std::result::Result<(), MuseError> {
    let invalid = |message| MuseError::new("invalidFrame", format!("Invalid MSP frame: {message}"));
    let object = frame
        .as_object()
        .ok_or_else(|| invalid("expected object"))?;
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(invalid("jsonrpc must be 2.0"));
    }
    if object
        .get("id")
        .is_some_and(|id| !id.is_null() && !id.is_string() && !id.is_number())
    {
        return Err(invalid("id must be a string, number, or null"));
    }
    if object
        .get("method")
        .is_some_and(|method| !method.is_string())
    {
        return Err(invalid("method must be a string"));
    }
    if let Some(error) = object.get("error") {
        let error = error
            .as_object()
            .ok_or_else(|| invalid("error must be an object"))?;
        if !error.get("code").is_some_and(Value::is_number) {
            return Err(invalid("error.code must be a number"));
        }
        if !error.get("message").is_some_and(Value::is_string) {
            return Err(invalid("error.message must be a string"));
        }
        if let Some(data) = error.get("data") {
            let data = data
                .as_object()
                .ok_or_else(|| invalid("error.data must be an object"))?;
            for key in ["kind", "reason"] {
                if data.get(key).is_some_and(|value| !value.is_string()) {
                    return Err(invalid(
                        "error.data.kind and error.data.reason must be strings when present",
                    ));
                }
            }
        }
    }
    Ok(())
}

fn rpc_error(error: &Value, correlated: bool) -> MuseError {
    let mut result = MuseError::new(
        error
            .get("data")
            .and_then(|data| data.get("kind"))
            .and_then(Value::as_str)
            .unwrap_or("rpc"),
        error["message"].as_str().unwrap_or_default(),
    );
    if correlated {
        if error["code"].as_f64() == Some(-32020.0) {
            result.kind = "sessionNotFound".into();
        }
        result.reason = error
            .get("data")
            .and_then(|data| data.get("reason"))
            .and_then(Value::as_str)
            .map(str::to_owned);
    }
    result
}

fn classify_exit(status: ExitStatus, stderr: &str) -> MuseError {
    let kind = match status.code() {
        Some(0) => "cleanExit",
        Some(2) => "usage",
        Some(3) => "credentials",
        Some(4) => "leaseHeld",
        Some(5) => "surfaceOff",
        _ => "crash",
    };
    let code = status
        .code()
        .map(|code| code.to_string())
        .unwrap_or_else(|| {
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                if let Some(signal) = status.signal() {
                    return nix::sys::signal::Signal::try_from(signal)
                        .map(|signal| signal.to_string())
                        .unwrap_or_else(|_| signal.to_string());
                }
            }
            status.to_string()
        });
    let mut units = 0;
    let mut start = stderr.len();
    for (index, character) in stderr.char_indices().rev() {
        units += character.len_utf16();
        if units > 8192 {
            break;
        }
        start = index;
    }
    MuseError::new(
        kind,
        format!("Muse host exited ({code}). {}", &stderr[start..]),
    )
}
