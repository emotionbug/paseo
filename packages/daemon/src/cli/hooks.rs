use anyhow::Result;
use std::{
    collections::BTreeMap,
    io::{IsTerminal, Read},
    time::{Duration, Instant},
};

pub async fn run(provider: &str, event: &str) -> Result<()> {
    let env: BTreeMap<String, String> = std::env::vars().collect();
    if [
        "PASEO_TERMINAL_ID",
        "PASEO_ACTIVITY_TOKEN",
        "PASEO_TERMINAL_ACTIVITY_URL",
    ]
    .iter()
    .any(|key| env.get(*key).is_none_or(|v| v.is_empty()))
    {
        return Ok(());
    }
    let input =
        if provider == "claude" && event == "Notification" && !std::io::stdin().is_terminal() {
            tokio::task::spawn_blocking(read_input).await?
        } else {
            None
        };
    if let Some(state) =
        crate::terminals::hooks::resolve_activity(provider, event, input.as_deref())
    {
        crate::terminals::hooks::post_activity(state, &env).await?;
    }
    Ok(())
}
// Do not use tokio::io::stdin for this deadline: its blocking reader cannot be
// cancelled, so a hook could otherwise wait for input during runtime teardown.
fn read_input() -> Option<String> {
    let mut stdin = std::io::stdin().lock();
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        if !input_ready(Duration::from_millis(100)) {
            return None;
        }
        match stdin.read(&mut chunk) {
            Ok(0) => return Some(String::from_utf8_lossy(&bytes).into_owned()),
            Ok(n) => bytes.extend_from_slice(&chunk[..n]),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        }
    }
}
#[cfg(unix)]
fn input_ready(timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    let mut fd = libc::pollfd {
        fd: libc::STDIN_FILENO,
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        let millis = deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .min(i32::MAX as u128) as i32;
        let result = unsafe { libc::poll(&mut fd, 1, millis) };
        if result >= 0 {
            return result > 0 && fd.revents & (libc::POLLIN | libc::POLLHUP) != 0;
        }
        if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
            || Instant::now() >= deadline
        {
            return false;
        }
    }
}
#[cfg(windows)]
fn input_ready(timeout: Duration) -> bool {
    use windows_sys::Win32::{
        Foundation::{ERROR_BROKEN_PIPE, INVALID_HANDLE_VALUE},
        Storage::FileSystem::{FILE_TYPE_PIPE, GetFileType},
        System::{
            Console::{GetStdHandle, STD_INPUT_HANDLE},
            Pipes::PeekNamedPipe,
        },
    };
    let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return false;
    }
    if unsafe { GetFileType(handle) } != FILE_TYPE_PIPE {
        return true;
    }
    let deadline = Instant::now() + timeout;
    loop {
        let mut available = 0;
        let result = unsafe {
            PeekNamedPipe(
                handle,
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut available,
                std::ptr::null_mut(),
            )
        };
        if result == 0 {
            return std::io::Error::last_os_error().raw_os_error()
                == Some(ERROR_BROKEN_PIPE as i32);
        }
        if available > 0 {
            return true;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        std::thread::sleep(remaining.min(Duration::from_millis(5)));
    }
}
