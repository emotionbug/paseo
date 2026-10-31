//! Owned subprocesses. No command string is evaluated by a shell; callers that
//! need shell syntax must explicitly select the shell and pass its arguments.
//! Windows command scripts use cmd with explicit literal-argument escaping.

use std::{
    collections::BTreeMap,
    fmt, io,
    path::PathBuf,
    process::ExitStatus,
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use tokio::{
    io::{AsyncReadExt, AsyncWrite, AsyncWriteExt},
    process::{ChildStderr, ChildStdin, ChildStdout, Command},
    sync::{Mutex, MutexGuard, watch},
    time::Instant,
};
use tokio_util::{sync::CancellationToken, task::AbortOnDropHandle};

#[cfg(unix)]
#[path = "process/unix.rs"]
mod platform;
#[cfg(windows)]
#[path = "process/windows.rs"]
mod platform;
mod tail;

use tail::Tail;

const STDERR_LIMIT: usize = 64 * 1024;
const PIPE_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
const FORCE_WAIT_TIMEOUT: Duration = Duration::from_secs(5);
const FAILURE_GRACE: Duration = Duration::from_millis(200);

/// An executable and literal argument vector, not an unparsed command line.
/// Environment and arguments deliberately have no Debug implementation.
#[derive(Clone)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
    pub clear_env: bool,
}

pub struct CommandOutput {
    pub stdout: Vec<u8>,
    /// Last 64 KiB of stderr, decoded lossily as UTF-8. A marker precedes a
    /// truncated tail; stderr is always drained even after reaching the bound.
    pub stderr: String,
    /// Exit code, or 128 + terminating signal on Unix.
    pub status: i32,
}

/// A failed stdin write. Accepted bytes are not proof of provider admission:
/// once I/O was attempted the caller must not automatically replay a mutation.
#[derive(Debug)]
pub struct WriteError {
    pub attempted: bool,
    /// Bytes accepted by AsyncWrite. On Windows this may include buffered
    /// bytes if the final OS-write flush fails; it never proves admission.
    pub written: usize,
    pub total: usize,
    pub source: io::Error,
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.attempted {
            write!(
                f,
                "subprocess stdin write failed after I/O was attempted ({}/{} bytes written); command admission is ambiguous: {}",
                self.written, self.total, self.source
            )
        } else {
            write!(
                f,
                "subprocess stdin write rejected before I/O (0/{} bytes written): {}",
                self.total, self.source
            )
        }
    }
}

impl std::error::Error for WriteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

#[derive(Clone, Copy)]
struct StopRequest {
    interrupt: bool,
    terminate_at: Instant,
    force_at: Instant,
}

#[derive(Clone)]
struct Control {
    deadline: watch::Sender<Option<StopRequest>>,
    stopping: CancellationToken,
}

impl Control {
    fn request_at(&self, deadline: Instant) {
        self.request(StopRequest {
            interrupt: false,
            terminate_at: Instant::now(),
            force_at: deadline,
        });
    }

    fn request(&self, request: StopRequest) {
        self.deadline.send_modify(|current| {
            if let Some(current) = current {
                current.interrupt |= request.interrupt;
                current.terminate_at = current.terminate_at.min(request.terminate_at);
                current.force_at = current.force_at.min(request.force_at);
            } else {
                *current = Some(request);
            }
        });
        self.stopping.cancel();
    }

    fn force(&self) {
        self.request_at(Instant::now());
    }
}

#[derive(Clone)]
enum Completion {
    Exited(ExitStatus),
    Failed(Arc<str>),
}

/// The supervisor exclusively owns Child. Waiting never locks the child or
/// blocks termination. Dropping the last owner requests immediate tree cleanup.
/// Stdout belongs exclusively to the returned reader and must be drained by the
/// caller. Neither the supervisor nor wait() consumes protocol output.
pub struct ManagedProcess {
    id: u32,
    stdin: Mutex<Option<ChildStdin>>,
    control: Control,
    completion: watch::Receiver<Option<Completion>>,
    stderr: Arc<StdMutex<Tail>>,
}

