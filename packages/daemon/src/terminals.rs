pub mod hooks;
mod runtime;
mod screen;
pub(crate) mod scripts;
mod setup;
mod shell;
mod streams;
#[cfg(test)]
mod tests;
#[cfg(windows)]
pub mod windows;

use crate::{state::AppState, transport::RequestContext};
use anyhow::{Result, anyhow, bail};
use base64::Engine;
use rand::RngCore;
pub use runtime::{CreateTerminal, Terminal, TerminalEvent};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock, Weak},
};
use subtle::ConstantTimeEq;
use tokio::sync::broadcast;

pub struct TerminalService {
    app: Weak<AppState>,
    terminals: RwLock<HashMap<String, Arc<Terminal>>>,
    workspace_env: RwLock<HashMap<String, BTreeMap<String, String>>>,
    changed: broadcast::Sender<()>,
    streams: Arc<Mutex<HashMap<(String, u8), streams::Registration>>>,
    source_cleanup: Mutex<std::collections::HashSet<String>>,
    pub(crate) scripts: scripts::Scripts,
    shutting_down: std::sync::atomic::AtomicBool,
}
impl TerminalService {
    pub fn new(app: Weak<AppState>) -> Self {
        let (changed, _) = broadcast::channel(64);
        Self {
            app: app.clone(),
            terminals: RwLock::new(HashMap::new()),
            workspace_env: RwLock::new(HashMap::new()),
            changed,
            streams: Arc::new(Mutex::new(HashMap::new())),
            source_cleanup: Mutex::new(std::collections::HashSet::new()),
            scripts: scripts::Scripts::new(app),
            shutting_down: std::sync::atomic::AtomicBool::new(false),
        }
    }
    pub async fn initialize(&self) -> Result<()> {
        let app = self
            .app
            .upgrade()
            .ok_or_else(|| anyhow!("Daemon is shutting down"))?;
        if app.config.snapshot()["enableTerminalAgentHooks"].as_bool() == Some(true) {
            self.apply_hooks(true).await?;
        }
        Ok(())
    }
    pub async fn apply_config(&self, next: &Value, previous: &Value) -> Result<()> {
        if next["enableTerminalAgentHooks"] != previous["enableTerminalAgentHooks"] {
            self.apply_hooks(next["enableTerminalAgentHooks"].as_bool() == Some(true))
                .await?;
        }
        Ok(())
    }
    async fn apply_hooks(&self, enabled: bool) -> Result<()> {
        let app = self
            .app
            .upgrade()
            .ok_or_else(|| anyhow!("Daemon is shutting down"))?;
        let home = app.user_home.clone();
        let env = app.config.environment();
        drop(app);
        match tokio::task::spawn_blocking(move || hooks::apply(enabled, &home, &env)).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracing::warn!(%error,"Failed to apply terminal activity hooks"),
            Err(error) => tracing::warn!(%error,"Terminal activity hook installer failed"),
        };
        Ok(())
    }
    pub fn register_workspace_env(&self, id: &str, env: BTreeMap<String, String>) {
        self.workspace_env
            .write()
            .unwrap()
            .insert(id.to_owned(), env);
    }
    pub async fn create(&self, mut options: CreateTerminal) -> Result<Arc<Terminal>> {
        if self
            .shutting_down
            .load(std::sync::atomic::Ordering::Acquire)
        {
            bail!("Terminal service is shutting down");
        }
        let app = self
            .app
            .upgrade()
            .ok_or_else(|| anyhow!("Daemon is shutting down"))?;
        let workspace = app
            .projects
            .workspace(&options.workspace_id)
            .await?
            .ok_or_else(|| {
                anyhow!(
                    "Workspace {} is not active or does not exist",
                    options.workspace_id
                )
            })?;
        if !workspace["archivedAt"].is_null() {
            bail!(
                "Workspace {} is not active or does not exist",
                options.workspace_id
            );
        }
        let cached = {
            self.workspace_env
                .read()
                .unwrap()
                .get(&options.workspace_id)
                .cloned()
        };
        let workspace_env = if let Some(env) = cached {
            env
        } else {
            let env = app.projects.environment(&options.workspace_id).await?;
            self.register_workspace_env(&options.workspace_id, env.clone());
            env
        };
        let mut env = app.config.environment();
        env.extend(workspace_env);
        env.append(&mut options.env);
        env.insert(
            "PASEO_HOOK_CLI".into(),
            app.executable.to_string_lossy().into_owned(),
        );
        if let Some(bin) = app.executable.parent() {
            let key = env
                .keys()
                .find(|key| key.eq_ignore_ascii_case("PATH"))
                .cloned()
                .unwrap_or_else(|| "PATH".into());
            let old = env.get(&key).cloned().unwrap_or_default();
            let mut paths = vec![bin.to_path_buf()];
            paths.extend(std::env::split_paths(&old).filter(|p| p != bin));
            env.insert(
                key,
                std::env::join_paths(paths)?.to_string_lossy().into_owned(),
            );
        }
        if let Some(url) = app.transport.local_http_endpoint() {
            env.insert(
                "PASEO_TERMINAL_ACTIVITY_URL".into(),
                format!("{url}/api/terminal-activity"),
            );
        }
        options.env = env;
        let mut random = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut random);
        let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random);
        let terminal = Terminal::spawn(options, token, self.changed.clone()).await?;
        let admitted = {
            let mut terminals = self.terminals.write().unwrap();
            if self
                .shutting_down
                .load(std::sync::atomic::Ordering::Acquire)
            {
                false
            } else {
                terminals.insert(terminal.id.clone(), terminal.clone());
                true
            }
        };
        if !admitted {
            terminal.terminate().await?;
            bail!("Terminal service is shutting down");
        }
        let _ = self.changed.send(());
        let weak = self.app.clone();
        let id = terminal.id.clone();
        let waiter = terminal.clone();
        let cancel = app.cancel.clone();
        let mut events = terminal.subscribe_lifecycle();
        tokio::spawn(async move {
            if let Some(app) = weak.upgrade() {
                if let Err(error) = app.projects.publish_workspace(&waiter.workspace_id).await {
                    tracing::warn!(%error,"Terminal workspace projection failed");
                }
            }
            loop {
                tokio::select! {biased;
                    _=cancel.cancelled()=>{let _=waiter.terminate().await;break;},
                    event=events.recv()=>{match event{
                        Ok(TerminalEvent::Activity(transition))=>{if let Some(app)=weak.upgrade(){let mut info=waiter.info();info["activity"]=transition["activity"].clone();if let Err(error)=app.agents.notifications.notify_terminal_activity(&transition["previous"],&info).await{tracing::warn!(%error,"Terminal attention delivery failed");}if let Err(error)=app.projects.publish_workspace(&waiter.workspace_id).await{tracing::warn!(%error,"Terminal workspace projection failed");}}},
                        Ok(TerminalEvent::Changed)|Err(broadcast::error::RecvError::Lagged(_))=>{if let Some(app)=weak.upgrade(){if let Err(error)=app.projects.publish_workspace(&waiter.workspace_id).await{tracing::warn!(%error,"Terminal workspace projection failed");}}},
                        Ok(TerminalEvent::Exit(_)|TerminalEvent::Error(_))|Err(broadcast::error::RecvError::Closed)=>break,_=>{}
                    }},
                    _=waiter.wait()=>break,
                }
            }
            if let Some(app) = weak.upgrade() {
                app.terminals.terminals.write().unwrap().remove(&id);
                let _ = app.terminals.changed.send(());
                if let Err(error) = app.projects.publish_workspace(&waiter.workspace_id).await {
                    tracing::warn!(%error,"Terminal exit projection failed");
                }
            }
        });
        Ok(terminal)
    }
    pub async fn get(&self, id: &str) -> Result<Arc<Terminal>> {
        self.terminals
            .read()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow!("Terminal not found"))
    }
    pub async fn list(&self, workspace_id: Option<&str>, cwd: Option<&str>) -> Vec<Value> {
        let terminals: Vec<_> = self
            .terminals
            .read()
            .unwrap()
            .values()
            .filter(|terminal| !terminal.is_exited())
            .cloned()
            .collect();
        let roots = if cwd.is_some() {
            if let Some(app) = self.app.upgrade() {
                let workspaces = match app.projects.list_workspaces().await {
                    Ok(workspaces) => workspaces,
                    Err(error) => {
                        tracing::warn!(%error,"Unable to resolve terminal workspace roots");
                        return Vec::new();
                    }
                };
                workspaces
                    .into_iter()
                    .filter(|w| w["archivedAt"].is_null())
                    .filter_map(|w| w["cwd"].as_str().map(PathBuf::from))
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };
        terminals
            .into_iter()
            .filter(|terminal| workspace_id.is_none_or(|id| terminal.workspace_id == id))
            .filter(|terminal| {
                cwd.is_none_or(|cwd| {
                    let root = Path::new(cwd);
                    let owner = roots
                        .iter()
                        .filter(|candidate| terminal.cwd.starts_with(candidate))
                        .max_by_key(|candidate| candidate.components().count());
                    owner.map_or_else(|| terminal.cwd.starts_with(root), |owner| owner == root)
                })
            })
            .map(|terminal| terminal.info())
            .collect()
    }
    /// `killTerminalAndWait`: an unknown id is a no-op, and the session is removed
    /// even when waiting for termination fails.
    pub async fn kill(&self, id: &str) -> Result<()> {
        let terminal = { self.terminals.read().unwrap().get(id).cloned() };
        if let Some(terminal) = terminal {
            let result = terminal.terminate().await;
            self.terminals.write().unwrap().remove(id);
            let _ = self.changed.send(());
            result?;
        }
        Ok(())
    }
    pub async fn kill_terminal_for_close(&self, id: &str) -> Result<Value> {
        self.kill(id).await?;
        Ok(json!({"terminalId":id,"success":true}))
    }
    pub async fn kill_workspace(&self, id: &str) -> Result<()> {
        self.scripts.stop_workspace(id).await?;
        let terminals: Vec<_> = self
            .terminals
            .read()
            .unwrap()
            .values()
            .filter(|terminal| terminal.workspace_id == id)
            .map(|terminal| terminal.id.clone())
            .collect();
        for terminal in terminals {
            self.kill(&terminal).await?;
        }
        self.workspace_env.write().unwrap().remove(id);
        Ok(())
    }
    pub async fn script_list(&self, id: &str) -> Result<Vec<Value>> {
        self.scripts.list(id).await
    }
    pub async fn script_start(&self, id: &str, name: &str) -> Result<Value> {
        self.scripts.launch(id, name).await
    }
    pub async fn script_stop(&self, id: &str, name: &str) -> Result<Value> {
        self.scripts.stop(id, name).await
    }
    pub async fn setup_status(&self, id: &str) -> Result<Option<Value>> {
        self.scripts.setup_status(id).await
    }
    pub async fn start_setup(&self, id: &str) -> Result<bool> {
        Ok(self.scripts.start_setup(id).await?["started"]
            .as_bool()
            .unwrap_or(false))
    }
    pub async fn stop_setup(&self, id: &str) -> Result<()> {
        self.scripts.stop_setup(id).await
    }
    pub async fn teardown(&self, id: &str) -> Result<()> {
        self.scripts.teardown(id).await
    }
    pub async fn teardown_path(&self, cwd: &Path, repo_root: Option<&Path>) -> Result<()> {
        self.scripts.teardown_path(cwd, repo_root).await
    }
    pub async fn refresh_routes(&self, id: &str) -> Result<()> {
        self.scripts.refresh_routes(id).await
    }
    pub fn remove_workspace_runtime(&self, id: &str) {
        self.scripts.remove_workspace_runtime(id);
        self.workspace_env.write().unwrap().remove(id);
    }
    pub async fn classify_service_host(&self, host: &str) -> Value {
        self.scripts.classify_service_host(host)
    }
    pub async fn report_activity(&self, id: &str, token: &str, event: &str) -> Result<bool> {
        let terminal = { self.terminals.read().unwrap().get(id).cloned() };
        let valid = terminal.as_ref().is_some_and(|terminal| {
            bool::from(terminal.activity_token.as_bytes().ct_eq(token.as_bytes()))
        });
        if !valid {
            return Ok(false);
        }
        let state = match event {
            "running" | "working" => "working",
            "idle" => "idle",
            "needs-input" | "attention" => "attention",
            _ => bail!("Invalid terminal activity"),
        };
        if terminal.unwrap().set_activity(state) {
            let _ = self.changed.send(());
        }
        Ok(true)
    }
    pub async fn clear_focused_attention(&self, id: &str) -> Result<()> {
        let terminal = { self.terminals.read().unwrap().get(id).cloned() };
        if let Some(terminal) = terminal {
            if !terminal.info()["activity"]["attentionReason"].is_null() {
                terminal.set_activity("clear_attention");
            }
        }
        Ok(())
    }
    fn track_source(&self, context: &RequestContext) {
        let id = context.source_id().to_owned();
        if !self.source_cleanup.lock().unwrap().insert(id.clone()) {
            return;
        }
        let weak = self.app.clone();
        let cancel = context.cancellation();
        tokio::spawn(async move {
            cancel.cancelled().await;
            if let Some(app) = weak.upgrade() {
                for terminal in app.terminals.terminals.read().unwrap().values() {
                    terminal.release_size(&id);
                }
                app.terminals.source_cleanup.lock().unwrap().remove(&id);
            }
        });
    }
    pub async fn legacy_attention_eligible(
        &self,
        context: &RequestContext,
        terminal: &Value,
    ) -> Result<bool> {
        let Some(cwd) = terminal["cwd"].as_str() else {
            return Ok(false);
        };
        let workspace = terminal["workspaceId"].as_str();
        let requests = context.subscription_requests("terminal-directories");
        if requests.is_empty() {
            return Ok(false);
        }
        let app = self
            .app
            .upgrade()
            .ok_or_else(|| anyhow!("Daemon is shutting down"))?;
        let roots: Vec<PathBuf> = app
            .projects
            .list_workspaces()
            .await?
            .into_iter()
            .filter(|record| record["archivedAt"].is_null())
            .filter_map(|record| record["cwd"].as_str().map(PathBuf::from))
            .collect();
        let terminal_path = Path::new(cwd);
        let owner = roots
            .iter()
            .filter(|root| terminal_path.starts_with(root))
            .max_by_key(|root| root.components().count());
        Ok(requests.iter().any(|request| {
            request["workspaceId"]
                .as_str()
                .is_none_or(|id| Some(id) == workspace)
                && request["cwd"].as_str().is_some_and(|cwd| {
                    owner.map_or_else(
                        || terminal_path.starts_with(cwd),
                        |owner| owner == Path::new(cwd),
                    )
                })
        }))
    }
    pub async fn handle(&self, context: &RequestContext, message: &Value) -> Result<bool> {
        if self.scripts.handle(context, message).await? {
            return Ok(true);
        }
        let kind = message["type"].as_str().unwrap_or("");
        match kind {
            "client_heartbeat" => {
                if message["appVisible"].as_bool() == Some(true) {
                    if let Some(id) = message["focusedTerminalId"]
                        .as_str()
                        .map(str::trim)
                        .filter(|id| !id.is_empty())
                    {
                        self.clear_focused_attention(id).await?;
                    }
                }
                return Ok(false);
            }
            "list_terminals_request" => {
                // `handleListTerminalsRequest`: a workspaceId scope wins over cwd, and
                // the response echoes cwd only when the request carried a non-empty one.
                let workspace_id = message["workspaceId"].as_str();
                let cwd = if workspace_id.is_some() {
                    None
                } else {
                    message["cwd"].as_str()
                };
                let mut payload = json!({"terminals":self.list(workspace_id,cwd).await,"requestId":message["requestId"]});
                if let Some(cwd) = message["cwd"].as_str().filter(|cwd| !cwd.is_empty()) {
                    payload["cwd"] = cwd.into();
                }
                context
                    .send(json!({"type":"list_terminals_response","payload":payload}))
                    .await?;
            }
            "create_terminal_request" => {
                let result = self.create_request(message).await;
                let payload = match result {
                    Ok(terminal) => {
                        json!({"terminal":terminal.info(),"error":null,"requestId":message["requestId"]})
                    }
                    Err(error) => {
                        json!({"terminal":null,"error":error.to_string(),"requestId":message["requestId"]})
                    }
                };
                context
                    .send(json!({"type":"create_terminal_response","payload":payload}))
                    .await?;
            }
            "kill_terminal_request" => {
                let success = self.kill(required(message, "terminalId")?).await.is_ok();
                context.send(json!({"type":"kill_terminal_response","payload":{"terminalId":message["terminalId"],"success":success,"requestId":message["requestId"]}})).await?;
            }
            "terminal.rename.request" => {
                let title = required(message, "title")?.trim();
                let result = if title.is_empty() {
                    Err(anyhow!("Title is required"))
                } else if title.encode_utf16().count() > 200 {
                    Err(anyhow!("Title is too long"))
                } else {
                    self.get(required(message, "terminalId")?)
                        .await
                        .map(|terminal| terminal.set_title(title))
                };
                let error = result.as_ref().err().map(ToString::to_string);
                if result.is_ok() {
                    let _ = self.changed.send(());
                }
                context.send(json!({"type":"terminal.rename.response","payload":{"success":result.is_ok(),"error":error,"requestId":message["requestId"]}})).await?;
            }
            "capture_terminal_request" => {
                let capture = match self.get(required(message, "terminalId")?).await {
                    Ok(terminal) => {
                        terminal
                            .capture(
                                message["start"].as_f64(),
                                message["end"].as_f64(),
                                message["stripAnsi"].as_bool().unwrap_or(true),
                            )
                            .await
                    }
                    Err(error) => Err(error),
                };
                let (lines, total) = capture.unwrap_or_default();
                context.send(json!({"type":"capture_terminal_response","payload":{"terminalId":message["terminalId"],"lines":lines,"totalLines":total,"requestId":message["requestId"]}})).await?;
            }
            "terminal_input" => {
                if !context.allows("workspace.write") {
                    bail!("Terminal input is not permitted");
                }
                let terminal = self.get(required(message, "terminalId")?).await?;
                self.apply_input(context, &terminal, &message["message"])
                    .await?;
            }
            "subscribe_terminal_request" => {
                streams::subscribe(self, context, message).await?;
            }
            "unsubscribe_terminal_request" => {
                context
                    .release_legacy_slot(&format!(
                        "terminal-output:{}",
                        required(message, "terminalId")?
                    ))
                    .await?;
            }
            "subscribe_terminals_request" => {
                streams::subscribe_directory(self, context, message).await?;
            }
            "unsubscribe_terminals_request" => {
                context
                    .release_legacy_slot(&streams::directory_slot(message))
                    .await?;
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
    async fn create_request(&self, message: &Value) -> Result<Arc<Terminal>> {
        if let Some(id) = message["agentId"].as_str() {
            bail!("Agent-backed terminals are no longer supported for agent {id}");
        }
        let cwd = PathBuf::from(required(message, "cwd")?);
        let workspace_id = if let Some(id) = message["workspaceId"].as_str() {
            id.to_owned()
        } else {
            let app = self
                .app
                .upgrade()
                .ok_or_else(|| anyhow!("Daemon is shutting down"))?;
            // `resolveLegacyTerminalWorkspaceId`: the first exact match, else the
            // first workspace at the deepest owning root.
            let mut owner: Option<(usize, String)> = None;
            for workspace in app.projects.list_workspaces().await? {
                if !workspace["archivedAt"].is_null() {
                    continue;
                }
                let (Some(root), Some(id)) = (
                    workspace["cwd"].as_str(),
                    workspace["workspaceId"].as_str(),
                ) else {
                    continue;
                };
                if cwd.starts_with(root)
                    && owner.as_ref().is_none_or(|(length, _)| root.len() > *length)
                {
                    owner = Some((root.len(), id.to_owned()));
                }
            }
            owner
                .map(|(_, id)| id)
                .ok_or_else(|| anyhow!("workspaceId is required"))?
        };
        let (rows, cols) = if message.get("size").is_some() {
            dimensions(&message["size"])?
        } else {
            (24, 80)
        };
        self.create(CreateTerminal {
            cwd,
            workspace_id,
            name: message["name"].as_str().map(str::to_owned),
            command: message["command"].as_str().map(str::to_owned),
            args: message["args"]
                .as_array()
                .map(|args| {
                    args.iter()
                        .map(|arg| {
                            arg.as_str()
                                .map(str::to_owned)
                                .ok_or_else(|| anyhow!("Invalid terminal argument"))
                        })
                        .collect::<Result<Vec<_>>>()
                })
                .transpose()?
                .unwrap_or_default(),
            env: BTreeMap::new(),
            rows,
            cols,
        })
        .await
    }
    async fn apply_input(
        &self,
        context: &RequestContext,
        terminal: &Arc<Terminal>,
        message: &Value,
    ) -> Result<()> {
        if !context.is_live() {
            bail!("Terminal input source has closed");
        }
        self.track_source(context);
        match message["type"].as_str() {
            Some("input") => {
                terminal
                    .input(required(message, "data")?.as_bytes())
                    .await?
            }
            Some("resize") => {
                let (rows, cols) = dimensions(message)?;
                terminal
                    .resize(
                        context.source_id(),
                        rows,
                        cols,
                        message["intent"] != "update",
                    )
                    .await?;
            }
            Some("mouse") => {}
            _ => bail!("Invalid terminal input"),
        };
        Ok(())
    }
    pub async fn handle_binary(&self, context: &RequestContext, bytes: &[u8]) -> Result<bool> {
        if bytes.first().is_none_or(|opcode| !(1..=5).contains(opcode)) {
            return Ok(false);
        }
        if bytes.len() < 2 {
            bail!("Invalid terminal frame");
        }
        if !context.allows("workspace.write") {
            bail!("Terminal input is not permitted");
        }
        let registration = {
            self.streams
                .lock()
                .unwrap()
                .get(&(context.source_id().to_owned(), bytes[1]))
                .cloned()
        };
        let Some(registration) = registration else {
            return Ok(true);
        };
        if registration.cancel.is_cancelled() {
            return Ok(true);
        }
        let terminal = self.get(&registration.terminal_id).await?;
        self.track_source(context);
        match bytes[0] {
            2 => terminal.input(&bytes[2..]).await?,
            3 => {
                let value: Value = serde_json::from_slice(&bytes[2..])?;
                let (rows, cols) = dimensions(&value)?;
                terminal
                    .resize(context.source_id(), rows, cols, value["intent"] != "update")
                    .await?;
            }
            _ => bail!("Invalid terminal frame direction"),
        };
        Ok(true)
    }
    pub async fn shutdown(&self) -> Result<()> {
        self.shutting_down
            .store(true, std::sync::atomic::Ordering::Release);
        let mut error = self.scripts.shutdown().await.err();
        let terminals: Vec<_> = self.terminals.read().unwrap().values().cloned().collect();
        for terminal in terminals {
            if let Err(e) = terminal.terminate().await {
                if error.is_none() {
                    error = Some(e);
                }
            }
        }
        self.terminals.write().unwrap().clear();
        for stream in self.streams.lock().unwrap().values() {
            stream.cancel.cancel();
        }
        if let Some(error) = error {
            return Err(error);
        }
        Ok(())
    }
}
fn required<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key].as_str().ok_or_else(|| anyhow!("Missing {key}"))
}
fn dimensions(value: &Value) -> Result<(u16, u16)> {
    let parse = |key: &str| {
        value[key]
            .as_u64()
            .and_then(|n| u16::try_from(n).ok())
            .filter(|n| *n > 0)
            .ok_or_else(|| anyhow!("Invalid terminal {key}"))
    };
    Ok((parse("rows")?, parse("cols")?))
}
