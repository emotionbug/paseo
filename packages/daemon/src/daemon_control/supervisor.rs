use super::{DaemonInstance, identity, lock, same_daemon_instance};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    io::{BufRead, Write},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::{Mutex, mpsc},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

pub struct WorkerGuard {
    task: JoinHandle<()>,
    alive: Arc<AtomicBool>,
}
impl Drop for WorkerGuard {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Release);
        self.task.abort();
    }
}

fn worker_identity(home: &Path) -> Result<DaemonInstance> {
    let expected: DaemonInstance = serde_json::from_str(
        &std::env::var("PASEO_NATIVE_SUPERVISOR")
            .context("Worker must be launched by the native supervisor")?,
    )?;
    let actual = lock::read(home)?.context("Supervisor lock disappeared")?;
    if !same_daemon_instance(&expected, &actual)
        || actual.process_start_identity != expected.process_start_identity
        || !identity::verified(&actual)?
    {
        bail!("Supervisor ownership could not be verified");
    }
    #[cfg(unix)]
    if unsafe { libc::getppid() } != actual.pid as i32 {
        bail!("Worker parent is not the lock owner");
    }
    #[cfg(windows)]
    if super::windows::parent_pid()? != actual.pid {
        bail!("Worker parent is not the lock owner");
    }
    Ok(actual)
}

pub fn attach_worker(app: &Arc<crate::state::AppState>) -> Result<WorkerGuard> {
    let owner = worker_identity(&app.home)?;
    let token = std::env::var("PASEO_NATIVE_TOKEN").context("Missing supervisor channel token")?;
    let app = Arc::downgrade(app);
    let lost = CancellationToken::new();
    let shutdown = CancellationToken::new();
    let alive = Arc::new(AtomicBool::new(true));
    let (thread_alive, thread_lost, thread_shutdown) =
        (alive.clone(), lost.clone(), shutdown.clone());
    // Tokio stdin uses its blocking pool, which cannot cancel an outstanding pipe
    // read at runtime shutdown. This dedicated OS thread is never joined: process
    // exit closes it, while the async ownership guard remains fully cancellable.
    std::thread::Builder::new()
        .name("paseo-supervisor-control".into())
        .spawn(move || {
            let stdin = std::io::stdin();
            let mut input = stdin.lock();
            let mut line = Vec::with_capacity(256);
            while thread_alive.load(Ordering::Acquire) {
                line.clear();
                match std::io::Read::take(&mut input, 4097).read_until(b'\n', &mut line) {
                    Ok(0) | Err(_) => {
                        thread_lost.cancel();
                        return;
                    }
                    Ok(_) if line.len() <= 4096 => {
                        let Ok(value) = serde_json::from_slice::<Value>(&line) else {
                            continue;
                        };
                        if value["token"].as_str() != Some(token.as_str()) {
                            continue;
                        }
                        if value["type"] == "paseo:graceful-shutdown" {
                            thread_shutdown.cancel();
                            return;
                        }
                    }
                    _ => {
                        thread_lost.cancel();
                        return;
                    }
                }
            }
        })?;
    let task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                _=lost.cancelled()=>{parent_lost(&app).await;return},
                _=shutdown.cancelled()=>{if let Some(app)=app.upgrade(){app.request_shutdown(false);}return},
                _=interval.tick()=>{
                    let check=owner.clone();
                    if !matches!(tokio::task::spawn_blocking(move||identity::verified(&check)).await,Ok(Ok(true))){parent_lost(&app).await;return}
                }
            }
        }
    });
    Ok(WorkerGuard { task, alive })
}
async fn parent_lost(app: &Weak<crate::state::AppState>) {
    if let Some(app) = app.upgrade() {
        app.request_shutdown(false);
    }
    // The ownership boundary must survive an unresponsive shutdown service.
    tokio::time::sleep(Duration::from_secs(10)).await;
    std::process::exit(1);
}

pub fn publish_worker_ready(listen: &str, server_id: &str) -> Result<()> {
    if listen.is_empty() || server_id.is_empty() {
        bail!("Worker readiness requires listen and server identity");
    }
    let token = std::env::var("PASEO_NATIVE_TOKEN").context("Missing supervisor channel token")?;
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(
        &mut stdout,
        &json!({"type":"paseo:ready","token":token,"listen":listen,"serverId":server_id,"version":env!("CARGO_PKG_VERSION"),"sourceSha":option_env!("PASEO_BUILD_SOURCE_SHA").unwrap_or("")}),
    )?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}

