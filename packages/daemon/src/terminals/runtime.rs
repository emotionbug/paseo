use super::screen::Screen;
use anyhow::{Result, anyhow, bail};
use bytes::Bytes;
use portable_pty::{CommandBuilder, MasterPty, PtySize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{broadcast, mpsc, oneshot, watch};

pub struct CreateTerminal {
    pub cwd: PathBuf,
    pub workspace_id: String,
    pub name: Option<String>,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub rows: u16,
    pub cols: u16,
}

#[derive(Clone, Debug)]
pub enum TerminalEvent {
    Output { revision: u64, data: Bytes },
    CommandFinished(Option<i32>),
    Exit(i32),
    Activity(Value),
    Error(String),
    Changed,
}

enum Command {
    Resize(u16, u16, Arc<Mutex<Display>>, oneshot::Sender<Result<()>>),
    Stop(oneshot::Sender<Result<()>>),
}
type Input = (Vec<u8>, oneshot::Sender<Result<()>>);
struct Display {
    screen: Screen,
    revision: u64,
    rows: u16,
    cols: u16,
    raw_tail: std::collections::VecDeque<u8>,
}
struct Metadata {
    name: String,
    title: Option<String>,
    custom_title: bool,
    activity: Value,
    exit_info: Option<Value>,
}

pub struct Terminal {
    pub id: String,
    pub workspace_id: String,
    pub cwd: PathBuf,
    pub(crate) activity_token: String,
    metadata: Mutex<Metadata>,
    display: Arc<Mutex<Display>>,
    commands: mpsc::Sender<Command>,
    input: mpsc::Sender<Input>,
    lifecycle: broadcast::Sender<TerminalEvent>,
    changed: broadcast::Sender<()>,
    events: broadcast::Sender<TerminalEvent>,
    exit: watch::Receiver<Option<std::result::Result<i32, String>>>,
    size_owner: Mutex<Option<String>>,
    // Assets remain alive until the PTY owner has stopped, not until a stream detaches.
    _shell_assets: Option<Arc<tempfile::TempDir>>,
}

impl Terminal {
    pub(crate) async fn spawn(
        mut options: CreateTerminal,
        activity_token: String,
        changed: broadcast::Sender<()>,
    ) -> Result<Arc<Self>> {
        if options.rows == 0 || options.cols == 0 {
            bail!("Terminal dimensions must be positive");
        }
        if !options.cwd.is_absolute() {
            bail!("Terminal cwd must be absolute");
        }
        let id = uuid::Uuid::new_v4().to_string();
        options
            .env
            .insert("PASEO_WORKSPACE_ID".into(), options.workspace_id.clone());
        options.env.insert("PASEO_TERMINAL_ID".into(), id.clone());
        options
            .env
            .insert("PASEO_ACTIVITY_TOKEN".into(), activity_token.clone());
        options.env.insert("TERM".into(), "xterm-256color".into());
        options.env.insert("TERM_PROGRAM".into(), "kitty".into());
        let shell = options
            .command
            .clone()
            .unwrap_or_else(|| default_shell(&options.env));
        let assets = super::shell::prepare(&shell, &options.args, &mut options.env)?;
        let assets = assets.map(Arc::new);
        let mut command = super::shell::command(
            &shell,
            &options.args,
            &options.env,
            assets.as_ref().map(|assets| assets.path()),
        )?;
        command.cwd(&options.cwd);
        command.env_clear();
        for (key, value) in &options.env {
            command.env(key, value);
        }
        let rows = options.rows;
        let cols = options.cols;
        let (commands, receiver) = mpsc::channel(64);
        let (events, _) = broadcast::channel(512);
        let (lifecycle, _) = broadcast::channel(512);
        let (input, input_receiver) = mpsc::channel(64);
        let io_finished = Arc::new(AtomicBool::new(false));
        let (exit_sender, exit) = watch::channel(None);
        let display = Arc::new(Mutex::new(Display {
            screen: Screen::new(rows, cols),
            revision: 0,
            rows,
            cols,
            raw_tail: std::collections::VecDeque::new(),
        }));
        let terminal = Arc::new(Self {
            id,
            workspace_id: options.workspace_id,
            cwd: options.cwd,
            activity_token,
            metadata: Mutex::new(Metadata {
                name: options.name.unwrap_or_else(|| "Terminal".into()),
                title: None,
                custom_title: false,
                activity: Value::Null,
                exit_info: None,
            }),
            display,
            commands,
            input: input.clone(),
            lifecycle,
            changed: changed.clone(),
            events,
            exit,
            size_owner: Mutex::new(None),
            _shell_assets: assets.clone(),
        });
        let weak = Arc::downgrade(&terminal);
        let (started_sender, started) = oneshot::channel();
        let runtime = tokio::runtime::Handle::current();
        std::thread::Builder::new().name("paseo-pty-owner".into()).spawn(move || {
            let _entered=runtime.enter();
            let result = Owner::spawn(command,rows,cols,receiver,assets);
            let mut owner = match result { Ok(owner)=>{ let _ = started_sender.send(Ok(())); owner }, Err(error)=>{let _=started_sender.send(Err(error)); return;} };
            let reader = owner.reader.take().expect("PTY reader owned once");
            let reader_terminal = weak.clone();
            let reader_changed = changed.clone();
            let (read_done_tx,read_done_rx) = std::sync::mpsc::channel();
            let writer=owner.writer.take().expect("PTY writer owned once");
            let writer_finished=io_finished.clone();
            let writer_ready=owner.readiness.clone();
            let writer_thread=std::thread::spawn(move || write_inputs(writer,input_receiver,writer_finished,writer_ready));
            let input_shutdown=input.clone();
            let reader_finished=io_finished.clone();
            let reader_ready=owner.readiness.clone();
            std::thread::spawn(move || {
                let mut reader = reader; let mut buffer = [0u8;16384];
                loop {
                    let count = match reader.read(&mut buffer) { Ok(0)=>break, Ok(n)=>n, Err(e) if e.kind()==std::io::ErrorKind::Interrupted=>continue, Err(e) if e.kind()==std::io::ErrorKind::WouldBlock=>{if reader_finished.load(Ordering::Acquire){break;}if reader_ready.wait(false).is_err(){break;}continue;},Err(_)=>break };
                    let Some(terminal)=reader_terminal.upgrade() else { break };
                    let mut display = terminal.display.lock().unwrap();
                    let effects = display.screen.process(&buffer[..count]);
                    display.raw_tail.extend(&buffer[..count]);let excess=display.raw_tail.len().saturating_sub(64000);display.raw_tail.drain(..excess);
                    display.revision = display.revision.wrapping_add(1);
                    let revision=display.revision;
                    let _ = terminal.events.send(TerminalEvent::Output { revision, data:Bytes::copy_from_slice(&buffer[..count]) });
                    drop(display);
                    if !effects.replies.is_empty() && !reader_finished.load(Ordering::Acquire) {let(tx,_)=oneshot::channel();let _=input.blocking_send((effects.replies,tx));}
                    for code in effects.command_finished {let event=TerminalEvent::CommandFinished(code);let _=terminal.lifecycle.send(event.clone());let _=terminal.events.send(event);}
                    if let Some(title)=effects.title {let mut metadata=terminal.metadata.lock().unwrap();if !metadata.custom_title && metadata.title.as_ref()!=Some(&title){metadata.title=if title.is_empty(){None}else{Some(title)};let _=terminal.events.send(TerminalEvent::Changed);let _=terminal.lifecycle.send(TerminalEvent::Changed);let _=reader_changed.send(());}}
                }
                let _=read_done_tx.send(());
            });
            let status=runtime.block_on(owner.run()).map_err(|error|error.to_string());
            let signal=owner.exit_signal;
            // All terminal bytes already in the kernel are parsed before publishing exit.
            drop(owner);
            io_finished.store(true,Ordering::Release);
            let(tx,_)=oneshot::channel();let _=input_shutdown.blocking_send((Vec::new(),tx));
            let _=writer_thread.join();
            let _=read_done_rx.recv();
            if let Some(terminal)=weak.upgrade() {let mut display=terminal.display.lock().unwrap();let mut lines=display.screen.exit_lines(12);if lines.is_empty(){lines=raw_exit_lines(display.raw_tail.make_contiguous());}drop(display);terminal.metadata.lock().unwrap().exit_info=Some(json!({"exitCode":status.as_ref().ok(),"signal":signal,"lastOutputLines":lines}));let event=match &status{Ok(code)=>TerminalEvent::Exit(*code),Err(error)=>TerminalEvent::Error(error.clone())};let _=terminal.lifecycle.send(event.clone());let _=terminal.events.send(event);}
            let _=exit_sender.send(Some(status)); let _=changed.send(());
        })?;
        started
            .await
            .map_err(|_| anyhow!("PTY startup owner closed"))??;
        Ok(terminal)
    }
    pub fn subscribe_lifecycle(&self) -> broadcast::Receiver<TerminalEvent> {
        self.lifecycle.subscribe()
    }
    pub fn subscribe(&self) -> broadcast::Receiver<TerminalEvent> {
        self.events.subscribe()
    }
    pub fn info(&self) -> Value {
        let m = self.metadata.lock().unwrap();
        let mut value = json!({"id":self.id,"workspaceId":self.workspace_id,"cwd":self.cwd,"name":m.name,"activity":m.activity});
        if let Some(title) = &m.title {
            value["title"] = json!(title);
        }
        value
    }
    pub fn is_exited(&self) -> bool {
        self.exit.borrow().is_some()
    }
    pub fn exit_info(&self) -> Option<Value> {
        self.metadata.lock().unwrap().exit_info.clone()
    }
    pub fn set_title(&self, title: &str) {
        let mut m = self.metadata.lock().unwrap();
        m.title = Some(title.into());
        m.custom_title = true;
        let _ = self.events.send(TerminalEvent::Changed);
        let _ = self.lifecycle.send(TerminalEvent::Changed);
        let _ = self.changed.send(());
    }
    pub fn set_activity(&self, state: &str) -> bool {
        let mut m = self.metadata.lock().unwrap();
        let previous = m.activity.clone();
        let old_state = previous.get("state").and_then(Value::as_str);
        let old_reason = previous.get("attentionReason").and_then(Value::as_str);
        if state == "idle" && old_reason == Some("finished") {
            return false;
        }
        let (state, reason) = match state {
            "idle" if old_state == Some("working") => (Some("idle"), Some("finished")),
            "attention" => (Some("idle"), Some("needs_input")),
            "working" => (Some("working"), None),
            "idle" => (Some("idle"), None),
            "clear_attention" => (Some("idle"), None),
            "interrupt" if old_state != Some("working") => return false,
            _ => (None, None),
        };
        if old_state == state && old_reason == reason {
            return false;
        }
        m.activity = state
            .map(|state| {
                let mut value =
                    json!({"state":state,"changedAt":chrono::Utc::now().timestamp_millis()});
                if let Some(reason) = reason {
                    value["attentionReason"] = json!(reason);
                }
                value
            })
            .unwrap_or(Value::Null);
        let event = TerminalEvent::Activity(json!({"activity":m.activity,"previous":previous}));
        let _ = self.lifecycle.send(event.clone());
        let _ = self.events.send(event);
        let _ = self.changed.send(());
        true
    }
    pub async fn input(&self, bytes: &[u8]) -> Result<()> {
        if bytes.contains(&3) {
            self.set_activity("interrupt");
        }
        for chunk in bytes.chunks(16 * 1024) {
            let (tx, rx) = oneshot::channel();
            self.input
                .send((chunk.to_vec(), tx))
                .await
                .map_err(|_| anyhow!("Terminal has exited"))?;
            rx.await.map_err(|_| anyhow!("Terminal has exited"))??;
        }
        Ok(())
    }
    pub async fn resize(&self, source: &str, rows: u16, cols: u16, claim: bool) -> Result<()> {
        if rows == 0 || cols == 0 {
            bail!("Terminal dimensions must be positive");
        }
        {
            let mut owner = self.size_owner.lock().unwrap();
            if !claim && owner.as_deref() != Some(source) {
                return Ok(());
            }
            if claim {
                *owner = Some(source.into());
            }
        }
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(Command::Resize(rows, cols, self.display.clone(), tx))
            .await
            .map_err(|_| anyhow!("Terminal has exited"))?;
        rx.await.map_err(|_| anyhow!("Terminal has exited"))?
    }
    pub fn release_size(&self, source: &str) {
        let mut owner = self.size_owner.lock().unwrap();
        if owner.as_deref() == Some(source) {
            *owner = None;
        }
    }
    pub async fn snapshot(&self, wrapped: bool) -> Result<Value> {
        Ok(self.display.lock().unwrap().screen.snapshot(wrapped))
    }
    pub(crate) fn restore_frame(
        &self,
        mode: Option<&str>,
        scrollback: usize,
        wrapped: bool,
    ) -> Result<(u64, u8, Vec<u8>)> {
        let display = self.display.lock().unwrap();
        let (opcode, data) = if let Some(mode) = mode {
            (5, display.screen.restore(mode, scrollback))
        } else {
            (4, serde_json::to_vec(&display.screen.snapshot(wrapped))?)
        };
        Ok((display.revision, opcode, data))
    }
    pub async fn capture(
        &self,
        start: Option<f64>,
        end: Option<f64>,
        strip: bool,
    ) -> Result<(Vec<String>, usize)> {
        Ok(self
            .display
            .lock()
            .unwrap()
            .screen
            .capture(start, end, strip))
    }
    pub async fn replay_preamble(&self) -> Result<String> {
        Ok(self.display.lock().unwrap().screen.replay_preamble())
    }
    pub async fn wait(&self) -> Result<i32> {
        let mut exit = self.exit.clone();
        loop {
            if let Some(result) = exit.borrow_and_update().clone() {
                return result.map_err(|error| anyhow!(error));
            }
            exit.changed()
                .await
                .map_err(|_| anyhow!("PTY owner lost exit status"))?;
        }
    }
    pub async fn terminate(&self) -> Result<()> {
        if self.is_exited() {
            self.wait().await?;
            return Ok(());
        }
        let (tx, rx) = oneshot::channel();
        if self.commands.send(Command::Stop(tx)).await.is_ok() {
            if let Ok(result) = rx.await {
                result?;
            }
        }
        self.wait().await?;
        Ok(())
    }
}

fn default_shell(env: &BTreeMap<String, String>) -> String {
    if cfg!(windows) {
        env.get("ComSpec")
            .or_else(|| env.get("COMSPEC"))
            .cloned()
            .unwrap_or_else(|| "C:\\Windows\\System32\\cmd.exe".into())
    } else {
        env.get("SHELL")
            .cloned()
            .unwrap_or_else(|| "/bin/sh".into())
    }
}

struct Owner {
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    reader: Option<Box<dyn Read + Send>>,
    writer: Option<Box<dyn Write + Send>>,
    commands: mpsc::Receiver<Command>,
    unreaped: bool,
    stopping: Option<Instant>,
    stop_waiters: Vec<oneshot::Sender<Result<()>>>,
    exit_signal: Option<i32>,
    _assets: Option<Arc<tempfile::TempDir>>,
    #[cfg(windows)]
    job: super::windows::PtyJob,
    #[cfg(windows)]
    _gate: super::windows::PtyGate,
    readiness: Arc<IoReadiness>,
    #[cfg(unix)]
    child_signal: tokio::signal::unix::Signal,
    #[cfg(windows)]
    child_exit: oneshot::Receiver<Result<()>>,
}
impl Owner {
    fn spawn(
        command: CommandBuilder,
        rows: u16,
        cols: u16,
        commands: mpsc::Receiver<Command>,
        assets: Option<Arc<tempfile::TempDir>>,
    ) -> Result<Self> {
        #[cfg(windows)]
        let (command, gate) = super::windows::prepare_gate(command)?;
        #[cfg(unix)]
        let child_signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::child())?;
        let pair = portable_pty::native_pty_system().openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let readiness = Arc::new(IoReadiness::new(&*pair.master)?);
        let reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        #[cfg(unix)]
        if let Some(fd) = pair.master.as_raw_fd() {
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
        }
        let child = pair.slave.spawn_command(command)?;
        drop(pair.slave);
        #[cfg(windows)]
        let mut child = child;
        #[cfg(windows)]
        let job = match super::windows::PtyJob::assign(child.as_raw_handle()) {
            Ok(job) => job,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        #[cfg(windows)]
        let child_exit = match super::windows::observe_exit(child.as_raw_handle()) {
            Ok(receiver) => receiver,
            Err(error) => {
                let _ = job.terminate();
                let _ = child.wait();
                return Err(error);
            }
        };
        #[cfg(windows)]
        if let Err(error) = gate.release() {
            let _ = job.terminate();
            let _ = child.wait();
            return Err(error);
        }
        Ok(Self {
            master: pair.master,
            child,
            reader: Some(reader),
            writer: Some(writer),
            commands,
            unreaped: true,
            stopping: None,
            stop_waiters: Vec::new(),
            exit_signal: None,
            _assets: assets,
            readiness,
            #[cfg(unix)]
            child_signal,
            #[cfg(windows)]
            child_exit,
            #[cfg(windows)]
            job,
            #[cfg(windows)]
            _gate: gate,
        })
    }
    fn signal(&mut self, force: bool) -> Result<()> {
        if !self.unreaped {
            return Ok(());
        }
        #[cfg(unix)]
        {
            let pid =
                self.child
                    .process_id()
                    .ok_or_else(|| anyhow!("PTY child has no PID"))? as libc::pid_t;
            let signal = if force { libc::SIGKILL } else { libc::SIGHUP };
            // The actor is the sole reaper. The leader PID remains reserved until
            // every signal is sent; Linux descendant signals use stable pidfds.
            #[cfg(target_os = "linux")]
            signal_session(pid, signal)?;
            if unsafe { libc::kill(-pid, signal) } != 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(error.into());
                }
            }
        }
        #[cfg(windows)]
        {
            self.job.terminate()?;
        }
        Ok(())
    }
    fn poll_exit(&mut self) -> Result<Option<i32>> {
        #[cfg(unix)]
        {
            let pid = self
                .child
                .process_id()
                .ok_or_else(|| anyhow!("PTY child has no PID"))?;
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            if unsafe {
                libc::waitid(
                    libc::P_PID,
                    pid,
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            } != 0
            {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ECHILD) {
                    self.unreaped = false;
                }
                return Err(error.into());
            }
            if unsafe { info.si_pid() } == 0 {
                return Ok(None);
            }
            let observed_status = unsafe { info.si_status() };
            let exited = info.si_code == libc::CLD_EXITED;
            self.exit_signal = if exited { None } else { Some(observed_status) };
            self.signal(true)?;
            let result = self.child.wait();
            self.unreaped = false;
            result?;
            return Ok(Some(if exited { observed_status } else { 0 }));
        }
        #[cfg(windows)]
        {
            if let Some(status) = self.child.try_wait()? {
                self.job.terminate()?;
                self.unreaped = false;
                return Ok(Some(status.exit_code() as i32));
            }
            Ok(None)
        }
    }
    async fn run(&mut self) -> Result<i32> {
        let mut commands_open = true;
        let mut forced = false;
        loop {
            if let Some(code) = self.poll_exit()? {
                for waiter in self.stop_waiters.drain(..) {
                    let _ = waiter.send(Ok(()));
                }
                return Ok(code);
            }
            let deadline = self
                .stopping
                .filter(|_| !forced)
                .map(|start| tokio::time::Instant::from_std(start + Duration::from_secs(2)));
            #[cfg(unix)]
            let child_changed = async {
                self.child_signal
                    .recv()
                    .await
                    .ok_or_else(|| anyhow!("PTY SIGCHLD observer closed"))
            };
            #[cfg(windows)]
            let child_changed = async {
                (&mut self.child_exit)
                    .await
                    .map_err(|_| anyhow!("ConPTY exit observer closed"))?
            };
            let command = tokio::select! {
                command=self.commands.recv(),if commands_open=>{if command.is_none(){commands_open=false;}command},
                result=child_changed=>{result?;continue;},
                _=async{if let Some(deadline)=deadline{tokio::time::sleep_until(deadline).await}else{std::future::pending::<()>().await}}=>{self.signal(true)?;forced=true;continue;}
            };
            match command {
                Some(Command::Resize(rows, cols, display, tx)) => {
                    let mut display = display.lock().unwrap();
                    let result = if display.rows == rows && display.cols == cols {
                        Ok(())
                    } else {
                        self.master.resize(PtySize {
                            rows,
                            cols,
                            pixel_width: 0,
                            pixel_height: 0,
                        })
                    };
                    if result.is_ok() && (display.rows != rows || display.cols != cols) {
                        display.screen.resize(rows, cols);
                        display.rows = rows;
                        display.cols = cols;
                        display.revision = display.revision.wrapping_add(1);
                    }
                    let _ = tx.send(result);
                }
                Some(Command::Stop(tx)) => {
                    self.stop_waiters.push(tx);
                    if self.stopping.is_none() {
                        self.signal(false)?;
                        self.stopping = Some(Instant::now());
                    }
                }
                None => {
                    if self.stopping.is_none() {
                        self.signal(false)?;
                        self.stopping = Some(Instant::now());
                    }
                }
            }
        }
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        if self.unreaped {
            let _ = self.signal(true);
            let _ = self.child.wait();
            self.unreaped = false;
        }
    }
}