impl ManagedProcess {
    pub async fn spawn(spec: CommandSpec) -> Result<(Arc<Self>, ChildStdout)> {
        let (process, stdout, _) = Self::spawn_inner(spec, false)?;
        Ok((process, stdout))
    }

    /// Like spawn(), but the caller owns and must drain stderr as well as
    /// stdout. stderr_tail() is empty for this mode. Used by reverse terminals,
    /// whose wire output includes both streams rather than diagnostic stderr.
    pub async fn spawn_with_stderr(
        spec: CommandSpec,
    ) -> Result<(Arc<Self>, ChildStdout, ChildStderr)> {
        let (process, stdout, stderr) = Self::spawn_inner(spec, true)?;
        Ok((process, stdout, stderr.expect("caller-owned stderr")))
    }

    fn spawn_inner(
        spec: CommandSpec,
        caller_stderr: bool,
    ) -> Result<(Arc<Self>, ChildStdout, Option<ChildStderr>)> {
        // Require a runtime before creating a child and registering its owner.
        // As with Tokio process I/O, that runtime must have I/O and time enabled.
        let runtime = tokio::runtime::Handle::try_current()
            .context("subprocess spawning requires a Tokio runtime")?;
        if spec.program.is_empty() {
            bail!("subprocess executable is empty");
        }
        let command = platform::command(&spec)?;
        let mut owner = platform::Owner::spawn(command).context("failed to spawn subprocess")?;
        let id = owner.id();
        let stdin = owner
            .child
            .stdin
            .take()
            .context("subprocess stdin pipe missing")?;
        let stdout = owner
            .child
            .stdout
            .take()
            .context("subprocess stdout pipe missing")?;
        let stderr = owner
            .child
            .stderr
            .take()
            .context("subprocess stderr pipe missing")?;
        let (deadline, requests) = watch::channel(None);
        let control = Control {
            deadline,
            stopping: CancellationToken::new(),
        };
        let (finished, completion) = watch::channel(None);
        let tail = Arc::new(StdMutex::new(Tail::new(STDERR_LIMIT)));
        let (reader, caller_pipe) = if caller_stderr {
            (None, Some(stderr))
        } else {
            (Some(stderr), None)
        };
        let process = Arc::new(Self {
            id,
            stdin: Mutex::new(Some(stdin)),
            control: control.clone(),
            completion,
            stderr: tail.clone(),
        });
        runtime.spawn(supervise(owner, requests, control, reader, tail, finished));
        Ok((process, stdout, caller_pipe))
    }

    pub fn id(&self) -> u32 {
        self.id
    }

    /// Serialize exactly one JSON value followed by LF. The write lock covers
    /// the whole frame, so concurrent provider requests cannot interleave.
    pub async fn write_json(&self, value: &serde_json::Value) -> Result<()> {
        let mut frame = serde_json::to_vec(value).context("serializing subprocess JSON frame")?;
        frame.push(b'\n');
        self.write_bytes(&frame).await
    }

