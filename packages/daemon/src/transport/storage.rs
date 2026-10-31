use anyhow::{Result, anyhow, bail};
use std::path::Path;
use tokio::io::AsyncWriteExt;

/// Atomic private replacement in the destination directory. Existing symlinks are never followed.
pub async fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("Private file has no parent"))?;
    tokio::fs::create_dir_all(parent).await?;
    if let Ok(metadata) = tokio::fs::symlink_metadata(path).await {
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            bail!("Refusing non-regular private file");
        }
    }
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("private"),
        uuid::Uuid::new_v4()
    ));
    let result = async {
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            options.mode(0o600);
        }
        let mut file = options.open(&temporary).await?;
        file.write_all(bytes).await?;
        file.sync_all().await?;
        drop(file);
        tokio::fs::rename(&temporary, path).await?;
        #[cfg(unix)]
        {
            let directory = tokio::fs::File::open(parent).await?;
            directory.sync_all().await?;
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if result.is_err() {
        match tokio::fs::remove_file(&temporary).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    result
}

pub async fn read_private(path: &Path) -> Result<Option<Vec<u8>>> {
    let metadata = match tokio::fs::symlink_metadata(path).await {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        bail!("Refusing non-regular private file");
    }
    let mut options = tokio::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(path).await?;
    let metadata = file.metadata().await?;
    if !metadata.is_file() || metadata.len() > 1024 * 1024 {
        bail!("Invalid private identity file");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .await?;
    }
    use tokio::io::AsyncReadExt;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.read_to_end(&mut bytes).await?;
    Ok(Some(bytes))
}
