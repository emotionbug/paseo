use super::{CreateTerminal, scripts::config};
use crate::{
    process::{CommandSpec, ManagedProcess},
    state::AppState,
};
use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Weak},
    time::{Duration, Instant},
};
use tokio::{
    io::AsyncReadExt,
    sync::{broadcast, watch},
};
use tokio_util::sync::CancellationToken;

const LIMIT: usize = 64 * 1024;
const MARKER: &str = "\n...<output truncated in the middle>...\n";
#[derive(Default)]
struct Output {
    head: Vec<u8>,
    tail: Vec<u8>,
    truncated: bool,
}
impl Output {
    fn append(&mut self, chunk: &[u8]) {
        let head_budget = (LIMIT - MARKER.len()) / 2;
        let tail_budget = LIMIT - MARKER.len() - head_budget;
        if !self.truncated {
            self.head.extend_from_slice(chunk);
            if self.head.len() <= LIMIT {
                return;
            }
            self.tail = self.head[self.head.len() - tail_budget..].to_vec();
            self.head.truncate(head_budget);
            self.truncated = true;
        } else if chunk.len() >= tail_budget {
            self.tail.clear();
            self.tail
                .extend_from_slice(&chunk[chunk.len() - tail_budget..]);
        } else {
            let remove = (self.tail.len() + chunk.len()).saturating_sub(tail_budget);
            self.tail.drain(..remove);
            self.tail.extend_from_slice(chunk);
        }
    }
    fn render(&self) -> String {
        if !self.truncated {
            return carriage_returns(&String::from_utf8_lossy(&self.head));
        }
        carriage_returns(&format!(
            "{}{}{}",
            String::from_utf8_lossy(&self.head),
            MARKER,
            String::from_utf8_lossy(&self.tail)
        ))
    }
}
fn carriage_returns(text: &str) -> String {
    let mut out = String::new();
    let mut line = Vec::new();
    let mut cursor = 0;
    for c in text.chars() {
        match c {
            '\r' => cursor = 0,
            '\n' => {
                out.extend(line.drain(..));
                out.push('\n');
                cursor = 0;
            }
            _ => {
                if cursor < line.len() {
                    line[cursor] = c;
                } else {
                    line.push(c);
                }
                cursor += 1;
            }
        }
    }
    out.extend(line);
    out
}

