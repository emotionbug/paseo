use std::sync::Arc;

use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};

use super::noise::{CipherState, NoiseHandshake};
use super::records::chunk_count;
use super::*;

fn bytes(hex_text: &str) -> Vec<u8> {
    hex::decode(hex_text).unwrap()
}
fn key(hex_text: &str) -> [u8; 32] {
    bytes(hex_text).try_into().unwrap()
}
fn identity() -> Arc<KeyPair> {
    Arc::new(KeyPair::from_secret([7; 32]))
}
fn cipher_pair() -> (CipherState, CipherState) {
    (CipherState::new([9; 32]), CipherState::new([9; 32]))
}

#[test]
fn complete_noise_nk_transcripts_match_both_external_cross_language_vectors() {
    // Same upstream noise-c/Cacophony fixtures consumed by the TypeScript port.
    // These pin both handshake messages, the optional transcript hash, split
    // direction, and several sequential messages under both transport keys.
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../relay/src/noise/fixtures/nk-25519-chachapoly-sha256.json"
    ))
    .unwrap();
    for vector in fixture["vectors"].as_array().unwrap() {
        let text = |field: &str| vector[field].as_str().unwrap();
        let local = Arc::new(KeyPair::from_secret(key(text("resp_static"))));
        assert_eq!(hex::encode(local.public_key()), text("init_remote_static"));
        let mut initiator = NoiseHandshake::initiator_with_ephemeral(
            key(text("init_remote_static")),
            &bytes(text("init_prologue")),
            Some(KeyPair::from_secret(key(text("init_ephemeral")))),
        );
        let mut responder = NoiseHandshake::responder_with_ephemeral(
            local,
            &bytes(text("resp_prologue")),
            Some(KeyPair::from_secret(key(text("resp_ephemeral")))),
        );
        let messages = vector["messages"].as_array().unwrap();
        let first = initiator
            .write_message(&bytes(messages[0]["payload"].as_str().unwrap()))
            .unwrap();
        assert_eq!(
            hex::encode(&first),
            messages[0]["ciphertext"].as_str().unwrap()
        );
        assert_eq!(
            responder.read_message(&first).unwrap(),
            bytes(messages[0]["payload"].as_str().unwrap())
        );
        let second = responder
            .write_message(&bytes(messages[1]["payload"].as_str().unwrap()))
            .unwrap();
        assert_eq!(
            hex::encode(&second),
            messages[1]["ciphertext"].as_str().unwrap()
        );
        assert_eq!(
            initiator.read_message(&second).unwrap(),
            bytes(messages[1]["payload"].as_str().unwrap())
        );
        assert!(initiator.is_complete() && responder.is_complete());
        assert_eq!(initiator.handshake_hash(), responder.handshake_hash());
        if let Some(expected) = vector["handshake_hash"].as_str() {
            assert_eq!(hex::encode(initiator.handshake_hash()), expected);
        }
        let mut client = initiator.split().unwrap();
        let mut server = responder.split().unwrap();
        assert_eq!((client.send.nonce(), client.receive.nonce()), (0, 0));
        assert_eq!((server.send.nonce(), server.receive.nonce()), (0, 0));
        for (index, message) in messages[2..].iter().enumerate() {
            let (sender, receiver) = if index % 2 == 0 {
                (&mut client.send, &mut server.receive)
            } else {
                (&mut server.send, &mut client.receive)
            };
            let plaintext = bytes(message["payload"].as_str().unwrap());
            let ciphertext = sender.encrypt_with_ad(&[], &plaintext).unwrap();
            assert_eq!(
                hex::encode(&ciphertext),
                message["ciphertext"].as_str().unwrap()
            );
            assert_eq!(
                receiver.decrypt_with_ad(&[], &ciphertext).unwrap(),
                plaintext
            );
        }
        assert!(matches!(initiator.split(), Err(CryptoError::Closed)));
    }
}

#[test]
fn noise_authentication_binds_static_key_prologue_and_directions() {
    let static_key = identity();
    let mut initiator = NoiseHandshake::initiator_with_ephemeral(
        static_key.public_key(),
        NOISE_PROLOGUE,
        Some(KeyPair::from_secret([3; 32])),
    );
    let first = initiator.write_message(&[1, 1]).unwrap();
    for (local, prologue) in [
        (identity(), b"wrong prologue".as_slice()),
        (Arc::new(KeyPair::from_secret([8; 32])), NOISE_PROLOGUE),
    ] {
        let mut wrong = NoiseHandshake::responder(local, prologue);
        assert_eq!(
            wrong.read_message(&first).unwrap_err(),
            CryptoError::Authentication
        );
    }
    let mut responder = NoiseHandshake::responder_with_ephemeral(
        static_key,
        NOISE_PROLOGUE,
        Some(KeyPair::from_secret([4; 32])),
    );
    assert_eq!(responder.read_message(&first).unwrap(), [1, 1]);
    assert_eq!(
        initiator
            .read_message(&responder.write_message(&[1, 1]).unwrap())
            .unwrap(),
        [1, 1]
    );
    let mut client = initiator.split().unwrap();
    let mut server = responder.split().unwrap();
    let sent = client.send.encrypt_with_ad(&[], b"only once").unwrap();
    assert_eq!(
        client.receive.decrypt_with_ad(&[], &sent).unwrap_err(),
        CryptoError::Authentication
    );
    assert_eq!(
        server.receive.decrypt_with_ad(&[], &sent).unwrap(),
        b"only once"
    );
    assert_eq!(
        server.receive.decrypt_with_ad(&[], &sent).unwrap_err(),
        CryptoError::Authentication
    );
}

