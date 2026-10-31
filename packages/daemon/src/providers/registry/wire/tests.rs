use super::{
    super::catalog::{Snapshot, SnapshotRecord},
    codec,
    compat::Compatibility,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;

const GENERATED: &str = "2026-09-06T12:00:00.000Z";
const FETCHED: &str = "2026-09-06T10:00:00.000Z";

fn compatibility(references: bool) -> Compatibility {
    Compatibility {
        all_providers: false,
        custom_icons: false,
        compact: true,
        references,
    }
}
fn fixture() -> Snapshot {
    Snapshot {
        cwd: "paseo:global".into(),
        records: vec![
            Arc::new(SnapshotRecord {
                content_hash: "fixture".into(),
                entry: json!({
                    "provider": "codex", "enabled": true, "status": "ready", "fetchedAt": FETCHED,
                    "models": [{"provider": "codex", "id": "astra", "label": "Astra", "thinkingOptions": [{"id": "high", "label": "High"}], "defaultThinkingOptionId": "high"}],
                    "modes": [{"id": "default", "label": "Default", "icon": "Sparkles"}], "defaultModeId": null
                }),
            }),
            Arc::new(SnapshotRecord {
                content_hash: "hidden".into(),
                entry: json!({"provider": "custom", "enabled": true, "status": "loading"}),
            }),
        ],
    }
}
fn payload(snapshot: &Snapshot, compatibility: Compatibility, request: Option<&Value>) -> Value {
    codec::payload(snapshot, compatibility, request, GENERATED.into())
}

#[test]
fn compact_codec_shares_sets_and_preserves_omission_and_extension_fields() {
    let entries: Vec<Value> = serde_json::from_str(r#"[
      {"provider":"codex","models":[
        {"provider":"codex","id":"a","thinkingOptions":[{"id":"high"}],"defaultThinkingOptionId":"high","metadata":{"kept":true}},
        {"provider":"codex","id":"b","defaultThinkingOptionId":"other"},
        {"provider":"codex","id":"c","thinkingOptions":[],"defaultThinkingOptionId":null}]},
      {"provider":"custom","models":[{"provider":"custom","id":"d","thinkingOptions":[{"id":"high"}],"defaultThinkingOptionId":"high"}]},
      {"provider":"claude","status":"loading"},
      {"provider":"opencode","models":[]}
    ]"#).unwrap();
    let compact = codec::compact(entries);
    assert_eq!(
        compact,
        json!({
            "entries": [
                {"provider":"codex","models":[{"id":"a","metadata":{"kept":true},"thinkingSet":0},{"id":"b","defaultThinkingOptionId":"other"},{"id":"c","thinkingSet":1}]},
                {"provider":"custom","models":[{"id":"d","thinkingSet":0}]},
                {"provider":"claude","status":"loading"},
                {"provider":"opencode","models":[]}
            ],
            "thinkingSets":[{"options":[{"id":"high"}],"defaultOptionId":"high"},{"options":[],"defaultOptionId":null}]
        })
    );
    assert_eq!(
        crate::json::stringify(&compact["entries"][0]["models"][0]).unwrap(),
        r#"{"id":"a","metadata":{"kept":true},"thinkingSet":0}"#
    );
}

#[test]
fn thinking_set_deduplication_uses_javascript_stringification() {
    let entries = vec![serde_json::from_str(r#"{"provider":"codex","models":[
        {"provider":"codex","id":"a","thinkingOptions":[{"id":"n","metadata":{"10":1,"2":2,"value":1.0}}]},
        {"provider":"codex","id":"b","thinkingOptions":[{"id":"n","metadata":{"2":2,"10":1,"value":1}}]}
    ]}"#).unwrap()];
    let compact = codec::compact(entries);
    assert_eq!(compact["thinkingSets"].as_array().unwrap().len(), 1);
    assert_eq!(compact["entries"][0]["models"][1]["thinkingSet"], 0);
}

#[test]
fn reference_pushes_advertise_identity_and_pull_omits_embedded_freshness() {
    let snapshot = fixture();
    let view = compatibility(true);
    let pushed = payload(&snapshot, view, None);
    let exact_identity =
        r#"["paseo.providers-snapshot/1","references","legacy-icons",[["codex","fixture",null]]]"#;
    let expected_hash = URL_SAFE_NO_PAD.encode(Sha256::digest(exact_identity.as_bytes()));
    assert_eq!(
        pushed,
        json!({"generatedAt":GENERATED,"entries":[],"snapshotHash":expected_hash,"fetchedAt":{"codex":FETCHED}})
    );
    let pulled = payload(&snapshot, view, Some(&json!({})));
    assert_eq!(pulled["snapshotHash"], pushed["snapshotHash"]);
    assert_eq!(
        pulled["compactSnapshot"]["entries"][0]["modes"][0]["icon"],
        "ShieldCheck"
    );
    assert_eq!(
        pulled["compactSnapshot"]["entries"][0]["defaultModeId"],
        Value::Null
    );
    assert!(
        pulled["compactSnapshot"]["entries"][0]
            .get("fetchedAt")
            .is_none()
    );
    let hit = payload(&snapshot, view, Some(&json!({"ifNoneMatch":expected_hash})));
    assert_eq!(hit["notModified"], true);
    assert!(hit.get("compactSnapshot").is_none());
    assert_eq!(hit["fetchedAt"]["codex"], FETCHED);
}

#[test]
fn freshness_changes_emit_but_only_embedded_hashes_change() {
    let before = fixture();
    let mut after = fixture();
    after.cwd = "/workspace".into();
    Arc::get_mut(&mut after.records[0]).unwrap().entry["fetchedAt"] =
        json!("2026-09-06T10:01:00.000Z");
    for references in [false, true] {
        let view = compatibility(references);
        assert!(!codec::same_records(&before, &after, view));
        let first = payload(&before, view, Some(&json!({})));
        let next = payload(
            &after,
            view,
            Some(&json!({"ifNoneMatch":first["snapshotHash"]})),
        );
        assert_eq!(next["snapshotHash"] == first["snapshotHash"], references);
        assert_eq!(next["notModified"] == true, references);
        assert_eq!(next["cwd"], "/workspace");
        if !references {
            assert!(next.get("fetchedAt").is_none());
            assert_eq!(
                next["compactSnapshot"]["entries"][0]["fetchedAt"],
                after.records[0].entry["fetchedAt"]
            );
        }
    }
    let embedded = payload(&before, compatibility(false), None);
    let exact_identity = r#"["paseo.providers-snapshot/1","embedded","legacy-icons",[["codex","fixture","2026-09-06T10:00:00.000Z"]]]"#;
    assert_eq!(
        embedded["snapshotHash"],
        URL_SAFE_NO_PAD.encode(Sha256::digest(exact_identity.as_bytes()))
    );
    assert!(embedded.get("compactSnapshot").is_some());
}

#[test]
fn hidden_changes_do_not_invalidate_and_icon_policy_changes_identity() {
    let before = fixture();
    let mut after = fixture();
    Arc::get_mut(&mut after.records[1]).unwrap().content_hash = "changed".into();
    Arc::get_mut(&mut after.records[1]).unwrap().entry["fetchedAt"] = json!(FETCHED);
    assert!(codec::same_records(&before, &after, compatibility(true)));
    let first = payload(&before, compatibility(true), None);
    assert_eq!(
        first["snapshotHash"],
        payload(&after, compatibility(true), None)["snapshotHash"]
    );
    let icons = Compatibility {
        custom_icons: true,
        ..compatibility(true)
    };
    let visible = Compatibility {
        all_providers: true,
        ..compatibility(true)
    };
    assert_ne!(
        first["snapshotHash"],
        payload(&before, icons, None)["snapshotHash"]
    );
    assert_ne!(
        first["snapshotHash"],
        payload(&before, visible, None)["snapshotHash"]
    );
    assert!(!codec::same_records(&before, &after, visible));
    assert_eq!(
        payload(&before, icons, Some(&json!({})))["compactSnapshot"]["entries"][0]["modes"][0]["icon"],
        "Sparkles"
    );
}

#[test]
fn references_without_compact_keep_legacy_body_and_ignore_conditional_hashes() {
    let snapshot = fixture();
    let view = Compatibility {
        compact: false,
        ..compatibility(true)
    };
    let result = payload(&snapshot, view, Some(&json!({"ifNoneMatch":"anything"})));
    assert_eq!(result["entries"].as_array().unwrap().len(), 1);
    assert_eq!(result["entries"][0]["fetchedAt"], FETCHED);
    assert_eq!(result["entries"][0]["models"][0]["provider"], "codex");
    assert_eq!(result["entries"][0]["modes"][0]["icon"], "ShieldCheck");
    for absent in [
        "snapshotHash",
        "notModified",
        "compactSnapshot",
        "fetchedAt",
        "cwd",
    ] {
        assert!(result.get(absent).is_none());
    }
}

#[test]
fn record_equality_retains_order_and_missing_versus_null_freshness() {
    let before = fixture();
    let mut after = fixture();
    let view = Compatibility {
        all_providers: true,
        ..compatibility(true)
    };
    after.records.reverse();
    assert!(!codec::same_records(&before, &after, view));
    assert_ne!(
        payload(&before, view, None)["snapshotHash"],
        payload(&after, view, None)["snapshotHash"]
    );
    after.records.reverse();
    Arc::get_mut(&mut after.records[1]).unwrap().entry["fetchedAt"] = Value::Null;
    assert!(!codec::same_records(&before, &after, view));
    assert_eq!(
        payload(&before, view, None)["snapshotHash"],
        payload(&after, view, None)["snapshotHash"]
    );
}
