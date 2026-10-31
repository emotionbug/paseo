use super::target::{Endpoint, ssh_args};
use super::*;

fn isolated(home: &std::path::Path) -> HostsService {
    let service = HostsService::new(Weak::new());
    service.inner.home.set(home.to_owned()).unwrap();
    service
        .inner
        .identity
        .set(json!({"serverId":"srv_local","label":"test"}))
        .unwrap();
    // Storage tests do not acquire any network or external process resources.
    service.inner.closed.store(true, Ordering::Release);
    service
}
#[tokio::test]
async fn registration_deduplicates_intent_and_preserves_ownership() {
    let directory = tempfile::tempdir().unwrap();
    let service = isolated(directory.path());
    let input = json!({"connection":"127.0.0.1:6767","sourceKey":"device","metadata":{"z":"last","a":"first"}});
    let first = service.add(&input, "owner-a").await.unwrap();
    let same=service.add(&json!({"metadata":{"a":"first","z":"last"},"sourceKey":"device","connection":"127.0.0.1:6767"}),"owner-a").await.unwrap();
    assert_eq!(first["registrationId"], same["registrationId"]);
    assert!(first.get("connection").is_none());
    let conflict = service
        .add(
            &json!({"connection":"127.0.0.1:6768","sourceKey":"device"}),
            "owner-a",
        )
        .await
        .unwrap_err();
    assert_eq!(
        conflict.downcast_ref::<HostError>().unwrap().code,
        "source_conflict"
    );
    let id = first["registrationId"].as_str().unwrap();
    service
        .update(
            id,
            &json!({"label":"Shared label","enabled":false}),
            "owner-b",
            false,
        )
        .await
        .unwrap();
    let denied = service
        .update(
            id,
            &json!({"connection":"127.0.0.1:6768"}),
            "owner-b",
            false,
        )
        .await
        .unwrap_err();
    assert_eq!(
        denied.downcast_ref::<HostError>().unwrap().code,
        "permission_denied"
    );
    assert!(service.remove(id, "owner-b", false).await.is_err());
    service.remove(id, "access-manager", true).await.unwrap();
    let saved: Value = serde_json::from_slice(
        &tokio::fs::read(directory.path().join("hosts.json"))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(saved, json!([]));
}
#[tokio::test]
async fn failed_persistence_does_not_publish_registration() {
    let directory = tempfile::tempdir().unwrap();
    tokio::fs::create_dir(directory.path().join("hosts.json"))
        .await
        .unwrap();
    let service = isolated(directory.path());
    let revision = *service.inner.changes.borrow();
    assert!(
        service
            .add(&json!({"connection":"127.0.0.1:6767"}), "owner")
            .await
            .is_err()
    );
    assert!(service.inner.rows.read().unwrap().is_empty());
    assert_eq!(*service.inner.changes.borrow(), revision);
}
#[tokio::test]
async fn updates_retain_unknown_fields_and_identity_until_explicit_replacement() {
    let directory = tempfile::tempdir().unwrap();
    let service = isolated(directory.path());
    let initial = service
        .add(&json!({"connection":"127.0.0.1:6767"}), "owner")
        .await
        .unwrap();
    let id = initial["registrationId"].as_str().unwrap();
    {
        let mut rows = service.inner.rows.write().unwrap();
        rows[0]["futureGrant"] = json!({"ceiling":["workspace.read"]});
        rows[0]["serverId"] = json!("srv_pinned");
    }
    let changed = service
        .update(id, &json!({"connection":"127.0.0.1:6768"}), "owner", false)
        .await
        .unwrap();
    assert_eq!(changed["serverId"], "srv_pinned");
    assert_eq!(
        changed["futureGrant"],
        json!({"ceiling":["workspace.read"]})
    );
    assert!(
        service
            .update(id, &json!({"replaceIdentity":true}), "owner", false)
            .await
            .is_err()
    );
    let replaced = service
        .update(
            id,
            &json!({"replaceIdentity":true,"connection":"127.0.0.1:6769"}),
            "owner",
            false,
        )
        .await
        .unwrap();
    assert!(replaced["serverId"].is_null());
    let saved: Value = serde_json::from_slice(
        &tokio::fs::read(directory.path().join("hosts.json"))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        saved[0]["futureGrant"],
        json!({"ceiling":["workspace.read"]})
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            tokio::fs::metadata(directory.path().join("hosts.json"))
                .await
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}
#[test]
fn binary_routes_preserve_payload_and_reject_malformed_ids() {
    let body = [0x11, 0x03, b'i', b'd', b'1', 0, 255];
    let frame = encode_frame("route-π", &body).unwrap();
    let (id, start) = decode_frame(&frame).unwrap();
    assert_eq!(id, "route-π");
    assert_eq!(&frame[start..], body);
    assert!(decode_frame(&[0x30, 0, 1, 0xff, 1]).is_err());
    assert!(decode_frame(&[0x30, 1, 1]).is_err());
    assert!(encode_frame("", &body).is_err());
    let max = "x".repeat(256);
    assert!(encode_frame(&max, &body).is_ok());
    assert!(encode_frame(&(max + "x"), &body).is_err());
}
#[test]
fn connection_grammar_hides_credentials_and_preserves_native_endpoints() {
    let target = Target::parse("wss://example.test/?password=secret&custom=hidden").unwrap();
    assert_eq!(target.description, "wss://example.test");
    assert_eq!(target.password.as_deref(), Some("secret"));
    let Endpoint::Socket { url, .. } = target.endpoint else {
        panic!("socket expected")
    };
    assert!(url.contains("custom=hidden"));
    assert!(!url.contains("password"));
    assert!(url.contains("/ws?"));
    let ssh =
        Target::parse("ssh://user@example.test:2222?daemonPort=7000&password=private").unwrap();
    assert_eq!(ssh.password.as_deref(), Some("private"));
    let Endpoint::Ssh {
        host,
        port,
        daemon_port,
    } = ssh.endpoint
    else {
        panic!("ssh expected")
    };
    assert_eq!(
        ssh_args(&host, port, daemon_port),
        [
            "-T",
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=10",
            "-o",
            "ClearAllForwardings=yes",
            "-o",
            "ExitOnForwardFailure=yes",
            "-p",
            "2222",
            "-W",
            "127.0.0.1:7000",
            "user@example.test"
        ]
    );
    for invalid in [
        "ssh://-option",
        "ssh://host?daemonPort=1&daemonPort=2",
        "tcp://host:6767/path",
        "host:0",
        "ws://user:secret@host/",
        "ssh://host?unknown=secret",
    ] {
        let error = Target::parse(invalid).err().unwrap();
        assert_eq!(
            error.downcast_ref::<HostError>().unwrap().code,
            "invalid_connection"
        );
        assert!(!error.to_string().contains("secret"));
    }
    assert!(matches!(
        Target::parse("unix:///tmp/paseo.sock").unwrap().endpoint,
        Endpoint::Socket { ipc: Some(_), .. }
    ));
    assert!(matches!(
        Target::parse("pipe://\\\\.\\pipe\\paseo").unwrap().endpoint,
        Endpoint::Socket { ipc: Some(_), .. }
    ));
}
#[test]
fn relay_grammar_keeps_security_floor_and_defers_key_validation_to_connector() {
    let target=Target::parse("relay://relay.example:443/srv_one?key=not-yet-a-key&security=protected&password=secret&ssl=true").unwrap();
    assert_eq!(
        target.security,
        Some(("not-yet-a-key".into(), SecurityMode::Protected))
    );
    assert_eq!(target.password.as_deref(), Some("secret"));
    assert_eq!(target.description, "relay://relay.example:443/srv_one");
    let Endpoint::Socket { url, .. } = target.endpoint else {
        panic!("socket expected")
    };
    assert!(url.starts_with("wss://"));
    assert!(!url.contains("secret"));
    assert!(!url.contains("key="));
    let ssh = Target::parse("ssh://host?daemonPort=6.767e3").unwrap();
    assert!(matches!(
        ssh.endpoint,
        Endpoint::Ssh {
            daemon_port: 6767,
            ..
        }
    ));
}
#[tokio::test]
async fn protected_security_floor_is_durable_without_rewriting_registration_intent() {
    let directory = tempfile::tempdir().unwrap();
    let service = isolated(directory.path());
    let connection = "relay://relay.example:443/srv_remote?key=public-key&ssl=true";
    let host = service
        .add(
            &json!({"connection":connection,"sourceKey":"persistent-intent"}),
            "owner",
        )
        .await
        .unwrap();
    let id = host["registrationId"].as_str().unwrap();
    service
        .persist_protected_floor(id, &CancellationToken::new())
        .await
        .unwrap();
    let saved: Value = serde_json::from_slice(
        &tokio::fs::read(directory.path().join("hosts.json"))
            .await
            .unwrap(),
    )
    .unwrap();
    validate_saved(&saved[0]).unwrap();
    assert_eq!(saved[0]["connection"], connection);
    assert_eq!(saved[0]["minimumSecurityMode"], "protected");
    assert_eq!(
        security_floor(&saved[0], &Target::parse(connection).unwrap()),
        SecurityMode::Protected
    );
    service
        .add(
            &json!({"connection":connection,"sourceKey":"persistent-intent"}),
            "owner",
        )
        .await
        .unwrap();
    let updated=service.update(id,&json!({"connection":"relay://relay.example:443/srv_new?key=new-key","replaceIdentity":true}),"owner",false).await.unwrap();
    assert_eq!(updated["minimumSecurityMode"], "protected");
    let target = Target::parse("relay://relay.example:443/srv_new?key=new-key").unwrap();
    assert_eq!(security_floor(&updated, &target), SecurityMode::Protected);
}
#[tokio::test]
async fn failed_security_floor_commit_is_not_published() {
    let directory = tempfile::tempdir().unwrap();
    let service = isolated(directory.path());
    let host = service
        .add(&json!({"connection":"127.0.0.1:6767"}), "owner")
        .await
        .unwrap();
    let id = host["registrationId"].as_str().unwrap();
    tokio::fs::remove_file(directory.path().join("hosts.json"))
        .await
        .unwrap();
    tokio::fs::create_dir(directory.path().join("hosts.json"))
        .await
        .unwrap();
    let revision = *service.inner.changes.borrow();
    assert!(
        service
            .persist_protected_floor(id, &CancellationToken::new())
            .await
            .is_err()
    );
    assert!(
        service.inner.rows.read().unwrap()[0]
            .get("minimumSecurityMode")
            .is_none()
    );
    assert_eq!(*service.inner.changes.borrow(), revision);
}
