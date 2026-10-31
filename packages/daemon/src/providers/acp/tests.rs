use super::mapping::*;
use serde_json::json;

#[test]
fn copilot_allow_all_tracks_config_not_autopilot() {
    let native = json!({"modes":{"availableModes":[{"id":AGENT,"name":"Agent"},{"id":AUTOPILOT,"name":"Autopilot"}],"currentModeId":AUTOPILOT},"configOptions":[{"id":"allow_all","type":"select","currentValue":"off","options":[]}]});
    let (available, current) = modes("copilot", &native);
    assert_eq!(current.as_deref(), Some(AGENT));
    assert!(available.iter().all(|mode| mode["id"] != AUTOPILOT));
    assert!(available.iter().any(|mode| mode["id"] == "allow-all"));
    let mut enabled = native;
    enabled["configOptions"][0]["currentValue"] = json!("on");
    assert_eq!(modes("copilot", &enabled).1.as_deref(), Some("allow-all"));
    assert_eq!(modes("acp", &enabled).1.as_deref(), Some(AUTOPILOT));
}

#[test]
fn grouped_config_options_preserve_selected_model_and_thinking() {
    let native = json!({"configOptions":[
        {"id":"models","category":"model","type":"select","currentValue":"second","options":[{"name":"group","options":[{"value":"first","name":"First"},{"value":"second","name":"Second","description":"Selected model"}]}]},
        {"id":"reasoning","category":"thought_level","type":"select","currentValue":"high","options":[{"value":"low","name":"Low"},{"value":"high","name":"High"}]}
    ]});
    let catalog = models("custom-kimi", &native);
    assert_eq!(catalog.len(), 2);
    assert_eq!(catalog[1]["provider"], "custom-kimi");
    assert_eq!(catalog[1]["isDefault"], true);
    assert_eq!(catalog[1]["defaultThinkingOptionId"], "high");
    assert_eq!(catalog[1]["thinkingOptions"][0]["id"], "low");
    let mut unavailable = catalog[0].clone();
    apply_thinking(&mut unavailable, &[]);
    assert!(unavailable.get("thinkingOptions").is_none());
    assert!(unavailable.get("defaultThinkingOptionId").is_none());
}

#[test]
fn permission_actions_preserve_ids_and_enforce_behavior() {
    let options = json!([
        {"optionId":"forever","kind":"allow_always","name":"Always"},
        {"optionId":"once","kind":"allow_once","name":"Once"},
        {"optionId":"no","kind":"reject_once","name":"No"}
    ]);
    assert_eq!(
        permission_option(&options, &json!({"behavior":"allow"}))
            .unwrap()
            .unwrap()["optionId"],
        "once"
    );
    assert_eq!(
        permission_option(
            &options,
            &json!({"behavior":"allow","selectedActionId":"forever"})
        )
        .unwrap()
        .unwrap()["optionId"],
        "forever"
    );
    assert!(
        permission_option(
            &options,
            &json!({"behavior":"deny","selectedActionId":"forever"})
        )
        .is_err()
    );
    assert!(
        permission_option(
            &options,
            &json!({"behavior":"allow","selectedActionId":"missing"})
        )
        .is_err()
    );
    assert!(!chooser(&options));
    assert!(chooser(
        &json!([{"kind":"allow_once"},{"kind":"allow_once"},{"kind":"reject_once"}])
    ));
}

#[test]
fn kiro_extension_commands_keep_skills_and_argument_hints() {
    let result = commands(
        &json!({"commands":[{"name":" /agent ","description":"Choose","meta":{"hint":"name"}},{"name":"///agent"}],"prompts":[{"name":"review","serverName":"skill:review","description":"Review"},{"name":" "}]}),
        true,
    );
    assert_eq!(
        result,
        json!([
            {"name":"agent","description":"Choose","argumentHint":"name","kind":"command"},
            {"name":"review","description":"Review","argumentHint":"","kind":"skill"}
        ])
        .as_array()
        .unwrap()
        .clone()
    );
}

#[test]
fn mcp_transports_use_acp_name_value_arrays() {
    let result = mcp(&json!({
        "local":{"type":"stdio","command":"tool","args":["run"],"env":{"KEY":"value"}},
        "remote":{"type":"http","url":"http://127.0.0.1/mcp","headers":{"Authorization":"Bearer runtime-only"}},
        "legacy":{"type":"sse","url":"http://127.0.0.1/sse"}
    }));
    let local = result.iter().find(|s| s["name"] == "local").unwrap();
    assert!(local.get("type").is_none());
    assert_eq!(local["env"], json!([{"name":"KEY","value":"value"}]));
    let remote = result.iter().find(|s| s["name"] == "remote").unwrap();
    assert_eq!(remote["type"], "http");
    assert_eq!(remote["headers"][0]["name"], "Authorization");
    assert_eq!(
        result.iter().find(|s| s["name"] == "legacy").unwrap()["headers"],
        json!([])
    );
}

