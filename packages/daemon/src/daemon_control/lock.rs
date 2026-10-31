use super::{DaemonInstance, identity, same_daemon_instance};
use anyhow::{Context, Result, bail};
use std::{
    fs::{self, File, OpenOptions},
    io::{Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

pub fn private_directory(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        bail!("Daemon home cannot be a symlink: {}", path.display());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if fs::metadata(path)?.uid() != identity::uid() {
            bail!("Daemon home belongs to another user");
        }
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

pub fn open_private(path: &Path, create_new: bool) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    if create_new {
        options.create_new(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() || fs::symlink_metadata(path)?.file_type().is_symlink() {
        bail!("Refusing non-regular lifecycle file: {}", path.display());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if file.metadata()?.uid() != identity::uid() {
            bail!("Lifecycle file belongs to another user");
        }
    }
    Ok(file)
}

fn decode(bytes: &[u8]) -> Result<DaemonInstance> {
    let instance: DaemonInstance = serde_json::from_slice(bytes)?;
    if instance.pid == 0
        || instance.pid > i32::MAX as u32
        || instance.started_at.is_empty()
        || instance.hostname.is_empty()
    {
        bail!("Invalid daemon lock shape");
    }
    chrono::DateTime::parse_from_rfc3339(&instance.started_at)
        .context("Invalid daemon lock start time")?;
    Ok(instance)
}

pub fn read(home: &Path) -> Result<Option<DaemonInstance>> {
    let path = home.join("paseo.pid");
    let mut last_error = None;
    let mut empty = false;
    for attempt in 0..10 {
        match fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
            Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => bail!(
                "DAEMON_STATE_READ_FAILED: {} is not a regular file",
                path.display()
            ),
            Ok(metadata) if metadata.len() > 64 * 1024 => {
                bail!("DAEMON_STATE_READ_FAILED: PID file exceeds size limit")
            }
            _ => {}
        }
        match fs::read(&path) {
            Ok(bytes) => {
                empty = bytes.is_empty();
                if !empty {
                    match decode(&bytes) {
                        Ok(lock) => return Ok(Some(lock)),
                        Err(e) => last_error = Some(e),
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                empty = false;
                last_error = Some(e.into());
            }
        }
        if attempt < 9 {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    if empty {
        return Ok(None);
    }
    bail!(
        "DAEMON_STATE_READ_FAILED: Cannot read daemon state at {}: {}",
        path.display(),
        last_error.map(|e| e.to_string()).unwrap_or_default()
    )
}

fn same_file(file: &File, path: &Path) -> Result<bool> {
    let a = file.metadata()?;
    let b = match fs::symlink_metadata(path) {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    if !b.is_file() || b.file_type().is_symlink() {
        return Ok(false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(a.dev() == b.dev() && a.ino() == b.ino())
    }
    #[cfg(not(unix))]
    {
        Ok(a.created()? == b.created()? && a.len() == b.len())
    }
}

pub fn release(home: &Path, expected: &DaemonInstance) -> Result<bool> {
    let Some(current) = read(home)? else {
        return Ok(false);
    };
    if !same_daemon_instance(&current, expected)
        || current.process_start_identity != expected.process_start_identity
    {
        return Ok(false);
    }
    let path = home.join("paseo.pid");
    let file = open_private(&path, false)?;
    if !same_file(&file, &path)? {
        return Ok(false);
    }
    let Some(confirmed) = read(home)? else {
        return Ok(false);
    };
    if !same_daemon_instance(&confirmed, expected)
        || confirmed.process_start_identity != expected.process_start_identity
    {
        return Ok(false);
    }
    fs::remove_file(path)?;
    Ok(true)
}

pub struct PidLock {
    home: PathBuf,
    file: File,
    pub instance: DaemonInstance,
}
impl PidLock {
    pub fn acquire(home: &Path, desktop_managed: bool) -> Result<Self> {
        private_directory(home)?;
        let path = home.join("paseo.pid");
        if let Some(existing) = read(home)? {
            if existing.hostname != hostname::get()?.to_string_lossy() {
                bail!("Another host owns this daemon home: {}", existing.hostname);
            }
            if identity::owner_running(&existing)? {
                bail!(
                    "Another Paseo daemon is already running (PID {}, started {})",
                    existing.pid,
                    existing.started_at
                );
            }
            let confirmed =
                read(home)?.context("PID lock changed while checking whether it was abandoned")?;
            if !same_daemon_instance(&existing, &confirmed) || identity::owner_running(&confirmed)?
            {
                bail!("PID lock changed while checking whether it was abandoned");
            }
            if !release(home, &confirmed)? {
                bail!("PID lock changed during stale owner cleanup");
            }
        } else if path.exists() {
            let file = open_private(&path, false)?;
            if file.metadata()?.len() != 0 || !same_file(&file, &path)? {
                bail!("PID lock changed while checking empty file");
            }
            fs::remove_file(&path)?;
        }
        let instance = DaemonInstance {
            pid: std::process::id(),
            started_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            hostname: hostname::get()?.to_string_lossy().into_owned(),
            uid: identity::uid(),
            listen: None,
            server_id: None,
            desktop_managed,
            heartbeat: true,
            boot_id: identity::boot_id(),
            process_start_identity: Some(
                identity::start_identity(std::process::id())?
                    .context("Cannot establish supervisor birth identity")?,
            ),
        };
        let mut file = open_private(&path, true)
            .context("Failed to acquire exclusive paseo.pid; another daemon may have started")?;
        file.write_all(&serde_json::to_vec(&instance)?)?;
        file.sync_all()?;
        Ok(Self {
            home: home.to_owned(),
            file,
            instance,
        })
    }
    fn assert_owner(&self) -> Result<()> {
        if !same_file(&self.file, &self.home.join("paseo.pid"))? {
            bail!("PID lock ownership lost");
        }
        let current = read(&self.home)?.context("PID lock missing")?;
        if !same_daemon_instance(&current, &self.instance)
            || current.process_start_identity != self.instance.process_start_identity
        {
            bail!("PID lock ownership lost");
        }
        Ok(())
    }
    pub fn refresh(&self) -> Result<()> {
        self.assert_owner()?;
        self.file
            .set_times(fs::FileTimes::new().set_modified(SystemTime::now()))?;
        Ok(())
    }
    pub fn publish(&mut self, listen: Option<String>, server_id: Option<String>) -> Result<()> {
        self.assert_owner()?;
        self.instance.listen = listen;
        self.instance.server_id = server_id;
        let data = serde_json::to_vec(&self.instance)?;
        self.file.seek(SeekFrom::Start(0))?;
        self.file.set_len(0)?;
        self.file.write_all(&data)?;
        self.file.sync_all()?;
        Ok(())
    }
}
impl Drop for PidLock {
    fn drop(&mut self) {
        let _ = release(&self.home, &self.instance);
    }
}
