use super::{
    refresh, require_ready,
    state::{Binding, Catalog, GLOBAL, Load, identify, same_records},
};
use serde_json::json;
use std::{collections::HashMap, sync::Arc};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

fn waiting() -> watch::Receiver<bool> {
    watch::channel(false).1
}

#[test]
fn refresh_intent_survives_key_lookup_supersession() {
    let first = Binding::superseding(1, None, true, waiting());
    let second = Binding::superseding(2, Some(&first), false, waiting());
    let third = Binding::superseding(3, Some(&second), false, waiting());
    assert!(third.force);
    let mut resolved = third;
    resolved.force = false;
    resolved.key = Some("host".to_owned());
    assert!(!Binding::superseding(4, Some(&resolved), false, waiting()).force);
}

#[test]
fn failed_identity_cannot_publish_the_previous_shared_success() {
    let initial = identify(json!({"provider":"codex","status":"loading","enabled":true}));
    let mut binding = Binding::superseding(1, None, true, waiting());
    binding.key = Some("previous-account".to_owned());
    binding.fail_key(&initial, "identity unavailable");
    assert!(binding.key.is_none());
    assert!(!binding.force);
    let failure = binding.failure.as_ref().unwrap();
    assert_eq!(failure.entry["status"], "error");
    assert_eq!(failure.entry["error"], "identity unavailable");
    assert!(failure.entry.get("fetchedAt").is_none());
    let pending = Binding::superseding(2, Some(&binding), false, waiting());
    assert!(Arc::ptr_eq(pending.failure.as_ref().unwrap(), failure));
}

#[test]
fn caches_reuse_all_completed_outcomes_without_a_ttl() {
    for status in ["ready", "unavailable", "error"] {
        let mut catalog = Catalog {
            result: Some(identify(json!({"provider":"codex","status":status}))),
            ..Catalog::default()
        };
        assert!(catalog.reusable(false));
        assert!(!catalog.reusable(true));
        catalog.stale = true;
        assert!(!catalog.reusable(false));
        catalog.load = Some(Load {
            id: 1,
            done: waiting(),
            cancel: CancellationToken::new(),
        });
        assert!(catalog.reusable(false));
        assert!(!catalog.reusable(true));
    }
}

#[test]
fn shared_key_never_shares_across_provider_ids() {
    let mut catalogs: HashMap<String, HashMap<String, Catalog>> = HashMap::new();
    let shared = catalogs.entry("host".to_owned()).or_default();
    shared.insert(
        "claude".to_owned(),
        Catalog {
            result: Some(identify(json!({"provider":"claude","status":"ready"}))),
            ..Catalog::default()
        },
    );
    assert!(catalogs["host"].get("codex").is_none());
    assert!(catalogs["host"]["claude"].reusable(false));
}

#[test]
fn freshness_changes_snapshot_identity_but_not_content_identity() {
    let first = identify(
        json!({"provider":"codex","status":"ready","fetchedAt":"2026-01-01T00:00:00.000Z","models":[]}),
    );
    let second = identify(
        json!({"provider":"codex","status":"ready","fetchedAt":"2026-01-02T00:00:00.000Z","models":[]}),
    );
    assert_eq!(first.content_hash, second.content_hash);
    assert!(!same_records(&[first.clone()], &[second]));
    let unchanged = identify(first.entry.clone());
    assert!(same_records(&[first], &[unchanged]));
}

#[test]
fn content_hash_preserves_js_object_order_and_number_semantics() {
    let first = identify(
        serde_json::from_str(r#"{"provider":"x","metadata":{"10":10,"2":2,"z":9007199254740993}}"#)
            .unwrap(),
    );
    let equivalent = identify(
        serde_json::from_str(r#"{"provider":"x","metadata":{"2":2,"10":10,"z":9007199254740992}}"#)
            .unwrap(),
    );
    assert_eq!(first.content_hash, equivalent.content_hash);
    let reordered = identify(
        serde_json::from_str(r#"{"metadata":{"2":2,"10":10,"z":9007199254740992},"provider":"x"}"#)
            .unwrap(),
    );
    assert_ne!(first.content_hash, reordered.content_hash);
}

#[test]
fn deadline_uses_config_then_js_numeric_environment_then_default() {
    assert_eq!(refresh::deadline_with_env(Some(&json!(5)), Some("1")), 5);
    assert_eq!(
        refresh::deadline_with_env(Some(&json!(0)), Some("0x10")),
        16
    );
    assert_eq!(refresh::deadline_with_env(None, Some(" 1e3 ")), 1_000);
    assert_eq!(refresh::deadline_with_env(None, Some("0b100")), 4);
    for invalid in ["0", "-1", "1.5", "Infinity", "2147483648", ""] {
        assert_eq!(refresh::deadline_with_env(None, Some(invalid)), 120_000);
    }
}

#[test]
fn global_scope_is_not_the_home_workspace() {
    assert_eq!(refresh::target(None).unwrap(), GLOBAL);
    assert_eq!(
        refresh::fetch_options(GLOBAL, true),
        json!({"scope":"global","force":true})
    );
    assert_eq!(
        refresh::fetch_options("/workspace", false),
        json!({"scope":"workspace","cwd":"/workspace","force":false})
    );
}

#[test]
fn creation_reads_require_an_enabled_ready_entry() {
    let message = |entry| require_ready("codex", entry).unwrap_err().to_string();
    assert_eq!(
        message(json!({"provider":"codex","enabled":false,"status":"unavailable"})),
        "Provider 'codex' is disabled"
    );
    assert_eq!(
        message(json!({"provider":"codex","enabled":true,"status":"error","error":"login required"})),
        "login required"
    );
    assert_eq!(
        message(json!({"provider":"codex","enabled":true,"status":"error"})),
        "Failed to load provider 'codex'"
    );
    for status in ["loading", "unavailable"] {
        assert_eq!(
            message(json!({"provider":"codex","enabled":true,"status":status})),
            "Provider 'codex' is not available"
        );
    }
    let ready = json!({"provider":"codex","enabled":true,"status":"ready","modes":[]});
    assert_eq!(require_ready("codex", ready.clone()).unwrap(), ready);
}
