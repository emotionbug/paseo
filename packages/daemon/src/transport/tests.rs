use super::*;

fn make_source(id: &str, modern: bool) -> (RequestContext, mpsc::Receiver<context::QueuedFrame>) {
    RequestContext::attach(
        id.into(),
        "same-session".into(),
        "same-client".into(),
        json!({"owned_subscriptions":modern}),
        json!({"id":"owner","kind":"owner"}),
        Arc::new(RwLock::new(
            permissions::OWNER_PERMISSIONS
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
        )),
        false,
        None,
        CancellationToken::new(),
    )
}
fn make_request(source: &RequestContext, kind: &str, id: &str) -> RequestContext {
    source.for_request(json!({"type":kind,"requestId":id}))
}
fn json_frame(frame: &context::QueuedFrame) -> Value {
    let OutboundFrame::Text(text) = &frame.frame else {
        panic!("expected text frame")
    };
    serde_json::from_str(text).unwrap()
}

#[test]
fn permission_arrays_are_alternatives_and_unknown_operations_fail_closed() {
    let create = json!({"type":"agent.create.request"});
    assert!(permissions::allows_inbound(&create, |p| p == "hub.execute"));
    assert!(permissions::allows_inbound(&create, |p| p == "workspace.write"));
    assert!(!permissions::allows_inbound(&create, |p| p == "workspace.read"));
    assert!(permissions::allows_inbound(
        &json!({"type":"hosts.route.frame.request"}),
        |_| false
    ));
    assert!(!permissions::allows_inbound(
        &json!({"type":"invented.request"}),
        |_| true
    ));
    assert!(permissions::allows_outbound(
        &json!({"type":"status","payload":{"status":"agent_created"}}),
        |p| p == "hub.execute"
    ));
    assert!(!permissions::allows_outbound(
        &json!({"type":"status","payload":{"status":"daemon_config_changed"}}),
        |p| p == "hub.execute"
    ));
}

#[tokio::test]
async fn replies_require_request_type_id_and_original_live_source() {
    let (old, mut old_output) = make_source("old", true);
    let (replacement, mut replacement_output) = make_source("replacement", true);
    let request = make_request(&old, "fetch_agent_request", "r");
    assert!(
        request
            .send(json!({"type":"fetch_agents_response","payload":{"requestId":"r"}}))
            .await
            .is_err()
    );
    assert!(
        request
            .send(json!({"type":"fetch_agent_response","payload":{"requestId":"other"}}))
            .await
            .is_err()
    );
    request
        .send(json!({"type":"fetch_agent_response","payload":{"requestId":"r","agent":null}}))
        .await
        .unwrap();
    assert_eq!(
        json_frame(&old_output.recv().await.unwrap())["message"]["payload"]["agent"],
        Value::Null
    );
    old.detach();
    assert!(
        request
            .send(json!({"type":"fetch_agent_response","payload":{"requestId":"r"}}))
            .await
            .is_err()
    );
    assert!(replacement_output.try_recv().is_err());
    assert!(replacement.is_live());
}

