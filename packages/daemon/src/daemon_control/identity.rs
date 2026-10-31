use super::DaemonInstance;
use anyhow::{Context, Result, bail};
use std::fs;
#[cfg(windows)]
use std::time::{SystemTime, UNIX_EPOCH};

pub fn uid() -> u32 {
    #[cfg(unix)]
    {
        unsafe { libc::getuid() }
    }
    #[cfg(not(unix))]
    {
        0
    }
}

static BOOT_ID: std::sync::LazyLock<Option<String>> = std::sync::LazyLock::new(|| {
    #[cfg(target_os = "linux")]
    {
        fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
});
pub fn boot_id() -> Option<String> {
    BOOT_ID.clone()
}

pub fn start_identity(pid: u32) -> Result<Option<String>> {
    if pid == 0 {
        return Ok(None);
    }
    #[cfg(target_os = "linux")]
    {
        let value = match fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(value) => value,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let end = value.rfind(')').context("Invalid process stat")?;
        let fields: Vec<_> = value[end + 1..].split_whitespace().collect();
        if matches!(fields.first(), Some(&"Z") | Some(&"X")) {
            return Ok(None);
        }
        return Ok(Some(format!(
            "linux:{}",
            fields.get(19).context("Missing process birth identity")?
        )));
    }
    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("/bin/ps")
            .args(["-o", "lstart=", "-p", &pid.to_string()])
            .env("LC_ALL", "C")
            .output()?;
        if !output.status.success() {
            return Ok(None);
        }
        let value = String::from_utf8(output.stdout)?;
        let value = value.trim();
        return Ok((!value.is_empty()).then(|| format!("darwin:{value}")));
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::{
            Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, GetLastError},
            System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
        };
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            if unsafe { GetLastError() } == ERROR_INVALID_PARAMETER {
                return Ok(None);
            }
            return Err(std::io::Error::last_os_error().into());
        }
        let result = windows_identity(handle);
        unsafe {
            CloseHandle(handle);
        }
        return result;
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        bail!("Process birth identity is unsupported on this platform")
    }
}

#[cfg(windows)]
fn windows_identity(handle: windows_sys::Win32::Foundation::HANDLE) -> Result<Option<String>> {
    use windows_sys::Win32::{
        Foundation::FILETIME,
        System::Threading::{GetExitCodeProcess, GetProcessTimes},
    };
    let mut exit = 0;
    if unsafe { GetExitCodeProcess(handle, &mut exit) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    if exit != 259 {
        return Ok(None);
    }
    let mut created: FILETIME = unsafe { std::mem::zeroed() };
    let mut exited = created;
    let mut kernel = created;
    let mut user = created;
    if unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let ticks = ((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64;
    Ok(Some(format!("win32:{}", ticks + 504_911_232_000_000_000)))
}

fn boot_time_ms() -> Option<i64> {
    #[cfg(target_os = "linux")]
    {
        fs::read_to_string("/proc/stat")
            .ok()?
            .lines()
            .find_map(|line| line.strip_prefix("btime ")?.parse::<i64>().ok())
            .map(|s| s * 1000)
    }
    #[cfg(target_os = "macos")]
    {
        let mut boot: libc::timeval = unsafe { std::mem::zeroed() };
        let mut size = std::mem::size_of_val(&boot);
        let result = unsafe {
            libc::sysctlbyname(
                c"kern.boottime".as_ptr(),
                (&mut boot as *mut libc::timeval).cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        (result == 0).then_some(boot.tv_sec * 1000)
    }
    #[cfg(windows)]
    {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_millis() as i64;
        Some(
            now - unsafe { windows_sys::Win32::System::SystemInformation::GetTickCount64() } as i64,
        )
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        None
    }
}

pub fn same_boot(instance: &DaemonInstance) -> bool {
    if let (Some(expected), Some(actual)) = (&instance.boot_id, BOOT_ID.as_deref()) {
        return expected == actual;
    }
    match (
        chrono::DateTime::parse_from_rfc3339(&instance.started_at),
        boot_time_ms(),
    ) {
        (Ok(start), Some(boot)) => start.timestamp_millis() >= boot - 5000,
        _ => false,
    }
}

pub fn owner_running(instance: &DaemonInstance) -> Result<bool> {
    if !same_boot(instance) {
        return Ok(false);
    }
    let observed = start_identity(instance.pid)?;
    Ok(match (&instance.process_start_identity, observed) {
        (Some(expected), Some(actual)) => expected == &actual,
        (None, Some(_)) => true,
        (_, None) => false,
    })
}

pub fn verified(instance: &DaemonInstance) -> Result<bool> {
    Ok(same_boot(instance)
        && instance.process_start_identity.is_some()
        && start_identity(instance.pid)?.as_ref() == instance.process_start_identity.as_ref())
}

/// Linux binds the signal to a kernel process handle before checking its birth token.
/// Windows likewise verifies and terminates through the same opened process handle.
pub fn signal(instance: &DaemonInstance, force: bool) -> Result<()> {
    if instance.pid <= 1 || instance.pid == std::process::id() {
        bail!("Refusing to stop invalid supervisor PID");
    }
    if instance.uid != uid() || instance.hostname != hostname::get()?.to_string_lossy() {
        bail!("DAEMON_IDENTITY_UNVERIFIED: daemon belongs to another user or host");
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, instance.pid as libc::pid_t, 0) };
        if fd < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                return Ok(());
            }
            return Err(error).context("Cannot safely open daemon process handle");
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd as i32) };
        if !verified(instance)? {
            bail!("DAEMON_IDENTITY_UNVERIFIED: refusing to signal reused PID");
        }
        let result = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                fd.as_raw_fd(),
                if force { libc::SIGKILL } else { libc::SIGTERM },
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        };
        if result < 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
            return Err(std::io::Error::last_os_error().into());
        }
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        if !verified(instance)? {
            bail!("DAEMON_IDENTITY_UNVERIFIED: refusing to signal reused PID");
        }
        if unsafe {
            libc::kill(
                instance.pid as i32,
                if force { libc::SIGKILL } else { libc::SIGTERM },
            )
        } != 0
            && std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
        {
            return Err(std::io::Error::last_os_error().into());
        }
        return Ok(());
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::{
            Foundation::CloseHandle,
            System::Threading::{
                OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE, TerminateProcess,
            },
        };
        if !force {
            bail!("STOP_NO_GRACEFUL_CHANNEL: Windows requires authenticated lifecycle RPC");
        }
        let handle = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE,
                0,
                instance.pid,
            )
        };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error().into());
        }
        let result = (|| {
            let Some(actual) = windows_identity(handle)? else {
                return Ok(());
            };
            if !same_boot(instance) || Some(actual) != instance.process_start_identity {
                bail!("DAEMON_IDENTITY_UNVERIFIED: refusing to terminate reused PID");
            }
            if unsafe { TerminateProcess(handle, 1) } == 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            Ok(())
        })();
        unsafe {
            CloseHandle(handle);
        }
        return result;
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        bail!("Safe signaling is unsupported on this platform")
    }
}

