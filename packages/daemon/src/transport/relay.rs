//! Outbound relay routing. Endpoint encryption never supplies application authority.
mod ingress;
#[cfg(test)]
mod tests;

use crate::{
    relay_crypto::{KeyPair, MAX_WIRE_BYTES},
    state::AppState,
};
use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{Mutex, mpsc},
    task::{JoinHandle, JoinSet},
    time::{Instant, MissedTickBehavior},
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async_with_config,
    tungstenite::{Message, protocol::WebSocketConfig},
};
use tokio_util::sync::CancellationToken;

const CONTROL_PING_INTERVAL: Duration = Duration::from_secs(10);
const CONTROL_STALE_TIMEOUT: Duration = Duration::from_secs(30);
const CONTROL_READY_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_DATA_CONNECTIONS: usize = 128;
const DATA_OPEN_TIMEOUT: Duration = Duration::from_secs(15);
type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

pub struct RelayService {
    app: Weak<AppState>,
    require_protected: Arc<AtomicBool>,
    runtime: Mutex<Option<Runtime>>,
}
struct Runtime {
    cancel: CancellationToken,
    task: JoinHandle<()>,
}
#[derive(Clone)]
struct Settings {
    base_url: url::Url,
    keypair: Arc<KeyPair>,
}

impl RelayService {
    pub fn new(app: Weak<AppState>) -> Self {
        Self {
            app,
            require_protected: Arc::new(AtomicBool::new(false)),
            runtime: Mutex::new(None),
        }
    }

    pub async fn initialize(&self) -> Result<()> {
        let config = self
            .app
            .upgrade()
            .context("Daemon shutting down")?
            .config
            .snapshot();
        self.apply_config(&config, &Value::Null).await
    }

    pub async fn apply_config(&self, next: &Value, _previous: &Value) -> Result<()> {
        let mut runtime = self.runtime.lock().await;
        let enabled = next
            .get("relayEnabled")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        if enabled && runtime.is_none() {
            let app = self.app.upgrade().context("Daemon shutting down")?;
            let identity = app.transport.identity();
            let server_id = identity
                .get("serverId")
                .and_then(Value::as_str)
                .context("Daemon server identity unavailable")?;
            let endpoint = next
                .get("relayEndpoint")
                .and_then(Value::as_str)
                .context("Relay endpoint unavailable")?;
            let tls = next
                .get("relayUseTls")
                .and_then(Value::as_bool)
                .context("Relay TLS configuration unavailable")?;
            let settings = Settings {
                base_url: relay_url(endpoint, tls, server_id)?,
                keypair: app.transport.daemon_keypair()?,
            };
            let cancel = app.cancel.child_token();
            drop(app);
            self.require_protected.store(
                next.get("relayRequireProtectedSecurity")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                Ordering::Release,
            );
            let task = tokio::spawn(run(
                self.app.clone(),
                settings,
                self.require_protected.clone(),
                cancel.clone(),
            ));
            *runtime = Some(Runtime { cancel, task });
        } else {
            self.require_protected.store(
                next.get("relayRequireProtectedSecurity")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                Ordering::Release,
            );
            if !enabled {
                if let Some(active) = runtime.take() {
                    active.cancel.cancel();
                    active.task.await.context("Relay runtime failed")?;
                }
            }
        }
        Ok(())
    }

    pub async fn shutdown(&self) -> Result<()> {
        let mut runtime = self.runtime.lock().await;
        if let Some(active) = runtime.take() {
            active.cancel.cancel();
            active.task.await.context("Relay runtime failed")?;
        }
        Ok(())
    }
}

fn relay_url(endpoint: &str, tls: bool, server_id: &str) -> Result<url::Url> {
    let mut url = url::Url::parse(&format!(
        "{}://{endpoint}/ws",
        if tls { "wss" } else { "ws" }
    ))?;
    if url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/ws"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!("Invalid relay endpoint");
    }
    url.query_pairs_mut()
        .append_pair("serverId", server_id)
        .append_pair("role", "server")
        .append_pair("v", "2");
    Ok(url)
}

fn socket_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(MAX_WIRE_BYTES))
        .max_frame_size(Some(MAX_WIRE_BYTES))
        .write_buffer_size(0)
        .max_write_buffer_size(super::context::MAX_BUFFERED_BYTES)
}

async fn connect(url: &url::Url, deadline: Duration) -> Result<Socket> {
    Ok(tokio::time::timeout(
        deadline,
        connect_async_with_config(url.as_str(), Some(socket_config()), false),
    )
    .await??
    .0)
}

