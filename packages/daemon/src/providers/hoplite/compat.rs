// Source: plugins/hoplite-provider/server/run-phase.ts and
// packages/plugin/src/server/acp-internal/connection.ts. The shared ACP engine
// owns processes/RPCs; these helpers retain the bundled plugin's dialect.
use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

fn rows(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or(&[])
}
fn text(value: &Value) -> &str {
    value.as_str().unwrap_or("")
}

pub(crate) fn client_capabilities() -> Value {
    json!({})
}
pub(crate) fn session_meta(config: &Value) -> Value {
    let mut paseo =
        json!({"persist":config.get("persist").and_then(Value::as_bool).unwrap_or(true)});
    for key in ["providerOptions", "toolPolicy"] {
        if let Some(value) = config.get(key) {
            paseo[key] = value.clone();
        }
    }
    let prompts: Vec<&str> = ["systemPrompt", "daemonAppendSystemPrompt"]
        .into_iter()
        .filter_map(|key| config[key].as_str())
        .filter(|s| !s.trim().is_empty())
        .collect();
    if !prompts.is_empty() {
        paseo["systemPrompt"] = json!(prompts.join("\n\n"));
    }
    json!({"_paseo":paseo})
}

pub(crate) fn options(option: &Value) -> Vec<Value> {
    if option["type"] == "boolean" {
        return Vec::new();
    }
    rows(&option["options"])
        .iter()
        .flat_map(|candidate| {
            if candidate.get("group").is_some() {
                rows(&candidate["options"])
            } else {
                std::slice::from_ref(candidate)
            }
        })
        .map(|candidate| json!({"label":candidate["name"],"value":candidate["value"]}))
        .collect()
}
fn category<'a>(native: &'a Value, name: &str) -> Option<&'a Value> {
    rows(&native["configOptions"])
        .iter()
        .find(|option| option["category"] == name)
}
pub(crate) fn selected(native: &Value, name: &str) -> Option<String> {
    category(native, name).filter(|option| option["type"] != "boolean")?["currentValue"]
        .as_str()
        .map(str::to_owned)
}
pub(crate) fn modes(native: &Value) -> Vec<Value> {
    rows(&native["modes"]["availableModes"])
        .iter()
        .map(|mode| {
            let mut value = json!({"id":mode["id"],"label":mode["name"]});
            if let Some(description) = mode.get("description").filter(|v| !v.is_null()) {
                value["description"] = description.clone();
            }
            value
        })
        .collect()
}
pub(crate) fn generic_features(native: &Value, _config: &Value) -> Vec<Value> {
    rows(&native["configOptions"]).iter().filter(|option|option["category"]!="model"&&option["category"]!="thought_level")
        .map(|option| {
            let boolean=option["type"]=="boolean";
            let mut feature=json!({"type":if boolean{"toggle"}else{"select"},"id":option["id"],"label":option["name"],"value":option["currentValue"]});
            if let Some(description)=option.get("description").filter(|v|!v.is_null()) {feature["description"]=description.clone();}
            if !boolean {feature["options"]=json!(options(option).into_iter().map(|mut value|{value["id"]=value["value"].clone();value}).collect::<Vec<_>>());}
            feature
        }).collect()
}
pub(crate) fn catalog(native: &Value, provider: &str) -> Value {
    let thinking = category(native, "thought_level")
        .map(options)
        .unwrap_or_default()
        .into_iter()
        .map(|option| json!({"id":option["value"],"label":option["label"]}))
        .collect::<Vec<_>>();
    let default_model = selected(native, "model");
    let default_thinking = selected(native, "thought_level");
    let models=category(native,"model").map(options).unwrap_or_default().into_iter().map(|option| {
        let mut model=json!({"provider":provider,"id":option["value"],"label":option["label"],"isDefault":option["value"].as_str()==default_model.as_deref(),"thinkingOptions":thinking});
        if let Some(default)=&default_thinking {model["defaultThinkingOptionId"]=json!(default);}
        model
    }).collect::<Vec<_>>();
    let mut result = json!({"models":models,"modes":modes(native)});
    if let Some(mode) = native["modes"]["currentModeId"].as_str() {
        result["defaultModeId"] = json!(mode);
    }
    result
}