pub async fn run_internal(command: &str, args: &[String]) -> Result<i32> {
    if args.len() != 2 || args[0] != "--home" {
        bail!("Internal daemon entry requires --home PATH");
    }
    let home = PathBuf::from(&args[1]);
    if !home.is_absolute() {
        bail!("Internal daemon home must be absolute")
    }
    let options: Value = serde_json::from_str(
        &std::env::var("PASEO_NATIVE_OPTIONS").unwrap_or_else(|_| "{}".into()),
    )?;
    match command {
        "__worker" => {
            worker_identity(&home)?;
            crate::state::run_daemon(home, options).await
        }
        #[cfg(windows)]
        "__update" => {
            let result = crate::daemon_admin::updater::run_update_helper(&home).await;
            if let Err(error) = &result {
                let log = DurableLog::new(&home);
                log.write(format!("{}\n",json!({"level":"error","time":chrono::Utc::now().to_rfc3339(),"name":"NativeUpdater","msg":error.to_string()})).as_bytes()).await;
            }
            result
        }
        "__supervisor" => {
            let result = run_supervisor(&home, options).await;
            if let Err(e) = &result {
                let log = DurableLog::new(&home);
                log.write(format!("{}\n",json!({"level":"fatal","time":chrono::Utc::now().to_rfc3339(),"pid":std::process::id(),"name":"DaemonRunner","msg":e.to_string()})).as_bytes()).await;
            }
            result
        }
        _ => bail!("Unknown internal daemon entry"),
    }
}