#[cfg(target_os = "linux")]
fn signal_session(session: libc::pid_t, signal: libc::c_int) -> Result<()> {
    use std::os::fd::{AsRawFd, FromRawFd};
    for entry in std::fs::read_dir("/proc")? {
        let entry = entry?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<libc::pid_t>().ok())
        else {
            continue;
        };
        if pid == session || unsafe { libc::getsid(pid) } != session {
            continue;
        }
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) } as i32;
        if fd < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                continue;
            }
            return Err(error.into());
        }
        let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };
        // If the PID was recycled around pidfd_open, either this recheck rejects
        // it or the fd refers to a dead task. It can never signal a replacement.
        if unsafe { libc::getsid(pid) } != session {
            continue;
        }
        if unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                fd.as_raw_fd(),
                signal,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        } < 0
        {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error.into());
            }
        }
    }
    Ok(())
}

fn write_inputs(
    mut writer: Box<dyn Write + Send>,
    mut input: mpsc::Receiver<Input>,
    finished: Arc<AtomicBool>,
    readiness: Arc<IoReadiness>,
) {
    while !finished.load(Ordering::Acquire) {
        let Some((data, reply)) = input.blocking_recv() else {
            break;
        };
        let result = (|| -> Result<()> {
            let mut offset = 0;
            while offset < data.len() {
                if finished.load(Ordering::Acquire) {
                    bail!("Terminal has exited");
                }
                match writer.write(&data[offset..]) {
                    Ok(0) => bail!("PTY input closed"),
                    Ok(count) => offset += count,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        readiness.wait(true)?
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            writer.flush()?;
            Ok(())
        })();
        let _ = reply.send(result);
    }
}

fn raw_exit_lines(bytes: &[u8]) -> Vec<String> {
    static ANSI: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new("\\x1b(?:[@-Z\\\\-_]|\\[[0-?]*[ -/]*[@-~]|\\].*?(?:\\x07|\\x1b\\\\))")
            .expect("source ANSI matcher")
    });
    let text = String::from_utf8_lossy(bytes);
    let utf16: Vec<u16> = text.encode_utf16().collect();
    let tail = String::from_utf16_lossy(&utf16[utf16.len().saturating_sub(16000)..]);
    let stripped = ANSI
        .replace_all(&tail, "")
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let mut lines: Vec<_> = stripped
        .split('\n')
        .map(|line| line.trim_end().to_owned())
        .collect();
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }
    let first = lines
        .iter()
        .position(|line| !line.trim().is_empty())
        .unwrap_or(lines.len());
    let start = first.max(lines.len().saturating_sub(12));
    lines.drain(..start);
    lines
}

struct IoReadiness {
    #[cfg(unix)]
    fd: std::os::fd::OwnedFd,
}
impl IoReadiness {
    fn new(master: &dyn MasterPty) -> Result<Self> {
        #[cfg(unix)]
        {
            use std::os::fd::FromRawFd;
            let fd = master
                .as_raw_fd()
                .ok_or_else(|| anyhow!("PTY has no native descriptor"))?;
            let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
            if duplicate < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            Ok(Self {
                fd: unsafe { std::os::fd::OwnedFd::from_raw_fd(duplicate) },
            })
        }
        #[cfg(windows)]
        {
            let _ = master;
            Ok(Self {})
        }
    }
    fn wait(&self, write: bool) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let mut poll = libc::pollfd {
                fd: self.fd.as_raw_fd(),
                events: if write { libc::POLLOUT } else { libc::POLLIN },
                revents: 0,
            };
            if unsafe { libc::poll(&mut poll, 1, 100) } < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::Interrupted {
                    return Err(error);
                }
            }
        }
        #[cfg(windows)]
        {
            let _ = write;
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(())
    }
}
