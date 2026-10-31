use super::{mapping, schema};
use serde_json::{Value, json};
use std::collections::HashMap;

#[test]
fn capabilities_ignore_extensions_but_reject_known_unoffered_features() {
    let offered = vec![
        "prompt.message".to_owned(),
        "permission.tool_policy".to_owned(),
    ];
    assert_eq!(
        mapping::negotiate(
            &json!([
                "extension.future",
                "permission.tool_policy",
                "prompt.message",
                "prompt.message"
            ]),
            &offered
        )
        .unwrap(),
        vec!["prompt.message", "permission.tool_policy"]
    );
    assert!(mapping::negotiate(&json!(["session.archive"]), &offered).is_err());
    assert!(mapping::negotiate(&json!([false]), &offered).is_err());
    let flags = mapping::agent_capabilities(&offered);
    assert_eq!(flags["supportsMcpServers"], true);
    assert_eq!(flags["supportsNativePaseoTools"], false);
    assert_eq!(flags["supportsSessionPersistence"], false);
}

fn config() -> Value {
    json!({"cwd":"/workspace","env":{},"mcpServers":{},"settings":{},"persist":true})
}
#[test]
fn exact_mcp_preapproval_is_required_even_for_an_empty_policy() {
    let mut value = config();
    value["toolPolicy"] = json!({"preapproved":[]});
    schema::session_config(&mut value).unwrap();
    let open = json!({"type":"session.open","config":value});
    assert_eq!(
        mapping::require_capabilities(&[], &open)
            .unwrap_err()
            .to_string(),
        "Provider does not support permission.tool_policy"
    );
    assert!(mapping::require_capabilities(&["permission.tool_policy".into()], &open).is_ok());
    let mut value = config();
    value["toolPolicy"] =
        json!({"preapproved":[{"kind":"mcp","server":"paseo","tool":"agents_create"}]});
    schema::session_config(&mut value).unwrap();
    assert_eq!(
        value["toolPolicy"]["preapproved"][0]["tool"],
        "agents_create"
    );
    value["toolPolicy"]["preapproved"][0]["allTools"] = json!(true);
    assert!(schema::session_config(&mut value).is_err());
}

#[test]
fn mcp_server_shape_is_strict_not_a_lossy_authority_conversion() {
    let mut value = config();
    value["mcpServers"] = json!({"remote":{"type":"http","url":"https://example.test/mcp","headers":{"Authorization":"fixture"},"alwaysLoad":true},"local":{"type":"stdio","command":"fixture","args":["--mcp"],"env":{"KEY":"value"}}});
    let expected = value.clone();
    schema::session_config(&mut value).unwrap();
    assert_eq!(value, expected);
    value["mcpServers"]["remote"]["env"] = json!({});
    assert!(schema::session_config(&mut value).is_err());
}

#[test]
fn slash_commands_only_dispatch_when_advertised_and_string_prompts() {
    let commands = vec![json!({"name":"compact","description":"Compact"})];
    assert_eq!(
        mapping::prompt_input(json!("/compact keep  spacing"), &commands, false).unwrap(),
        json!({"type":"command","name":"compact","arguments":"keep  spacing"})
    );
    assert_eq!(
        mapping::prompt_input(json!("/compact"), &commands, false).unwrap()["arguments"],
        ""
    );
    assert_eq!(
        mapping::prompt_input(json!("/unknown"), &commands, false).unwrap()["type"],
        "message"
    );
    assert_eq!(
        mapping::prompt_input(json!("/compact"), &commands, true).unwrap()["type"],
        "message"
    );
    assert_eq!(
        mapping::prompt_input(json!([{"type":"text","text":"/compact"}]), &commands, false)
            .unwrap()["type"],
        "message"
    );
}

#[test]
fn prompt_requirements_distinguish_steering_images_and_null_output_schema() {
    let input = json!({"type":"session.prompt","prompt":{"delivery":"steer","input":{"type":"message","content":[{"type":"image","data":"fixture","mimeType":"image/png"}]},"outputSchema":null}});
    let mut capabilities: Vec<String> = Vec::new();
    for missing in [
        "prompt.steer",
        "prompt.message",
        "prompt.image",
        "prompt.output_schema",
    ] {
        assert_eq!(
            mapping::require_capabilities(&capabilities, &input)
                .unwrap_err()
                .to_string(),
            format!("Provider does not support {missing}")
        );
        capabilities.push(missing.into());
    }
    mapping::require_capabilities(&capabilities, &input).unwrap();
}

