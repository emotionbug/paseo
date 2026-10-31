use super::{compat, decode_handle, decode_id, encode_id, persistence};
use serde_json::{Value, json};

#[test]
fn plugin_handles_survive_native_cutover_without_identity_changes() {
    let id = "remote/session-\"one\"";
    let encoded = encode_id(id);
    assert_eq!(
        encoded,
        "plugin:{\"version\":1,\"data\":{\"sessionId\":\"remote/session-\\\"one\\\"\"}}"
    );
    assert_eq!(decode_id(&encoded).unwrap()["data"]["sessionId"], id);
    assert_eq!(
        decode_handle("private-hoplite", &persistence("private-hoplite", id)).unwrap()["data"]["sessionId"],
        id
    );
    assert!(decode_handle("other", &persistence("private-hoplite", id)).is_err());
    assert!(decode_id("native-session-id").is_err());
    assert!(decode_id("plugin:{\"version\":1,\"data\":{},\"extra\":1}").is_err());
}

#[test]
fn source_catalog_uses_config_options_not_legacy_models() {
    let native = json!({"models":{"availableModels":[{"modelId":"wrong-legacy-model"}]},"modes":{"currentModeId":"code","availableModes":[{"id":"code","name":"Code"}]},"configOptions":[
        {"id":"model-selector","category":"model","type":"select","currentValue":"model-b","options":[{"group":"group-id","name":"Group","options":[{"value":"model-a","name":"A"},{"value":"model-b","name":"B"}]}]},
        {"id":"thinking-selector","category":"thought_level","type":"select","currentValue":"deep","options":[{"value":"deep","name":"Deep"}]},
        {"id":"approval","name":"Approval","type":"boolean","currentValue":true},
        {"id":"region","name":"Region","type":"select","currentValue":"eu","options":[{"value":"eu","name":"Europe"}]}
    ]});
    let catalog = compat::catalog(&native, "custom-hoplite");
    assert_eq!(catalog["models"].as_array().unwrap().len(), 2);
    assert_eq!(catalog["models"][1]["id"], "model-b");
    assert_eq!(catalog["models"][1]["provider"], "custom-hoplite");
    assert_eq!(catalog["models"][1]["isDefault"], true);
    assert_eq!(catalog["models"][1]["defaultThinkingOptionId"], "deep");
    let features =
        compat::generic_features(&native, &json!({"featureValues":{"auto_accept":true}}));
    assert_eq!(features.len(), 2);
    assert_eq!(features[0]["type"], "toggle");
    assert_eq!(
        features[1]["options"][0],
        json!({"id":"eu","value":"eu","label":"Europe"})
    );
    let (method, params) = compat::config_request(
        &native,
        "session",
        "setting",
        Some("approval"),
        json!(false),
    )
    .unwrap();
    assert_eq!(method, "session/set_config_option");
    assert_eq!(
        params,
        json!({"sessionId":"session","configId":"approval","type":"boolean","value":false})
    );
    let (method, params) =
        compat::config_request(&native, "session", "model", None, json!("model-b")).unwrap();
    assert_eq!(method, "session/set_config_option");
    assert_eq!(params["configId"], "model-selector");
    assert!(compat::config_request(&native, "session", "model", None, Value::Null).is_err());
}

#[test]
fn workspace_phases_are_strict_and_terminalize_with_the_turn() {
    assert!(
        compat::transform_notification(
            "_hoplite/run_phase",
            &json!({"runId":"run","phase":"provision"})
        )
        .is_none()
    );
    assert!(
        compat::transform_notification(
            "_hoplite/run_phase",
            &json!({"runId":"","phase":"provision","label":null})
        )
        .is_none()
    );
    let mut projection = compat::Projection::default();
    let events = projection
        .project(
            "_hoplite/run_phase",
            &json!({"runId":"run","phase":"provision","label":null}),
            false,
        )
        .unwrap();
    assert_eq!(events[0]["item"]["callId"], "hoplite-run-phase:run");
    assert_eq!(events[0]["item"]["detail"]["label"], "provision");
    let final_events = projection.finish("canceled");
    assert_eq!(final_events[0]["item"]["status"], "canceled");
    assert!(final_events[0]["item"]["error"].is_null());
    assert!(projection.finish("completed").is_empty());
    let events = projection
        .project(
            "_hoplite/run_phase",
            &json!({"runId":"run2","phase":null,"label":"ignored"}),
            false,
        )
        .unwrap();
    assert_eq!(events[0]["item"]["detail"]["label"], "Workspace ready");
    assert!(projection.finish("completed").is_empty());
}

#[test]
fn chunk_boundaries_and_replayed_user_snapshots_follow_plugin_semantics() {
    let mut projection = compat::Projection::default();
    let mut chunk = |kind: &str, text: &str, replay: bool| {
        projection
            .project(
                "session/update",
                &json!({"update":{"sessionUpdate":kind,"content":{"type":"text","text":text}}}),
                replay,
            )
            .unwrap()
    };
    let a = chunk("agent_message_chunk", "hello", false);
    let b = chunk("agent_message_chunk", " world", false);
    assert_eq!(a[0]["item"]["messageId"], b[0]["item"]["messageId"]);
    assert_eq!(b[0]["item"]["text"], " world");
    chunk("agent_thought_chunk", "thinking", false);
    let c = chunk("agent_message_chunk", "new", false);
    assert_ne!(a[0]["item"]["messageId"], c[0]["item"]["messageId"]);
    assert!(chunk("user_message_chunk", "live duplicate", false).is_empty());
    let a = chunk("user_message_chunk", "old ", true);
    let b = chunk("user_message_chunk", "question", true);
    assert_eq!(a[0]["item"]["messageId"], b[0]["item"]["messageId"]);
    assert_eq!(b[0]["item"]["text"], "old question");
}

#[test]
fn permission_selection_must_match_action_and_behavior() {
    let options = json!([{"optionId":"allow-always","kind":"allow_always"},{"optionId":"allow-once","kind":"allow_once"},{"optionId":"reject","kind":"reject_once"}]);
    assert_eq!(
        compat::permission_option(&options, &json!({"behavior":"allow"}))
            .unwrap()
            .unwrap()["optionId"],
        "allow-always"
    );
    assert!(
        compat::permission_option(
            &options,
            &json!({"behavior":"deny","selectedActionId":"allow-once"})
        )
        .is_err()
    );
    assert_eq!(
        compat::permission_option(
            &options,
            &json!({"behavior":"deny","selectedActionId":"reject"})
        )
        .unwrap()
        .unwrap()["optionId"],
        "reject"
    );
}

#[test]
fn failed_turn_finishes_tools_with_non_null_errors() {
    let mut projection = compat::Projection::default();
    projection.project("session/update",&json!({"update":{"sessionUpdate":"tool_call","toolCallId":"edit","name":"Edit","kind":"edit","rawInput":{"filePath":"a.rs","oldString":"old","newString":"new"},"status":"in_progress"}}),false).unwrap();
    let events = projection.finish("failed");
    assert_eq!(events[0]["item"]["detail"]["type"], "edit");
    assert_eq!(events[0]["item"]["status"], "failed");
    assert_eq!(events[0]["item"]["error"], json!({"message":"Turn failed"}));
    assert!(projection.finish("failed").is_empty());
}
