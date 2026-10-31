use anyhow::{Result, anyhow};
use portable_pty::CommandBuilder;
use std::{mem, os::windows::io::RawHandle};
use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT, SetConsoleCtrlHandler};
use windows_sys::Win32::System::Threading::{
    CreateEventW, INFINITE, OpenEventW, SYNCHRONIZATION_SYNCHRONIZE, SetEvent, WaitForSingleObject,
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE},
    System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    },
};

// The job owns descendants independently of PID reuse or direct-child exit.
pub(super) struct PtyJob(HANDLE);
unsafe impl Send for PtyJob {}
impl PtyJob {
    pub(super) fn assign(process: Option<RawHandle>) -> Result<Self> {
        let process = process.ok_or_else(|| anyhow!("ConPTY child has no process handle"))?;
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if job.is_null() {
            return Err(std::io::Error::last_os_error().into());
        }
        let owner = Self(job);
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as _,
                mem::size_of_val(&limits) as u32,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        if unsafe { AssignProcessToJobObject(job, process as HANDLE) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(owner)
    }
    pub(super) fn terminate(&self) -> Result<()> {
        if unsafe { TerminateJobObject(self.0, 1) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }
}
impl Drop for PtyJob {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

// portable-pty does not expose CREATE_SUSPENDED or a process-creation job list.
// Its initial process is therefore our trusted gate, never the user command.
// No untrusted descendant can run until the parent's job assignment succeeds.
pub(super) struct PtyGate {
    event: HANDLE,
    name: String,
}
unsafe impl Send for PtyGate {}
impl PtyGate {
    fn new() -> Result<Self> {
        let name = format!("Local\\PaseoPty-{}", uuid::Uuid::new_v4());
        let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, wide.as_ptr()) };
        if event.is_null() {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(Self { event, name })
    }
    pub(super) fn release(&self) -> Result<()> {
        if unsafe { SetEvent(self.event) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }
}
impl Drop for PtyGate {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.event);
        }
    }
}

pub(super) fn prepare_gate(command: CommandBuilder) -> Result<(CommandBuilder, PtyGate)> {
    let executable = command
        .get_env("PASEO_HOOK_CLI")
        .ok_or_else(|| anyhow!("Native PTY helper executable is missing"))?;
    let argv: Vec<String> = command
        .get_argv()
        .iter()
        .map(|arg| {
            arg.to_str()
                .map(str::to_owned)
                .ok_or_else(|| anyhow!("PTY command is not valid UTF-8"))
        })
        .collect::<Result<_>>()?;
    let program = argv
        .first()
        .ok_or_else(|| anyhow!("PTY command is empty"))?;
    let cwd = command
        .get_cwd()
        .map(std::path::PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    which::which_in(program, command.get_env("PATH"), &cwd)?;
    let gate = PtyGate::new()?;
    let mut helper = CommandBuilder::new(executable);
    helper.arg("__paseo_pty_child");
    helper.cwd(cwd);
    helper.env_clear();
    for (key, value) in command.iter_full_env_as_str() {
        helper.env(key, value);
    }
    helper.env("PASEO_NATIVE_PTY_GATE", &gate.name);
    helper.env("PASEO_NATIVE_PTY_COMMAND", serde_json::to_string(&argv)?);
    Ok((helper, gate))
}

/// Earliest hidden CLI dispatch: this process owns no daemon configuration,
/// credentials, sockets, or background services.
pub fn run_child() -> Result<i32> {
    let name = std::env::var("PASEO_NATIVE_PTY_GATE")?;
    let argv: Vec<String> = serde_json::from_str(&std::env::var("PASEO_NATIVE_PTY_COMMAND")?)?;
    let program = argv
        .first()
        .ok_or_else(|| anyhow!("PTY command is empty"))?;
    let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let event = unsafe { OpenEventW(SYNCHRONIZATION_SYNCHRONIZE, 0, wide.as_ptr()) };
    if event.is_null() {
        return Err(std::io::Error::last_os_error().into());
    }
    let waited = unsafe { WaitForSingleObject(event, INFINITE) };
    unsafe {
        CloseHandle(event);
    }
    if waited != WAIT_OBJECT_0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // A custom handler is not inherited by the actual shell, unlike the
    // process-wide CTRL+C-ignore flag. Only the gate ignores keyboard signals.
    if unsafe { SetConsoleCtrlHandler(Some(ignore_keyboard_control), 1) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let status = std::process::Command::new(program)
        .args(&argv[1..])
        .env_remove("PASEO_NATIVE_PTY_GATE")
        .env_remove("PASEO_NATIVE_PTY_COMMAND")
        .status()?;
    status
        .code()
        .ok_or_else(|| anyhow!("PTY child exited without a Windows exit code"))
}
unsafe extern "system" fn ignore_keyboard_control(event: u32) -> i32 {
    if matches!(event, CTRL_C_EVENT | CTRL_BREAK_EVENT) {
        1
    } else {
        0
    }
}

pub(super) fn observe_exit(
    process: Option<RawHandle>,
) -> Result<tokio::sync::oneshot::Receiver<Result<()>>> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let process = process.ok_or_else(|| anyhow!("ConPTY child has no process handle"))?;
    let mut duplicate = std::ptr::null_mut();
    if unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            process as HANDLE,
            GetCurrentProcess(),
            &mut duplicate,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    let handle = unsafe { OwnedHandle::from_raw_handle(duplicate as RawHandle) };
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("paseo-conpty-exit".into())
        .spawn(move || {
            let result =
                if unsafe { WaitForSingleObject(handle.as_raw_handle() as HANDLE, INFINITE) }
                    == WAIT_OBJECT_0
                {
                    Ok(())
                } else {
                    Err(std::io::Error::last_os_error().into())
                };
            let _ = tx.send(result);
        })?;
    Ok(rx)
}
