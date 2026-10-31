use std::{
    ffi::OsString,
    io,
    os::windows::{
        ffi::OsStringExt,
        io::{AsHandle, AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::{Path, PathBuf},
    process::ExitStatus,
};

use tokio::process::{ChildStderr, ChildStdin, ChildStdout, Command};
use windows_sys::Win32::{
    Foundation::{HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0},
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
        },
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject, TerminateJobObject,
        },
        SystemInformation::GetSystemDirectoryW,
        Threading::{
            CREATE_NO_WINDOW, CREATE_SUSPENDED, GetProcessIdOfThread, INFINITE, OpenThread,
            ResumeThread, THREAD_QUERY_LIMITED_INFORMATION, THREAD_SUSPEND_RESUME,
            WaitForSingleObject,
        },
    },
};

use super::CommandSpec;

pub(super) fn command(spec: &CommandSpec) -> io::Result<Command> {
    let program = resolve_program(spec)?;
    if !is_batch(&program) {
        let mut command = Command::new(program);
        command.args(&spec.args);
        super::configure(&mut command, spec);
        return Ok(command);
    }
    // cmd expands %VAR% even inside a quoted executable path. Indirection makes
    // path expansion single-pass, preserving literal '%' without interpreting
    // its contents again. Windows file paths cannot contain double quotes.
    let script_key = format!("PASEO_SCRIPT_{}", uuid::Uuid::new_v4().simple());
    let mut line = format!("\"\"%{script_key}%\"");
    for argument in &spec.args {
        line.push(' ');
        append_batch_argument(&mut line, argument)?;
    }
    line.push('"');
    let mut command = Command::new(system_cmd()?);
    command.args(["/e:ON", "/v:OFF", "/d", "/s", "/c"]);
    // Only this escaped batch grammar uses raw_arg, never a native executable's
    // argv. /d disables AutoRun and /v:OFF disables delayed ! expansion.
    command.raw_arg(line);
    super::configure(&mut command, spec);
    command.env(script_key, program);
    Ok(command)
}

fn system_cmd() -> io::Result<PathBuf> {
    let mut buffer = vec![0u16; 260];
    loop {
        let size = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) };
        if size == 0 {
            return Err(io::Error::last_os_error());
        }
        if (size as usize) < buffer.len() {
            return Ok(PathBuf::from(OsString::from_wide(&buffer[..size as usize])).join("cmd.exe"));
        }
        buffer.resize(size as usize + 1, 0);
    }
}

fn append_batch_argument(line: &mut String, argument: &str) -> io::Result<()> {
    if argument.contains(['\0', '\r', '\n']) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Windows command-script arguments cannot contain NUL, CR or LF",
        ));
    }
    // Rust 1.93 std's make_bat_command_line/append_bat_arg algorithm, with
    // force-quotes enabled. Unlike shell interpolation this also preserves
    // empty args, backslashes, embedded JSON quotes and literal %VAR% atoms.
    line.push('"');
    let mut backslashes = 0;
    for character in argument.chars() {
        if character == '\\' {
            backslashes += 1;
        } else {
            if character == '"' {
                line.extend(std::iter::repeat_n('\\', backslashes));
                line.push('"');
            } else if character == '%' {
                // %cd:~,% is an empty substring that prevents cmd from pairing
                // this literal percent with a later environment expansion.
                line.push_str("%%cd:~,");
            }
            backslashes = 0;
        }
        line.push(character);
    }
    line.extend(std::iter::repeat_n('\\', backslashes));
    line.push('"');
    Ok(())
}

fn is_batch(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
        })
}

fn effective_env(spec: &CommandSpec, name: &str) -> Option<OsString> {
    spec.env
        .iter()
        .rev()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| OsString::from(value))
        .or_else(|| (!spec.clear_env).then(|| std::env::var_os(name)).flatten())
}

fn resolve_program(spec: &CommandSpec) -> io::Result<PathBuf> {
    let parent_cwd = std::env::current_dir()?;
    let cwd = spec
        .cwd
        .as_ref()
        .map(|cwd| parent_cwd.join(cwd))
        .unwrap_or(parent_cwd);
    let program = Path::new(&spec.program);
    let extensions = effective_env(spec, "PATHEXT").unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".into());
    let extensions = extensions.to_string_lossy();
    let find = |candidate: PathBuf| -> Option<PathBuf> {
        if candidate.is_file() {
            return Some(candidate);
        }
        if candidate.extension().is_none() {
            for extension in extensions
                .split(';')
                .filter(|extension| extension.starts_with('.'))
            {
                // PATHEXT consists of suffixes, never paths or command strings.
                if extension.contains(['/', '\\', ':']) {
                    continue;
                }
                let mut name = candidate.as_os_str().to_owned();
                name.push(extension);
                let path = PathBuf::from(name);
                if path.is_file() {
                    return Some(path);
                }
            }
        }
        None
    };
    if program.is_absolute() || spec.program.contains(['/', '\\']) || program.has_root() {
        return find(cwd.join(program)).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "subprocess executable was not found",
            )
        });
    }
    // Bare commands previously went through cmd's cwd/PATH/PATHEXT lookup.
    // Resolve that prefix first, then launch the resulting executable directly.
    if let Some(program) = find(cwd.join(program)) {
        return Ok(program);
    }
    if let Some(path) = effective_env(spec, "PATH") {
        for directory in std::env::split_paths(&path) {
            if let Some(program) = find(cwd.join(directory).join(program)) {
                return Ok(program);
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "subprocess executable was not found in PATH",
    ))
}

