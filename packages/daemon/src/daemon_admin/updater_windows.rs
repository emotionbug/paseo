//! Windows cannot replace an executing runtime in place. A copied native helper
//! waits for its exact supervisor to exit, swaps the complete runtime, and starts
//! the replacement supervisor. No command interpreter performs installation.
use super::*;
use crate::daemon_control::{self, DaemonInstance};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

async fn transaction(home: &Path) -> Result<Option<Transaction>> {
    let path = home.join(RECORD);
    let metadata = match tokio::fs::symlink_metadata(&path).await {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 64 * 1024 {
        bail!("Invalid native update transaction")
    }
    Ok(Some(serde_json::from_slice(&tokio::fs::read(path).await?)?))
}

pub async fn launch_deferred_update(
    home: &Path,
    executable: &Path,
    options: &Value,
    owner: &DaemonInstance,
) -> Result<bool> {
    let Some(t) = transaction(home).await? else {
        return Ok(false);
    };
    if !t.deferred {
        return Ok(false);
    }
    launch_helper(home, executable, options, owner, &t, "install").await
}
pub async fn launch_recovery_helper(
    home: &Path,
    executable: &Path,
    options: &Value,
    owner: &DaemonInstance,
) -> Result<bool> {
    let Some(t) = transaction(home).await? else {
        return Ok(false);
    };
    if t.deferred || t.committed {
        return Ok(false);
    }
    launch_helper(home, executable, options, owner, &t, "rollback").await
}
async fn launch_helper(
    home: &Path,
    executable: &Path,
    options: &Value,
    owner: &DaemonInstance,
    t: &Transaction,
    action: &str,
) -> Result<bool> {
    validate_transaction(t, executable)?;
    let _lease = lease(&t.target)?;
    if action == "install" {
        verify_tree(&t.target, &t.original_digest).await?;
        verify_tree(&t.staged, &t.new_digest).await?;
    }
    let directory = tempfile::Builder::new()
        .prefix(".paseo-update-helper-")
        .tempdir_in(home)?;
    let helper = directory.path().join("paseo.exe");
    tokio::fs::copy(executable, &helper).await?;
    let digest = tree_digest_async(directory.path().to_owned()).await?;
    let mut command = Command::new(&helper);
    command
        .arg("__update")
        .arg("--home")
        .arg(home)
        .current_dir(home)
        .env("PASEO_NATIVE_OPTIONS", serde_json::to_string(options)?)
        .env("PASEO_UPDATE_SUPERVISOR", serde_json::to_string(owner)?)
        .env("PASEO_UPDATE_EXECUTABLE", executable)
        .env("PASEO_UPDATE_ACTION", action)
        .env("PASEO_UPDATE_HELPER_DIRECTORY", directory.path())
        .env("PASEO_UPDATE_HELPER_DIGEST", digest)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(0x00000008 | 0x00000200 | 0x08000000);
    let _child = command
        .spawn()
        .context("Cannot launch native update helper")?;
    let _directory = directory.keep();
    Ok(true)
}

pub async fn run_update_helper(home: &Path) -> Result<i32> {
    let expected: DaemonInstance = serde_json::from_str(
        &std::env::var("PASEO_UPDATE_SUPERVISOR").context("Missing update supervisor identity")?,
    )?;
    let executable = PathBuf::from(
        std::env::var_os("PASEO_UPDATE_EXECUTABLE").context("Missing owned runtime executable")?,
    );
    let helper_directory = PathBuf::from(
        std::env::var_os("PASEO_UPDATE_HELPER_DIRECTORY")
            .context("Missing update helper directory")?,
    );
    validate_helper_directory(home, &helper_directory)?;
    if std::env::current_exe()?.canonicalize()?
        != helper_directory.join("paseo.exe").canonicalize()?
    {
        bail!("Update helper executable identity mismatch")
    }
    let digest = std::env::var("PASEO_UPDATE_HELPER_DIGEST").context("Missing helper digest")?;
    verify_tree(&helper_directory, &digest).await?;
    if !daemon_control::wait_exit(
        home,
        &expected,
        Duration::from_secs(30),
        &CancellationToken::new(),
    )
    .await?
    {
        bail!("Supervisor did not exit; no runtime files were replaced")
    }
    let mut t = transaction(home)
        .await?
        .context("Native update transaction disappeared")?;
    validate_transaction(&t, &executable)?;
    let action = std::env::var("PASEO_UPDATE_ACTION").context("Missing native update action")?;
    if t.committed
        || !matches!(action.as_str(), "install" | "rollback")
        || (action == "install" && !t.deferred)
    {
        bail!("Native update transaction is not awaiting this handoff")
    }
    let mut owner = expected;
    owner.pid = std::process::id();
    owner.started_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    owner.process_start_identity = daemon_control::read_process_start_identity(owner.pid)?;
    let install_lock = lease(&t.target)?;
    if action == "rollback" {
        rollback(&t)
            .await
            .context("Native runtime recovery failed; previous runtime retained")?;
        cleanup(home, &t).await?;
        drop(install_lock);
        launch_supervisor(home, &executable, &owner)
            .await
            .context("Recovered native runtime could not launch")?;
        return Ok(0);
    }
    verify_tree(&t.target, &t.original_digest).await?;
    verify_tree(&t.staged, &t.new_digest).await?;
    validate_signature(&t.target.join("paseo.exe"), &t.staged.join("paseo.exe")).await?;
    t.deferred = false;
    write_record(home, &t, true).await?;
    let mut update_error = None;
    if let Err(error) = replace(&t).await {
        rollback(&t).await.context(
            "Native runtime replacement and rollback both failed; recovery files retained",
        )?;
        cleanup(home, &t).await?;
        update_error = Some(error);
    }
    drop(install_lock);
    let launch = launch_supervisor(home, &executable, &owner).await;
    if let Err(error) = launch {
        if update_error.is_none() {
            let recovery_lock = lease(&t.target)?;
            rollback(&t).await.context(
                "Replacement could not launch and rollback failed; recovery files retained",
            )?;
            cleanup(home, &t).await?;
            drop(recovery_lock);
            launch_supervisor(home, &executable, &owner)
                .await
                .context("Restored runtime could not launch")?;
        }
        return Err(error).context("Native replacement could not launch; previous runtime restored");
    }
    if let Some(error) = update_error {
        return Err(error)
            .context("Native replacement failed; previous runtime restored and restarted");
    }
    Ok(0)
}

async fn launch_supervisor(home: &Path, executable: &Path, helper: &DaemonInstance) -> Result<()> {
    let mut command = Command::new(executable);
    command
        .arg("__supervisor")
        .arg("--home")
        .arg(home)
        .current_dir(home)
        .env("PASEO_UPDATE_HELPER_OWNER", serde_json::to_string(helper)?)
        .env_remove("PASEO_UPDATE_SUPERVISOR")
        .env_remove("PASEO_UPDATE_EXECUTABLE")
        .env_remove("PASEO_UPDATE_ACTION")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(0x00000008 | 0x00000200 | 0x08000000);
    let _child = command.spawn()?;
    Ok(())
}
fn validate_helper_directory(home: &Path, directory: &Path) -> Result<()> {
    if directory.parent() != Some(home)
        || !directory
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with(".paseo-update-helper-"))
        || fs::symlink_metadata(directory)?.file_type().is_symlink()
    {
        bail!("Invalid native update helper directory")
    };
    Ok(())
}

pub async fn cleanup_update_helper(home: &Path) -> Result<()> {
    let Ok(raw) = std::env::var("PASEO_UPDATE_HELPER_OWNER") else {
        return Ok(());
    };
    let owner: DaemonInstance = serde_json::from_str(&raw)?;
    let expected = owner
        .process_start_identity
        .as_deref()
        .context("Missing helper birth identity")?;
    let directory = PathBuf::from(
        std::env::var_os("PASEO_UPDATE_HELPER_DIRECTORY")
            .context("Missing helper cleanup directory")?,
    );
    validate_helper_directory(home, &directory)?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while daemon_control::read_process_start_identity(owner.pid)?.as_deref() == Some(expected) {
        if tokio::time::Instant::now() >= deadline {
            bail!("Update helper has not exited; refusing cleanup")
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    verify_tree(
        &directory,
        &std::env::var("PASEO_UPDATE_HELPER_DIGEST").context("Missing helper cleanup digest")?,
    )
    .await?;
    tokio::fs::remove_dir_all(directory).await?;
    Ok(())
}
