use super::{input, permissions, projection, session, transport::RpcError};
use serde_json::{Value, json};

#[test]
fn steer_only_reports_unavailable_for_definitive_native_rejections() {
    let error = |code: i64, message: &str, data: Value| RpcError {
        code: json!(code),
        message: message.into(),
        data,
    };
    assert!(error(-32601, "Method not found", Value::Null).definitive_steer_rejection());
    assert!(
        error(
            -32600,
            "expected active turn id `A` but found `B`",
            Value::Null
        )
        .definitive_steer_rejection()
    );
    assert!(
        error(
            -32600,
            "Not steerable",
            json!({"codexErrorInfo":{"activeTurnNotSteerable":{}}})
        )
        .definitive_steer_rejection()
    );
    assert!(!error(-32000, "connection lost", Value::Null).definitive_steer_rejection());
    assert!(
        !error(-32600, "input may have been accepted", Value::Null).definitive_steer_rejection()
    );
    assert!(
        !error(
            -32600,
            "expected active turn id `A` but found `B` then disconnected",
            Value::Null
        )
        .definitive_steer_rejection()
    );
}

#[test]
fn permission_denial_uses_each_native_response_shape() {
    let tool = json!({"threadId":"thread","turnId":"native-turn","itemId":"tool","command":"git status","questions":[]});
    for method in [
        "item/commandExecution/requestApproval",
        "item/fileChange/requestApproval",
    ] {
        let pending = permissions::request("custom-codex", json!(7), method, tool.clone()).unwrap();
        assert_eq!(permissions::deny(&pending), json!({"decision":"decline"}));
        assert_eq!(pending.request["provider"], "custom-codex");
    }
    let question = permissions::request("codex", json!(8), "tool/requestUserInput", tool).unwrap();
    assert_eq!(permissions::deny(&question), json!({"answers":{}}));
    let elicitation=permissions::request("codex",json!(9),"mcpServer/elicitation/request",json!({"threadId":"thread","serverName":"browser","mode":"openai/form","message":"Open page?","requestedSchema":{"type":"object","properties":{}}})).unwrap();
    assert_eq!(
        permissions::deny(&elicitation),
        json!({"action":"decline","content":null,"_meta":null})
    );
    assert!(permissions::automatic_reply(&elicitation).is_none());
}

#[test]
fn asynchronous_question_is_not_committed_before_steer_acceptance() {
    let mut questions = permissions::AsyncQuestions::default();
    let request=questions.receive("codex",&json!({"type":"agentMessage","id":"question","delivery":"async","questions":[{"title":"Choose a branch","options":["main","release"]}]})).unwrap();
    let id = request["id"].as_str().unwrap();
    let prepared = questions
        .prepare_response(
            id,
            &json!({"behavior":"allow","updatedInput":{"answers":{"Question 1":"release"}}}),
        )
        .unwrap();
    assert!(questions.has_pending(id));
    assert_eq!(
        prepared.prompt.as_deref(),
        Some("Answers to your questions:\n\nChoose a branch\nrelease")
    );
    let persisted = questions.serialize();
    assert!(permissions::AsyncQuestions::new(&persisted).has_pending(id));
    let completed = questions.complete(prepared).unwrap();
    assert!(!questions.has_pending(id));
    assert_eq!(completed["name"], "request_user_input_async");
}

#[test]
fn output_schema_requires_all_object_properties_at_every_depth() {
    let output=input::output_schema(&json!({"type":"object","properties":{"result":{"type":"array","items":{"type":"object","properties":{"name":{"type":"string"}}}}}})).unwrap();
    assert_eq!(output["additionalProperties"], false);
    assert_eq!(output["required"], json!(["result"]));
    assert_eq!(
        output["properties"]["result"]["items"]["required"],
        json!(["name"])
    );
    assert!(input::output_schema(&json!({"type":"object","additionalProperties":true})).is_err());
}

#[test]
fn history_retains_native_turn_ids_separate_from_user_message_ids() {
    let history = projection::history_with_images(
        &json!({"turns":[{"id":"native-turn-1","startedAt":1700000000,"completedAt":1700000001,"items":[{"id":"native-message-1","type":"userMessage","content":[{"type":"text","text":"hello"}]},{"id":"assistant-1","type":"agentMessage","text":"world"}]}]}),
        None,
        &mut |_, _| None,
    );
    assert_eq!(history[0]["item"]["messageId"], "native-message-1");
    assert_eq!(history[0]["turnId"], "native-turn-1");
    assert!(
        history[0]["timestamp"]
            .as_str()
            .unwrap()
            .starts_with("2023-11-14")
    );
    assert_eq!(history[1]["item"]["text"], "world");
}

#[test]
fn speed_options_come_from_native_catalog_not_model_name_guesses() {
    let models = vec![
        json!({"id":"future-provider-model","serviceTiers":[{"id":"burst","name":"Fast","description":"Native tier"}]}),
    ];
    let config = json!({"model":"future-provider-model","featureValues":{"service_tier":"burst"}});
    let features = session::features(&config, &models, &[]);
    assert_eq!(features[0]["value"], "burst");
    assert_eq!(
        features[0]["options"][1],
        json!({"id":"burst","label":"Fast"})
    );
    assert!(session::features(&json!({"model":"gpt-familiar"}), &[], &[]).is_empty());
}