#[test]
fn handshake_turn_and_split_boundaries_are_enforced() {
    let static_key = identity();
    let mut client = NoiseHandshake::initiator(static_key.public_key(), NOISE_PROLOGUE);
    let mut server = NoiseHandshake::responder(static_key, NOISE_PROLOGUE);
    assert!(matches!(
        client.split(),
        Err(CryptoError::Handshake("handshake incomplete"))
    ));
    assert!(matches!(
        server.write_message(&[]),
        Err(CryptoError::Handshake("handshake message out of turn"))
    ));
    server
        .read_message(&client.write_message(&[]).unwrap())
        .unwrap();
    assert!(matches!(
        client.write_message(&[]),
        Err(CryptoError::Handshake("handshake message out of turn"))
    ));
    client
        .read_message(&server.write_message(&[]).unwrap())
        .unwrap();
    assert!(matches!(
        client.read_message(&[]),
        Err(CryptoError::Handshake("handshake complete"))
    ));
    client.split().unwrap();
    assert!(matches!(
        client.write_message(&[]),
        Err(CryptoError::Closed)
    ));
}

#[test]
fn cipher_nonce_exhaustion_and_failed_authentication_are_exact() {
    let mut sender = CipherState::with_nonce([1; 32], u64::MAX - 1);
    let mut receiver = CipherState::with_nonce([1; 32], u64::MAX - 1);
    let valid = sender.encrypt_with_ad(b"ad", b"last nonce").unwrap();
    assert_eq!(sender.nonce(), u64::MAX);
    assert_eq!(
        sender.encrypt_with_ad(&[], b"no reuse").unwrap_err(),
        CryptoError::NonceExhausted
    );
    assert_eq!(
        receiver.decrypt_with_ad(b"wrong ad", &valid).unwrap_err(),
        CryptoError::Authentication
    );
    assert_eq!(receiver.nonce(), u64::MAX - 1);
    let mut invalid = valid.clone();
    invalid[0] ^= 1;
    assert_eq!(
        receiver.decrypt_with_ad(b"ad", &invalid).unwrap_err(),
        CryptoError::Authentication
    );
    assert_eq!(receiver.nonce(), u64::MAX - 1);
    assert_eq!(
        receiver.decrypt_with_ad(b"ad", &valid).unwrap(),
        b"last nonce"
    );
    assert_eq!(receiver.nonce(), u64::MAX);
    assert_eq!(
        receiver.decrypt_with_ad(b"ad", &valid).unwrap_err(),
        CryptoError::NonceExhausted
    );
    receiver.clear();
    assert_eq!(
        receiver.decrypt_with_ad(b"ad", &valid).unwrap_err(),
        CryptoError::Closed
    );
}

#[test]
fn out_of_order_ciphertext_never_consumes_the_expected_nonce() {
    let (mut sender, mut receiver) = cipher_pair();
    let first = sender.encrypt_with_ad(&[], b"one").unwrap();
    let second = sender.encrypt_with_ad(&[], b"two").unwrap();
    assert_eq!(
        receiver.decrypt_with_ad(&[], &second).unwrap_err(),
        CryptoError::Authentication
    );
    assert_eq!(receiver.nonce(), 0);
    assert_eq!(receiver.decrypt_with_ad(&[], &first).unwrap(), b"one");
    assert_eq!(receiver.decrypt_with_ad(&[], &second).unwrap(), b"two");
}

#[test]
fn stable_key_import_preserves_secret_bytes_and_rejects_noncanonical_public_keys() {
    let original = KeyPair::from_secret([0xa5; 32]);
    let public = original.export_public_key();
    let secret = original.export_secret_key();
    let restored = KeyPair::from_base64(&public, &secret).unwrap();
    assert_eq!(restored.public_key(), original.public_key());
    assert_eq!(restored.export_secret_key().as_str(), secret.as_str());
    assert_eq!(
        import_public_key(&format!("{}B=", "A".repeat(42))).unwrap_err(),
        CryptoError::InvalidBase64
    );
    assert!(import_public_key(&format!("{}=", "!".repeat(43))).is_err());
    assert!(KeyPair::from_base64(&identity().export_public_key(), &secret).is_err());
    assert!(!format!("{original:?}").contains(secret.as_str()));
}

