use super::*;
use serde_json::json;

#[test]
fn native_lock_excludes_node_compatible_writer_before_any_worker_start() {
    let home = tempfile::tempdir().unwrap();
    let mut lock = lock::PidLock::acquire(home.path(), true).unwrap();
    let serialized: Value =
        serde_json::from_slice(&std::fs::read(home.path().join("paseo.pid")).unwrap()).unwrap();
    assert_eq!(serialized["pid"], std::process::id());
    assert!(serialized["startedAt"].is_string());
    assert_eq!(serialized["heartbeat"], true);
    assert_eq!(serialized["desktopManaged"], true);
    assert!(serialized["processStartIdentity"].is_string());
    assert!(
        !std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(home.path().join("paseo.pid"))
            .is_ok()
    );
    assert!(lock::PidLock::acquire(home.path(), false).is_err());
    lock.publish(Some("127.0.0.1:4242".into()), Some("server".into()))
        .unwrap();
    assert_eq!(
        lock::read(home.path()).unwrap().unwrap().listen.as_deref(),
        Some("127.0.0.1:4242")
    );
    drop(lock);
    assert!(!home.path().join("paseo.pid").exists());
}

#[test]
fn legacy_live_lock_is_reachable_but_never_signal_authority() {
    let home = tempfile::tempdir().unwrap();
    let guard = lock::PidLock::acquire(home.path(), false).unwrap();
    let mut legacy = guard.instance.clone();
    legacy.process_start_identity = None;
    std::fs::write(
        home.path().join("paseo.pid"),
        serde_json::to_vec(&legacy).unwrap(),
    )
    .unwrap();
    assert!(identity::owner_running(&legacy).unwrap());
    assert!(!identity::verified(&legacy).unwrap());
    assert!(lock::PidLock::acquire(home.path(), false).is_err());
    drop(guard);
    assert!(home.path().join("paseo.pid").exists());
}

#[test]
fn reused_pid_and_prior_boot_are_not_live_owners() {
    let home = tempfile::tempdir().unwrap();
    let guard = lock::PidLock::acquire(home.path(), false).unwrap();
    let mut stale = guard.instance.clone();
    stale.process_start_identity = Some("not-the-process-birth-token".into());
    assert!(!identity::owner_running(&stale).unwrap());
    assert!(!identity::verified(&stale).unwrap());
    stale = guard.instance.clone();
    stale.started_at = "1970-01-01T00:00:00.000Z".into();
    stale.boot_id = None;
    assert!(!identity::owner_running(&stale).unwrap());
    #[cfg(target_os = "linux")]
    {
        stale = guard.instance.clone();
        stale.boot_id = Some("a-different-boot".into());
        assert!(!identity::owner_running(&stale).unwrap());
    }
}

#[test]
fn release_and_heartbeat_refuse_changed_same_pid_instance() {
    let home = tempfile::tempdir().unwrap();
    let guard = lock::PidLock::acquire(home.path(), false).unwrap();
    let mut replacement = guard.instance.clone();
    replacement.started_at = "2000-01-01T00:00:00.000Z".into();
    std::fs::write(
        home.path().join("paseo.pid"),
        serde_json::to_vec(&replacement).unwrap(),
    )
    .unwrap();
    assert!(guard.refresh().is_err());
    assert!(!lock::release(home.path(), &guard.instance).unwrap());
    drop(guard);
    assert_eq!(
        lock::read(home.path()).unwrap().unwrap().started_at,
        replacement.started_at
    );
}

#[tokio::test]
async fn readiness_is_instance_pinned_cancellable_and_timeout_bounded() {
    let home = tempfile::tempdir().unwrap();
    let mut guard = lock::PidLock::acquire(home.path(), false).unwrap();
    let cancel = CancellationToken::new();
    let failure =
        wait_for_daemon_ready(home.path(), Duration::ZERO, Some(&guard.instance), &cancel)
            .await
            .unwrap_err();
    assert_eq!(
        failure.downcast_ref::<DaemonInstanceError>().unwrap().code,
        "DAEMON_NOT_READY"
    );
    let mut replaced = guard.instance.clone();
    replaced.started_at = "2000-01-01T00:00:00.000Z".into();
    let failure = wait_for_daemon_ready(home.path(), Duration::ZERO, Some(&replaced), &cancel)
        .await
        .unwrap_err();
    assert_eq!(
        failure.downcast_ref::<DaemonInstanceError>().unwrap().code,
        "DAEMON_REPLACED"
    );
    guard
        .publish(
            Some("unix:///isolated/paseo.sock".into()),
            Some("server".into()),
        )
        .unwrap();
    assert!(
        wait_for_daemon_ready(home.path(), Duration::ZERO, Some(&guard.instance), &cancel)
            .await
            .is_ok()
    );
    cancel.cancel();
    assert!(
        wait_for_daemon_ready(home.path(), Duration::ZERO, None, &cancel)
            .await
            .is_err()
    );
}

#[test]
fn invalid_nonempty_lock_is_not_silently_reclaimed() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("paseo.pid"), b"{broken").unwrap();
    assert!(lock::PidLock::acquire(home.path(), false).is_err());
    assert_eq!(
        std::fs::read(home.path().join("paseo.pid")).unwrap(),
        b"{broken"
    );
}

#[cfg(unix)]
#[test]
fn symlinked_pid_file_never_modifies_its_target() {
    use std::os::unix::fs::symlink;
    let home = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let target = other.path().join("target");
    std::fs::write(&target, b"untouched").unwrap();
    symlink(&target, home.path().join("paseo.pid")).unwrap();
    assert!(lock::PidLock::acquire(home.path(), false).is_err());
    assert_eq!(std::fs::read(target).unwrap(), b"untouched");
}

#[test]
fn configured_log_path_is_used_for_startup_diagnostics() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.json"),
        serde_json::to_vec(&json!({"log":{"file":{"path":"logs/worker.log"}}})).unwrap(),
    )
    .unwrap();
    assert_eq!(
        daemon_log_path(home.path()),
        home.path().join("logs/worker.log")
    );
}