#[derive(Debug, PartialEq, Eq)]
enum Control {
    Sync(Vec<String>),
    Connected(String),
    Disconnected(String),
    Ping,
    Pong,
}
fn parse_control(bytes: &[u8]) -> Option<Control> {
    let text = String::from_utf8_lossy(bytes);
    let value: Value = serde_json::from_str(&text).ok()?;
    match value.get("type")?.as_str()? {
        "ping" => Some(Control::Ping),
        "pong" => Some(Control::Pong),
        "sync" => Some(Control::Sync(
            value
                .get("connectionIds")?
                .as_array()?
                .iter()
                .filter_map(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .map(str::to_owned)
                .collect(),
        )),
        "connected" | "disconnected" => {
            let id = value.get("connectionId")?.as_str()?.trim();
            if id.is_empty() {
                return None;
            }
            Some(if value["type"] == "connected" {
                Control::Connected(id.to_owned())
            } else {
                Control::Disconnected(id.to_owned())
            })
        }
        _ => None,
    }
}
fn reconnect_delay(attempt: u32) -> Duration {
    Duration::from_millis(u64::from(attempt).saturating_mul(1000).min(30_000))
}

async fn run(
    app: Weak<AppState>,
    settings: Settings,
    policy: Arc<AtomicBool>,
    cancel: CancellationToken,
) {
    let (tx, mut rx) = mpsc::channel(256);
    let control = tokio::spawn(control_loop(settings.base_url.clone(), tx, cancel.clone()));
    let mut data = JoinSet::new();
    let mut sockets: HashMap<String, (u64, CancellationToken)> = HashMap::new();
    let mut sequence = 0u64;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            result = data.join_next(), if !data.is_empty() => {
                if let Some(Ok((id, generation))) = result {
                    if sockets.get(&id).is_some_and(|(current, _)| *current == generation) { sockets.remove(&id); }
                }
            }
            message = rx.recv() => {
                let ids = match message {
                    Some(Control::Sync(ids)) => ids,
                    Some(Control::Connected(id)) => vec![id],
                    Some(Control::Disconnected(id)) => {
                        if let Some((_, token)) = sockets.remove(&id) { token.cancel(); }
                        continue;
                    }
                    Some(Control::Ping | Control::Pong) => continue,
                    None => break,
                };
                // sync only ensures reported sockets; it does not prune other live data sockets.
                for id in ids {
                    if sockets.contains_key(&id) || sockets.len() >= MAX_DATA_CONNECTIONS { continue; }
                    sequence = sequence.wrapping_add(1);
                    let generation = sequence;
                    let disconnected = CancellationToken::new();
                    sockets.insert(id.clone(), (generation, disconnected.clone()));
                    let app = app.clone();
                    let settings = settings.clone();
                    let policy = policy.clone();
                    let stopped = cancel.clone();
                    data.spawn(async move {
                        let mut url = settings.base_url.clone();
                        url.query_pairs_mut().append_pair("connectionId", &id);
                        let result = async {
                            let socket = tokio::select! {
                                _ = stopped.cancelled() => return Ok(()),
                                _ = disconnected.cancelled() => return Ok(()),
                                result = connect(&url, DATA_OPEN_TIMEOUT) => result?,
                            };
                            ingress::run(app, socket, settings.keypair, policy, stopped, disconnected).await
                        }.await;
                        if let Err(error) = result { tracing::warn!(%error, connection_id = %id, "Relay data socket closed"); }
                        (id, generation)
                    });
                }
            }
        }
    }
    cancel.cancel();
    // The control task can be blocked in a kernel write; cancellation must still stop it.
    control.abort();
    let _ = control.await;
    while data.join_next().await.is_some() {}
}

async fn control_loop(url: url::Url, events: mpsc::Sender<Control>, cancel: CancellationToken) {
    let mut attempt = 0u32;
    loop {
        let result = tokio::select! {
            _ = cancel.cancelled() => return,
            result = control_connection(&url, &events, &cancel, &mut attempt) => result,
        };
        if let Err(error) = result {
            tracing::warn!(%error, "Relay control disconnected");
        }
        attempt = attempt.saturating_add(1);
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(reconnect_delay(attempt)) => {},
        }
    }
}

async fn control_connection(
    url: &url::Url,
    events: &mpsc::Sender<Control>,
    cancel: &CancellationToken,
    attempt: &mut u32,
) -> Result<()> {
    let mut socket = connect(url, Duration::from_secs(10)).await?;
    let opened = Instant::now();
    let ready_deadline = opened + CONTROL_READY_TIMEOUT;
    let mut last_seen = opened;
    let mut ready = false;
    let mut ping = tokio::time::interval_at(opened + CONTROL_PING_INTERVAL, CONTROL_PING_INTERVAL);
    ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
    tokio::time::timeout_at(
        ready_deadline,
        socket.send(Message::Ping(bytes::Bytes::new())),
    )
    .await??;
    loop {
        let write_deadline = if ready {
            last_seen + CONTROL_STALE_TIMEOUT + CONTROL_PING_INTERVAL
        } else {
            ready_deadline
        };
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            _ = tokio::time::sleep_until(ready_deadline), if !ready => bail!("Relay control readiness timed out"),
            _ = ping.tick() => {
                if last_seen.elapsed() > CONTROL_STALE_TIMEOUT { bail!("Relay control is stale"); }
                tokio::time::timeout_at(write_deadline, socket.send(Message::Ping(bytes::Bytes::new()))).await??;
            }
            message = socket.next() => {
                let message = message.context("Relay control closed")??;
                match message {
                    Message::Pong(_) => last_seen = Instant::now(),
                    Message::Ping(payload) => tokio::time::timeout_at(write_deadline, socket.send(Message::Pong(payload))).await??,
                    Message::Close(_) => return Ok(()),
                    Message::Text(_) | Message::Binary(_) => {
                        last_seen = Instant::now();
                        let Some(message) = parse_control(message.into_data().as_ref()) else { continue; };
                        if !ready { ready = true; *attempt = 0; }
                        if message == Control::Ping {
                            tokio::time::timeout_at(last_seen + CONTROL_STALE_TIMEOUT + CONTROL_PING_INTERVAL,
                                socket.send(Message::Text(json!({"type":"pong","ts":chrono::Utc::now().timestamp_millis()}).to_string().into()))).await??;
                        } else if message != Control::Pong {
                            events.send(message).await.context("Relay supervisor stopped")?;
                        }
                    }
                    _ => {},
                }
            }
        }
    }
}