#[test]
fn all_known_low_order_curve25519_inputs_are_rejected() {
    let low_order = [
        "00".repeat(32),
        format!("01{}", "00".repeat(31)),
        "e0eb7a7c3b41b8ae1656e3faf19fc46ada098deb9c32b1fd866205165f49b800".into(),
        "5f9c95bca3508c24b1d0b1559c83ef5b04445cc4581c8e86d8224eddd09f1157".into(),
        format!("ec{}7f", "ff".repeat(30)),
        format!("ed{}7f", "ff".repeat(30)),
        format!("ee{}7f", "ff".repeat(30)),
    ];
    for value in low_order {
        assert!(matches!(
            derive_shared_key(&[3; 32], &key(&value)),
            Err(CryptoError::InvalidPublicKey)
        ));
        let mut server = NoiseHandshake::responder(identity(), NOISE_PROLOGUE);
        let mut message = key(&value).to_vec();
        message.extend_from_slice(&[0; 18]);
        assert_eq!(
            server.read_message(&message).unwrap_err(),
            CryptoError::InvalidPublicKey
        );
    }
}

fn nacl_plaintext() -> Vec<u8> {
    bytes(concat!(
        "be075fc53c81f2d5cf141316ebeb0c7b5228c52a4c62cbd44b66849b64244ffce5ecbaaf33bd751a",
        "1ac728d45e6c61296cdc3c01233561f41db66cce314adb310e3be8250c46f06dceea3a7fa1348057",
        "e2f6556ad6b1318a024a838f21af1fde048977eb48f59ffd4924ca1c60902e52f0a089bc76897040",
        "e082f937763848645e0705",
    ))
}

#[test]
fn legacy_secretbox_matches_published_nacl_vector_with_prefix_tag() {
    // NaCl tests/secretbox.{c,out}, also consumed by RustCrypto crypto_secretbox.
    let cipher = LegacyCipher::new(SharedKey::from_bytes(key(
        "1b27556473e985d462cd51197a9a46c76009549eac6474f206c4ee0844f68389",
    )));
    let nonce: [u8; 24] = bytes("69696ee955b62b73cd62bda875fc73d68219e0036b7a0b37")
        .try_into()
        .unwrap();
    let wire = cipher.encrypt_with_nonce(&nacl_plaintext(), nonce).unwrap();
    assert_eq!(&wire[..24], &nonce);
    assert_eq!(
        hex::encode(&wire[24..]),
        concat!(
            "f3ffc7703f9400e52a7dfb4b3d3305d98e993b9f48681273c29650ba32fc76ce48332ea7164d96a4",
            "476fb8c531a1186ac0dfc17c98dce87b4da7f011ec48c97271d2c20f9b928fe2270d6fb863d51738",
            "b48eeee314a7cc8ab932164548e526ae90224368517acfeabd6bb3732bc0e9da99832b61ca01b6de",
            "56244a9e88d5f9b37973f622a43d14a6599b1f654cb45a74e355a5",
        )
    );
    assert_eq!(cipher.decrypt(wire.clone()).unwrap(), nacl_plaintext());
    let mut invalid = wire;
    invalid[24] ^= 1;
    assert_eq!(
        cipher.decrypt(invalid).unwrap_err(),
        CryptoError::Authentication
    );
}