#[tokio::test]
async fn released_request_cannot_emit_without_retained_operation() {
    let (source, mut output) = make_source("physical", true);
    let request = make_request(&source, "usage.list_reports.request", "usage");
    let operation = request
        .retain_operation(&["usage.list_reports.update"])
        .unwrap();
    request.finish_request();
    assert!(
        request
            .send(json!({"type":"usage.list_reports.update","payload":{"requestId":"usage"}}))
            .await
            .is_err()
    );
    operation
        .emit(json!({"type":"usage.list_reports.update","payload":{"requestId":"usage"}}))
        .await
        .unwrap();
    assert_eq!(
        json_frame(&output.recv().await.unwrap())["message"]["type"],
        "usage.list_reports.update"
    );
    operation.release().await.unwrap();
    assert!(
        operation
            .emit(json!({"type":"usage.list_reports.update","payload":{}}))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn snapshot_precedes_buffered_updates_and_only_owner_releases() {
    let (source, mut output) = make_source("one", true);
    let (other, _) = make_source("two", true);
    let request = make_request(&source, "fetch_agents_request", "snapshot");
    let subscription = request.begin_subscription("agents", None, None).unwrap();
    subscription
        .emit(json!({"type":"agent_update","payload":{"agent":{"id":"changed"}}}))
        .await
        .unwrap();
    assert!(output.try_recv().is_err());
    request.send(json!({"type":"fetch_agents_response","payload":{"requestId":"snapshot","subscriptionId":subscription.response_id(),"agents":[]}})).await.unwrap();
    subscription.activate().await.unwrap();
    assert_eq!(
        json_frame(&output.recv().await.unwrap())["message"]["type"],
        "fetch_agents_response"
    );
    let update = json_frame(&output.recv().await.unwrap());
    assert_eq!(update["message"]["type"], "agent_update");
    assert_eq!(
        update["message"]["payload"]["subscriptionId"],
        subscription.id()
    );
    make_request(&other, "subscription.release.request", "release")
        .release_subscription(subscription.id())
        .await
        .unwrap();
    assert!(!subscription.cancellation().is_cancelled());
    make_request(&source, "subscription.release.request", "release")
        .release_subscription(subscription.id())
        .await
        .unwrap();
    assert!(subscription.cancellation().is_cancelled());
}

#[tokio::test]
async fn terminal_slots_are_physical_source_and_subscription_owned() {
    let (first, mut output) = make_source("first", true);
    let (second, _) = make_source("second", true);
    let subscription = make_request(&first, "subscribe_terminal_request", "sub")
        .begin_subscription("terminal", None, None)
        .unwrap();
    subscription.bind_terminal_slot(7).unwrap();
    subscription.activate().await.unwrap();
    assert!(first.validate_binary(&[2, 7, b'x'], true).is_ok());
    assert!(second.validate_binary(&[2, 7, b'x'], true).is_err());
    assert!(
        first
            .send_binary(Bytes::from_static(&[1, 7, b'x']))
            .await
            .is_err()
    );
    subscription
        .emit_binary(Bytes::from_static(&[1, 7, b'x']))
        .await
        .unwrap();
    assert!(matches!(
        &output.recv().await.unwrap().frame,
        OutboundFrame::Binary(_)
    ));
    subscription.release().await.unwrap();
    assert!(first.validate_binary(&[2, 7, b'x'], true).is_err());
}

#[tokio::test]
async fn legacy_slot_replacement_reclaims_binary_binding_synchronously() {
    let (source, _) = make_source("legacy", false);
    let request = make_request(&source, "subscribe_terminal_request", "r");
    let first = request
        .begin_subscription("terminals", Some("client-id"), Some("terminal-A"))
        .unwrap();
    first.bind_terminal_slot(5).unwrap();
    let second = request
        .begin_subscription("terminals", Some("client-next"), Some("terminal-A"))
        .unwrap();
    second.bind_terminal_slot(5).unwrap();
    assert!(first.cancellation().is_cancelled());
    assert_eq!(second.response_id(), "client-next");
}

#[tokio::test]
async fn modern_subscription_ids_are_host_assigned_and_snapshot_budget_is_shared() {
    let (source, _) = make_source("modern", true);
    let request = make_request(&source, "subscribe_terminal_request", "r");
    assert!(
        request
            .begin_subscription("terminal", Some("caller-id"), None)
            .is_err()
    );
    let first = request.begin_subscription("terminal", None, None).unwrap();
    first.bind_terminal_slot(1).unwrap();
    let second = request.begin_subscription("terminal", None, None).unwrap();
    second.bind_terminal_slot(2).unwrap();
    let mut frame = vec![0u8; MAX_BUFFERED_BYTES / 2 + 1];
    frame[0] = 1;
    frame[1] = 1;
    first.emit_binary(Bytes::from(frame)).await.unwrap();
    let mut frame = vec![0u8; MAX_BUFFERED_BYTES / 2 + 1];
    frame[0] = 1;
    frame[1] = 2;
    assert!(second.emit_binary(Bytes::from(frame)).await.is_err());
    assert!(!source.is_live());
}

#[test]
fn file_upload_frames_require_source_registration_and_exact_end_length() {
    let (source, _) = make_source("a", true);
    let (other, _) = make_source("b", true);
    let req = make_request(&source, "file.upload.request", "upload");
    req.bind_file_transfer("u").unwrap();
    assert!(source.validate_binary(&[0x11, 1, b'u', 9], true).is_ok());
    assert!(other.validate_binary(&[0x11, 1, b'u', 9], true).is_err());
    assert!(source.validate_binary(&[0x12, 1, b'u', 9], true).is_err());
    assert!(source.validate_binary(&[0x12, 1, b'u'], true).is_ok());
    req.release_file_transfer("u");
    assert!(source.validate_binary(&[0x11, 1, b'u', 9], true).is_err());
}

#[tokio::test]
async fn notification_subscription_deduplicates_per_physical_source() {
    let (source, mut output) = make_source("a", true);
    for id in ["one", "two"] {
        let request = make_request(&source, "session.events.set_subscription.request", id);
        let subscription = request.begin_subscription("events", None, None).unwrap();
        request.register_events(
            &subscription,
            ["agent_attention_required".to_owned()]
                .into_iter()
                .collect(),
            true,
        );
        subscription.activate().await.unwrap();
    }
    source
        .emit_event(json!({"type":"agent_attention_required","payload":{"shouldNotify":true}}))
        .await
        .unwrap();
    let first = json_frame(&output.recv().await.unwrap());
    let second = json_frame(&output.recv().await.unwrap());
    assert_eq!(
        [
            first["message"]["payload"]["shouldNotify"]
                .as_bool()
                .unwrap(),
            second["message"]["payload"]["shouldNotify"]
                .as_bool()
                .unwrap()
        ]
        .into_iter()
        .filter(|v| *v)
        .count(),
        1
    );
}

#[test]
fn host_binary_preserves_payload_and_rejects_bad_route_utf8() {
    let payload = [0x10, 2, b'a', b'b', 0, 0];
    let frame = encode_host_frame("route-δ", &payload).unwrap();
    let (route, body) = decode_host_frame(&frame).unwrap();
    assert_eq!(route, "route-δ");
    assert_eq!(body, payload);
    assert!(decode_host_frame(&[0x30, 0, 1, 0xff, 0]).is_err());
    assert!(encode_host_frame("", &[]).is_err());
}

#[tokio::test]
async fn loopback_websocket_rejects_wrong_protocol_and_reserved_plugin_identity() {
    for (client_id, version, code, reason) in [
        ("regular", 0, 4003, "incompatible_protocol"),
        // Original order: protocol rejection precedes the trimmed-clientId check.
        ("   ", 0, 4003, "incompatible_protocol"),
        ("   ", 1, 4002, ""),
        ("plugin:spoof", 1, 4002, ""),
    ] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let transport = Arc::new(TransportService::new(Weak::new()));
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            transport
                .run_socket(socket, None, CancellationToken::new())
                .await
                .unwrap();
        });
        let (mut client, _) = tokio_tungstenite::connect_async(format!("ws://{address}/ws"))
            .await
            .unwrap();
        client.send(Message::Text(json!({"type":"hello","clientId":client_id,"clientType":"cli","protocolVersion":version,"capabilities":{"hello_rejection":true}}).to_string().into())).await.unwrap();
        if !reason.is_empty() {
            let message = client.next().await.unwrap().unwrap();
            let value: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
            assert_eq!(value["type"], "hello.rejected");
            assert_eq!(value["reason"], reason);
        }
        let Message::Close(Some(close)) = client.next().await.unwrap().unwrap() else {
            panic!("expected close")
        };
        assert_eq!(u16::from(close.code), code);
        server.await.unwrap();
    }
}

