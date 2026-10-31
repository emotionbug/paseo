use super::*;
use crate::{
    relay_crypto::{Frame, InitiatorOptions, SecureChannel, SecurityMode},
    transport::OutboundFrame,
};

#[test]
fn control_messages_preserve_source_normalization() {
    assert_eq!(
        parse_control(br#"{"type":"sync","connectionIds":[" a ","",null,4,"\t","b"]}"#),
        Some(Control::Sync(vec![" a ".into(), "b".into()]))
    );
    assert_eq!(
        parse_control(br#"{"type":"connected","connectionId":" a "}"#),
        Some(Control::Connected("a".into()))
    );
    assert_eq!(
        parse_control(br#"{"type":"disconnected","connectionId":" b "}"#),
        Some(Control::Disconnected("b".into()))
    );
    assert_eq!(
        parse_control(br#"{"type":"ping","ts":1}"#),
        Some(Control::Ping)
    );
    assert_eq!(parse_control(br#"{"type":"pong"}"#), Some(Control::Pong));
    for bytes in [
        b"null".as_slice(),
        b"{",
        br#"{"type":"sync","connectionIds":{}}"#,
        br#"{"type":"connected","connectionId":" "}"#,
        br#"{"type":"ready"}"#,
    ] {
        assert_eq!(parse_control(bytes), None);
    }
}

#[test]
fn reconnect_delay_is_linear_and_capped() {
    assert_eq!(reconnect_delay(1), Duration::from_secs(1));
    assert_eq!(reconnect_delay(2), Duration::from_secs(2));
    assert_eq!(reconnect_delay(29), Duration::from_secs(29));
    assert_eq!(reconnect_delay(30), Duration::from_secs(30));
    assert_eq!(reconnect_delay(u32::MAX), Duration::from_secs(30));
}

#[test]
fn relay_server_urls_use_v2_and_encode_routing_ids() {
    let mut url = relay_url("[::1]:8787", false, "srv_+/&").unwrap();
    url.query_pairs_mut()
        .append_pair("connectionId", "client /&");
    assert_eq!(url.scheme(), "ws");
    assert_eq!(url.path(), "/ws");
    let pairs: HashMap<_, _> = url.query_pairs().collect();
    assert_eq!(pairs["v"], "2");
    assert_eq!(pairs["role"], "server");
    assert_eq!(pairs["serverId"], "srv_+/&");
    assert_eq!(pairs["connectionId"], "client /&");
    assert!(relay_url("host:8787/path", false, "server").is_err());
    assert!(relay_url("user@host:8787", false, "server").is_err());
}

#[test]
fn queued_early_plaintext_keeps_order_and_binary_provenance() {
    let mut pending = ingress::Pending::default();
    pending.push(OutboundFrame::Text("first".into())).unwrap();
    pending
        .push(OutboundFrame::Binary(bytes::Bytes::from_static(
            b"looks like text",
        )))
        .unwrap();
    pending.push(OutboundFrame::Text("third".into())).unwrap();
    assert!(matches!(pending.pop(), Some(OutboundFrame::Text(text)) if text == "first"));
    assert!(
        matches!(pending.pop(), Some(OutboundFrame::Binary(bytes)) if bytes.as_ref() == b"looks like text")
    );
    assert!(matches!(pending.pop(), Some(OutboundFrame::Text(text)) if text == "third"));
    assert!(pending.pop().is_none());
}

#[test]
fn zero_length_early_records_cannot_make_an_unbounded_queue() {
    let mut pending = ingress::Pending::default();
    for _ in 0..4096 {
        pending.push(OutboundFrame::Text(String::new())).unwrap();
    }
    assert!(pending.push(OutboundFrame::Text(String::new())).is_err());
    pending.pop();
    pending.push(OutboundFrame::Text(String::new())).unwrap();
}

#[test]
fn policy_is_read_for_the_hello_and_never_downgrades() {
    let keypair = Arc::new(KeyPair::from_secret([7; 32]));
    let client = SecureChannel::initiator_with_options(
        keypair.public_key(),
        InitiatorOptions {
            offer_protected: false,
            ..Default::default()
        },
    )
    .unwrap();
    let mut server = SecureChannel::responder(keypair, SecurityMode::Legacy);
    server.set_minimum_mode(SecurityMode::Protected);
    let error = server
        .receive(Frame::Text(client.hello().unwrap().into()))
        .unwrap_err();
    assert_eq!(error.close_code, 1008);
    assert_eq!(error.reason, "protected endpoint security required");
    assert!(server.is_closed());
}

fn protected_pair() -> (SecureChannel, SecureChannel, String) {
    let keypair = Arc::new(KeyPair::from_secret([11; 32]));
    let mut client =
        SecureChannel::initiator(keypair.public_key(), SecurityMode::Protected).unwrap();
    let hello = client.hello().unwrap().to_owned();
    let mut server = SecureChannel::responder(keypair, SecurityMode::Legacy);
    let opened = server.receive(Frame::Text(hello.clone())).unwrap();
    assert_eq!(opened.opened, Some(SecurityMode::Protected));
    assert!(opened.message.is_none());
    server.acknowledge_ready();
    client.receive(opened.outbound.unwrap()).unwrap();
    (client, server, hello)
}

#[test]
fn identical_hello_retry_does_not_reset_record_counters() {
    let (mut client, mut server, hello) = protected_pair();
    let first = client.encode(Frame::Text("first".into())).unwrap();
    assert_eq!(
        server.receive(first).unwrap().message,
        Some(Frame::Text("first".into()))
    );
    let retry = server.receive(Frame::Text(hello)).unwrap();
    assert!(retry.opened.is_none());
    server.acknowledge_ready();
    client.receive(retry.outbound.unwrap()).unwrap();
    let next = client.encode(Frame::Binary(vec![1, 2, 3])).unwrap();
    assert_eq!(
        server.receive(next).unwrap().message,
        Some(Frame::Binary(vec![1, 2, 3]))
    );
}

#[test]
fn transcript_change_is_policy_fatal_and_plaintext_is_record_fatal() {
    let (_, mut server, hello) = protected_pair();
    let error = server
        .receive(Frame::Text(format!(" {hello}")))
        .unwrap_err();
    assert_eq!(error.close_code, 1008);
    assert!(server.is_closed());
    let (_, mut server, _) = protected_pair();
    let error = server
        .receive(Frame::Text(r#"{"type":"hello","clientId":"owner"}"#.into()))
        .unwrap_err();
    assert_eq!(error.close_code, 1011);
    assert!(server.is_closed());
}

#[test]
fn malformed_protected_hello_cannot_fall_back_to_legacy() {
    let keypair = Arc::new(KeyPair::from_secret([19; 32]));
    let legacy_key = KeyPair::from_secret([23; 32]);
    let mut server = SecureChannel::responder(keypair, SecurityMode::Legacy);
    let hello = json!({"type":"e2ee_hello","key":legacy_key.export_public_key(),"protected":{"v":1,"message":"!"}});
    let error = server.receive(Frame::Text(hello.to_string())).unwrap_err();
    assert_eq!(error.close_code, 1011);
    assert_eq!(error.reason, "E2EE handshake failed");
    assert!(server.is_closed());
}

#[test]
fn wire_budget_counts_base64_and_protected_chunk_expansion() {
    let (_, server, _) = protected_pair();
    let length = crate::relay_crypto::MAX_CHUNK_DATA_BYTES + 1;
    let text = Frame::Text("a".repeat(length));
    let record_length = crate::relay_crypto::protected_record_byte_length(length).unwrap();
    assert_eq!(
        server.outbound_wire_byte_length(&text).unwrap(),
        record_length.div_ceil(3) * 4
    );
    let binary = Frame::Binary(vec![0; length]);
    assert_eq!(
        server.outbound_wire_byte_length(&binary).unwrap(),
        record_length
    );
    assert!(record_length > length);
}