#[test]
fn legacy_curve25519_hsalsa_key_agreement_matches_sodium_box_vector() {
    // PHP Sodium Compat fixture, also published in RustCrypto crypto_box tests.
    let alice = KeyPair::from_secret(key(
        "68f208412d8dd5db9d0c6d18512e86f0ec75665ab841372d57b042b27ef89d4c",
    ));
    let bob = KeyPair::from_secret(key(
        "b581fb5ae182a16f603f39270d4e3b95bc008310b727a11dd4e784a0044d461b",
    ));
    assert_eq!(
        hex::encode(alice.public_key()),
        "ac3a70ba35df3c3fae427a7c72021d68f2c1e044040b75f17313c0c8b5d4241d"
    );
    assert_eq!(
        hex::encode(bob.public_key()),
        "e8980c86e032f1eb2975052e8d65bddd15c3b59641174ec9678a53789d92c754"
    );
    let sender = LegacyCipher::from_keypair(&alice, &bob.public_key()).unwrap();
    let receiver = LegacyCipher::from_keypair(&bob, &alice.public_key()).unwrap();
    let nonce = bytes("69696ee955b62b73cd62bda875fc73d68219e0036b7a0b37")
        .try_into()
        .unwrap();
    let wire = sender.encrypt_with_nonce(&nacl_plaintext(), nonce).unwrap();
    assert_eq!(
        &wire[24..],
        &[
            0xc0, 0x3f, 0x27, 0xd1, 0x88, 0xef, 0x65, 0x0c, 0xd1, 0x29, 0x36, 0x91, 0x31, 0x37,
            0xbb, 0x17, 0xed, 0x4c, 0x98, 0xc2, 0x64, 0x89, 0x39, 0xe2, 0xe1, 0xd2, 0xe8, 0x55,
            0x47, 0x0a, 0x7b, 0x8c, 0x63, 0x2c, 0xab, 0xfd, 0x5a, 0xb3, 0xb3, 0xc2, 0xd3, 0x13,
            0xdc, 0x8c, 0x9e, 0xcf, 0x5d, 0xa1, 0x73, 0xe1, 0xf9, 0xc3, 0x18, 0xcd, 0xef, 0x1d,
            0xce, 0xd6, 0xd2, 0x51, 0x9e, 0x69, 0x50, 0x85, 0xe6, 0xb5, 0xc4, 0x01, 0xa2, 0xbd,
            0x53, 0x31, 0x44, 0x29, 0x86, 0xc7, 0x07, 0x6d, 0x41, 0x26, 0x25, 0x49, 0x7c, 0x4c,
            0xb2, 0xfd, 0x94, 0xc6, 0xf1, 0x03, 0x96, 0x10, 0x33, 0xb2, 0xc9, 0x30, 0xd7, 0xe8,
            0x2e, 0x03, 0x41, 0xf2, 0x9d, 0x38, 0x79, 0xbd, 0x6a, 0xb9, 0xd8, 0x81, 0xea, 0x3a,
            0x1f, 0x36, 0x5d, 0x63, 0x4e, 0x65, 0x3c, 0x6e, 0x17, 0x1a, 0xac, 0x7f, 0xc1, 0xe7,
            0x69, 0x34, 0xd2, 0x3b, 0xe6, 0xf0, 0x4a, 0x54, 0x01, 0x08, 0x08, 0xdb, 0xf0, 0xf9,
            0xbd, 0x30, 0xf6, 0x3b, 0x68, 0xd0, 0x26,
        ]
    );
    assert_eq!(receiver.decrypt(wire).unwrap(), nacl_plaintext());
}

#[test]
fn chunk_boundaries_empty_frames_and_nonce_counts_roundtrip() {
    for length in [
        0,
        1,
        MAX_CHUNK_DATA_BYTES,
        MAX_CHUNK_DATA_BYTES + 1,
        MAX_CHUNK_DATA_BYTES * 3,
        MAX_CHUNK_DATA_BYTES * 3 + 17,
    ] {
        for kind in [FrameKind::Text, FrameKind::Binary] {
            let (mut sender, mut receiver) = cipher_pair();
            let frame: Vec<u8> = (0..length).map(|index| (index * 7 + 3) as u8).collect();
            let wire = encode_protected_record(kind, &frame, &mut sender).unwrap();
            assert_eq!(wire.len(), protected_record_byte_length(length).unwrap());
            let decoded = decode_protected_record(wire, &mut receiver).unwrap();
            assert_eq!(decoded.kind, kind);
            assert_eq!(decoded.data, frame);
            assert_eq!(sender.nonce(), chunk_count(length) as u64);
            assert_eq!(sender.nonce(), receiver.nonce());
        }
    }
}

fn forged_chunk(sender: &mut CipherState, kind: u8, flags: u8, total: u32, data: &[u8]) -> Vec<u8> {
    let mut plaintext = vec![kind, flags];
    plaintext.extend_from_slice(&total.to_be_bytes());
    plaintext.extend_from_slice(data);
    let encrypted = sender.encrypt_with_ad(&[], &plaintext).unwrap();
    let mut result = (encrypted.len() as u16).to_be_bytes().to_vec();
    result.extend_from_slice(&encrypted);
    result
}

#[test]
fn authenticated_malformed_headers_never_escape_or_allocate_claimed_length() {
    for (kind, flags, total, expected) in [
        (0, 2, 1, "record does not start with FIRST"),
        (0, 7, 1, "reserved flag set"),
        (2, 3, 1, "invalid frame kind"),
        (0, 1, 1, "FINAL flag mismatch"),
        (0, 3, u32::MAX, "record length mismatch"),
    ] {
        let (mut sender, mut receiver) = cipher_pair();
        let wire = forged_chunk(&mut sender, kind, flags, total, &[7]);
        assert_eq!(
            decode_protected_record(wire, &mut receiver).unwrap_err(),
            CryptoError::Record(expected)
        );
        assert_eq!(receiver.nonce(), 1); // Framing follows authentication.
    }
    for (kind, flags, total, expected) in [
        (
            0,
            3,
            MAX_CHUNK_DATA_BYTES as u32 + 1,
            "FIRST repeated inside record",
        ),
        (
            1,
            2,
            MAX_CHUNK_DATA_BYTES as u32 + 1,
            "frame kind changed inside record",
        ),
        (
            0,
            2,
            MAX_CHUNK_DATA_BYTES as u32 + 2,
            "frame length changed inside record",
        ),
    ] {
        let (mut sender, mut receiver) = cipher_pair();
        let mut wire = forged_chunk(
            &mut sender,
            0,
            1,
            MAX_CHUNK_DATA_BYTES as u32 + 1,
            &vec![1; MAX_CHUNK_DATA_BYTES],
        );
        wire.extend(forged_chunk(&mut sender, kind, flags, total, &[2]));
        assert_eq!(
            decode_protected_record(wire, &mut receiver).unwrap_err(),
            CryptoError::Record(expected)
        );
    }
}