    /// Writes all bytes asynchronously. If this future is dropped during I/O,
    /// stdin is closed and the tree is terminated: a partial protocol frame may
    /// not be followed by another request. Cancellation while waiting for the
    /// write lock does not alter the process because no I/O has been attempted.
    pub async fn write_bytes(&self, bytes: &[u8]) -> Result<()> {
        let mut stdin = tokio::select! {
            biased;
            _ = self.control.stopping.cancelled() => return Err(not_written(bytes.len())),
            stdin = self.stdin.lock() => stdin,
        };
        if stdin.is_none() || self.control.stopping.is_cancelled() {
            return Err(not_written(bytes.len()));
        }
        if bytes.is_empty() {
            return Ok(());
        }
        let mut attempt = WriteAttempt {
            stdin: &mut stdin,
            control: &self.control,
            active: true,
            io_attempted: false,
        };
        let mut written = 0;
        while written < bytes.len() {
            let result = tokio::select! {
                biased;
                _ = self.control.stopping.cancelled() => {
                    Err(io::Error::new(io::ErrorKind::Interrupted, "subprocess is terminating"))
                }
                result = std::future::poll_fn(|cx| {
                    attempt.io_attempted = true;
                    std::pin::Pin::new(attempt.stdin.as_mut().expect("open stdin")).poll_write(cx, &bytes[written..])
                }) => result,
            };
            match result {
                Ok(0) => {
                    return Err(WriteError {
                        attempted: attempt.io_attempted,
                        written,
                        total: bytes.len(),
                        source: io::Error::new(
                            io::ErrorKind::WriteZero,
                            "subprocess stdin accepted zero bytes",
                        ),
                    }
                    .into());
                }
                Ok(count) => written += count,
                Err(error)
                    if error.kind() == io::ErrorKind::Interrupted
                        && !self.control.stopping.is_cancelled() =>
                {
                    continue;
                }
                Err(source) => {
                    return Err(WriteError {
                        attempted: attempt.io_attempted,
                        written,
                        total: bytes.len(),
                        source,
                    }
                    .into());
                }
            }
        }
        // Tokio's Windows pipe adapter queues blocking writes. Flush must finish
        // before success, otherwise a later EPIPE could be reported by a
        // different protocol request after this one was incorrectly acknowledged.
        let flushed = tokio::select! {
            biased;
            _ = self.control.stopping.cancelled() => Err(io::Error::new(io::ErrorKind::Interrupted, "subprocess is terminating")),
            result = attempt.stdin.as_mut().expect("open stdin").flush() => result,
        };
        if let Err(source) = flushed {
            return Err(WriteError {
                attempted: true,
                written,
                total: bytes.len(),
                source,
            }
            .into());
        }
        attempt.active = false;
        Ok(())
    }

    /// Close after all preceding writes, delivering EOF. Every successful write
    /// has already flushed the platform adapter before releasing the lock.
    pub async fn close_stdin(&self) -> Result<()> {
        self.stdin.lock().await.take();
        Ok(())
    }

    pub async fn wait(&self) -> Result<i32> {
        self.wait_status().await.map(status_code)
    }

    /// Retains the distinction between exit(143) and SIGTERM for ACP terminals.
    /// Safe to call concurrently, repeatedly, or concurrently with terminate().
    pub async fn wait_status(&self) -> Result<ExitStatus> {
        let mut completion = self.completion.clone();
        loop {
            let observed = completion.borrow_and_update().clone();
            match observed {
                Some(Completion::Exited(status)) => return Ok(status),
                Some(Completion::Failed(message)) => return Err(anyhow!(message.to_string())),
                None => {}
            }
            completion
                .changed()
                .await
                .context("subprocess supervisor stopped before recording exit")?;
        }
    }

    /// Stop the owned tree, escalating after grace. Multiple requests share the
    /// earliest deadline. Cancelling this future does not cancel the cleanup.
    /// Windows has no SIGTERM for arbitrary background programs: EOF is offered
    /// for the grace interval, then the owned Job is terminated atomically.
    pub async fn terminate(&self, grace: Duration) -> Result<()> {
        let now = Instant::now();
        let force_at = now
            .checked_add(grace)
            .context("termination grace exceeds clock range")?;
        let finish_by = force_at
            .checked_add(FORCE_WAIT_TIMEOUT)
            .context("termination timeout exceeds clock range")?;
        self.control.request_at(force_at);
        // Never wait for a writer lock before signalling the supervisor.
        // Active writes observe stopping and release/close their pipe.
        if let Ok(mut stdin) = self.stdin.try_lock() {
            stdin.take();
        }
        tokio::time::timeout_at(finish_by, self.wait_status()).await
            .context("subprocess did not report exit after forced tree termination; cleanup remains active")??;
        Ok(())
    }