#[test]
fn hello_validates_grant_vocabulary_without_rejecting_newer_protocol_versions() {
    let mut hello = json!({"type":"hello","clientId":"client","clientType":"cli","protocolVersion":2,"sessionPermissions":["workspace.read"]});
    assert!(validate_hello(&hello).is_ok());
    hello["sessionPermissions"] = json!(["unknown.manage"]);
    assert!(validate_hello(&hello).is_err());
    hello["sessionPermissions"] = json!([]);
    hello["clientId"] = json!("plugin:allowed");
    assert!(validate_hello(&hello).is_ok());
    assert!(validate_hello_identity(&hello, Some("allowed")).is_ok());
    assert!(validate_hello_identity(&hello, None).is_err());
    hello["clientId"] = json!("  ");
    assert!(validate_hello(&hello).is_ok());
    assert!(validate_hello_identity(&hello, None).is_err());
    hello["clientId"] = json!("");
    assert!(validate_hello(&hello).is_err());
    hello["clientId"] = json!("regular");
    hello["protocolVersion"] = json!(1.0);
    assert!(validate_hello(&hello).is_ok());
    hello["protocolVersion"] = json!(1.5);
    assert!(validate_hello(&hello).is_err());
}

#[tokio::test]
async fn disabled_pairing_does_not_initialize_identity_or_credentials() {
    let temporary = tempfile::tempdir().unwrap();
    let home = temporary.path().join("not-created");
    let offer = generate_local_pairing_offer(&home, &json!({"relayEnabled":false}), true)
        .await
        .unwrap();
    assert_eq!(offer, json!({"relayEnabled":false,"url":null,"qr":null}));
    assert!(!home.exists());
}