#[test]
fn content_projection_keeps_non_text_history_markers() {
    assert_eq!(
        content_text(&json!({"type":"image","data":"ignored"})),
        "[image]"
    );
    assert_eq!(content_text(&json!({"type":"audio"})), "[audio]");
    assert_eq!(
        content_text(
            &json!({"type":"resource_link","title":"Title","name":"wrong","uri":"file:///file"})
        ),
        "Title"
    );
    assert_eq!(
        content_text(&json!({"type":"resource_link","uri":"file:///file"})),
        "file:///file"
    );
    assert_eq!(
        content_text(
            &json!({"type":"resource","resource":{"mimeType":"application/octet-stream"}})
        ),
        "[resource:application/octet-stream]"
    );
}

#[test]
fn tool_terminal_output_and_failure_are_projected() {
    let item = tool_item(
        &json!({"toolCallId":"call","kind":"execute","title":"Run","status":"failed","rawInput":{"command":"build","args":["--all"]},"rawOutput":{"message":"Failed"}}),
        Some(json!({"output":"compiler failure","exitStatus":{"exitCode":2}})),
    );
    assert_eq!(item["detail"]["command"], "build --all");
    assert_eq!(item["detail"]["output"], "compiler failure");
    assert_eq!(item["detail"]["exitCode"], 2);
    assert_eq!(item["status"], "failed");
    assert_eq!(item["error"]["message"], "Failed");
}

#[test]
fn variant_features_do_not_leak_between_families() {
    let native = json!({"configOptions":[{"id":"fast","type":"select","currentValue":"on","options":[{"value":"off","name":"Off"},{"value":"on","name":"On"}]},{"id":"agent","category":"_agent","type":"select","currentValue":"","options":[{"value":"","name":""}]}]});
    assert_eq!(features("acp", &native, &json!({})).len(), 1);
    let cursor = features(
        "cursor",
        &native,
        &json!({"featureValues":{"auto_accept":true}}),
    );
    assert_eq!(cursor[0]["value"], true);
    assert_eq!(cursor[1]["id"], "fast");
    let copilot = features("copilot", &native, &json!({}));
    assert_eq!(copilot[1]["id"], "agent");
    assert_eq!(copilot[1]["options"][0]["label"], "Default");
}

#[test]
fn unattended_acp_creation_does_not_inherit_another_provider_mode() {
    let input = json!({"provider":"acp","unattended":false,"parent":{"provider":"claude","modeId":"bypassPermissions","isUnattended":true},"availableModes":[{"id":"agent"},{"id":"plan"}]});
    assert_eq!(
        create_config(&input, "acp").unwrap(),
        json!({"featureValues":{"auto_accept":true}})
    );
    let mut explicit = input.clone();
    explicit["requestedMode"] = json!("plan");
    explicit["featureValues"] = json!({"auto_accept":false,"opaque":"keep"});
    assert_eq!(
        create_config(&explicit, "acp").unwrap(),
        json!({"modeId":"plan","featureValues":{"auto_accept":false,"opaque":"keep"}})
    );
    explicit["requestedMode"] = json!("missing");
    assert!(create_config(&explicit, "acp").is_err());
    let mut interactive = input;
    interactive["parent"]["isUnattended"] = json!(false);
    assert!(create_config(&interactive, "acp").is_err());
}

#[test]
fn grouped_choices_keep_group_metadata_without_inventing_mode_fields() {
    let options = json!([{"id":"model","category":"model","type":"select","currentValue":"x","options":[{"group":"Local","options":[{"value":"x","name":"X"}]}]}]);
    assert_eq!(
        selectors(&options, "model")[0]["metadata"],
        json!({"group":"Local"})
    );
    let native = json!({"configOptions":[{"id":"mode","category":"mode","type":"select","currentValue":"agent","options":[{"value":"agent","name":"Agent"}]}]});
    let available = modes("acp", &native).0;
    assert_eq!(
        available,
        json!([{"id":"agent","label":"Agent"}])
            .as_array()
            .unwrap()
            .clone()
    );
}