    /// Interrupt a process that uses OS signals rather than protocol cancellation.
    /// Unix sends SIGINT, then SIGTERM after grace, then SIGKILL after another
    /// grace. Stdin remains open unless an in-flight write must be cancelled.
    /// Windows immediately terminates the owned Job, matching native tree-stop
    /// semantics without ever broadcasting a console event to unrelated jobs.
    pub async fn interrupt(&self, grace: Duration) -> Result<()> {
        let terminate_at = Instant::now()
            .checked_add(grace)
            .context("interrupt grace exceeds clock range")?;
        let force_at = terminate_at
            .checked_add(grace)
            .context("interrupt escalation exceeds clock range")?;
        let finish_by = force_at
            .checked_add(FORCE_WAIT_TIMEOUT)
            .context("interrupt timeout exceeds clock range")?;
        self.control.request(StopRequest {
            interrupt: true,
            terminate_at,
            force_at,
        });
        tokio::time::timeout_at(finish_by, self.wait_status())
            .await
            .context(
                "subprocess did not report exit after interrupt escalation; cleanup remains active",
            )??;
        Ok(())
    }

    pub fn stderr_tail(&self) -> String {
        self.stderr
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .text()
    }
}

impl Drop for ManagedProcess {
    fn drop(&mut self) {
        self.control.force();
    }
}

struct WriteAttempt<'a, 'b> {
    stdin: &'a mut MutexGuard<'b, Option<ChildStdin>>,
    control: &'a Control,
    active: bool,
    io_attempted: bool,
}

impl Drop for WriteAttempt<'_, '_> {
    fn drop(&mut self) {
        if self.active && self.io_attempted {
            self.stdin.take();
            if !self.control.stopping.is_cancelled() {
                self.control.force();
            }
        }
    }
}

fn not_written(total: usize) -> anyhow::Error {
    WriteError {
        attempted: false,
        written: 0,
        total,
        source: io::Error::new(io::ErrorKind::BrokenPipe, "subprocess stdin is closed"),
    }
    .into()
}

fn configure(command: &mut Command, spec: &CommandSpec) {
    if let Some(cwd) = &spec.cwd {
        command.current_dir(cwd);
    }
    if spec.clear_env {
        command.env_clear();
    }
    command.envs(&spec.env);
    command.stdin(std::process::Stdio::piped());
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());
    command.kill_on_drop(true);
}

async fn supervise(
    mut owner: platform::Owner,
    mut requests: watch::Receiver<Option<StopRequest>>,
    control: Control,
    stderr: Option<ChildStderr>,
    tail: Arc<StdMutex<Tail>>,
    finished: watch::Sender<Option<Completion>>,
) {
    let mut stderr_reader =
        stderr.map(|stderr| AbortOnDropHandle::new(tokio::spawn(drain_stderr(stderr, tail))));
    let result = supervise_exit(&mut owner, &mut requests).await;
    control.stopping.cancel();
    // Owner drop synchronously kills any still-owned tree, including error and
    // runtime-shutdown paths. Do this before waiting for the diagnostic pipe.
    drop(owner);
    let drain_result = if let Some(reader) = stderr_reader.as_mut() {
        match tokio::time::timeout(PIPE_DRAIN_TIMEOUT, &mut *reader).await {
            Ok(result) => result
                .context("subprocess stderr reader failed")
                .and_then(|result| result.context("reading subprocess stderr")),
            Err(_) => {
                reader.abort();
                let _ = reader.await;
                Err(anyhow!(
                    "subprocess stderr pipe remained open after tree termination"
                ))
            }
        }
    } else {
        Ok(())
    };
    let outcome = match result.and_then(|status| drain_result.map(|()| status)) {
        Ok(status) => Completion::Exited(status),
        Err(error) => Completion::Failed(format!("{error:#}").into()),
    };
    finished.send_replace(Some(outcome));
}

