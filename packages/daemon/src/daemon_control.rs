//! Native lifecycle ownership. paseo.pid is shared with older Node supervisors.
mod identity;
mod lock;
mod supervisor;
#[cfg(test)]
mod tests;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(crate) use identity::start_identity as read_process_start_identity;
pub use supervisor::{WorkerGuard, attach_worker, publish_worker_ready, run_internal};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{process::Command, time::Instant};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonInstance {
    pub pid: u32,
    pub started_at: String,
    pub hostname: String,
    pub uid: u32,
    pub listen: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_id: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub desktop_managed: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub heartbeat: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boot_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_start_identity: Option<String>,
}
fn is_false(value: &bool) -> bool {
    !value
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct DaemonInstanceError {
    pub code: String,
    pub message: String,
}
fn error(code: &str, message: impl Into<String>) -> anyhow::Error {
    DaemonInstanceError {
        code: code.to_owned(),
        message: message.into(),
    }
    .into()
}
pub fn same_daemon_instance(a: &DaemonInstance, b: &DaemonInstance) -> bool {
    a.pid == b.pid && a.started_at == b.started_at
}

pub async fn read_pid_lock(home: &Path) -> Result<Option<DaemonInstance>> {
    let home = home.to_owned();
    tokio::task::spawn_blocking(move || lock::read(&home)).await?
}
pub async fn read_daemon_instance(home: &Path) -> Result<Option<DaemonInstance>> {
    let home = home.to_owned();
    tokio::task::spawn_blocking(move || {
        let Some(instance) = lock::read(&home)? else {
            return Ok(None);
        };
        if identity::owner_running(&instance)? {
            Ok(Some(instance))
        } else {
            Ok(None)
        }
    })
    .await?
}
pub fn daemon_log_path(home: &Path) -> PathBuf {
    std::fs::read(home.join("config.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|config| {
            config
                .pointer("/log/file/path")
                .and_then(Value::as_str)
                .map(|path| home.join(path))
        })
        .unwrap_or_else(|| home.join("daemon.log"))
}
fn not_ready(home: &Path, instance: &DaemonInstance) -> anyhow::Error {
    error(
        "DAEMON_NOT_READY",
        format!(
            "Daemon PID {} remains running but is not ready for {}.\nLogs: {}\nStatus: paseo daemon status --home {:?}\nStop: paseo daemon stop --home {:?}",
            instance.pid,
            home.display(),
            daemon_log_path(home).display(),
            home,
            home
        ),
    )
}
async fn delay(cancel: &CancellationToken) -> Result<()> {
    tokio::select! { _ = cancel.cancelled() => bail!("Operation cancelled"), _ = tokio::time::sleep(Duration::from_millis(100)) => Ok(()) }
}

pub async fn wait_for_daemon_ready(
    home: &Path,
    timeout: Duration,
    expected: Option<&DaemonInstance>,
    cancel: &CancellationToken,
) -> Result<DaemonInstance> {
    let deadline = Instant::now() + timeout;
    loop {
        if cancel.is_cancelled() {
            bail!("Operation cancelled");
        }
        let instance = read_daemon_instance(home).await?.ok_or_else(|| {
            error(
                "DAEMON_NOT_RUNNING",
                format!(
                    "Daemon is not running for {}. Start with: paseo daemon start --home {:?}",
                    home.display(),
                    home
                ),
            )
        })?;
        if expected.is_some_and(|expected| !same_daemon_instance(expected, &instance)) {
            return Err(error(
                "DAEMON_REPLACED",
                format!(
                    "Supervisor changed for {}; refusing to follow PID {}.",
                    home.display(),
                    instance.pid
                ),
            ));
        }
        if instance.listen.as_ref().is_some_and(|s| !s.is_empty()) {
            return Ok(instance);
        }
        if Instant::now() >= deadline {
            return Err(not_ready(home, &instance));
        }
        delay(cancel).await?;
    }
}

pub struct StartOptions {
    pub home: PathBuf,
    pub options: Value,
    pub foreground: bool,
    pub desktop_managed: bool,
    pub deployment: bool,
    pub timeout: Duration,
    pub executable: Option<PathBuf>,
    pub working_directory: Option<PathBuf>,
    pub cli_shim_path: Option<PathBuf>,
    pub on_acquired: Option<Arc<dyn Fn(DaemonInstance) + Send + Sync>>,
}
impl Default for StartOptions {
    fn default() -> Self {
        Self {
            home: PathBuf::new(),
            options: serde_json::json!({}),
            foreground: false,
            desktop_managed: false,
            deployment: false,
            timeout: Duration::from_secs(600),
            executable: None,
            working_directory: None,
            cli_shim_path: None,
            on_acquired: None,
        }
    }
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartResult {
    pub instance: DaemonInstance,
    pub spawned: bool,
    pub exit_code: Option<i32>,
}

pub async fn start_daemon_instance(
    options: StartOptions,
    cancel: &CancellationToken,
) -> Result<StartResult> {
    if options.home.as_os_str().is_empty() {
        bail!("Daemon home is required");
    }
    if cancel.is_cancelled() {
        bail!("Operation cancelled");
    }
    if let Some(instance) = read_daemon_instance(&options.home).await? {
        return Ok(StartResult {
            instance,
            spawned: false,
            exit_code: None,
        });
    }
    let executable = options
        .executable
        .clone()
        .map(Ok)
        .unwrap_or_else(std::env::current_exe)?;
    let mut command = Command::new(executable);
    command
        .arg("__supervisor")
        .arg("--home")
        .arg(&options.home)
        .env("PASEO_HOME", &options.home)
        .env(
            "PASEO_NATIVE_OPTIONS",
            serde_json::to_string(&options.options)?,
        )
        .env_remove("PASEO_HOST")
        .env_remove("PASEO_NATIVE_SUPERVISOR")
        .env_remove("PASEO_NATIVE_TOKEN")
        .env_remove("PASEO_DESKTOP_MANAGED");
    if !options.deployment && !options.foreground {
        for key in MANAGED_SETTING_KEYS {
            command.env_remove(key);
        }
    }
    if options.desktop_managed {
        command.env("PASEO_DESKTOP_MANAGED", "1");
    }
    if let Some(cwd) = &options.working_directory {
        command.current_dir(cwd);
    }
    if let Some(path) = &options.cli_shim_path {
        command.env("PASEO_CLI", path);
    }
    if options.foreground {
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit());
    } else {
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        #[cfg(unix)]
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        #[cfg(windows)]
        command.creation_flags(0x00000008 | 0x00000200 | 0x08000000);
    }
    let mut child = command
        .spawn()
        .context("Failed to launch native daemon supervisor")?;
    let pid = child.id().context("Supervisor has no PID")?;
    // The outer foreground CLI owns the supervisor lifetime on Windows, where
    // TerminateProcess cannot run a cancellation handler. Nested supervisor jobs
    // retain worker ownership; closing this handle tears down the complete tree.
    #[cfg(windows)]
    let _foreground_job = if options.foreground {
        match windows::WorkerJob::assign(pid) {
            Ok(job) => Some(job),
            Err(error) => {
                if child.try_wait()?.is_none() {
                    child.start_kill()?;
                }
                let _ = child.wait().await;
                return Err(error).context("Cannot establish foreground supervisor ownership");
            }
        }
    } else {
        None
    };
    let birth = match identity::start_identity(pid) {
        Ok(Some(birth)) => Some(birth),
        result => {
            child.start_kill()?;
            let _ = child.wait().await;
            return Err(result.err().unwrap_or_else(|| {
                error(
                    "DAEMON_START_FAILED",
                    "Cannot establish spawned supervisor identity",
                )
            }));
        }
    };
    let deadline = Instant::now() + options.timeout;
    let mut acquired: Option<DaemonInstance> = None;
    loop {
        if cancel.is_cancelled() {
            if let Some(instance) = acquired.clone() {
                stop_daemon_instance(
                    &options.home,
                    StopOptions {
                        instance: Some(instance),
                        force: true,
                        ..Default::default()
                    },
                    &CancellationToken::new(),
                )
                .await?;
            } else {
                child.start_kill()?;
                let _ = child.wait().await;
            }
            bail!("Operation cancelled");
        }
        let current = match read_daemon_instance(&options.home).await {
            Ok(current) => current,
            Err(error) => {
                child.start_kill()?;
                let _ = child.wait().await;
                return Err(error);
            }
        };
        if let Some(instance) = current {
            if instance.pid != pid || instance.process_start_identity != birth {
                // A concurrent winner is not ours. Reap only the child we created.
                let _ = child.wait().await;
                return Ok(StartResult {
                    instance,
                    spawned: false,
                    exit_code: None,
                });
            }
            if acquired.is_none() {
                if let Some(callback) = &options.on_acquired {
                    callback(instance.clone());
                }
                acquired = Some(instance.clone());
            }
            if instance.listen.as_ref().is_some_and(|s| !s.is_empty()) {
                let exit_code = if options.foreground {
                    Some(
                        tokio::select! { result=child.wait()=>result?.code().unwrap_or(1), _=cancel.cancelled()=>{ stop_daemon_instance(&options.home,StopOptions{instance:Some(instance.clone()),force:true,..Default::default()},&CancellationToken::new()).await?; child.wait().await?.code().unwrap_or(1) } },
                    )
                } else {
                    None
                };
                return Ok(StartResult {
                    instance,
                    spawned: true,
                    exit_code,
                });
            }
        }
        if let Some(status) = child.try_wait()? {
            return Err(error(
                "DAEMON_START_FAILED",
                format!(
                    "Daemon failed to start (exit {}). Logs: {}",
                    status.code().unwrap_or(1),
                    daemon_log_path(&options.home).display()
                ),
            ));
        }
        if Instant::now() >= deadline {
            if acquired.is_none() {
                child.start_kill()?;
                let _ = child.wait().await;
            }
            return Err(acquired
                .as_ref()
                .map(|instance| not_ready(&options.home, instance))
                .unwrap_or_else(|| {
                    error(
                        "DAEMON_NOT_READY",
                        format!(
                            "Supervisor PID {pid} has not published its lock. Logs: {}",
                            daemon_log_path(&options.home).display()
                        ),
                    )
                }));
        }
        let _ = tokio::time::timeout(Duration::from_millis(100), cancel.cancelled()).await;
    }
}

#[derive(Clone)]
pub struct StopOptions {
    pub instance: Option<DaemonInstance>,
    pub force: bool,
    pub timeout: Duration,
    pub kill_timeout: Duration,
}
impl Default for StopOptions {
    fn default() -> Self {
        Self {
            instance: None,
            force: false,
            timeout: Duration::from_secs(15),
            kill_timeout: Duration::from_secs(3),
        }
    }
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StopResult {
    pub action: String,
    pub pid: Option<u32>,
    pub forced: bool,
    pub used_lifecycle_rpc: bool,
}
async fn confirm(home: &Path, expected: &DaemonInstance) -> Result<()> {
    if read_pid_lock(home).await?.as_ref().is_some_and(|current| {
        !same_daemon_instance(current, expected)
            || current.process_start_identity != expected.process_start_identity
    }) {
        return Err(error(
            "DAEMON_REPLACED",
            format!(
                "Supervisor changed for {}; refusing to stop PID {}",
                home.display(),
                expected.pid
            ),
        ));
    }
    Ok(())
}
pub(crate) async fn wait_exit(
    home: &Path,
    instance: &DaemonInstance,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<bool> {
    let deadline = Instant::now() + timeout;
    loop {
        confirm(home, instance).await?;
        if !identity::owner_running(instance)? {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        delay(cancel).await?;
    }
}
pub async fn stop_daemon_instance(
    home: &Path,
    options: StopOptions,
    cancel: &CancellationToken,
) -> Result<StopResult> {
    if cancel.is_cancelled() {
        bail!("Operation cancelled")
    }
    let current = read_pid_lock(home).await?;
    if let (Some(expected), Some(current)) = (&options.instance, &current) {
        if !same_daemon_instance(expected, current)
            || expected.process_start_identity != current.process_start_identity
        {
            return Err(error(
                "DAEMON_REPLACED",
                "Supervisor changed; refusing to stop replacement",
            ));
        }
    }
    let Some(instance) = current else {
        return Ok(StopResult {
            action: "not_running".into(),
            pid: None,
            forced: false,
            used_lifecycle_rpc: false,
        });
    };
    if !identity::owner_running(&instance)? {
        let home = home.to_owned();
        let expected = instance.clone();
        tokio::task::spawn_blocking(move || lock::release(&home, &expected)).await??;
        return Ok(StopResult {
            action: "not_running".into(),
            pid: Some(instance.pid),
            forced: false,
            used_lifecycle_rpc: false,
        });
    }
    if instance.pid <= 1 || instance.pid == std::process::id() {
        bail!("Refusing to stop invalid supervisor PID")
    }
    let started = Instant::now();
    let mut used_lifecycle_rpc = false;
    let mut forced = false;
    let verified = identity::verified(&instance)?;
    if instance.listen.is_some() && (!verified || cfg!(windows)) {
        let mut connect =
            crate::client::ConnectOptions::new(crate::client::DaemonTarget::Instance {
                home: home.to_owned(),
            });
        connect.instance = Some(instance.clone());
        connect.timeout = options.timeout.min(Duration::from_secs(5));
        let rpc = async {
            let client = crate::client::DaemonClient::connect(connect, cancel.clone()).await?;
            let result = client
                .shutdown(options.timeout.min(Duration::from_secs(5)))
                .await;
            client.close().await;
            result
        };
        match rpc.await {
            Ok(_) => used_lifecycle_rpc = true,
            Err(e) => {
                if !verified || !options.force {
                    return Err(e);
                }
            }
        }
    }
    if !used_lifecycle_rpc {
        if cancel.is_cancelled() {
            bail!("Operation cancelled")
        }
        confirm(home, &instance).await?;
        if !identity::verified(&instance)? {
            return Err(error(
                "DAEMON_IDENTITY_UNVERIFIED",
                "Cannot verify daemon PID ownership; use the authenticated lifecycle channel",
            ));
        }
        forced = cfg!(windows) && options.force;
        if cfg!(windows) && !options.force {
            return Err(error(
                "STOP_NO_GRACEFUL_CHANNEL",
                "Daemon has no graceful shutdown channel; use --force explicitly",
            ));
        }
        let owned = instance.clone();
        tokio::task::spawn_blocking(move || identity::signal_tree(&owned, forced)).await??;
    }
    let mut stopped = wait_exit(
        home,
        &instance,
        if forced {
            options.kill_timeout
        } else {
            options.timeout.saturating_sub(started.elapsed())
        },
        cancel,
    )
    .await?;
    if !stopped && options.force && !forced {
        if cancel.is_cancelled() {
            bail!("Operation cancelled")
        }
        confirm(home, &instance).await?;
        let owned = instance.clone();
        tokio::task::spawn_blocking(move || identity::signal_tree(&owned, true)).await??;
        forced = true;
        stopped = wait_exit(home, &instance, options.kill_timeout, cancel).await?;
    }
    if !stopped {
        return Err(error(
            "STOP_NOT_CONFIRMED",
            format!(
                "Timed out waiting for supervisor PID {} in {} to exit{}",
                instance.pid,
                home.display(),
                if options.force {
                    ""
                } else {
                    "; use --force to permit forced cleanup"
                }
            ),
        ));
    }
    let home = home.to_owned();
    let expected = instance.clone();
    tokio::task::spawn_blocking(move || lock::release(&home, &expected)).await??;
    Ok(StopResult {
        action: "stopped".into(),
        pid: Some(instance.pid),
        forced,
        used_lifecycle_rpc,
    })
}

const MANAGED_SETTING_KEYS: &[&str] = &[
    "MCP_DEBUG",
    "OPENAI_STT_BASE_URL",
    "OPENAI_TTS_BASE_URL",
    "PASEO_ALLOWED_HOSTS",
    "PASEO_APP_BASE_URL",
    "PASEO_CORS_ORIGINS",
    "PASEO_DICTATION_ENABLED",
    "PASEO_DICTATION_LANGUAGE",
    "PASEO_DICTATION_LOCAL_STT_MODEL",
    "PASEO_DICTATION_STT_PROVIDER",
    "PASEO_GIT_CONCURRENCY",
    "PASEO_GIT_MAX_PROCESSES_PER_SECOND",
    "PASEO_GIT_MAX_PROCESS_CONCURRENCY",
    "PASEO_HOSTNAMES",
    "PASEO_LISTEN",
    "PASEO_LOCAL_MODELS_DIR",
    "PASEO_LOG",
    "PASEO_LOG_CONSOLE_FORMAT",
    "PASEO_LOG_CONSOLE_LEVEL",
    "PASEO_LOG_FILE_LEVEL",
    "PASEO_LOG_FILE_PATH",
    "PASEO_LOG_FILE_ROTATE_COUNT",
    "PASEO_LOG_FILE_ROTATE_SIZE",
    "PASEO_LOG_FORMAT",
    "PASEO_LOG_LEVEL",
    "PASEO_LOG_ROTATE_COUNT",
    "PASEO_LOG_ROTATE_SIZE",
    "PASEO_PASSWORD",
    "PASEO_PLUGIN_REGISTRY",
    "PASEO_RELAY_ENABLED",
    "PASEO_RELAY_ENDPOINT",
    "PASEO_RELAY_PUBLIC_ENDPOINT",
    "PASEO_RELAY_PUBLIC_USE_TLS",
    "PASEO_RELAY_USE_TLS",
    "PASEO_SERVICE_PROXY_ENABLED",
    "PASEO_SERVICE_PROXY_LISTEN",
    "PASEO_SERVICE_PROXY_PUBLIC_BASE_URL",
    "PASEO_TRUSTED_PROXIES",
    "PASEO_VOICE_LANGUAGE",
    "PASEO_VOICE_LLM_PROVIDER",
    "PASEO_VOICE_LOCAL_STT_MODEL",
    "PASEO_VOICE_LOCAL_TTS_MODEL",
    "PASEO_VOICE_LOCAL_TTS_SPEAKER_ID",
    "PASEO_VOICE_LOCAL_TTS_SPEED",
    "PASEO_VOICE_MODE_ENABLED",
    "PASEO_VOICE_STT_PROVIDER",
    "PASEO_VOICE_TTS_PROVIDER",
    "PASEO_VOICE_TURN_DETECTION_PROVIDER",
    "PASEO_WEB_UI_DIST_DIR",
    "PASEO_WEB_UI_ENABLED",
    "PORT",
    "STT_CONFIDENCE_THRESHOLD",
    "STT_MODEL",
    "TTS_MODEL",
    "TTS_VOICE",
];