/// Snapshot descendants while the root still has its verified birth identity.
/// Each signal independently revalidates the child's identity; numeric ancestry
/// alone never grants permission to terminate a reused PID.
pub fn signal_tree(instance: &DaemonInstance, force: bool) -> Result<()> {
    if !force {
        return signal(instance, false);
    }
    #[cfg(unix)]
    {
        if !verified(instance)? {
            bail!("DAEMON_IDENTITY_UNVERIFIED: process tree owner changed")
        }
        let mut relationships = Vec::new();
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::MetadataExt;
            for entry in fs::read_dir("/proc")? {
                let entry = entry?;
                let Some(pid) = entry
                    .file_name()
                    .to_str()
                    .and_then(|s| s.parse::<u32>().ok())
                else {
                    continue;
                };
                if entry
                    .metadata()
                    .map(|m| m.uid() != instance.uid)
                    .unwrap_or(true)
                {
                    continue;
                }
                let Ok(stat) = fs::read_to_string(entry.path().join("stat")) else {
                    continue;
                };
                let Some(end) = stat.rfind(')') else { continue };
                let fields = stat[end + 1..].split_whitespace().collect::<Vec<_>>();
                let Some(parent) = fields.get(1).and_then(|s| s.parse::<u32>().ok()) else {
                    continue;
                };
                if let Some(start) = fields.get(19) {
                    relationships.push((pid, parent, format!("linux:{start}")));
                }
            }
        }
        #[cfg(target_os = "macos")]
        {
            let output = std::process::Command::new("/bin/ps")
                .args(["-axo", "pid=,ppid=,uid="])
                .env("LC_ALL", "C")
                .output()?;
            if !output.status.success() {
                bail!("Cannot inspect owned process descendants")
            }
            for line in String::from_utf8(output.stdout)?.lines() {
                let fields = line
                    .split_whitespace()
                    .filter_map(|s| s.parse::<u32>().ok())
                    .collect::<Vec<_>>();
                if fields.len() == 3 && fields[2] == instance.uid {
                    if let Some(start) = start_identity(fields[0])? {
                        relationships.push((fields[0], fields[1], start));
                    }
                }
            }
        }
        let mut descendants = Vec::new();
        let mut parents = vec![instance.pid];
        let mut visited = std::collections::HashSet::new();
        while let Some(parent) = parents.pop() {
            for (pid, _, start) in relationships.iter().filter(|(_, ppid, _)| *ppid == parent) {
                if *pid == instance.pid || !visited.insert(*pid) {
                    continue;
                }
                let mut child = instance.clone();
                child.pid = *pid;
                child.process_start_identity = Some(start.clone());
                descendants.push(child);
                parents.push(*pid);
            }
        }
        if !verified(instance)? {
            bail!("DAEMON_IDENTITY_UNVERIFIED: process tree owner changed")
        }
        for child in descendants.iter().rev() {
            if verified(child)? {
                signal(child, true)?;
            }
        }
    }
    signal(instance, true)
}