async fn supervise_exit(
    owner: &mut platform::Owner,
    requests: &mut watch::Receiver<Option<StopRequest>>,
) -> Result<ExitStatus> {
    let mut interrupt_sent = false;
    let mut graceful_sent = false;
    let mut forced = false;
    loop {
        let request = *requests.borrow_and_update();
        let mut deadline = None;
        if let Some(request) = request {
            if request.interrupt && !interrupt_sent && !graceful_sent {
                owner.interrupt().context("interrupting subprocess tree")?;
                interrupt_sent = true;
            }
            if !graceful_sent {
                if request.terminate_at <= Instant::now() {
                    owner
                        .terminate(false)
                        .context("signalling subprocess tree")?;
                    graceful_sent = true;
                } else {
                    deadline = Some(request.terminate_at);
                }
            }
            if !forced {
                if request.force_at <= Instant::now() {
                    owner
                        .terminate(true)
                        .context("force-killing subprocess tree")?;
                    forced = true;
                } else {
                    deadline =
                        Some(deadline.map_or(request.force_at, |old| old.min(request.force_at)));
                }
            }
        }
        tokio::select! {
            result = owner.wait() => return result.context("waiting for subprocess exit"),
            changed = requests.changed() => {
                if changed.is_err() {
                    owner.terminate(true).context("force-killing unowned subprocess")?;
                    return owner.wait().await.context("waiting for unowned subprocess exit");
                }
            }
            _ = async {
                match deadline {
                    Some(deadline) => tokio::time::sleep_until(deadline).await,
                    None => std::future::pending::<()>().await,
                }
            } => {}
        }
    }
}

async fn drain_stderr(mut stderr: ChildStderr, tail: Arc<StdMutex<Tail>>) -> io::Result<()> {
    let mut buffer = [0; 8192];
    loop {
        let count = stderr.read(&mut buffer).await?;
        if count == 0 {
            return Ok(());
        }
        tail.lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .append(&buffer[..count]);
    }
}

fn status_code(status: ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        128 + status.signal().unwrap_or(0)
    }
    #[cfg(windows)]
    {
        // Windows ExitStatus always carries a numeric process exit code.
        unreachable!("Windows process exit without a status code")
    }
}

/// Run without stdin, collecting stdout up to output_limit bytes and the bounded
/// stderr tail. Nonzero exits are returned, not turned into invented failures.
/// Output overflow, I/O failure, timeout and cancellation terminate the owned
/// tree before returning. Dropping this future also requests forced cleanup.
pub async fn run(
    spec: CommandSpec,
    output_limit: usize,
    timeout: Duration,
    cancel: CancellationToken,
) -> Result<CommandOutput> {
    if cancel.is_cancelled() {
        bail!("subprocess command cancelled before spawn");
    }
    let deadline = Instant::now()
        .checked_add(timeout)
        .context("command timeout exceeds clock range")?;
    if deadline <= Instant::now() {
        bail!("subprocess command timed out before spawn");
    }
    let (process, mut stdout) = ManagedProcess::spawn(spec).await?;
    let operation = async {
        process.close_stdin().await?;
        let read = async {
            let mut output = Vec::new();
            let mut buffer = [0; 8192];
            loop {
                let count = stdout
                    .read(&mut buffer)
                    .await
                    .context("reading subprocess stdout")?;
                if count == 0 {
                    return Ok::<_, anyhow::Error>(output);
                }
                if count > output_limit.saturating_sub(output.len()) {
                    bail!("subprocess stdout exceeded output limit of {output_limit} bytes");
                }
                output.extend_from_slice(&buffer[..count]);
            }
        };
        let (stdout, status) = tokio::try_join!(read, process.wait())?;
        Ok(CommandOutput {
            stdout,
            stderr: process.stderr_tail(),
            status,
        })
    };
    let result = tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(anyhow!("subprocess command cancelled")),
        _ = tokio::time::sleep_until(deadline) => Err(anyhow!("subprocess command timed out")),
        result = operation => result,
    };
    if result.is_err() {
        if let Err(cleanup) = process.terminate(FAILURE_GRACE).await {
            return result.context(format!("subprocess cleanup failed: {cleanup:#}"));
        }
    }
    result
}

#[cfg(test)]
mod tests;
