use super::*;
use tokio::io::AsyncReadExt;

#[test]
fn intake_cancels_current_and_queued_starts_before_dispatch() {
    let mut intake = Intake::default();
    let generation = intake.generation;
    let pending = intake.begin_start(generation).unwrap();
    intake.cancel();
    assert!(pending.is_cancelled());
    assert!(intake.begin_start(generation).is_err());
    assert!(
        !intake
            .begin_start(intake.generation)
            .unwrap()
            .is_cancelled()
    );
}
#[test]
fn persisted_desktop_marker_never_reconstructs_session_ownership() {
    let session = Session::default();
    let status=session.status_from(&json!({"localDaemon":"running","desktopManaged":true,"pid":777,"startedAt":"2026-01-01T00:00:00.000Z"}));
    assert_eq!(status["desktopManaged"], true);
    assert_eq!(status["ownedByDesktop"], false);
}
#[tokio::test]
async fn jsonl_errors_keep_correlation_and_continue_until_eof() {
    let (input, mut producer) = tokio::io::duplex(4096);
    let (output, mut consumer) = tokio::io::duplex(4096);
    producer
        .write_all(b"not json\n{\"id\":4,\"command\":\"status\"}\n")
        .await
        .unwrap();
    producer.shutdown().await.unwrap();
    let task = tokio::spawn(run_io(input, output));
    let mut contents = String::new();
    consumer.read_to_string(&mut contents).await.unwrap();
    assert_eq!(task.await.unwrap().unwrap(), 0);
    let replies: Vec<Value> = contents
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(replies.len(), 2);
    assert_eq!(replies[0]["id"], Value::Null);
    assert_eq!(replies[0]["error"]["code"], "INVALID_REQUEST");
    assert_eq!(replies[1]["id"], 4);
    assert_eq!(
        replies[1]["error"]["message"],
        "Daemon helper has not been initialized."
    );
}
#[tokio::test]
async fn initializes_native_runtime_without_starting_a_process() {
    let directory = tempfile::tempdir().unwrap();
    let executable = directory
        .path()
        .join(if cfg!(windows) { "paseo.exe" } else { "paseo" });
    tokio::fs::write(&executable, b"fixture, never executed")
        .await
        .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))
            .await
            .unwrap();
    }
    let home = directory.path().join("isolated-home");
    let mut session = Session::default();
    session.initialize(&json!({"runtime":{"executable":executable,"cliShimPath":executable,"workingDirectory":directory.path()},"home":home,"settings":{"managed":false,"keepRunningAfterQuit":false}})).await.unwrap();
    assert_eq!(session.home.as_deref(), Some(home.as_path()));
    assert!(session.start(&CancellationToken::new()).await.is_err());
    assert!(
        session
            .stop(&json!({"reason":"manual_ipc","pid":1,"startedAt":"unconfirmed"}))
            .await
            .is_err()
    );
    session.close(false).await.unwrap();
    assert!(!home.exists());
}
#[tokio::test]
async fn logs_are_bounded_and_drop_truncated_first_line() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("daemon.log");
    let mut contents = String::from("first line\n");
    contents.push_str(&"x".repeat(1024 * 1024));
    contents.push_str("\nlast line\n");
    tokio::fs::write(&path, contents).await.unwrap();
    let logs = read_logs(&path).await.unwrap();
    assert_eq!(logs["contents"], "last line\n");
}
