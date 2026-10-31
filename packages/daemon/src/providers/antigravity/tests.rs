// Behavioral fixtures are the original provider's sanitized native CLI captures.
use super::{
    catalog,
    wire::{self, Frame, Turn},
};
use serde_json::{Value, json};
use std::collections::HashMap;

fn frames(capture: &str) -> Vec<Frame> {
    capture
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let record: Value = serde_json::from_str(line).unwrap();
            wire::decode(&record["data"].to_string()).unwrap()
        })
        .collect()
}
macro_rules! capture {
    ($name:literal) => {
        include_str!(concat!(
            "../../../../../plugins/antigravity-provider/test/fixtures/",
            $name,
            ".stdout.ndjson"
        ))
    };
}
#[test]
fn native_text_projects_deltas_not_duplicate_snapshots() {
    let mut turn = Turn::new();
    let mut events = Vec::new();
    for frame in frames(capture!("text-turn")) {
        if let Frame::Step { step_update } = frame {
            events.extend(turn.step(step_update, "custom-agy"));
        }
    }
    let text: Vec<_> = events
        .iter()
        .filter_map(|e| e.pointer("/item/text").and_then(Value::as_str))
        .collect();
    assert_eq!(text, ["PHASE0_HELLO", "\n"]);
    assert!(events.iter().all(|e| e["provider"] == "custom-agy"));
    let usages: Vec<_> = events
        .iter()
        .filter(|e| e["type"] == "usage_updated")
        .collect();
    assert_eq!(usages.len(), 1);
    assert_eq!(usages[0]["usage"]["inputTokens"].as_f64(), Some(11235.));
}
#[test]
fn done_usage_is_counted_once_per_generation_step() {
    let input = json!({"event":"step_update","step_update":{"conversation_id":"native","step_index":2,
        "state":"DONE","step_type":"agent_response","usage":{"input_tokens":9,"output_tokens":3,"cache_read_tokens":4}}});
    let mut turn = Turn::new();
    let mut events = Vec::new();
    for _ in 0..2 {
        if let Frame::Step { step_update } = wire::decode(&input.to_string()).unwrap() {
            events.extend(turn.step(step_update, "antigravity"));
        }
    }
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["usage"]["cachedInputTokens"].as_f64(), Some(4.));
    let mut next = input;
    next["step_update"]["step_index"] = json!(5);
    if let Frame::Step { step_update } = wire::decode(&next.to_string()).unwrap() {
        let events = turn.step(step_update, "antigravity");
        assert_eq!(events[0]["usage"]["inputTokens"].as_f64(), Some(18.));
    }
}
#[test]
fn native_tool_and_child_details_survive_projection() {
    let mut turn = Turn::new();
    let mut events = Vec::new();
    for capture in [capture!("tools-default"), capture!("subagent")] {
        for frame in frames(capture) {
            if let Frame::Step { step_update } = frame {
                events.extend(turn.step(step_update, "antigravity"));
            }
        }
    }
    for kind in ["read", "write", "shell", "sub_agent"] {
        assert!(
            events.iter().any(|e| e["item"]["detail"]["type"] == kind),
            "missing {kind}"
        );
    }
    assert!(
        events.iter().any(
            |e| e["item"]["detail"]["childSessionId"] == "b6f02329-7415-49a7-a9a9-1cd09e898d26"
        )
    );
    assert!(
        events
            .iter()
            .any(|e| e["item"]["name"] == "run_command" && e["item"]["status"] == "failed")
    );
}
#[test]
fn running_tools_settle_in_native_insertion_order() {
    let mut turn = Turn::new();
    for index in [2, 10, 1] {
        let value = json!({"event":"step_update","step_update":{"conversation_id":"c","step_index":index,
            "state":"ACTIVE","step_type":"tool","tool_name":"run_command"}});
        if let Frame::Step { step_update } = wire::decode(&value.to_string()).unwrap() {
            turn.step(step_update, "antigravity");
        }
    }
    let finished = turn.finish("antigravity", "canceled");
    let ids: Vec<_> = finished
        .iter()
        .map(|e| {
            e["item"]["callId"]
                .as_str()
                .unwrap()
                .rsplit(':')
                .next()
                .unwrap()
        })
        .collect();
    assert_eq!(ids, ["2", "10", "1"]);
    assert!(
        finished
            .iter()
            .all(|e| e["item"]["status"] == "canceled" && e["item"]["error"].is_null())
    );
}
#[test]
fn replayed_denials_are_counted_not_reannounced() {
    let mut reported = HashMap::new();
    let mut notices = Vec::new();
    for frame in frames(capture!("denial-replay")) {
        if let Frame::Result { result } = frame {
            if let Some(title) = wire::denial_title(&result.denied_actions, &mut reported) {
                notices.push(title);
            }
        }
    }
    assert_eq!(notices, ["Shell command denied"]);
    let actions = || {
        (0..3)
            .map(|_| wire::Denial {
                action: "command".into(),
                display_name: "sensitive-command".into(),
            })
            .collect::<Vec<_>>()
    };
    reported.clear();
    assert_eq!(
        wire::denial_title(&actions(), &mut reported).as_deref(),
        Some("3 actions denied")
    );
    assert_eq!(wire::denial_title(&actions(), &mut reported), None);
}
#[test]
fn invalid_frames_fail_without_success_shaped_defaults() {
    for value in [
        json!({"event":"init","conversation_id":"","init":{}}),
        json!({"event":"result","result":{"conversation_id":"c","status":"OTHER"}}),
        json!({"event":"step_update","step_update":{"conversation_id":"c","step_index":0,"state":"DONE","step_type":"agent_response","usage":{"input_tokens":-1,"output_tokens":0}}}),
    ] {
        assert!(wire::decode(&value.to_string()).is_err());
    }
}
#[test]
fn catalogs_reject_malformed_rows_and_validate_exact_model_selection() {
    let catalog = catalog::parse_models(
        "model-low\tModel (Low)\r\nmodel-high\tModel (High)\n",
        "custom",
    )
    .unwrap();
    assert!(
        catalog::validate(
            &json!({"model":"model-low","modeId":"full-access"}),
            &catalog
        )
        .is_ok()
    );
    assert!(catalog::validate(&json!({"model":"other"}), &catalog).is_err());
    assert!(catalog::validate(&json!({"modeId":"plan"}), &catalog).is_err());
    assert!(catalog::validate(&json!({"thinkingOptionId":"high"}), &catalog).is_err());
    for malformed in [
        "",
        "model",
        "model\tlabel\textra",
        "model\tlabel\nmodel\tduplicate",
    ] {
        assert!(catalog::parse_models(malformed, "custom").is_err());
    }
}
#[test]
fn opaque_plugin_handles_round_trip_without_replacing_native_identity() {
    let handle = catalog::persistence("custom", "native-conversation");
    assert_eq!(
        handle["sessionId"],
        "plugin:{\"version\":1,\"data\":{\"conversationId\":\"native-conversation\"}}"
    );
    assert_eq!(
        catalog::conversation(&handle).unwrap(),
        "native-conversation"
    );
    assert_eq!(
        catalog::conversation(&handle["sessionId"]).unwrap(),
        "native-conversation"
    );
    for malformed in [
        "native-conversation",
        "plugin:{\"version\":2,\"data\":{\"conversationId\":\"x\"}}",
        "plugin:{\"version\":1,\"data\":{\"conversationId\":\"\"}}",
        "plugin:{\"version\":1,\"data\":{\"conversationId\":\"x\",\"extra\":true}}",
    ] {
        assert!(catalog::conversation(&json!(malformed)).is_err());
    }
}
#[test]
fn authentication_diagnostics_do_not_echo_login_details() {
    for message in [
        "authentication required: secret",
        "Sign in using credential",
        "log-in first",
    ] {
        assert_eq!(
            wire::diagnostic(message),
            "Antigravity authentication failed. Run `agy` and sign in, then retry."
        );
    }
    assert_eq!(wire::diagnostic("native failure"), "native failure");
}