#[test]
fn timeline_snapshots_emit_deltas_and_preserve_opaque_revert_tokens() {
    let mut snapshots = HashMap::new();
    let mut tokens = HashMap::new();
    let first = json!({"id":"item","type":"assistant_message","text":"hello 🌍","messageId":"message","revertToken":{"offset":2,"native":null}});
    assert_eq!(
        mapping::timeline_item(first.clone(), &mut snapshots, Some(&mut tokens)).unwrap(),
        json!({"type":"assistant_message","text":"hello 🌍","messageId":"message"})
    );
    assert!(mapping::timeline_item(first, &mut snapshots, Some(&mut tokens)).is_none());
    let next =
        json!({"id":"item","type":"assistant_message","text":"hello 🌍!","messageId":"message"});
    assert_eq!(
        mapping::timeline_item(next, &mut snapshots, Some(&mut tokens)).unwrap()["text"],
        "!"
    );
    assert_eq!(tokens["item"], json!({"offset":2,"native":null}));
    assert_eq!(tokens["message"], tokens["item"]);
    let replacement = json!({"id":"item","type":"assistant_message","text":"replacement"});
    assert_eq!(
        mapping::timeline_item(replacement, &mut snapshots, None).unwrap()["text"],
        "replacement"
    );
    let user = json!({"id":"user","type":"user_message","text":"question","clientMessageId":"client","revertToken":null});
    assert_eq!(
        mapping::timeline_item(user.clone(), &mut snapshots, Some(&mut tokens)).unwrap()["messageId"],
        "user"
    );
    assert!(mapping::timeline_item(user, &mut snapshots, Some(&mut tokens)).is_none());
    assert!(tokens.contains_key("user"));
    assert_eq!(tokens["user"], Value::Null);
}

#[test]
fn plugin_items_keep_identity_but_other_items_do_not_leak_provider_identity() {
    let mut snapshots = HashMap::new();
    let plugin = json!({"id":"widget","type":"plugin","pluginId":"extension","kind":"progress","version":1,"data":{"percent":4},"revertToken":"opaque"});
    let output = mapping::timeline_item(plugin, &mut snapshots, None).unwrap();
    assert_eq!(output["id"], "widget");
    assert!(output.get("revertToken").is_none());
    let notice = json!({"id":"notice","type":"notification","level":"info","message":"hello"});
    assert!(
        mapping::timeline_item(notice, &mut snapshots, None)
            .unwrap()
            .get("id")
            .is_none()
    );
}