// Generic plugin settings are forwarded, including booleans. There is no
// allowlist/auto-accept pseudo-setting and no session/set_model fallback.
pub(crate) fn config_request(
    native: &Value,
    session: &str,
    target: &str,
    setting: Option<&str>,
    value: Value,
) -> Result<(&'static str, Value)> {
    if target == "mode" {
        let mode = value
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("ACP session does not expose mode configuration"))?;
        if native["modes"].is_null() {
            bail!("ACP session does not expose mode configuration");
        }
        return Ok((
            "session/set_mode",
            json!({"sessionId":session,"modeId":mode}),
        ));
    }
    let config_id = if target == "setting" {
        setting
    } else {
        category(
            native,
            if target == "model" {
                "model"
            } else {
                "thought_level"
            },
        )
        .and_then(|option| option["id"].as_str())
    }
    .ok_or_else(|| anyhow!("ACP session does not expose {target} configuration"))?;
    if value.is_null() {
        bail!("ACP configuration {config_id} cannot be cleared");
    }
    let mut params = json!({"sessionId":session,"configId":config_id});
    if value.is_boolean() {
        params["type"] = json!("boolean");
        params["value"] = value;
    } else {
        params["value"] = json!(js_string(&value));
    }
    Ok(("session/set_config_option", params))
}
fn js_string(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Null => "null".into(),
        Value::Array(values) => values
            .iter()
            .map(|value| {
                if value.is_null() {
                    String::new()
                } else {
                    js_string(value)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".into(),
        _ => value.to_string(),
    }
}
pub(crate) fn permission_request(provider: &str, params: &Value) -> Value {
    let tool = &params["toolCall"];
    let mut request = json!({"id":format!("permission:{}",text(&tool["toolCallId"])),"provider":provider,"name":tool["name"].as_str().or_else(||tool["title"].as_str()).unwrap_or("Tool"),"kind":"tool","input":tool.get("rawInput").filter(|v|v.is_object()).cloned().unwrap_or_else(||json!({})),"actions":rows(&params["options"]).iter().map(|option|json!({"id":option["optionId"],"label":option["name"],"behavior":behavior(option)})).collect::<Vec<_>>()});
    if let Some(title) = tool.get("title").filter(|v| !v.is_null()) {
        request["title"] = title.clone();
    }
    request
}
fn behavior(option: &Value) -> &'static str {
    if text(&option["kind"]).starts_with("allow") {
        "allow"
    } else {
        "deny"
    }
}
pub(crate) fn permission_option(options: &Value, response: &Value) -> Result<Option<Value>> {
    let action = response.get("selectedActionId");
    let selected = rows(options).iter().find(|option| {
        action.is_none_or(|action| option["optionId"] == *action)
            && response["behavior"] == behavior(option)
    });
    if action.is_some() && selected.is_none() {
        bail!(
            "ACP permission action '{}' does not exist or does not match '{}' behavior",
            text(&response["selectedActionId"]),
            text(&response["behavior"])
        );
    }
    Ok(selected.cloned())
}

pub(crate) fn transform_notification(method: &str, params: &Value) -> Option<Value> {
    if method != "_hoplite/run_phase" {
        return None;
    }
    let run_id = params["runId"].as_str().filter(|s| !s.is_empty())?;
    let phase = params.get("phase")?;
    let label = params.get("label")?;
    if !(phase.is_null() || phase.is_string()) || !(label.is_null() || label.is_string()) {
        return None;
    }
    let running = !phase.is_null();
    let id = format!("hoplite-run-phase:{run_id}");
    Some(
        json!({"type":"timeline","item":{"type":"tool_call","callId":id,"name":"Hoplite workspace","detail":{"type":"plain_text","label":if running{label.as_str().or_else(||phase.as_str()).unwrap_or("")}else{"Workspace ready"},"icon":"wrench"},"status":if running{"running"}else{"completed"},"error":null}}),
    )
}

#[derive(Default)]
pub(crate) struct Projection {
    sequence: u64,
    chunks: HashMap<String, String>,
    messages: HashMap<String, String>,
    tools: serde_json::Map<String, Value>,
    vendor: serde_json::Map<String, Value>,
    compactions: HashSet<String>,
}
impl Projection {
    pub(crate) fn reset_chunks(&mut self) {
        self.chunks.clear();
    }
    // None leaves a state/control notification to the shared ACP engine.
    // Some(empty) is a deliberately ignored source notification, not a fallback.
    pub(crate) fn project(
        &mut self,
        method: &str,
        params: &Value,
        replay: bool,
    ) -> Option<Vec<Value>> {
        if method != "session/update" {
            let event = transform_notification(method, params)?;
            let key = event["item"]["callId"].as_str()?.to_owned();
            if event["item"]["status"] == "running" {
                self.vendor.insert(key, event["item"].clone());
            } else {
                self.vendor.shift_remove(&key);
            }
            return Some(vec![event]);
        }
        let update = &params["update"];
        let kind = text(&update["sessionUpdate"]);
        if matches!(
            kind,
            "agent_message_chunk" | "agent_thought_chunk" | "user_message_chunk"
        ) {
            if kind == "user_message_chunk" && !replay {
                self.chunks.clear();
                return Some(Vec::new());
            }
            self.chunks.retain(|key, _| key == kind);
            if update["content"]["type"] != "text" {
                return Some(Vec::new());
            }
            let id = if let Some(id) = update["messageId"].as_str().filter(|s| !s.is_empty()) {
                self.chunks.remove(kind);
                id.to_owned()
            } else if let Some(id) = self.chunks.get(kind) {
                id.clone()
            } else {
                self.sequence += 1;
                let id = format!("{kind}:{}", self.sequence);
                self.chunks.insert(kind.into(), id.clone());
                id
            };
            let delta = text(&update["content"]["text"]);
            let current = self.messages.entry(id.clone()).or_default();
            current.push_str(delta);
            let item = match kind {
                "user_message_chunk" => {
                    json!({"type":"user_message","text":current,"messageId":id})
                }
                "agent_message_chunk" => {
                    json!({"type":"assistant_message","text":delta,"messageId":id})
                }
                _ => json!({"type":"reasoning","text":delta}),
            };
            if delta.is_empty() {
                return Some(Vec::new());
            }
            return Some(vec![json!({"type":"timeline","item":item})]);
        }
        match kind {
            "tool_call" | "tool_call_update" => {
                if kind == "tool_call" {
                    self.chunks.clear();
                }
                let id = text(&update["toolCallId"]).to_owned();
                let snapshot=self.tools.entry(id.clone()).or_insert_with(||json!({"id":id,"title":update["title"].as_str().or_else(||update["name"].as_str()).unwrap_or("Tool call"),"status":"pending","input":null,"output":null,"locations":[]}));
                for key in ["name", "title", "kind"] {
                    if update[key].is_string() {
                        snapshot[key] = update[key].clone();
                    }
                }
                if let Some(status) = update.get("status") {
                    snapshot["status"] = if status.is_null() {
                        json!("pending")
                    } else {
                        status.clone()
                    };
                }
                for (source, target) in [("rawInput", "input"), ("rawOutput", "output")] {
                    if let Some(value) = update.get(source) {
                        snapshot[target] = value.clone();
                    }
                }
                if let Some(locations) = update.get("locations").filter(|v| v.is_array()) {
                    snapshot["locations"] = json!(
                        rows(locations)
                            .iter()
                            .map(|v| v["path"].clone())
                            .collect::<Vec<_>>()
                    );
                }
                Some(vec![json!({"type":"timeline","item":tool_item(snapshot)})])
            }
            "plan" => {
                self.chunks.clear();
                Some(vec![
                    json!({"type":"timeline","item":{"type":"todo","items":rows(&update["entries"]).iter().enumerate().map(|(i,entry)|json!({"id":i.to_string(),"text":entry["content"],"completed":entry["status"]=="completed","status":entry["status"]})).collect::<Vec<_>>()}}),
                ])
            }
            "compaction_update" => {
                let id = text(&update["compactionId"]).to_owned();
                let completed = update["status"] == "completed";
                if completed {
                    self.compactions.remove(&id);
                } else {
                    self.compactions.insert(id);
                }
                Some(vec![
                    json!({"type":"timeline","item":{"type":"compaction","status":if completed{"completed"}else{"loading"}}}),
                ])
            }
            "usage_update" => {
                let mut usage = json!({"contextWindowUsedTokens":update["used"],"contextWindowMaxTokens":update["size"]});
                if update["cost"]["currency"] == "USD" {
                    usage["totalCostUsd"] = update["cost"]["amount"].clone();
                }
                Some(vec![json!({"type":"usage_updated","usage":usage})])
            }
            _ => None,
        }
    }
    pub(crate) fn finish(&mut self, status: &str) -> Vec<Value> {
        self.chunks.clear();
        let mut items = Vec::new();
        for snapshot in self.tools.values_mut() {
            if snapshot["status"] != "pending" && snapshot["status"] != "in_progress" {
                continue;
            }
            let item = terminal_tool_item(tool_item(snapshot), status);
            snapshot["status"] = json!(if status == "completed" {
                "completed"
            } else {
                "failed"
            });
            items.push(item);
        }
        items.extend(
            std::mem::take(&mut self.vendor)
                .into_iter()
                .map(|(_, item)| terminal_tool_item(item, status)),
        );
        items.extend(
            self.compactions
                .drain()
                .map(|_| json!({"type":"compaction","status":"completed"})),
        );
        items
            .into_iter()
            .map(|item| json!({"type":"timeline","item":item}))
            .collect()
    }
}
fn tool_item(snapshot: &Value) -> Value {
    let input = &snapshot["input"];
    let edit = snapshot["kind"] == "edit"
        || snapshot["name"]
            .as_str()
            .is_some_and(|s| s.to_lowercase().contains("edit"));
    let detail = if edit {
        let mut detail = json!({"type":"edit","filePath":input["filePath"].as_str().or_else(||snapshot["locations"][0].as_str()).unwrap_or("")});
        for key in ["oldString", "newString", "unifiedDiff"] {
            if input[key].is_string() {
                detail[key] = input[key].clone();
            }
        }
        detail
    } else {
        json!({"type":"unknown","input":snapshot["input"],"output":snapshot["output"]})
    };
    let status = match text(&snapshot["status"]) {
        "pending" | "in_progress" => "running",
        status => status,
    };
    json!({"type":"tool_call","callId":snapshot["id"],"name":snapshot["name"].as_str().or_else(||snapshot["title"].as_str()).unwrap_or("Tool call"),"detail":detail,"status":status,"error":if status=="failed"{snapshot["output"].clone()}else{Value::Null}})
}
fn terminal_tool_item(mut item: Value, status: &str) -> Value {
    item["status"] = json!(status);
    if status == "failed" {
        if item["error"].is_null() {
            item["error"] = json!({"message":"Turn failed"});
        }
    } else {
        item["error"] = Value::Null;
    }
    item
}