#[test]
fn records_reject_truncation_concatenation_tamper_and_trailing_chunks() {
    for mutation in 0..5 {
        let (mut sender, mut receiver) = cipher_pair();
        let mut wire = encode_protected_record(
            FrameKind::Binary,
            &vec![3; MAX_CHUNK_DATA_BYTES + 1],
            &mut sender,
        )
        .unwrap();
        match mutation {
            0 => {
                wire.truncate(2 + 65_535);
            }
            1 => {
                wire.pop();
            }
            2 => {
                wire.extend(
                    encode_protected_record(FrameKind::Binary, b"extra", &mut sender).unwrap(),
                );
            }
            3 => {
                wire[20] ^= 1;
            }
            _ => {
                wire.push(0);
            }
        }
        assert!(decode_protected_record(wire, &mut receiver).is_err());
        if mutation == 3 {
            assert_eq!(receiver.nonce(), 0);
        }
    }
    let (_, mut receiver) = cipher_pair();
    assert!(decode_protected_record(vec![], &mut receiver).is_err());
    assert!(decode_protected_record(vec![0], &mut receiver).is_err());
    assert!(decode_protected_record(vec![0, 21], &mut receiver).is_err());
}

#[test]
fn multichunk_send_checks_nonce_capacity_before_reserving_any() {
    let mut sender = CipherState::with_nonce([9; 32], u64::MAX - 1);
    assert_eq!(
        encode_protected_record(
            FrameKind::Binary,
            &vec![0; MAX_CHUNK_DATA_BYTES + 1],
            &mut sender
        )
        .unwrap_err(),
        CryptoError::NonceExhausted
    );
    assert_eq!(sender.nonce(), u64::MAX - 1);
    encode_protected_record(FrameKind::Binary, &[], &mut sender).unwrap();
    assert_eq!(sender.nonce(), u64::MAX);
}

fn open_pair(options: InitiatorOptions) -> (SecureChannel, SecureChannel, String, Frame) {
    let static_key = identity();
    let mut client =
        SecureChannel::initiator_with_options(static_key.public_key(), options).unwrap();
    let mut server = SecureChannel::responder(static_key, SecurityMode::Legacy);
    let hello = client.hello().unwrap().to_owned();
    let update = server.receive(Frame::Text(hello.clone())).unwrap();
    let ready = update.outbound.unwrap();
    server.acknowledge_ready();
    client.receive(ready.clone()).unwrap();
    (client, server, hello, ready)
}

#[test]
fn protected_retry_is_byte_exact_coalesced_and_never_resets_counters() {
    let (mut client, mut server, hello, ready) = open_pair(Default::default());
    let first = client.encode(Frame::Text("first".into())).unwrap();
    assert_eq!(
        server.receive(first).unwrap().message,
        Some(Frame::Text("first".into()))
    );
    assert_eq!(
        server.receive(Frame::Text(hello.clone())).unwrap().outbound,
        Some(ready.clone())
    );
    assert!(
        server
            .receive(Frame::Text(hello.clone()))
            .unwrap()
            .outbound
            .is_none()
    );
    server.acknowledge_ready();
    assert_eq!(
        server.receive(Frame::Text(hello.clone())).unwrap().outbound,
        Some(ready.clone())
    );
    assert_eq!(client.receive(ready).unwrap(), ChannelUpdate::default());
    let second = client.encode(Frame::Text("second".into())).unwrap();
    assert_eq!(
        server.receive(second).unwrap().message,
        Some(Frame::Text("second".into()))
    );
    let changed = hello.replace("\"v\":1", "\"v\":1 ");
    let error = server.receive(Frame::Text(changed)).unwrap_err();
    assert_eq!(error.kind, CryptoError::TranscriptMismatch);
    assert_eq!(error.close_code, 1008);
    assert!(server.is_closed());
}

