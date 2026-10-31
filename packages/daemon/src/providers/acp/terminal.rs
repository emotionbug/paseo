use super::mapping::{array, text};
use crate::process::{CommandSpec, ManagedProcess};
use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    sync::Notify,
};

const MAX_OUTPUT: usize = 8 * 1024 * 1024;
pub struct Terminal {
    process: Arc<ManagedProcess>,
    state: Mutex<State>,
    changed: Notify,
    limit: usize,
}
struct State {
    output: VecDeque<u8>,
    truncated: bool,
    exit: Option<Value>,
    failure: Option<String>,
    readers: usize,
}
impl Terminal {
    pub async fn spawn(
        params: &Value,
        cwd: &str,
        mut env: BTreeMap<String, String>,
    ) -> Result<Arc<Self>> {
        let mut command = params["command"]
            .as_str()
            .ok_or_else(|| anyhow!("Missing terminal command"))?
            .to_owned();
        if command.is_empty() {
            bail!("Empty terminal command");
        }
        let mut args: Vec<String> = array(&params["args"])
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| anyhow!("Terminal argv must contain strings"))
            })
            .collect::<Result<_>>()?;
        if args.is_empty() && command.trim().chars().any(char::is_whitespace) {
            #[cfg(windows)]
            {
                args = vec!["/d".into(), "/s".into(), "/c".into(), command];
                command = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into());
            }
            #[cfg(not(windows))]
            {
                args = vec!["-c".into(), command];
                command = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
            }
        }
        for variable in array(&params["env"]) {
            env.insert(
                text(&variable["name"]).into(),
                text(&variable["value"]).into(),
            );
        }
        let requested = params["outputByteLimit"]
            .as_u64()
            .unwrap_or(MAX_OUTPUT as u64);
        let limit = if requested == 0 {
            MAX_OUTPUT
        } else {
            requested.min(MAX_OUTPUT as u64) as usize
        };
        let (process, stdout, stderr) = ManagedProcess::spawn_with_stderr(CommandSpec {
            program: command,
            args,
            cwd: Some(PathBuf::from(params["cwd"].as_str().unwrap_or(cwd))),
            env,
            clear_env: false,
        })
        .await?;
        process.close_stdin().await?;
        let terminal = Arc::new(Self {
            process: process.clone(),
            state: Mutex::new(State {
                output: VecDeque::new(),
                truncated: false,
                exit: None,
                failure: None,
                readers: 2,
            }),
            changed: Notify::new(),
            limit,
        });
        terminal.read(stdout);
        terminal.read(stderr);
        let watcher = terminal.clone();
        tokio::spawn(async move {
            let result = process.wait_status().await;
            {
                let mut state = watcher.state.lock().unwrap();
                match result {
                    Ok(status) => {
                        #[cfg(unix)]
                        let signal = {
                            use std::os::unix::process::ExitStatusExt;
                            status.signal().map(|number| {
                                format!(
                                    "SIG{}",
                                    nix::sys::signal::Signal::try_from(number)
                                        .map(|s| format!("{s:?}")
                                            .trim_start_matches("SIG")
                                            .to_owned())
                                        .unwrap_or_else(|_| number.to_string())
                                )
                            })
                        };
                        #[cfg(not(unix))]
                        let signal: Option<String> = None;
                        state.exit = Some(json!({"exitCode":status.code(),"signal":signal}));
                    }
                    Err(error) => state.failure = Some(error.to_string()),
                }
            }
            watcher.changed.notify_waiters();
        });
        Ok(terminal)
    }
    fn read<R: AsyncRead + Unpin + Send + 'static>(self: &Arc<Self>, mut reader: R) {
        let terminal = self.clone();
        tokio::spawn(async move {
            let mut buffer = [0u8; 8192];
            loop {
                match reader.read(&mut buffer).await {
                    Ok(0) => break,
                    Ok(count) => {
                        let mut state = terminal.state.lock().unwrap();
                        state.output.extend(&buffer[..count]);
                        if state.output.len() > terminal.limit {
                            let excess = state.output.len() - terminal.limit;
                            state.output.drain(..excess);
                            while state.output.front().is_some_and(|b| b & 0xc0 == 0x80) {
                                state.output.pop_front();
                            }
                            state.truncated = true;
                        }
                    }
                    Err(error) => {
                        terminal.state.lock().unwrap().failure = Some(error.to_string());
                        break;
                    }
                }
            }
            terminal.state.lock().unwrap().readers -= 1;
            terminal.changed.notify_waiters();
        });
    }
    pub fn output(&self) -> Value {
        let mut state = self.state.lock().unwrap();
        let output = String::from_utf8_lossy(state.output.make_contiguous()).into_owned();
        let mut result = json!({"truncated":state.truncated});
        result["output"] = Value::String(output);
        if let Some(exit) = &state.exit {
            result["exitStatus"] = exit.clone();
        }
        result
    }
    pub async fn wait(&self) -> Result<Value> {
        loop {
            let notified = self.changed.notified();
            {
                let state = self.state.lock().unwrap();
                if let Some(error) = &state.failure {
                    bail!("ACP terminal failed: {error}");
                }
                if state.readers == 0 {
                    if let Some(exit) = &state.exit {
                        return Ok(exit.clone());
                    }
                }
            }
            notified.await;
        }
    }
    pub async fn kill(&self) -> Result<()> {
        self.process.terminate(Duration::from_secs(2)).await
    }
}