pub(super) struct ChildPipes {
    pub(super) stdin: Option<ChildStdin>,
    pub(super) stdout: Option<ChildStdout>,
    pub(super) stderr: Option<ChildStderr>,
}

pub(super) struct Owner {
    pub(super) child: ChildPipes,
    process: std::process::Child,
    pid: u32,
    job: OwnedHandle,
    waiter: Option<tokio::task::JoinHandle<io::Result<()>>>,
    status: Option<ExitStatus>,
}

impl Owner {
    pub(super) fn spawn(mut command: Command) -> io::Result<Self> {
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if job.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = unsafe { OwnedHandle::from_raw_handle(job) };
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let result = unsafe {
            SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        };
        if result == 0 {
            return Err(io::Error::last_os_error());
        }
        // A post-spawn Job assignment to a running child races its descendants.
        // Keep its primary thread suspended until ownership is established.
        command.creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW);
        let process = command.into_std().spawn()?;
        let pid = process.id();
        // Own the native child before any fallible pipe conversion or Job
        // assignment. Tokio's stable Command::spawn cannot provide that hook.
        let mut owner = Self {
            child: ChildPipes {
                stdin: None,
                stdout: None,
                stderr: None,
            },
            process,
            pid,
            job,
            waiter: None,
            status: None,
        };
        if unsafe {
            AssignProcessToJobObject(owner.job.as_raw_handle(), owner.process.as_raw_handle())
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        owner.child.stdin = owner
            .process
            .stdin
            .take()
            .map(ChildStdin::from_std)
            .transpose()?;
        owner.child.stdout = owner
            .process
            .stdout
            .take()
            .map(ChildStdout::from_std)
            .transpose()?;
        owner.child.stderr = owner
            .process
            .stderr
            .take()
            .map(ChildStderr::from_std)
            .transpose()?;
        let handle = owner.process.as_handle().try_clone_to_owned()?;
        resume_primary_thread(pid)?;
        // One wait per process, retained across cancellation of wait(). The
        // blocking task owns only a duplicate handle, never a Child mutex.
        owner.waiter = Some(tokio::task::spawn_blocking(move || {
            if unsafe { WaitForSingleObject(handle.as_raw_handle(), INFINITE) } == WAIT_OBJECT_0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        }));
        Ok(owner)
    }

    pub(super) fn id(&self) -> u32 {
        self.pid
    }

    pub(super) fn interrupt(&mut self) -> io::Result<()> {
        self.terminate(true)
    }

    pub(super) fn terminate(&mut self, force: bool) -> io::Result<()> {
        // EOF is the only universal graceful notification for background
        // subprocesses. GenerateConsoleCtrlEvent requires a shared console and
        // must not target the daemon's or an unrelated console process group.
        if force && unsafe { TerminateJobObject(self.job.as_raw_handle(), 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub(super) async fn wait(&mut self) -> io::Result<ExitStatus> {
        if let Some(status) = self.status {
            return Ok(status);
        }
        self.waiter
            .as_mut()
            .ok_or_else(|| io::Error::other("subprocess wait was not registered"))?
            .await
            .map_err(io::Error::other)??;
        self.waiter.take();
        let status = self
            .process
            .try_wait()?
            .ok_or_else(|| io::Error::other("signalled subprocess is still running"))?;
        self.status = Some(status);
        self.terminate(true)?;
        Ok(status)
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        let _ = self.terminate(true);
        // Also kill by the owned native handle if Job assignment itself failed.
        // This cannot target a recycled PID and Windows has no Unix-style zombie.
        let _ = self.process.kill();
        // Closing the sole Job handle is a second tree-termination guarantee.
    }
}

fn resume_primary_thread(pid: u32) -> io::Result<()> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot) };
    let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
    entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
    let mut available = unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) };
    while available != 0 {
        if entry.th32OwnerProcessID == pid {
            let thread: HANDLE = unsafe {
                OpenThread(
                    THREAD_SUSPEND_RESUME | THREAD_QUERY_LIMITED_INFORMATION,
                    0,
                    entry.th32ThreadID,
                )
            };
            if thread.is_null() {
                return Err(io::Error::last_os_error());
            }
            let thread = unsafe { OwnedHandle::from_raw_handle(thread) };
            // Validate the opened handle, not just a stale snapshot thread ID.
            if unsafe { GetProcessIdOfThread(thread.as_raw_handle()) } != pid {
                return Err(io::Error::other(
                    "subprocess thread identity changed before resume",
                ));
            }
            let previous = unsafe { ResumeThread(thread.as_raw_handle()) };
            if previous == u32::MAX {
                return Err(io::Error::last_os_error());
            }
            if previous != 1 {
                return Err(io::Error::other(
                    "subprocess primary thread had an unexpected suspend count",
                ));
            }
            return Ok(());
        }
        entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        available = unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) };
    }
    Err(io::Error::other(
        "suspended subprocess primary thread was not found",
    ))
}