#[test]
fn protected_frame_kind_is_authenticated_even_without_binary_negotiation() {
    for binary in [false, true] {
        let (mut client, mut server, _, _) = open_pair(InitiatorOptions {
            binary_ciphertext: binary,
            ..Default::default()
        });
        for plaintext in [
            Frame::Text("unicode 안녕".into()),
            Frame::Binary(b"valid UTF-8 is still binary".to_vec()),
            Frame::Binary(vec![]),
        ] {
            let expected_length = client.outbound_wire_byte_length(&plaintext).unwrap();
            let wire = client.encode(plaintext.clone()).unwrap();
            assert_eq!(wire.as_bytes().len(), expected_length);
            assert_eq!(
                wire.kind(),
                if binary && plaintext.kind() == FrameKind::Binary {
                    FrameKind::Binary
                } else {
                    FrameKind::Text
                }
            );
            assert_eq!(server.receive(wire).unwrap().message, Some(plaintext));
        }
        let reply = server.encode(Frame::Text("reply".into())).unwrap();
        assert_eq!(
            client.receive(reply).unwrap().message,
            Some(Frame::Text("reply".into()))
        );
    }
}

#[test]
fn protected_record_errors_close_each_role_with_distinct_codes() {
    let (mut client, mut server, _, _) = open_pair(Default::default());
    let wire = client.encode(Frame::Binary(vec![1, 2, 3])).unwrap();
    server.receive(wire.clone()).unwrap();
    let error = server.receive(wire.clone()).unwrap_err();
    assert_eq!(error.kind, CryptoError::Authentication);
    assert_eq!(error.close_code, 1011);
    assert!(server.is_closed());
    let error = client.receive(wire).unwrap_err();
    assert_eq!(error.kind, CryptoError::Authentication);
    assert_eq!(error.close_code, 4003);
    assert!(client.is_closed());
    assert!(client.hello().is_none());
    assert!(client.encode(Frame::Text("cannot send".into())).is_err());
}

#[test]
fn malformed_protected_envelopes_never_fall_back_to_legacy() {
    for protected in [
        Value::Null,
        json!({}),
        json!([]),
        json!("bad"),
        json!({"v":"1","message":"AAAA"}),
        json!({"v":1,"message":""}),
        json!({"v":2,"message":"AAAA"}),
    ] {
        let static_key = identity();
        let mut client =
            SecureChannel::initiator(static_key.public_key(), SecurityMode::Legacy).unwrap();
        let mut hello: Value = serde_json::from_str(client.hello().unwrap()).unwrap();
        hello["protected"] = protected.clone();
        let mut server = SecureChannel::responder(static_key, SecurityMode::Legacy);
        assert!(server.receive(Frame::Text(hello.to_string())).is_err());
        assert!(server.is_closed());
        let error = client
            .receive(Frame::Text(
                json!({"type":"e2ee_ready","protected":protected}).to_string(),
            ))
            .unwrap_err();
        assert_eq!(error.close_code, 4002);
        assert!(client.is_closed());
        assert!(
            client
                .receive(Frame::Text("{\"type\":\"e2ee_ready\"}".into()))
                .is_err()
        );
    }
}

#[test]
fn protected_floor_is_dynamic_and_refuses_legacy_on_both_roles() {
    let static_key = identity();
    let mut client = SecureChannel::initiator_with_options(
        static_key.public_key(),
        InitiatorOptions {
            offer_protected: false,
            ..Default::default()
        },
    )
    .unwrap();
    let mut server = SecureChannel::responder(static_key, SecurityMode::Legacy);
    server.set_minimum_mode(SecurityMode::Protected);
    let error = server
        .receive(Frame::Text(client.hello().unwrap().into()))
        .unwrap_err();
    assert_eq!(error.close_code, 1008);
    assert_eq!(error.reason, "protected endpoint security required");
    client.set_minimum_mode(SecurityMode::Protected);
    let error = client
        .receive(Frame::Text("{\"type\":\"e2ee_ready\"}".into()))
        .unwrap_err();
    assert_eq!(error.close_code, 4002);
    assert_eq!(error.kind, CryptoError::ProtectedRequired);
}

