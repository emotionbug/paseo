use super::*;
use std::time::Duration;

#[cfg(unix)]
async fn shell(home: &tempfile::TempDir, args: Vec<String>) -> Arc<Terminal> {
    let mut env = BTreeMap::new();
    env.insert("HOME".into(), home.path().to_string_lossy().into_owned());
    env.insert("PATH".into(), "/usr/bin:/bin".into());
    env.insert("SHELL".into(), "/bin/sh".into());
    let (changed, _) = broadcast::channel(8);
    Terminal::spawn(
        CreateTerminal {
            cwd: home.path().to_path_buf(),
            workspace_id: "wks_isolated".into(),
            name: None,
            command: Some("/bin/sh".into()),
            args,
            env,
            rows: 24,
            cols: 80,
        },
        "test-token".into(),
        changed,
    )
    .await
    .unwrap()
}

#[cfg(unix)]
#[tokio::test]
async fn real_pty_has_terminal_descriptors_and_drains_final_capture() {
    let home = tempfile::tempdir().unwrap();
    let terminal = shell(
        &home,
        vec![
            "-c".into(),
            r"test -t 0 && test -t 1 && printf '\033[31mPASEO_PTY_FINAL\033[0m\r\n'; exit 7".into(),
        ],
    )
    .await;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), terminal.wait())
            .await
            .unwrap()
            .unwrap(),
        7
    );
    let (lines, total) = terminal.capture(None, None, true).await.unwrap();
    assert!(total >= 24);
    assert!(lines.iter().any(|line| line.contains("PASEO_PTY_FINAL")));
    assert!(!lines.iter().any(|line| line.contains('\x1b')));
    assert_eq!(terminal.info()["workspaceId"], "wks_isolated");
    terminal.terminate().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn source_resize_claims_and_real_shell_completion() {
    let home = tempfile::tempdir().unwrap();
    let terminal = shell(&home, vec![]).await;
    terminal.resize("one", 31, 97, true).await.unwrap();
    terminal.resize("two", 20, 40, false).await.unwrap();
    let snapshot = terminal.snapshot(true).await.unwrap();
    assert_eq!(snapshot["rows"], 31);
    assert_eq!(snapshot["cols"], 97);
    terminal.resize("two", 22, 70, true).await.unwrap();
    let mut lifecycle = terminal.subscribe_lifecycle();
    terminal
        .input(b"stty size; printf '\\033]633;D;9\\007'\r")
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(
                lifecycle.recv().await.unwrap(),
                TerminalEvent::CommandFinished(Some(9))
            ) {
                break;
            }
        }
    })
    .await
    .unwrap();
    let (lines, _) = terminal.capture(None, None, true).await.unwrap();
    assert!(lines.iter().any(|line| line.trim() == "22 70"));
    terminal.input(b"exit\r").await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), terminal.wait())
        .await
        .unwrap()
        .unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn noisy_output_does_not_drop_command_lifecycle() {
    let home = tempfile::tempdir().unwrap();
    let terminal = shell(&home, vec![]).await;
    let mut lifecycle = terminal.subscribe_lifecycle();
    terminal.input(b"i=0; while [ $i -lt 6000 ]; do printf 'output-%s\\r\\n' $i; i=$((i+1)); done; printf '\\033]633;D;0\\007'\r").await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if matches!(
                lifecycle.recv().await.unwrap(),
                TerminalEvent::CommandFinished(Some(0))
            ) {
                break;
            }
        }
    })
    .await
    .unwrap();
    terminal.terminate().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn blocked_input_does_not_prevent_owned_termination() {
    let home = tempfile::tempdir().unwrap();
    let terminal = shell(
        &home,
        vec!["-c".into(), "stty -echo -icanon; sleep 60".into()],
    )
    .await;
    let writer = terminal.clone();
    let task = tokio::spawn(async move { writer.input(&vec![b'x'; 1024 * 1024]).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    tokio::time::timeout(Duration::from_secs(5), terminal.terminate())
        .await
        .unwrap()
        .unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn attention_unknown_finished_and_interrupt_are_distinct() {
    let home = tempfile::tempdir().unwrap();
    let terminal = shell(&home, vec![]).await;
    assert!(terminal.info()["activity"].is_null());
    terminal.set_activity("working");
    terminal.set_activity("idle");
    assert_eq!(terminal.info()["activity"]["attentionReason"], "finished");
    assert!(!terminal.set_activity("idle"));
    terminal.set_activity("clear_attention");
    assert!(terminal.info()["activity"].get("attentionReason").is_none());
    terminal.set_activity("attention");
    assert_eq!(
        terminal.info()["activity"]["attentionReason"],
        "needs_input"
    );
    terminal.set_activity("working");
    terminal.input(&[3]).await.unwrap();
    assert!(terminal.info()["activity"].is_null());
    terminal.terminate().await.unwrap();
}

#[test]
fn binary_frames_keep_exact_five_opcode_layout() {
    for opcode in 1..=5 {
        let bytes = streams::frame(opcode, 255, b"payload");
        assert_eq!(&bytes[..2], &[opcode, 255]);
        assert_eq!(&bytes[2..], b"payload");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn physical_source_slots_cannot_write_each_others_pty() {
    use crate::transport::OutboundFrame;
    use std::collections::HashSet;
    let home = tempfile::tempdir().unwrap();
    let terminal = shell(&home, vec![]).await;
    let service = TerminalService::new(Weak::new());
    service
        .terminals
        .write()
        .unwrap()
        .insert(terminal.id.clone(), terminal.clone());
    let permissions = Arc::new(RwLock::new(HashSet::from([
        "workspace.read".to_owned(),
        "workspace.write".to_owned(),
    ])));
    let (owner, mut frames) = RequestContext::attach(
        "source-one".into(),
        "session".into(),
        "client-one".into(),
        json!({"owned_subscriptions":true}),
        json!({"kind":"owner"}),
        permissions.clone(),
        false,
        None,
        tokio_util::sync::CancellationToken::new(),
    );
    let (other, _) = RequestContext::attach(
        "source-two".into(),
        "session".into(),
        "client-two".into(),
        json!({"owned_subscriptions":true}),
        json!({"kind":"owner"}),
        permissions,
        false,
        None,
        tokio_util::sync::CancellationToken::new(),
    );
    let message = json!({"type":"subscribe_terminal_request","terminalId":terminal.id,"requestId":"owned-subscribe"});
    let request = owner.for_request(message.clone());
    streams::subscribe(&service, &request, &message)
        .await
        .unwrap();
    let first = frames.recv().await.unwrap();
    let slot = match &first.frame {
        OutboundFrame::Text(text) => {
            serde_json::from_str::<Value>(text).unwrap()["message"]["payload"]["slot"]
                .as_u64()
                .unwrap() as u8
        }
        _ => panic!("subscription acknowledgement must precede binary state"),
    };
    let second = frames.recv().await.unwrap();
    assert!(matches!(&second.frame,OutboundFrame::Binary(bytes) if bytes[0]==4 && bytes[1]==slot));
    service
        .handle_binary(
            &other,
            &streams::frame(2, slot, b"printf 'FOREIGN_SOURCE_EXECUTED\\n'\r"),
        )
        .await
        .unwrap();
    let mut lifecycle = terminal.subscribe_lifecycle();
    service
        .handle_binary(
            &owner,
            &streams::frame(
                2,
                slot,
                b"printf 'OWNED_SOURCE_EXECUTED\\n'; printf '\\033]633;D;0\\007'\r",
            ),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(
                lifecycle.recv().await.unwrap(),
                TerminalEvent::CommandFinished(Some(0))
            ) {
                break;
            }
        }
    })
    .await
    .unwrap();
    let (lines, _) = terminal.capture(None, None, true).await.unwrap();
    assert!(
        lines
            .iter()
            .any(|line| line.contains("OWNED_SOURCE_EXECUTED"))
    );
    assert!(
        !lines
            .iter()
            .any(|line| line.contains("FOREIGN_SOURCE_EXECUTED"))
    );
    terminal.terminate().await.unwrap();
    owner.cancellation().cancel();
    other.cancellation().cancel();
}

#[cfg(unix)]
#[tokio::test]
async fn update_without_claim_does_not_take_terminal_size() {
    let home = tempfile::tempdir().unwrap();
    let terminal = shell(&home, vec![]).await;
    terminal.resize("observer", 10, 20, false).await.unwrap();
    let snapshot = terminal.snapshot(false).await.unwrap();
    assert_eq!(snapshot["rows"], 24);
    assert_eq!(snapshot["cols"], 80);
    terminal.resize("owner", 30, 90, true).await.unwrap();
    terminal.release_size("owner");
    terminal.resize("observer", 10, 20, false).await.unwrap();
    assert_eq!(terminal.snapshot(false).await.unwrap()["cols"], 90);
    terminal.terminate().await.unwrap();
}
