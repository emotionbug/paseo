use super::{
    record::{self, Record},
    remote::Remote,
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

fn pending(origin: &str) -> Record {
    serde_json::from_value(json!({"version":2,"state":"pending",
        "relationship":{"daemonId":"relationship-a","idempotencyKey":"stable-enrollment-key","hubOrigin":origin,"createdAt":"2026-01-01T00:00:00.000Z","permissions":["hub.execute"]},
        "credential":{"secret":"stable-relationship-secret"},"enrollment":{"token":"enrollment-token"},
        "identity":{"serverId":"srv_fixture","daemonPublicKey":"fixture-public-key"}})).unwrap()
}

#[test]
fn websocket_origin_cannot_change_transport_security_or_authority() {
    assert!(
        record::validate_socket_url("https://hub.example", "wss://hub.example/ws?session=1")
            .is_ok()
    );
    assert!(
        record::validate_socket_url("http://hub.example:8080/base", "ws://hub.example:8080/ws")
            .is_ok()
    );
    for socket in [
        "ws://hub.example/ws",
        "wss://evil.example/ws",
        "wss://hub.example:444/ws",
        "wss://hub.example/ws#fragment",
    ] {
        assert!(
            record::validate_socket_url("https://hub.example", socket).is_err(),
            "{socket}"
        );
    }
}
#[test]
fn permissions_preserve_first_occurrence_and_acknowledgment_cardinality() {
    assert_eq!(
        record::permissions(&json!(["hub.execute", "daemon.read", "hub.execute"])).unwrap(),
        vec!["hub.execute", "daemon.read"]
    );
    assert!(record::permissions(&json!(["owner"])).is_err());
    let expected = vec!["hub.execute".into(), "daemon.read".into()];
    assert!(record::same_permissions(
        &json!(["daemon.read", "hub.execute"]),
        &expected
    ));
    assert!(!record::same_permissions(
        &json!(["hub.execute", "hub.execute"]),
        &expected
    ));
}
#[tokio::test]
async fn durable_pending_record_reuses_credential_and_migrates_legacy_grants() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("hub-relationship.json");
    let original = pending("https://hub.example");
    record::persist(&path, &original).await.unwrap();
    let restored = record::load(&path).await.unwrap().unwrap();
    assert_eq!(restored.secret().unwrap(), original.secret().unwrap());
    assert_eq!(
        restored.relationship.idempotency_key,
        original.relationship.idempotency_key
    );
    assert_eq!(
        restored.principal_id().unwrap(),
        original.principal_id().unwrap()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            tokio::fs::metadata(&path)
                .await
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let mut legacy = serde_json::to_value(original).unwrap();
    legacy["version"] = 1.into();
    legacy["relationship"]
        .as_object_mut()
        .unwrap()
        .remove("permissions");
    legacy["relationship"]["scopes"] = json!(["hub.execution.*"]);
    tokio::fs::write(&path, serde_json::to_vec(&legacy).unwrap())
        .await
        .unwrap();
    let migrated = record::load(&path).await.unwrap().unwrap();
    assert_eq!(migrated.relationship.permissions, vec!["hub.execute"]);
    let disk: Value = serde_json::from_slice(&tokio::fs::read(&path).await.unwrap()).unwrap();
    assert_eq!(disk["version"], 2);
    assert!(disk["relationship"].get("scopes").is_none());
}
#[tokio::test]
async fn corrupt_authority_is_quarantined_not_deleted_or_used() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("hub-relationship.json");
    let malformed = b"{not a valid relationship";
    tokio::fs::write(&path, malformed).await.unwrap();
    assert!(record::load(&path).await.unwrap().is_none());
    assert!(!path.exists());
    let mut entries = tokio::fs::read_dir(directory.path()).await.unwrap();
    let entry = entries.next_entry().await.unwrap().unwrap();
    assert!(
        entry
            .file_name()
            .to_string_lossy()
            .starts_with("hub-relationship.invalid-")
    );
    assert_eq!(tokio::fs::read(entry.path()).await.unwrap(), malformed);
    assert!(entries.next_entry().await.unwrap().is_none());
}
#[tokio::test]
async fn legacy_disconnecting_record_never_resumes_remote_authority() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("hub-relationship.json");
    let mut record = pending("https://hub.example");
    record.state = "disconnecting".into();
    record.enrollment = None;
    record.identity = None;
    record::persist(&path, &record).await.unwrap();
    assert!(record::load(&path).await.unwrap().is_none());
    assert!(!path.exists());
}
#[tokio::test]
async fn enrollment_retry_sends_identical_durable_id_and_verifier() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let record = pending(&format!("http://{address}"));
    let task = tokio::spawn(async move {
        let mut captured = Vec::new();
        for attempt in 0..2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let (headers, body) = loop {
                let mut buffer = [0u8; 2048];
                let count = socket.read(&mut buffer).await.unwrap();
                assert_ne!(count, 0);
                request.extend_from_slice(&buffer[..count]);
                if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = std::str::from_utf8(&request[..end]).unwrap();
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            line.split_once(':')
                                .filter(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                                .map(|(_, v)| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap();
                    if request.len() >= end + 4 + length {
                        break (
                            headers.to_owned(),
                            serde_json::from_slice::<Value>(&request[end + 4..end + 4 + length])
                                .unwrap(),
                        );
                    }
                }
                assert!(request.len() < 16_384);
            };
            assert!(headers.starts_with("POST /api/daemons/enroll HTTP/1.1"));
            assert!(
                headers
                    .to_ascii_lowercase()
                    .contains("authorization: bearer enrollment-token")
            );
            captured.push(body);
            let (status, body) = if attempt == 0 {
                ("503 Service Unavailable", json!({}))
            } else {
                (
                    "200 OK",
                    json!({"daemonId":"relationship-a","permissions":["hub.execute"],"webSocketUrl":format!("ws://{address}/ws")}),
                )
            };
            let body = body.to_string();
            socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        }
        captured
    });
    let remote = Remote::new().unwrap();
    assert!(remote.enroll(&record).await.is_err());
    assert_eq!(
        remote.enroll(&record).await.unwrap(),
        format!("ws://{address}/ws")
    );
    let requests = task.await.unwrap();
    assert_eq!(requests[0], requests[1]);
    assert_eq!(requests[0]["idempotencyKey"], "stable-enrollment-key");
    assert_eq!(
        requests[0]["credentialVerifier"],
        record::verifier("stable-relationship-secret")
    );
    assert!(requests[0].get("credential").is_none());
    assert!(requests[0].get("secret").is_none());
}