#[test]
fn authenticated_parameters_ignore_outer_capabilities_and_reject_reserved_bits() {
    for payload in [vec![1, 2], vec![2, 0], vec![1], vec![1, 1, 0]] {
        let static_key = identity();
        let mut noise = NoiseHandshake::initiator_with_ephemeral(
            static_key.public_key(),
            NOISE_PROLOGUE,
            Some(KeyPair::from_secret([2; 32])),
        );
        let message = noise.write_message(&payload).unwrap();
        let hello = json!({"type":"e2ee_hello","key":"legacy key is not used in protected mode","protected":{"v":1,"message":STANDARD.encode(message)}});
        let mut server = SecureChannel::responder(static_key, SecurityMode::Legacy);
        assert!(matches!(
            server
                .receive(Frame::Text(hello.to_string()))
                .unwrap_err()
                .kind,
            CryptoError::Handshake(_)
        ));
    }
    let static_key = identity();
    let mut client =
        SecureChannel::initiator(static_key.public_key(), SecurityMode::Protected).unwrap();
    let mut server = SecureChannel::responder(static_key, SecurityMode::Protected);
    let ready = server
        .receive(Frame::Text(client.hello().unwrap().into()))
        .unwrap()
        .outbound
        .unwrap();
    let mut value: Value = serde_json::from_slice(ready.as_bytes()).unwrap();
    value["capabilities"] = json!({"binaryCiphertext":false});
    assert_eq!(
        client
            .receive(Frame::Text(value.to_string()))
            .unwrap()
            .opened,
        Some(SecurityMode::Protected)
    );
    assert!(matches!(
        client.encode(Frame::Binary(vec![1])).unwrap(),
        Frame::Binary(_)
    ));
}

#[test]
fn legacy_retry_keeps_the_original_key_and_negotiation() {
    let (mut client, mut server, hello, ready) = open_pair(InitiatorOptions {
        offer_protected: false,
        ..Default::default()
    });
    assert_eq!(client.security_mode(), Some(SecurityMode::Legacy));
    let mut retry: Value = serde_json::from_str(&hello).unwrap();
    retry["capabilities"] = json!({"binaryCiphertext":false});
    assert_eq!(
        server
            .receive(Frame::Text(retry.to_string()))
            .unwrap()
            .outbound,
        Some(ready)
    );
    let message = Frame::Binary(b"still binary".to_vec());
    let wire = client.encode(message.clone()).unwrap();
    assert_eq!(server.receive(wire).unwrap().message, Some(message));
    retry["key"] = json!(KeyPair::from_secret([99; 32]).export_public_key());
    let error = server.receive(Frame::Text(retry.to_string())).unwrap_err();
    assert_eq!(error.close_code, 1008);
    assert_eq!(error.kind, CryptoError::KeyMismatch);
}

#[test]
fn legacy_unnegotiated_mode_retains_base64_binary_opcode_compatibility() {
    let (mut client, mut server, _, _) = open_pair(InitiatorOptions {
        offer_protected: false,
        binary_ciphertext: false,
        ..Default::default()
    });
    let text = client.encode(Frame::Text("old adapter".into())).unwrap();
    assert_eq!(
        server
            .receive(Frame::Binary(text.into_bytes()))
            .unwrap()
            .message,
        Some(Frame::Text("old adapter".into()))
    );
    let binary = client.encode(Frame::Binary(vec![0xff])).unwrap();
    assert_eq!(
        server.receive(binary).unwrap().message,
        Some(Frame::Binary(vec![0xff]))
    );
}

#[test]
fn modern_client_can_negotiate_with_a_legacy_only_responder() {
    let static_key = identity();
    let mut client =
        SecureChannel::initiator(static_key.public_key(), SecurityMode::Legacy).unwrap();
    let hello: Value = serde_json::from_str(client.hello().unwrap()).unwrap();
    let legacy_server = LegacyCipher::from_keypair(
        &static_key,
        &import_public_key(hello["key"].as_str().unwrap()).unwrap(),
    )
    .unwrap();
    let ready = Frame::Text(
        "{\"type\":\"e2ee_ready\",\"capabilities\":{\"binaryCiphertext\":true}}".into(),
    );
    assert_eq!(
        client.receive(ready).unwrap().opened,
        Some(SecurityMode::Legacy)
    );
    let outbound = client.encode(Frame::Text("legacy interop".into())).unwrap();
    let wire = STANDARD.decode(outbound.as_bytes()).unwrap();
    assert_eq!(legacy_server.decrypt(wire).unwrap(), b"legacy interop");
    let reply = legacy_server.encrypt(b"legacy response").unwrap();
    assert_eq!(
        client
            .receive(Frame::Text(STANDARD.encode(reply)))
            .unwrap()
            .message,
        Some(Frame::Text("legacy response".into()))
    );
}

#[test]
fn outbound_wire_length_accounts_for_utf8_base64_and_chunk_overhead() {
    let (client, _, _, _) = open_pair(Default::default());
    let text = Frame::Text("안녕".into());
    assert_eq!(
        client.outbound_wire_byte_length(&text).unwrap(),
        base64_length(6 + 24).unwrap()
    );
}