#[tokio::test]
async fn pairing_reuses_persisted_identity_and_stamps_protected_floor() {
    let home = tempfile::tempdir().unwrap();
    let config = json!({"relayEnabled":true,"relayPublicEndpoint":"relay.example:443","relayPublicUseTls":true,"relayRequireProtectedSecurity":true,"appBaseUrl":"https://native.example/"});
    let first = generate_local_pairing_offer(home.path(), &config, false)
        .await
        .unwrap();
    let second = generate_local_pairing_offer(home.path(), &config, false)
        .await
        .unwrap();
    assert_eq!(first, second);
    assert!(first["qr"].is_null());
    let encoded = first["url"]
        .as_str()
        .unwrap()
        .split("#offer=")
        .nth(1)
        .unwrap();
    let offer: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(encoded).unwrap()).unwrap();
    assert_eq!(offer["v"], 2);
    assert_eq!(offer["minimumSecurityMode"], "protected");
    let key: Value = serde_json::from_slice(
        &tokio::fs::read(home.path().join("daemon-keypair.json"))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(offer["daemonPublicKeyB64"], key["publicKeyB64"]);
    assert!(!home.path().join("local-credential").exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(home.path().join("daemon-keypair.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[tokio::test]
async fn malformed_existing_identity_never_silently_changes_pairing_keys() {
    let home = tempfile::tempdir().unwrap();
    storage::write_private(&home.path().join("server-id"), b"srv_existing\n")
        .await
        .unwrap();
    let corrupt = b"{\"v\":2,\"publicKeyB64\":\"invalid\",\"secretKeyB64\":\"invalid\"}";
    storage::write_private(&home.path().join("daemon-keypair.json"), corrupt)
        .await
        .unwrap();
    assert!(
        generate_local_pairing_offer(home.path(), &json!({"relayEnabled":true}), false)
            .await
            .is_err()
    );
    assert_eq!(
        tokio::fs::read(home.path().join("daemon-keypair.json"))
            .await
            .unwrap(),
        corrupt
    );
}

#[tokio::test]
async fn hub_live_grant_updates_cannot_exceed_hello_permission_ceiling() {
    let transport = TransportService::new(Weak::new());
    let (context, _) = make_source("source", true);
    let key = serde_json::to_string(&("hub:relationship", "client")).unwrap();
    transport.sessions.lock().unwrap().sessions.insert(
        key,
        Session {
            id: "session".into(),
            permissions: context.source.permissions.clone(),
            sources: [(context.source_id().into(), Arc::downgrade(&context.source))]
                .into_iter()
                .collect(),
            retained_until: None,
            ephemeral: false,
            permission_ceiling: Some(["daemon.read".to_owned()].into_iter().collect()),
        },
    );
    transport
        .update_hub_permissions(
            "hub:relationship",
            &[
                "daemon.read".into(),
                "workspace.write".into(),
                "hub.execute".into(),
            ],
        )
        .await
        .unwrap();
    assert!(context.allows("daemon.read"));
    assert!(!context.allows("workspace.write"));
    assert!(!context.allows("hub.execute"));
}

#[tokio::test]
async fn flushed_reply_waits_for_writer_acknowledgement_and_fails_on_detach() {
    let (source, mut output) = make_source("source", true);
    let request = make_request(&source, "shutdown_server_request", "stop");
    let send = tokio::spawn(async move {
        request.send_flushed(json!({"type":"status","payload":{"status":"shutdown_requested","requestId":"stop"}})).await
    });
    assert_eq!(
        json_frame(&output.recv().await.unwrap())["message"]["payload"]["status"],
        "shutdown_requested"
    );
    let mut barrier = output.recv().await.unwrap();
    let OutboundFrame::Flush(acknowledge) =
        std::mem::replace(&mut barrier.frame, OutboundFrame::Pong(Bytes::new()))
    else {
        panic!("expected flush barrier")
    };
    assert!(!send.is_finished());
    acknowledge.send(Ok(())).unwrap();
    send.await.unwrap().unwrap();
    let flushing_source = source.clone();
    let pending = tokio::spawn(async move { flushing_source.flush().await });
    let barrier = output.recv().await.unwrap();
    assert!(matches!(barrier.frame, OutboundFrame::Flush(_)));
    source.detach();
    assert!(pending.await.unwrap().is_err());
}

#[test]
fn control_validation_preserves_missing_null_and_numeric_semantics() {
    assert!(
        validate_control_message(&json!({"type":"ping","requestId":"p","clientSentAt":1.0}))
            .is_ok()
    );
    assert!(
        validate_control_message(&json!({"type":"ping","requestId":"p","clientSentAt":null}))
            .is_err()
    );
    assert!(validate_control_message(&json!({"type":"session.events.set_subscription.request","requestId":"s","events":[],"notifications":null})).is_err());
    let mut heartbeat = json!({"type":"client_heartbeat","deviceType":"mobile","focusedAgentId":null,"lastActivityAt":"2026-01-01T00:00:00Z","appVisible":false});
    assert!(validate_control_message(&heartbeat).is_ok());
    heartbeat.as_object_mut().unwrap().remove("focusedAgentId");
    assert!(validate_control_message(&heartbeat).is_err());
}

#[test]
fn source_guard_cancels_producers_and_retains_only_ordinary_sessions() {
    for ephemeral in [false, true] {
        let transport = TransportService::new(Weak::new());
        let (source, _) = make_source("physical", true);
        let subscription = make_request(&source, "fetch_agents_request", "watch")
            .begin_subscription("agents", None, None)
            .unwrap();
        let key = serde_json::to_string(&("owner", "client")).unwrap();
        transport.sessions.lock().unwrap().sessions.insert(
            key.clone(),
            Session {
                id: source.session_id().into(),
                permissions: source.source.permissions.clone(),
                sources: [(source.source_id().into(), Arc::downgrade(&source.source))]
                    .into_iter()
                    .collect(),
                retained_until: None,
                ephemeral,
                permission_ceiling: None,
            },
        );
        {
            let _guard = SourceGuard {
                transport: &transport,
                context: &source,
                key: &key,
                completed: false,
            };
        }
        assert!(!source.is_live());
        assert!(subscription.cancellation().is_cancelled());
        let registry = transport.sessions.lock().unwrap();
        if ephemeral {
            assert!(!registry.sessions.contains_key(&key));
        } else {
            let retained = &registry.sessions[&key];
            assert!(retained.sources.is_empty());
            assert!(
                retained
                    .retained_until
                    .unwrap()
                    .duration_since(Instant::now())
                    > Duration::from_secs(89)
            );
        }
    }
}

#[test]
fn bearer_tokens_follow_source_whitespace_and_single_token_rules() {
    assert_eq!(
        extract_http_bearer_token(" \tBearer\t token \r\n"),
        Some("token")
    );
    assert_eq!(
        extract_http_bearer_token("\u{feff}Bearer\u{00a0}token\u{feff}"),
        Some("token")
    );
    for invalid in [
        "Bearer",
        "Bearer ",
        "Bearer token other",
        "bearer token",
        "Basic token",
        "Bearer\u{0085}token",
    ] {
        assert_eq!(extract_http_bearer_token(invalid), None);
    }
}

#[tokio::test]
async fn event_source_selection_requires_demand_authority_and_liveness() {
    let transport = TransportService::new(Weak::new());
    let (source, _) = make_source("catalog", true);
    transport.sessions.lock().unwrap().sessions.insert(
        "owner".into(),
        Session {
            id: source.session_id().into(),
            permissions: source.source.permissions.clone(),
            sources: [(source.source_id().into(), Arc::downgrade(&source.source))]
                .into_iter()
                .collect(),
            retained_until: None,
            ephemeral: false,
            permission_ceiling: None,
        },
    );
    assert!(
        transport
            .event_sources("providers_snapshot_update")
            .is_empty()
    );
    let request = make_request(
        &source,
        "session.events.set_subscription.request",
        "catalog",
    );
    let subscription = request.begin_subscription("events", None, None).unwrap();
    request.register_events(
        &subscription,
        [
            "providers_snapshot_update".to_owned(),
            "status.daemon_config_changed".to_owned(),
        ]
        .into_iter()
        .collect(),
        false,
    );
    subscription.activate().await.unwrap();
    assert_eq!(
        transport.event_sources("providers_snapshot_update").len(),
        1
    );
    assert_eq!(
        transport
            .event_sources("status.daemon_config_changed")
            .len(),
        1
    );
    assert!(transport.event_sources("not_an_event").is_empty());
    source.source.permissions.write().unwrap().clear();
    assert!(
        transport
            .event_sources("providers_snapshot_update")
            .is_empty()
    );
    assert!(
        transport
            .event_sources("status.daemon_config_changed")
            .is_empty()
    );
    source.source.permissions.write().unwrap().extend(
        permissions::OWNER_PERMISSIONS
            .iter()
            .map(|permission| (*permission).to_owned()),
    );
    source.detach();
    assert!(!transport.has_event_demand("providers_snapshot_update"));
}