#[derive(Default)]
struct AnsiFilter {
    state: u8,
}
impl AnsiFilter {
    fn strip(&mut self, bytes: &[u8], clean: &mut Vec<u8>) {
        clean.clear();
        for &byte in bytes {
            self.state = match (self.state, byte) {
                (0, 0x1b) => 1,
                (0, _) => {
                    clean.push(byte);
                    0
                }
                (1, b'[') => 2,
                (1, b']' | b'P' | b'X' | b'^' | b'_') => 3,
                (1, 0x20..=0x2f) => 1,
                (1, _) => 0,
                (2, 0x40..=0x7e) => 0,
                (2, _) => 2,
                (3, 0x07) => 0,
                (3, 0x1b) => 4,
                (3, _) => 3,
                (4, b'\\') => 0,
                (4, _) => 3,
                _ => 0,
            };
        }
    }
}
struct Run {
    cancel: CancellationToken,
    complete: watch::Receiver<bool>,
    generation: uuid::Uuid,
}
struct Inner {
    app: Weak<AppState>,
    snapshots: Mutex<HashMap<String, Value>>,
    runs: Mutex<HashMap<String, Run>>,
    events: broadcast::Sender<(String, Value)>,
    admission: tokio::sync::Mutex<()>,
}
#[derive(Clone)]
pub(crate) struct Setup {
    inner: Arc<Inner>,
}
impl Setup {
    pub fn new(app: Weak<AppState>) -> Self {
        Self {
            inner: Arc::new(Inner {
                app,
                snapshots: Mutex::new(HashMap::new()),
                runs: Mutex::new(HashMap::new()),
                events: broadcast::channel(256).0,
                admission: tokio::sync::Mutex::new(()),
            }),
        }
    }
    pub fn subscribe(&self) -> broadcast::Receiver<(String, Value)> {
        self.inner.events.subscribe()
    }
    pub fn cached(&self, id: &str) -> Option<Value> {
        self.inner.snapshots.lock().unwrap().get(id).cloned()
    }
    pub async fn status(&self, id: &str) -> Result<Option<Value>> {
        if let Some(value) = self.cached(id) {
            return Ok(Some(value));
        }
        let app = self
            .inner
            .app
            .upgrade()
            .ok_or_else(|| anyhow!("Daemon is shutting down"))?;
        let Some(workspace) = app.projects.workspace(id).await? else {
            return Ok(None);
        };
        Ok(self.cached(id).or_else(|| blocked(&workspace)))
    }
    async fn publish(&self, id: &str, value: Value) {
        let status_changed = {
            let mut snapshots = self.inner.snapshots.lock().unwrap();
            let changed = snapshots.get(id).and_then(summary) != summary(&value);
            snapshots.insert(id.to_owned(), value.clone());
            changed
        };
        let _ = self.inner.events.send((id.to_owned(), value.clone()));
        if let Some(app) = self.inner.app.upgrade() {
            let mut payload = value;
            payload["workspaceId"] = id.into();
            let _ = app
                .transport
                .publish_event(json!({"type":"workspace_setup_progress","payload":payload}))
                .await;
            if status_changed {
                if let Err(error) = app.projects.publish_workspace(id).await {
                    tracing::warn!(%error,workspace_id=id,"Failed to publish workspace setup status");
                }
            }
        }
    }
    pub async fn start(&self, id: &str) -> Result<Value> {
        let _admission = self.inner.admission.lock().await;
        let app = self
            .inner
            .app
            .upgrade()
            .ok_or_else(|| anyhow!("Daemon is shutting down"))?;
        let workspace = app
            .projects
            .workspace(id)
            .await?
            .ok_or_else(|| anyhow!("Workspace not found: {id}"))?;
        if !workspace["archivedAt"].is_null() {
            bail!("Workspace not found: {id}");
        }
        if let Some(snapshot) = blocked(&workspace) {
            self.publish(id, snapshot.clone()).await;
            return Ok(snapshot);
        }
        self.stop(id).await?;
        let snapshot = snapshot(&workspace, "running", Vec::new(), None, false);
        self.publish(id, snapshot.clone()).await;
        let cancel = app.cancel.child_token();
        let generation = uuid::Uuid::new_v4();
        let (completed, complete) = watch::channel(false);
        self.inner.runs.lock().unwrap().insert(
            id.to_owned(),
            Run {
                cancel: cancel.clone(),
                complete,
                generation,
            },
        );
        let this = self.clone();
        let id = id.to_owned();
        drop(app);
        tokio::spawn(async move {
            this.run(&id, &workspace, cancel).await;
            let mut runs = this.inner.runs.lock().unwrap();
            if runs.get(&id).is_some_and(|r| r.generation == generation) {
                runs.remove(&id);
            }
            let _ = completed.send(true);
        });
        Ok(snapshot)
    }
    pub async fn stop(&self, id: &str) -> Result<()> {
        let run = self
            .inner
            .runs
            .lock()
            .unwrap()
            .get(id)
            .map(|r| (r.cancel.clone(), r.complete.clone()));
        if let Some((cancel, mut complete)) = run {
            cancel.cancel();
            while !*complete.borrow_and_update() {
                if complete.changed().await.is_err() {
                    break;
                }
            }
        }
        Ok(())
    }
    pub async fn shutdown(&self) -> Result<()> {
        let ids: Vec<_> = self.inner.runs.lock().unwrap().keys().cloned().collect();
        for id in ids {
            self.stop(&id).await?;
        }
        Ok(())
    }
    async fn run(&self, id: &str, workspace: &Value, cancel: CancellationToken) {
        let mut commands = Vec::new();
        let mut truncated = false;
        let result = self
            .run_commands(
                id,
                workspace,
                &cancel,
                &mut commands,
                &mut truncated,
                "setup",
            )
            .await;
        match result {
            Ok(config) => {
                self.publish(
                    id,
                    snapshot(workspace, "completed", commands, None, truncated),
                )
                .await;
                if cancel.is_cancelled() {
                    return;
                }
                let operations=config::terminals(&config).into_iter().map(|(name,command)| {
                    let cancel=&cancel;
                    async move {
                    if cancel.is_cancelled() { return; }
                    let Some(app) = self.inner.app.upgrade() else { return };
                    let operation = async {
                        let env = BTreeMap::new();
                        let terminal = app.terminals.create(CreateTerminal { cwd: cwd(workspace)?,workspace_id:id.to_owned(),name,command:None,args:Vec::new(),env,rows:24,cols:80 }).await?;
                        super::scripts::wait_ready(&terminal).await;
                        if cancel.is_cancelled() { app.terminals.kill(&terminal.id).await?; return Ok::<_,anyhow::Error>(()); }
                        terminal.input(format!("{command}\r").as_bytes()).await
                    }.await;
                    if let Err(error) = operation { tracing::warn!(%error,workspace_id=id,"Failed to bootstrap worktree terminal"); }
                    }
                });
                futures_util::future::join_all(operations).await;
            }
            Err(error) => {
                self.publish(
                    id,
                    snapshot(
                        workspace,
                        "failed",
                        commands,
                        Some(error.to_string()),
                        truncated,
                    ),
                )
                .await
            }
        }
    }
    async fn run_commands(
        &self,
        id: &str,
        workspace: &Value,
        cancel: &CancellationToken,
        commands: &mut Vec<Value>,
        truncated: &mut bool,
        kind: &str,
    ) -> Result<Value> {
        let directory = cwd(workspace)?;
        if kind == "teardown" {
            let root = workspace["worktreeRoot"]
                .as_str()
                .map(PathBuf::from)
                .unwrap_or_else(|| directory.clone());
            let root = crate::projects::worktrees::normalize_path(&root).await?;
            let target = crate::projects::worktrees::normalize_path(&directory).await?;
            if !target.starts_with(root) {
                bail!(
                    "Worktree teardown cwd is outside the worktree: {}",
                    directory.display()
                );
            }
        }
        let config = config::read(&directory).await?;
        let authored_commands = config::commands(&config, kind);
        if kind == "teardown" && authored_commands.is_empty() {
            return Ok(config);
        }
        let app = self
            .inner
            .app
            .upgrade()
            .ok_or_else(|| anyhow!("Daemon is shutting down"))?;
        app.projects.assert_automation_allowed(id).await?;
        let runtime_env = if kind == "teardown" {
            app.projects.teardown_environment(id).await?
        } else {
            app.projects.environment(id).await?
        };
        if kind == "setup" {
            app.terminals
                .register_workspace_env(id, runtime_env.clone());
        }
        let mut env = app.config.environment();
        env.extend(runtime_env);
        sanitize_environment(&mut env);
        drop(app);
        let observation = if kind == "setup" {
            Some((id, workspace))
        } else {
            None
        };
        self.execute_commands(
            observation,
            &directory,
            env,
            authored_commands,
            cancel,
            commands,
            truncated,
            kind,
        )
        .await?;
        Ok(config)
    }
    async fn execute_commands(
        &self,
        observation: Option<(&str, &Value)>,
        directory: &Path,
        env: BTreeMap<String, String>,
        authored_commands: Vec<String>,
        cancel: &CancellationToken,
        commands: &mut Vec<Value>,
        truncated: &mut bool,
        kind: &str,
    ) -> Result<()> {
        for (index, command) in authored_commands.into_iter().enumerate() {
            if cancel.is_cancelled() {
                bail!("Worktree {kind} cancelled");
            }
            let started = Instant::now();
            commands.push(json!({"index":index+1,"command":command,"cwd":directory,"log":"","status":"running","exitCode":null}));
            if let Some((id, workspace)) = observation {
                self.publish(
                    id,
                    snapshot(workspace, "running", commands.clone(), None, *truncated),
                )
                .await;
            }
            let mut output = Output::default();
            let mut stderr = Output::default();
            let spawned = ManagedProcess::spawn_with_stderr(shell_spec(
                &command,
                directory.to_path_buf(),
                env.clone(),
            ))
            .await;
            let (process, mut stdout, mut error_stream) = match spawned {
                Ok(parts) => parts,
                Err(error) => {
                    commands[index]["log"] = error.to_string().into();
                    bail!("Worktree {kind} command failed: {command}\n{error}");
                }
            };
            process.close_stdin().await?;
            let mut out_open = true;
            let mut err_open = true;
            let mut out_buf = [0u8; 8192];
            let mut err_buf = [0u8; 8192];
            let mut out_filter = AnsiFilter::default();
            let mut err_filter = AnsiFilter::default();
            let mut clean = Vec::with_capacity(8192);
            let mut cancelled = false;
            let mut drain_deadline = None;
            let mut exit = None;
            loop {
                if !out_open && !err_open && exit.is_some() {
                    break;
                }
                tokio::select! {
                    _ = cancel.cancelled(), if !cancelled => { cancelled = true; process.terminate(Duration::from_secs(1)).await?; drain_deadline = Some(tokio::time::Instant::now()+Duration::from_secs(2)); }
                    result = process.wait_status(), if exit.is_none() => { exit = Some(result?.code()); drain_deadline = Some(tokio::time::Instant::now()+Duration::from_secs(2)); }
                    _ = async { if let Some(deadline) = drain_deadline { tokio::time::sleep_until(deadline).await; } else { std::future::pending::<()>().await; } }, if out_open || err_open => { out_open=false;err_open=false; }
                    read = stdout.read(&mut out_buf), if out_open => { let n=read?; if n==0 {out_open=false;} else {out_filter.strip(&out_buf[..n],&mut clean);output.append(&clean);} }
                    read = error_stream.read(&mut err_buf), if err_open => { let n=read?; if n==0 {err_open=false;} else {err_filter.strip(&err_buf[..n],&mut clean);output.append(&clean);stderr.append(&clean);} }
                }
                commands[index]["log"] = output.render().into();
                *truncated |= output.truncated;
                if let Some((id, workspace)) = observation {
                    self.publish(
                        id,
                        snapshot(workspace, "running", commands.clone(), None, *truncated),
                    )
                    .await;
                }
            }
            let code = exit.unwrap();
            commands[index]["exitCode"] = json!(code);
            // Source detail derives command status solely from nullable exitCode;
            // the enclosing snapshot carries cancellation/spawn failures.
            commands[index]["status"] = match code {
                None => "running",
                Some(0) => "completed",
                Some(_) => "failed",
            }
            .into();
            let elapsed = started.elapsed().as_millis() as u64;
            if elapsed > 0 {
                commands[index]["durationMs"] = elapsed.into();
            }
            if code != Some(0) || cancelled {
                bail!(
                    "{}",
                    format!(
                        "Worktree {kind} command failed: {command}\n{}",
                        stderr.render()
                    )
                    .trim()
                );
            }
        }
        Ok(())
    }
    pub async fn teardown(&self, id: &str) -> Result<()> {
        let app = self
            .inner
            .app
            .upgrade()
            .ok_or_else(|| anyhow!("Daemon is shutting down"))?;
        let workspace = app
            .projects
            .workspace(id)
            .await?
            .ok_or_else(|| anyhow!("Workspace not found: {id}"))?;
        let cancel = app.cancel.child_token();
        drop(app);
        self.run_commands(
            id,
            &workspace,
            &cancel,
            &mut Vec::new(),
            &mut false,
            "teardown",
        )
        .await?;
        Ok(())
    }
    pub async fn teardown_path(&self, directory: &Path, repo_root: Option<&Path>) -> Result<()> {
        let config = config::read(directory).await?;
        let commands = config::commands(&config, "teardown");
        if commands.is_empty() {
            return Ok(());
        }
        let app = self
            .inner
            .app
            .upgrade()
            .ok_or_else(|| anyhow!("Daemon is shutting down"))?;
        let mut env = app.config.environment();
        env.extend(
            app.projects
                .teardown_path_environment(directory, repo_root)
                .await?,
        );
        let cancel = app.cancel.child_token();
        drop(app);
        sanitize_environment(&mut env);
        self.execute_commands(
            None,
            directory,
            env,
            commands,
            &cancel,
            &mut Vec::new(),
            &mut false,
            "teardown",
        )
        .await
    }
}
fn sanitize_environment(env: &mut BTreeMap<String, String>) {
    for key in [
        "BASH_ENV",
        "PASEO_NODE_ENV",
        "PASEO_DESKTOP_MANAGED",
        "PASEO_SUPERVISED",
        "ELECTRON_RUN_AS_NODE",
        "ELECTRON_NO_ATTACH_CONSOLE",
        "ESBUILD_BINARY_PATH",
    ] {
        env.remove(key);
    }
}
fn cwd(workspace: &Value) -> Result<PathBuf> {
    Ok(PathBuf::from(
        workspace["cwd"]
            .as_str()
            .ok_or_else(|| anyhow!("Workspace cwd is missing"))?,
    ))
}
fn snapshot(
    workspace: &Value,
    status: &str,
    commands: Vec<Value>,
    error: Option<String>,
    truncated: bool,
) -> Value {
    let mut detail = json!({"type":"worktree_setup","worktreePath":workspace["worktreeRoot"].as_str().or(workspace["cwd"].as_str()).unwrap_or(""),"branchName":workspace["branch"].as_str().unwrap_or(""),"log":"","commands":commands});
    if truncated {
        detail["truncated"] = true.into();
    }
    json!({"status":status,"detail":detail,"error":error})
}
fn summary(snapshot: &Value) -> Option<&str> {
    let status = snapshot["status"].as_str()?;
    if matches!(status, "failed" | "blocked")
        || !snapshot["error"].is_null()
        || snapshot["detail"]["commands"]
            .as_array()
            .is_some_and(|commands| !commands.is_empty())
    {
        Some(status)
    } else {
        None
    }
}
fn blocked(workspace: &Value) -> Option<Value> {
    let source = workspace.get("untrustedSource").filter(|v| !v.is_null())?;
    let mut value = snapshot(workspace, "blocked", Vec::new(), None, false);
    value["blockedSource"] = source.clone();
    Some(value)
}
fn shell_spec(command: &str, cwd: PathBuf, env: BTreeMap<String, String>) -> CommandSpec {
    #[cfg(windows)]
    let (program, args) = (
        "powershell",
        vec![
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-ExecutionPolicy".into(),
            "Bypass".into(),
            "-Command".into(),
            command.into(),
        ],
    );
    #[cfg(not(windows))]
    let (program, args) = ("bash", vec!["-c".into(), command.into()]);
    CommandSpec {
        program: program.into(),
        args,
        cwd: Some(cwd),
        env,
        clear_env: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn output_preserves_both_ends_with_bounded_storage() {
        let mut output = Output::default();
        output.append(&vec![b'a'; LIMIT]);
        output.append(b"END");
        assert!(output.truncated);
        assert!(output.render().ends_with("END"));
        assert_eq!(output.head.len() + MARKER.len() + output.tail.len(), LIMIT);
    }
    #[test]
    fn carriage_return_overwrites_without_erasing_suffix() {
        assert_eq!(carriage_returns("abcdef\rxy\r\nnext"), "xycdef\nnext");
    }
    #[test]
    fn blocked_identity_is_not_inferred_from_path() {
        let value=blocked(&json!({"cwd":"/x","branch":null,"untrustedSource":{"kind":"change_request","forge":"github","number":2,"headRepository":"other/repo"}})).unwrap();
        assert_eq!(value["status"], "blocked");
        assert_eq!(value["detail"]["commands"], json!([]));
    }
    #[test]
    fn ansi_control_sequences_split_across_chunks_are_removed() {
        let mut filter = AnsiFilter::default();
        let mut clean = Vec::new();
        filter.strip(b"before\x1b[3", &mut clean);
        assert_eq!(clean, b"before");
        filter.strip(b"1mred\x1b]title", &mut clean);
        assert_eq!(clean, b"red");
        filter.strip(b"\x1b\\after", &mut clean);
        assert_eq!(clean, b"after");
    }
    #[test]
    fn empty_setup_does_not_publish_a_directory_status_but_failures_do() {
        let workspace = json!({"cwd":"/workspace","worktreeRoot":"/root","branch":"feature"});
        assert_eq!(
            summary(&snapshot(&workspace, "completed", vec![], None, false)),
            None
        );
        let failed = snapshot(
            &workspace,
            "failed",
            vec![],
            Some("invalid config".into()),
            false,
        );
        assert_eq!(summary(&failed), Some("failed"));
        let running = snapshot(
            &workspace,
            "running",
            vec![
                json!({"index":1,"command":"build","log":"live","status":"running","exitCode":null}),
            ],
            None,
            false,
        );
        assert_eq!(summary(&running), Some("running"));
        assert_eq!(running["detail"]["log"], "");
        assert_eq!(running["detail"]["commands"][0]["log"], "live");
        assert_eq!(running["detail"]["worktreePath"], "/root");
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn path_only_executor_streams_both_pipes_and_stops_at_first_failure() {
        let directory = tempfile::tempdir().unwrap();
        let setup = Setup::new(Weak::new());
        let env = BTreeMap::from([("PATH".to_owned(), "/usr/bin:/bin".to_owned())]);
        let mut commands = Vec::new();
        let mut truncated = false;
        let result = setup
            .execute_commands(
                None,
                directory.path(),
                env,
                vec![
                    "printf 'out\\n'; printf 'err\\n' >&2; exit 7".into(),
                    "touch should-not-exist".into(),
                ],
                &CancellationToken::new(),
                &mut commands,
                &mut truncated,
                "teardown",
            )
            .await;
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Worktree teardown command failed")
        );
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0]["exitCode"], 7);
        assert_eq!(commands[0]["status"], "failed");
        let log = commands[0]["log"].as_str().unwrap();
        assert!(log.contains("out\n"));
        assert!(log.contains("err\n"));
        assert!(!directory.path().join("should-not-exist").exists());
        assert!(!truncated);
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn signalled_command_has_null_exit_code_without_inventing_workspace_identity() {
        let directory = tempfile::tempdir().unwrap();
        let setup = Setup::new(Weak::new());
        let env = BTreeMap::from([("PATH".to_owned(), "/usr/bin:/bin".to_owned())]);
        let mut commands = Vec::new();
        assert!(
            setup
                .execute_commands(
                    None,
                    directory.path(),
                    env,
                    vec!["kill -TERM $$".into()],
                    &CancellationToken::new(),
                    &mut commands,
                    &mut false,
                    "teardown"
                )
                .await
                .is_err()
        );
        assert_eq!(commands[0]["exitCode"], Value::Null);
        assert_eq!(commands[0]["status"], "running");
        assert_eq!(setup.inner.snapshots.lock().unwrap().len(), 0);
    }
}