#[test]
fn client_rejects_authenticated_unoffered_binary_parameter() {
    let static_key = identity();
    let mut client = SecureChannel::initiator_with_options(
        static_key.public_key(),
        InitiatorOptions {
            binary_ciphertext: false,
            ..Default::default()
        },
    )
    .unwrap();
    let hello: Value = serde_json::from_str(client.hello().unwrap()).unwrap();
    let mut server = NoiseHandshake::responder_with_ephemeral(
        static_key,
        NOISE_PROLOGUE,
        Some(KeyPair::from_secret([17; 32])),
    );
    let message = STANDARD
        .decode(hello["protected"]["message"].as_str().unwrap())
        .unwrap();
    assert_eq!(server.read_message(&message).unwrap(), [1, 0]);
    let selected = server.write_message(&[1, 1]).unwrap();
    let ready =
        json!({"type":"e2ee_ready","protected":{"v":1,"message":STANDARD.encode(selected)}});
    let error = client.receive(Frame::Text(ready.to_string())).unwrap_err();
    assert_eq!(
        error.kind,
        CryptoError::Handshake("daemon selected an unoffered parameter")
    );
    assert_eq!(error.close_code, 4002);
    assert!(client.is_closed());
}

#[test]
fn protected_invalid_utf8_is_rejected_after_authentication() {
    let static_key = identity();
    let mut client = NoiseHandshake::initiator_with_ephemeral(
        static_key.public_key(),
        NOISE_PROLOGUE,
        Some(KeyPair::from_secret([18; 32])),
    );
    let mut server = SecureChannel::responder(static_key, SecurityMode::Protected);
    let message = client.write_message(&[1, 1]).unwrap();
    let hello = json!({"type":"e2ee_hello","key":"unused compatibility key","protected":{"v":1,"message":STANDARD.encode(message)}});
    let ready = server
        .receive(Frame::Text(hello.to_string()))
        .unwrap()
        .outbound
        .unwrap();
    let ready: Value = serde_json::from_slice(ready.as_bytes()).unwrap();
    let message = STANDARD
        .decode(ready["protected"]["message"].as_str().unwrap())
        .unwrap();
    assert_eq!(client.read_message(&message).unwrap(), [1, 1]);
    let mut keys = client.split().unwrap();
    let wire = encode_protected_record(FrameKind::Text, &[0xff], &mut keys.send).unwrap();
    let error = server
        .receive(Frame::Text(STANDARD.encode(wire)))
        .unwrap_err();
    assert_eq!(error.kind, CryptoError::InvalidUtf8);
    assert_eq!(error.close_code, 1011);
    assert!(server.is_closed());
}

#[test]
fn recorded_hello_cannot_replay_records_into_fresh_responder() {
    let (mut client, mut server, hello, _) = open_pair(Default::default());
    let record = client
        .encode(Frame::Binary(b"old session".to_vec()))
        .unwrap();
    assert!(server.receive(record.clone()).unwrap().message.is_some());
    let mut fresh = SecureChannel::responder(identity(), SecurityMode::Protected);
    assert_eq!(
        fresh.receive(Frame::Text(hello)).unwrap().opened,
        Some(SecurityMode::Protected)
    );
    assert_eq!(
        fresh.receive(record).unwrap_err().kind,
        CryptoError::Authentication
    );
    assert!(fresh.is_closed());
}

#[test]
fn authenticated_chunk_sizes_cannot_be_redistributed_between_chunks() {
    let (mut sender, mut receiver) = cipher_pair();
    let total = MAX_CHUNK_DATA_BYTES as u32 + 1;
    let mut wire = forged_chunk(&mut sender, 1, 1, total, &vec![0; MAX_CHUNK_DATA_BYTES - 1]);
    wire.extend(forged_chunk(&mut sender, 1, 2, total, &[0, 0]));
    assert_eq!(
        wire.len(),
        protected_record_byte_length(total as usize).unwrap()
    );
    assert_eq!(
        decode_protected_record(wire, &mut receiver).unwrap_err(),
        CryptoError::Record("chunk size mismatch")
    );
}

#[test]
fn oversized_received_wire_is_rejected_before_decryption_and_closes() {
    let (_, mut server, _, _) = open_pair(Default::default());
    let error = server
        .receive(Frame::Binary(vec![0; MAX_WIRE_BYTES + 1]))
        .unwrap_err();
    assert_eq!(error.kind, CryptoError::WireLimit);
    assert!(server.is_closed());
}

#[test]
fn premature_send_is_not_a_silent_queue_or_handshake_restart() {
    let mut client =
        SecureChannel::initiator(identity().public_key(), SecurityMode::Protected).unwrap();
    let hello = client.hello().unwrap().to_owned();
    assert_eq!(
        client
            .encode(Frame::Text("not yet open".into()))
            .unwrap_err()
            .kind,
        CryptoError::NotOpen
    );
    assert_eq!(client.hello(), Some(hello.as_str()));
    assert_eq!(
        client.receive(Frame::Binary(vec![0])).unwrap(),
        ChannelUpdate::default()
    );
    assert_eq!(
        client
            .receive(Frame::Text("unrelated traffic".into()))
            .unwrap(),
        ChannelUpdate::default()
    );
    client.close();
    assert!(client.hello().is_none());
    assert!(client.is_closed());
}