struct DurableLog {
    path: PathBuf,
    max_size: u64,
    max_files: usize,
    serial: Mutex<()>,
}
impl DurableLog {
    fn new(home: &Path) -> Self {
        let config = std::fs::read(home.join("config.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .unwrap_or(Value::Null);
        let env_size = std::env::var("PASEO_LOG_ROTATE_SIZE").ok();
        let size = config
            .pointer("/log/file/rotate/maxSize")
            .and_then(Value::as_str)
            .or(env_size.as_deref())
            .and_then(parse_size)
            .unwrap_or(10 * 1024 * 1024);
        let env_count = std::env::var("PASEO_LOG_ROTATE_COUNT")
            .ok()
            .and_then(|value| {
                value
                    .trim()
                    .trim_start_matches('+')
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
                    .parse::<u64>()
                    .ok()
            })
            .filter(|value| *value > 0);
        let count = config
            .pointer("/log/file/rotate/maxFiles")
            .and_then(Value::as_u64)
            .or(env_count)
            .and_then(|value| usize::try_from(value).ok())
            .filter(|value| *value > 0)
            .unwrap_or(3);
        Self {
            path: super::daemon_log_path(home),
            max_size: size,
            max_files: count,
            serial: Mutex::new(()),
        }
    }
    async fn write(&self, bytes: &[u8]) {
        let _guard = self.serial.lock().await;
        let result = async {
            if let Some(parent) = self.path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            if tokio::fs::metadata(&self.path)
                .await
                .map(|m| m.len() >= self.max_size)
                .unwrap_or(false)
            {
                for index in (1..self.max_files).rev() {
                    let old = self.path.with_extension(format!("log.{index}"));
                    let next = self.path.with_extension(format!("log.{}", index + 1));
                    match tokio::fs::rename(old, next).await {
                        Ok(()) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => return Err(e),
                    }
                }
                tokio::fs::rename(&self.path, self.path.with_extension("log.1")).await?;
            }
            let mut options = tokio::fs::OpenOptions::new();
            options.create(true).append(true);
            #[cfg(unix)]
            {
                options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
            }
            let mut file = options.open(&self.path).await?;
            file.write_all(bytes).await?;
            file.flush().await
        }
        .await;
        if let Err(e) = result {
            let _ = tokio::io::stderr()
                .write_all(format!("Cannot write {}: {e}\n", self.path.display()).as_bytes())
                .await;
        }
    }
}
fn parse_size(value: &str) -> Option<u64> {
    let value = value.trim();
    let digits = value
        .trim_end_matches(|c: char| c.is_ascii_alphabetic())
        .trim();
    let count = digits.parse::<u64>().ok()?;
    let suffix = value[digits.len()..].trim().to_ascii_lowercase();
    count.checked_mul(match suffix.as_str() {
        "b" => 1,
        "k" | "kb" => 1024,
        "" | "m" | "mb" => 1024 * 1024,
        "g" | "gb" => 1024 * 1024 * 1024,
        _ => return None,
    })
}
async fn drain<R: AsyncRead + Unpin>(
    mut source: R,
    log: Arc<DurableLog>,
    events: Option<(mpsc::Sender<Value>, String)>,
    stderr: bool,
) {
    let mut buffer = [0u8; 8192];
    let mut pending = Vec::new();
    loop {
        let size = match source.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(size) => size,
        };
        pending.extend_from_slice(&buffer[..size]);
        while let Some(end) = pending.iter().position(|b| *b == b'\n') {
            let line = &pending[..=end];
            let mut lifecycle = false;
            if let Some((sender, token)) = &events {
                if let Ok(value) = serde_json::from_slice::<Value>(line) {
                    if value["type"] == "paseo:ready"
                        && value["token"].as_str() == Some(token.as_str())
                    {
                        lifecycle = true;
                        let _ = sender.send(value).await;
                    }
                }
            }
            if !lifecycle {
                log.write(line).await;
                if stderr {
                    let _ = tokio::io::stderr().write_all(line).await;
                } else {
                    let _ = tokio::io::stdout().write_all(line).await;
                }
            }
            pending.drain(..=end);
        }
        if pending.len() > 64 * 1024 {
            log.write(&pending).await;
            if stderr {
                let _ = tokio::io::stderr().write_all(&pending).await;
            } else {
                let _ = tokio::io::stdout().write_all(&pending).await;
            }
            pending.clear();
        }
    }
    if !pending.is_empty() {
        log.write(&pending).await;
        if stderr {
            let _ = tokio::io::stderr().write_all(&pending).await;
        } else {
            let _ = tokio::io::stdout().write_all(&pending).await;
        }
    }
}

async fn run_supervisor(home: &Path, options: Value) -> Result<i32> {
    let owned_home = home.to_owned();
    let desktop = std::env::var("PASEO_DESKTOP_MANAGED").ok().as_deref() == Some("1");
    let mut lock =
        tokio::task::spawn_blocking(move || lock::PidLock::acquire(&owned_home, desktop)).await??;
    let executable = std::env::current_exe()?.canonicalize()?;
    #[cfg(windows)]
    crate::daemon_admin::updater::cleanup_update_helper(home).await?;
    let log = Arc::new(DurableLog::new(home));
    let stop = CancellationToken::new();
    let signal_stop = stop.clone();
    #[cfg(unix)]
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    #[cfg(unix)]
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let signals = tokio::spawn(async move {
        #[cfg(unix)]
        {
            tokio::select! {_=term.recv()=>{},_=interrupt.recv()=>{}}
        }
        #[cfg(windows)]
        {
            if let Err(error) = tokio::signal::ctrl_c().await {
                tracing::debug!(%error,"Detached supervisor has no console shutdown channel; using lifecycle RPC");
                std::future::pending::<()>().await;
            }
        }
        signal_stop.cancel();
    });
    let result=async {
        loop{
            if stop.is_cancelled(){return Ok(0)}
            #[cfg(windows)]
            if crate::daemon_admin::updater::launch_deferred_update(home,&executable,&options,&lock.instance).await?{return Ok(0)}
            let token=uuid::Uuid::new_v4().to_string();
            let mut command=Command::new(&executable);
            command.arg("__worker").arg("--home").arg(home).env("PASEO_NATIVE_OPTIONS",serde_json::to_string(&options)?).env("PASEO_NATIVE_SUPERVISOR",serde_json::to_string(&lock.instance)?).env("PASEO_NATIVE_TOKEN",&token).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
            // Updates may rename/remove the installation root; never inherit its
            // old directory handle into the replacement worker.
            command.current_dir(home);
            #[cfg(windows)] command.creation_flags(0x08000000);
            let mut child=match command.spawn(){
                Ok(child)=>child,
                Err(error)=>{
                    #[cfg(windows)]
                    if crate::daemon_admin::updater::launch_recovery_helper(home,&executable,&options,&lock.instance).await?{return Ok(0)}
                    if crate::daemon_admin::updater::finish_restart(home,&executable,None).await?{continue}
                    return Err(error).context("Failed to launch native daemon worker")
                }
            };
            #[cfg(windows)] let _job=super::windows::WorkerJob::assign(child.id().context("Worker has no PID")?)?;
            let mut worker=lock.instance.clone();worker.pid=child.id().context("Worker has no PID")?;worker.process_start_identity=identity::start_identity(worker.pid)?;
            let mut input=child.stdin.take().context("Missing worker control channel")?;
            let (send,mut events)=mpsc::channel(4);
            let output=tokio::spawn(drain(child.stdout.take().context("Missing worker stdout")?,log.clone(),Some((send,token.clone())),false));
            let errors=tokio::spawn(drain(child.stderr.take().context("Missing worker stderr")?,log.clone(),None,true));
            let mut heartbeat=tokio::time::interval(Duration::from_secs(1));let mut last_refresh=tokio::time::Instant::now();let mut ready=false;let mut stopping=false;let mut deadline=None;let mut events_open=true;let mut control_open=true;
            let status=loop{
                tokio::select!{
                    status=child.wait()=>break status?,
                    event=events.recv(),if !ready&&events_open=>{
                        if let Some(event)=event{
                            let listen=event["listen"].as_str().filter(|s|!s.is_empty()).context("Invalid readiness listen")?;
                            let server_id=event["serverId"].as_str().filter(|s|!s.is_empty()).context("Invalid readiness server identity")?;
                            if let Err(error)=crate::daemon_admin::updater::finish_restart(home,&executable,Some((event["version"].as_str().unwrap_or(""),event["sourceSha"].as_str().unwrap_or("")))).await{
                                tracing::error!(%error,"Updated worker readiness rejected");
                                force_worker(&mut child,&worker).await?;
                                break child.wait().await?;
                            }
                            lock.publish(Some(listen.into()),Some(server_id.into()))?;
                            ready=true;
                        }else{events_open=false;}
                    },
                    _=stop.cancelled(),if !stopping=>{stopping=true;deadline=Some(tokio::time::Instant::now()+Duration::from_secs(10));let bytes=format!("{}\n",json!({"type":"paseo:graceful-shutdown","token":token}));let _=input.write_all(bytes.as_bytes()).await;},
                    _=heartbeat.tick()=>{
                        if deadline.is_some_and(|deadline|tokio::time::Instant::now()>=deadline){force_worker(&mut child,&worker).await?;}
                        if last_refresh.elapsed()>=Duration::from_secs(30){if let Err(error)=lock.refresh(){tracing::error!(%error,"Supervisor PID lock ownership lost");stop.cancel();}last_refresh=tokio::time::Instant::now();}
                        if !stopping&&control_open{
                            let bytes=format!("{}\n",json!({"type":"paseo:supervisor-heartbeat","token":token}));
                            if let Err(error)=input.write_all(bytes.as_bytes()).await{tracing::debug!(%error,"Worker control channel closed");control_open=false;}
                        }
                    }
                }
            };
            lock.publish(None,None)?;
            drop(input);
            #[cfg(windows)] drop(_job);
            for mut task in [output,errors]{if tokio::time::timeout(Duration::from_secs(2),&mut task).await.is_err(){task.abort();}}
            log.write(format!("{}\n",json!({"level":"info","time":chrono::Utc::now().to_rfc3339(),"pid":std::process::id(),"name":"DaemonRunner","msg":"Worker exited","code":status.code()})).as_bytes()).await;
            if stopping||stop.is_cancelled(){return Ok(0)}
            if !ready{
                #[cfg(windows)]
                if crate::daemon_admin::updater::launch_recovery_helper(home,&executable,&options,&lock.instance).await?{return Ok(0)}
                if crate::daemon_admin::updater::finish_restart(home,&executable,None).await?{continue}
                return Ok(1)
            }
            if status.code()==Some(75){continue}
            if status.success(){return Ok(0)}
            #[cfg(unix)] {use std::os::unix::process::ExitStatusExt;if status.signal()==Some(libc::SIGTERM){return Ok(1)}}
            // Source supervisor restarts a crashed worker only after it reached readiness.
        }
    }.await;
    signals.abort();
    result
}

async fn force_worker(child: &mut tokio::process::Child, worker: &DaemonInstance) -> Result<()> {
    let owner = worker.clone();
    let signaled = tokio::task::spawn_blocking(move || {
        if identity::verified(&owner)? {
            identity::signal_tree(&owner, true)?;
            return Ok::<_, anyhow::Error>(true);
        }
        Ok(false)
    })
    .await??;
    if !signaled && child.try_wait()?.is_none() {
        child.start_kill()?;
    }
    Ok(())
}
