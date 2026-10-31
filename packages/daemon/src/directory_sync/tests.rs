use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::*;

fn id(value: &Value) -> Result<&str> {
    value
        .get("id")
        .and_then(Value::as_str)
        .context("missing id")
}

fn collection_read(read: CollectionRead<'_>) -> Value {
    serde_json::to_value(read).unwrap()
}

#[test]
fn latest_state_deduplicates_and_retains_opaque_fields() {
    let mut collection = VersionedCollection::new(id);
    assert_eq!(
        collection
            .replace(json!({"id":"one","title":"first"}))
            .unwrap(),
        Some(1)
    );
    collection
        .replace(json!({"id":"one","title":"second"}))
        .unwrap();
    let latest = json!({"id":"one","title":"latest","provider":{"opaque":[null,{"future":true}]}});
    collection.replace(latest.clone()).unwrap();
    assert_eq!(collection.replace(latest.clone()).unwrap(), None);
    collection.replace(json!({"id":"two"})).unwrap();
    assert_eq!(
        collection_read(collection.read_after(1)),
        json!({
            "mode":"changes","headSeq":4,"values":[
                {"seq":3,"value":latest}, {"seq":4,"value":{"id":"two"}}
            ],"removals":[]
        })
    );
}

#[test]
fn repeated_and_unknown_removals_do_not_advance() {
    let mut collection = VersionedCollection::new(id);
    assert_eq!(collection.remove("unknown").unwrap(), None);
    collection.replace(json!({"id":"one"})).unwrap();
    assert_eq!(collection.remove("one").unwrap(), Some(2));
    assert_eq!(collection.remove("one").unwrap(), None);
    assert_eq!(collection.sequence_for("one"), Some(2));
    assert_eq!(collection.head_seq(), 2);
    collection.replace(json!({"id":"one","new":null})).unwrap();
    assert_eq!(
        collection_read(collection.read_after(1)),
        json!({
            "mode":"changes","headSeq":3,"values":[{"seq":3,"value":{"id":"one","new":null}}],"removals":[]
        })
    );
}

#[test]
fn tombstone_expiration_boundary_is_exclusive() {
    let mut collection = VersionedCollection::with_options(id, None, 1);
    collection.replace(json!({"id":"one"})).unwrap();
    collection.replace(json!({"id":"two"})).unwrap();
    collection.remove("one").unwrap();
    collection.remove("two").unwrap();
    assert_eq!(
        collection_read(collection.read_after(2)),
        json!({
            "mode":"snapshot","reason":"cursor_expired","headSeq":4,"values":[],"removals":[]
        })
    );
    assert_eq!(
        collection_read(collection.read_after(3)),
        json!({
            "mode":"changes","headSeq":4,"values":[],"removals":[{"id":"two","seq":4}]
        })
    );
    assert_eq!(collection.read_after(5).reason, Some("cursor_expired"));
    assert_eq!(collection.sequence_for("one"), None);
}

#[test]
fn default_retains_exactly_five_thousand_tombstones() {
    let mut collection = VersionedCollection::new(id);
    for number in 0..=DEFAULT_MAX_TOMBSTONES {
        let key = number.to_string();
        collection.replace(json!({"id":key})).unwrap();
        collection.remove(&key).unwrap();
    }
    assert_eq!(collection.sequence_for("0"), None);
    assert_eq!(collection.sequence_for("1"), Some(4));
    assert_eq!(collection.read_after(1).reason, Some("cursor_expired"));
    let read = collection.read_after(2);
    assert_eq!(read.mode, "changes");
    assert_eq!(read.removals.len(), DEFAULT_MAX_TOMBSTONES);
    assert_eq!(read.removals.first().unwrap().seq, 4);
}

#[test]
fn resurrection_does_not_expire_a_retained_cursor() {
    let mut collection = VersionedCollection::with_options(id, None, 1);
    collection.replace(json!({"id":"one"})).unwrap();
    collection.remove("one").unwrap();
    collection.replace(json!({"id":"one"})).unwrap();
    collection.replace(json!({"id":"two"})).unwrap();
    collection.remove("two").unwrap();
    assert_eq!(collection.read_after(0).mode, "changes");
    assert_eq!(collection.read_after(0).removals.len(), 1);
}

#[test]
fn zero_tombstone_limit_expires_immediately() {
    let mut collection = VersionedCollection::with_options(id, None, 0);
    collection.replace(json!({"id":"one"})).unwrap();
    collection.remove("one").unwrap();
    assert_eq!(collection.read_after(1).reason, Some("cursor_expired"));
    assert_eq!(collection.read_after(2).mode, "changes");
}