#[test]
fn persistence_uses_version_data_order_and_metadata_takes_precedence() {
    let persistence: Value =
        serde_json::from_str(r#"{"data":{"10":"ten","2":"two","text":"opaque"},"version":1}"#)
            .unwrap();
    assert_eq!(
        mapping::persistence_id(&persistence),
        r#"plugin:{"version":1,"data":{"2":"two","10":"ten","text":"opaque"}}"#
    );
    let handle = mapping::persistence_handle("plugin-provider", &persistence);
    assert_eq!(mapping::decode_persistence(&handle).unwrap(), persistence);
    let metadata = json!({"sessionId":"not-plugin","metadata":{"pluginProviderPersistence":{"version":0,"data":null}}});
    assert_eq!(
        mapping::decode_persistence(&metadata).unwrap(),
        json!({"version":0,"data":null})
    );
    assert!(mapping::decode_persistence_id("native:123").is_err());
    assert!(
        mapping::decode_persistence_id(r#"plugin:{"version":1,"data":null,"extra":true}"#).is_err()
    );
    assert!(mapping::decode_persistence_id(r#"plugin:{"version":-1,"data":null}"#).is_err());
}

#[test]
fn catalog_preserves_model_overrides_and_inherits_thinking_options() {
    let catalog = json!({"models":[{"id":"default","label":"Default"},{"id":"custom","label":"Custom","isDefault":false,"thinkingOptions":[],"defaultThinkingOptionId":"none","metadata":{"native":true}}],"modes":[{"id":"plan","label":"Plan"}],"thinkingOptions":[{"id":"high","label":"High"}],"defaultModel":"default","defaultMode":"plan","defaultThinkingOption":"high"});
    let result = mapping::catalog("extension", &catalog);
    assert_eq!(result["models"][0]["isDefault"], true);
    assert_eq!(
        result["models"][0]["thinkingOptions"],
        catalog["thinkingOptions"]
    );
    assert_eq!(result["models"][1]["thinkingOptions"], json!([]));
    assert_eq!(result["models"][1]["defaultThinkingOptionId"], "none");
    assert_eq!(result["defaultModeId"], "plan");
}

#[test]
fn events_strip_unknown_fields_recursively_without_losing_plugin_data() {
    let mut event = json!({"type":"timeline.item","sessionId":"session","extra":true,"item":{"id":"item","type":"plugin","pluginId":"extension","kind":"widget","version":1,"data":{"extra":true},"unexpected":true}});
    schema::event(&mut event).unwrap();
    assert!(event.get("extra").is_none());
    assert!(event["item"].get("unexpected").is_none());
    assert_eq!(event["item"]["data"], json!({"extra":true}));
    let mut event = json!({"type":"session.config","sessionId":"session","config":{"models":[],"modes":[],"thinkingOptions":[],"settings":[{"type":"select","id":"effort","label":"Effort","value":null,"options":[{"label":"High","value":"high","extra":true}]}],"unknown":true}});
    schema::event(&mut event).unwrap();
    assert!(event["config"].get("unknown").is_none());
    assert!(
        event["config"]["settings"][0]["options"][0]
            .get("extra")
            .is_none()
    );
}

#[test]
fn permission_and_error_protocols_preserve_distinct_failure_shapes() {
    let mut response = json!({"behavior":"deny","message":"No","interrupt":true,"selectedActionId":"deny","updatedInput":{"doNotForward":true}});
    schema::permission_response(&mut response).unwrap();
    assert_eq!(
        response,
        json!({"behavior":"deny","message":"No","interrupt":true,"selectedActionId":"deny"})
    );
    let mut event = json!({"type":"session.prompt_result","sessionId":"s","clientMessageId":"m","result":{"type":"failed","error":{"message":"Rejected","code":"POLICY","diagnostic":"Exact detail","private":true}}});
    schema::event(&mut event).unwrap();
    let error = super::ProviderOperationError::from_value(&event["result"]["error"]);
    assert_eq!(error.to_string(), "Rejected");
    assert_eq!(error.code.as_deref(), Some("POLICY"));
    assert_eq!(error.diagnostic.as_deref(), Some("Exact detail"));
    assert!(event["result"]["error"].get("private").is_none());
    let mut invalid = json!({"type":"session.prompt_result","sessionId":"s","clientMessageId":"m","result":{"type":"turn"}});
    assert!(schema::event(&mut invalid).is_err());
}

#[test]
fn tool_call_lifecycle_enforces_error_null_except_for_failures() {
    let mut event = json!({"type":"timeline.item","sessionId":"s","item":{"id":"tool","type":"tool_call","callId":"call","name":"shell","detail":{"type":"shell","command":"fixture","cwd":"/workspace","exitCode":null},"status":"completed","error":null}});
    schema::event(&mut event).unwrap();
    event["item"]["error"] = json!({"message":"failure"});
    assert!(schema::event(&mut event).is_err());
    event["item"]["status"] = json!("failed");
    schema::event(&mut event).unwrap();
}

#[test]
fn session_configuration_keeps_exact_tool_policy_and_strips_parent_runtime() {
    use crate::providers::api::LaunchContext;
    use tokio_util::sync::CancellationToken;
    let context = LaunchContext {
        agent_id: "agent".into(),
        home: "/fixture".into(),
        executable: "/fixture/paseo".into(),
        env: [
            ("KEEP".into(), "value".into()),
            ("CLAUDECODE".into(), "1".into()),
            ("PASEO_NODE_ENV".into(), "test".into()),
            ("ELECTRON_RUN_AS_NODE".into(), "1".into()),
        ]
        .into(),
        mcp_servers: json!({}),
        tool_catalog: json!({}),
        tool_manifest: json!({}),
        tool_executor: None,
        cancel: CancellationToken::new(),
    };
    let config = json!({"cwd":"/workspace","systemPrompt":"  ","daemonAppendSystemPrompt":"daemon text","modeId":"plan","thinkingOptionId":"high","featureValues":{"toggle":true},"toolPolicy":{"preapproved":[{"kind":"mcp","server":"paseo","tool":"agents_list"}]},"title":null});
    let result = mapping::session_config(&config, &context, false).unwrap();
    assert_eq!(result["env"], json!({"KEEP":"value"}));
    assert_eq!(result["systemPrompt"], "daemon text");
    assert_eq!(result["mode"], "plan");
    assert_eq!(result["thinkingOption"], "high");
    assert_eq!(result["toolPolicy"], config["toolPolicy"]);
    assert_eq!(result["settings"], config["featureValues"]);
    assert_eq!(result["persist"], false);
    assert!(result.get("title").is_none());
}
