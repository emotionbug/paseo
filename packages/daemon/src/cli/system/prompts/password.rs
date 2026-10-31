use super::super::error;
use anyhow::{Result, ensure};
use std::{
    fs::File,
    io::{self, Write},
};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

// Only the async owner changes terminal modes. A cancelled prompt restores
// echo immediately; its detached blocking reader never changes modes later.
struct TerminalMode {
    file: File,
    #[cfg(unix)]
    original: libc::termios,
    #[cfg(windows)]
    original: u32,
}
impl TerminalMode {
    fn capture() -> io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/tty")?;
            let mut original = std::mem::MaybeUninit::<libc::termios>::uninit();
            if unsafe { libc::tcgetattr(file.as_raw_fd(), original.as_mut_ptr()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self {
                file,
                original: unsafe { original.assume_init() },
            })
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("CONIN$")?;
            let mut original = 0;
            if unsafe { GetConsoleMode(file.as_raw_handle(), &mut original) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self { file, original })
        }
    }
    fn hide_echo(&self) -> io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let mut mode = self.original;
            mode.c_lflag &= !libc::ECHO;
            if unsafe { libc::tcsetattr(self.file.as_raw_fd(), libc::TCSANOW, &mode) } != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            if unsafe { SetConsoleMode(self.file.as_raw_handle(), self.original & !0x0004) } == 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }
}
impl Drop for TerminalMode {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            unsafe {
                libc::tcsetattr(self.file.as_raw_fd(), libc::TCSANOW, &self.original);
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            unsafe {
                SetConsoleMode(self.file.as_raw_handle(), self.original);
            }
        }
    }
}
#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetConsoleMode(handle: *mut std::ffi::c_void, mode: *mut u32) -> i32;
    fn SetConsoleMode(handle: *mut std::ffi::c_void, mode: u32) -> i32;
    fn ReadConsoleW(
        handle: *mut std::ffi::c_void,
        buffer: *mut std::ffi::c_void,
        count: u32,
        read: *mut u32,
        control: *mut std::ffi::c_void,
    ) -> i32;
}
struct PasswordReader {
    #[cfg(unix)]
    reader: io::BufReader<File>,
    #[cfg(windows)]
    file: File,
}
impl PasswordReader {
    fn new(file: File) -> Self {
        #[cfg(unix)]
        {
            Self {
                reader: io::BufReader::new(file),
            }
        }
        #[cfg(windows)]
        {
            Self { file }
        }
    }
    fn read(&mut self, prompt: &str) -> Result<Zeroizing<String>> {
        eprint!("{prompt}");
        io::stderr().flush()?;
        #[cfg(unix)]
        let mut password = {
            use io::BufRead;
            let mut value = Zeroizing::new(String::new());
            if self.reader.read_line(&mut value)? == 0 {
                return Err(error("PASSWORD_CANCELLED", "Password update cancelled"));
            }
            value
        };
        #[cfg(windows)]
        let mut password = {
            use std::os::windows::io::AsRawHandle;
            use zeroize::Zeroize;
            let mut value = Zeroizing::new(Vec::<u16>::new());
            loop {
                let mut chunk = [0u16; 256];
                let mut count = 0;
                if unsafe {
                    ReadConsoleW(
                        self.file.as_raw_handle(),
                        chunk.as_mut_ptr().cast(),
                        chunk.len() as u32,
                        &mut count,
                        std::ptr::null_mut(),
                    )
                } == 0
                {
                    return Err(io::Error::last_os_error().into());
                }
                if count == 0 {
                    return Err(error("PASSWORD_CANCELLED", "Password update cancelled"));
                }
                value.extend_from_slice(&chunk[..count as usize]);
                chunk.zeroize();
                if value.last() == Some(&10) {
                    break;
                }
            }
            Zeroizing::new(String::from_utf16(&value)?)
        };
        while password.ends_with(['\r', '\n']) {
            password.pop();
        }
        eprintln!();
        Ok(password)
    }
}
pub(crate) async fn read(cancel: &CancellationToken) -> Result<Zeroizing<String>> {
    ensure!(
        !cancel.is_cancelled(),
        error("PASSWORD_CANCELLED", "Password update cancelled")
    );
    let mode = TerminalMode::capture()?;
    mode.hide_echo()?;
    let mut reader = PasswordReader::new(mode.file.try_clone()?);
    let (sender, receiver) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("paseo-password-prompt".into())
        .spawn(move || {
            let result = (|| -> Result<Zeroizing<String>> {
                let first = reader.read("New daemon password: ")?;
                ensure!(
                    !first.is_empty(),
                    error("PASSWORD_REQUIRED", "Password cannot be empty")
                );
                let second = reader.read("Confirm daemon password: ")?;
                ensure!(
                    *first == *second,
                    error("PASSWORD_MISMATCH", "Passwords do not match")
                );
                Ok(first)
            })();
            let _ = sender.send(result);
        })?;
    tokio::select! {_=cancel.cancelled()=>Err(error("PASSWORD_CANCELLED","Password update cancelled")),result=receiver=>result.map_err(|_|error("PASSWORD_CANCELLED","Password update cancelled"))?}
}