#[test]
fn replace_all_uses_original_map_insertion_order_for_removals() {
    let mut collection = VersionedCollection::new(id);
    collection
        .replace_all(vec![json!({"id":"z"}), json!({"id":"a"})])
        .unwrap();
    collection
        .replace(json!({"id":"z","changed":true}))
        .unwrap();
    collection.replace_all(vec![]).unwrap();
    assert_eq!(
        collection_read(collection.read_after(3))["removals"],
        json!([
            {"id":"z","seq":4},{"id":"a","seq":5}
        ])
    );
}

#[test]
fn custom_fingerprint_controls_unchanged_projection() {
    let mut collection =
        VersionedCollection::with_options(id, Some(|value| value["title"].to_string()), 1);
    collection
        .replace(json!({"id":"one","title":"same","noise":1}))
        .unwrap();
    assert_eq!(
        collection
            .replace(json!({"id":"one","title":"same","noise":2}))
            .unwrap(),
        None
    );
    assert_eq!(
        collection.read_snapshot("no_cursor").values[0].value["noise"],
        1
    );
}

#[test]
fn fingerprint_matches_javascript_object_iteration_and_number_semantics() {
    let parse = |source: &str| serde_json::from_str::<Value>(source).unwrap();
    assert!(same_json_fingerprint(
        &parse(r#"{"9":1,"2":2,"a":3}"#),
        &parse(r#"{"2":2,"a":3,"9":1}"#)
    ));
    assert!(!same_json_fingerprint(
        &parse(r#"{"a":1,"b":2}"#),
        &parse(r#"{"b":2,"a":1}"#)
    ));
    assert!(!same_json_fingerprint(
        &parse(r#"{"01":1,"a":2}"#),
        &parse(r#"{"a":2,"01":1}"#)
    ));
    assert!(!same_json_fingerprint(
        &parse(r#"{"4294967295":1,"a":2}"#),
        &parse(r#"{"a":2,"4294967295":1}"#)
    ));
    assert!(same_json_fingerprint(&parse("-0.0"), &parse("0")));
    assert!(same_json_fingerprint(&parse("1.0"), &parse("1")));
    assert!(same_json_fingerprint(
        &parse("9007199254740992"),
        &parse("9007199254740993")
    ));
    assert!(!same_json_fingerprint(&json!({}), &json!({"opaque":null})));
}

#[test]
fn generations_and_cursors_have_exact_snapshot_reasons() {
    let service = DirectorySyncService::with_generation("generation");
    let snapshot = vec![json!({"projectId":"one","opaque":null})];
    let read = service
        .synchronize_projects(snapshot.clone(), &json!({}))
        .unwrap();
    assert_eq!(
        read["sync"],
        json!({"generation":"generation","headSeq":1,"mode":"snapshot","reason":"no_cursor","removals":[]})
    );
    for (cursor, reason) in [
        (
            json!({"generation":"old","afterSeq":1}),
            "generation_changed",
        ),
        (json!({"generation":"generation"}), "no_cursor"),
        (
            json!({"generation":"generation","afterSeq":2}),
            "cursor_expired",
        ),
    ] {
        assert_eq!(
            service
                .synchronize_projects(snapshot.clone(), &cursor)
                .unwrap()["sync"]["reason"],
            reason
        );
    }
    assert_eq!(
        service
            .synchronize_projects(snapshot, &json!({"generation":"generation","afterSeq":1}))
            .unwrap(),
        json!({
            "projects":[],"sync":{"generation":"generation","headSeq":1,"mode":"changes","removals":[]}
        })
    );
}

#[test]
fn background_generation_shares_sequences_without_false_removals() {
    let service = DirectorySyncService::with_generation("generation");
    let snapshot = vec![
        json!({"id":"public"}),
        json!({"id":"background","background":true}),
    ];
    service
        .synchronize_workspaces(snapshot.clone(), &json!({}), false)
        .unwrap();
    let read = service
        .synchronize_workspaces(
            snapshot.clone(),
            &json!({"generation":"generation","afterSeq":2}),
            true,
        )
        .unwrap();
    assert_eq!(
        read["sync"],
        json!({"generation":"generation:background","headSeq":2,"mode":"snapshot","reason":"generation_changed","removals":[]})
    );
    assert_eq!(read["entries"].as_array().unwrap().len(), 2);
    let changes = service
        .synchronize_workspaces(
            snapshot,
            &json!({"generation":"generation:background","afterSeq":2}),
            true,
        )
        .unwrap();
    assert_eq!(changes["entries"], json!([]));
    assert_eq!(changes["sync"]["mode"], "changes");
}

#[test]
fn filtered_update_versions_use_full_source_projection() {
    let service = DirectorySyncService::with_generation("generation");
    let full =
        json!({"agent":{"id":"one","opaque":{"future":null}},"project":{"projectKey":"project"}});
    let remove_for_observer = json!({"kind":"remove","agentId":"one"});
    let update = service
        .sequence_agent_update(
            remove_for_observer.clone(),
            Some(full.clone()),
            "one",
            true,
            true,
        )
        .unwrap();
    assert_eq!(
        update,
        json!({"kind":"remove","agentId":"one","generation":"generation:background","seq":1})
    );
    let read = service
        .synchronize_agents(vec![full], &json!({}), false)
        .unwrap();
    assert_eq!(read["entries"][0]["syncSeq"], 1);
    assert_eq!(read["sync"]["removals"], json!([]));
    let legacy = service
        .sequence_agent_update(remove_for_observer.clone(), None, "one", false, false)
        .unwrap();
    assert_eq!(legacy, remove_for_observer);
    let repeated = service
        .sequence_agent_update(remove_for_observer, None, "one", true, false)
        .unwrap();
    assert_eq!(repeated["seq"], 2);
}

#[test]
fn project_removals_and_effective_icon_changes_are_sequenced_once() {
    let service = DirectorySyncService::with_generation("generation");
    let initial = json!({"projectId":"one","projectIconRevision":"a"});
    let unchanged = json!({"kind":"upsert","project":initial});
    assert_eq!(
        service
            .sequence_project_update(unchanged.clone(), true)
            .unwrap()["seq"],
        1
    );
    assert_eq!(
        service.sequence_project_update(unchanged, true).unwrap()["seq"],
        1
    );
    assert_eq!(
        service
            .sequence_project_update(
                json!({"kind":"upsert","project":{"projectId":"one","projectIconRevision":"b"}}),
                true
            )
            .unwrap()["seq"],
        2
    );
    let remove = json!({"kind":"remove","projectId":"one"});
    assert_eq!(
        service
            .sequence_project_update(remove.clone(), true)
            .unwrap()["seq"],
        3
    );
    assert_eq!(
        service.sequence_project_update(remove, true).unwrap()["seq"],
        3
    );
}

#[test]
fn source_filters_preserve_background_internal_archive_and_thinking_rules() {
    let background =
        json!({"id":"workspace","name":"Name","projectId":"project","background":true});
    assert!(!matches_workspace_filter(&background, None));
    assert!(matches_workspace_filter(
        &background,
        Some(&json!({"query":"workspace"}))
    ));
    assert!(!matches_workspace_filter(
        &background,
        Some(&json!({"query":" workspace "}))
    ));
    assert!(matches_workspace_filter(
        &background,
        Some(&json!({"includeBackground":true,"query":"NAME","projectId":" project "}))
    ));
    let agent = json!({"id":"one","workspaceId":"workspace","status":"running","labels":{"key":"value"},"thinkingOptionId":"high","runtimeInfo":{"thinkingOptionId":null}});
    let project = json!({"projectKey":"project"});
    assert!(!agent_directory_includes(&agent, Some(&background), false));
    assert!(agent_directory_includes(&agent, None, false));
    assert!(matches_agent_updates_filter(
        &agent,
        &project,
        Some(
            &json!({"thinkingOptionId":null,"labels":{"key":"value"},"statuses":["running"],"requiresAttention":false,"projectKeys":[" ","project"]})
        )
    ));
    assert!(!matches_agent_updates_filter(
        &agent,
        &project,
        Some(&json!({"thinkingOptionId":"high"}))
    ));
    assert!(!matches_agent_updates_filter(
        &json!({"internal":true}),
        &project,
        None
    ));
    assert!(!matches_agent_updates_filter(
        &json!({"archivedAt":"2026-01-01"}),
        &project,
        None
    ));
    assert!(matches_agent_updates_filter(
        &json!({"internal":true,"archivedAt":"2026-01-01"}),
        &project,
        Some(&json!({"includeInternal":true,"includeArchived":true}))
    ));
}
