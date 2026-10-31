use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::collections::HashSet;

pub const AGENT: &str = "https://agentclientprotocol.com/protocol/session-modes#agent";
pub const AUTOPILOT: &str = "https://agentclientprotocol.com/protocol/session-modes#autopilot";
pub fn array(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or(&[])
}
pub fn text(value: &Value) -> &str {
    value.as_str().unwrap_or("")
}
pub fn option<'a>(options: &'a Value, category: &str) -> Option<&'a Value> {
    array(options)
        .iter()
        .find(|o| o["type"] == "select" && o["category"] == category)
}
pub fn option_id<'a>(options: &'a Value, id: &str) -> Option<&'a Value> {
    array(options)
        .iter()
        .find(|o| o["type"] == "select" && o["id"] == id)
}
fn choice_rows(option: &Value) -> impl Iterator<Item = (&Value, Option<&Value>)> {
    array(&option["options"]).iter().flat_map(|parent| {
        let direct = parent.get("value").is_some();
        let children = if direct {
            std::slice::from_ref(parent)
        } else {
            array(&parent["options"])
        };
        children.iter().map(move |choice| {
            (
                choice,
                if direct {
                    choice.get("group")
                } else {
                    parent.get("group")
                },
            )
        })
    })
}
pub fn choices(option: &Value) -> impl Iterator<Item = &Value> {
    choice_rows(option).map(|(choice, _)| choice)
}
pub fn selectors(options: &Value, category: &str) -> Vec<Value> {
    option(options, category).map(|o| choice_rows(o).map(|(c,group)| {
        let mut value = json!({"id":c["value"],"label":c["name"],"isDefault":c["value"] == o["currentValue"]});
        if let Some(description)=c.get("description").filter(|v|!v.is_null()){value["description"]=description.clone();}
        if let Some(group)=group.filter(|v|!v.is_null()&&v.as_str()!=Some("")){value["metadata"]=json!({"group":group});}
        value
    }).collect()).unwrap_or_default()
}
pub fn models(provider: &str, state: &Value) -> Vec<Value> {
    let thinking = selectors(&state["configOptions"], "thought_level");
    let mut result: Vec<Value> = if !array(&state["models"]["availableModels"]).is_empty() {
        array(&state["models"]["availableModels"]).iter().map(|m| {
            let mut v = json!({"provider":provider,"id":m["modelId"],"label":m["name"],"isDefault":m["modelId"]==state["models"]["currentModelId"]});
            if let Some(d) = m.get("description") { v["description"] = d.clone(); }
            v
        }).collect()
    } else {
        selectors(&state["configOptions"], "model")
            .into_iter()
            .map(|mut m| {
                m["provider"] = json!(provider);
                m
            })
            .collect()
    };
    for model in &mut result {
        apply_thinking(model, &thinking);
    }
    result
}
pub fn apply_thinking(model: &mut Value, thinking: &[Value]) {
    if let Some(object) = model.as_object_mut() {
        object.remove("thinkingOptions");
        object.remove("defaultThinkingOptionId");
    }
    if !thinking.is_empty() {
        model["thinkingOptions"] = json!(thinking);
    }
    if let Some(default) = thinking.iter().find(|o| o["isDefault"] == true) {
        model["defaultThinkingOptionId"] = default["id"].clone();
    }
}
pub fn modes(family: &str, state: &Value) -> (Vec<Value>, Option<String>) {
    let mut modes: Vec<Value>;
    let mut current;
    if !array(&state["modes"]["availableModes"]).is_empty() {
        modes = array(&state["modes"]["availableModes"])
            .iter()
            .map(|m| {
                let mut v = json!({"id":m["id"],"label":m["name"]});
                if let Some(d) = m.get("description") {
                    v["description"] = d.clone();
                }
                v
            })
            .collect();
        current = state["modes"]["currentModeId"].as_str().map(str::to_owned);
    } else {
        modes = selectors(&state["configOptions"], "mode")
            .into_iter()
            .map(|mut mode| {
                let object = mode.as_object_mut().unwrap();
                object.remove("isDefault");
                object.remove("metadata");
                mode
            })
            .collect();
        current = option(&state["configOptions"], "mode")
            .and_then(|o| o["currentValue"].as_str())
            .map(str::to_owned);
    }
    if family == "copilot" {
        if modes.is_empty() {
            modes = vec![
                json!({"id":AGENT,"label":"Agent","description":"Default agent mode for conversational interactions"}),
                json!({"id":"https://agentclientprotocol.com/protocol/session-modes#plan","label":"Plan","description":"Plan mode for creating and executing multi-step plans"}),
            ];
        }
        modes.retain(|m| m["id"] != AUTOPILOT && m["id"] != "allow-all");
        modes.push(json!({"id":"allow-all","label":"Allow All","description":"Automatically approves all Copilot tool, path, and URL requests."}));
        if option_id(&state["configOptions"], "allow_all")
            .is_some_and(|o| o["currentValue"] == "on")
        {
            current = Some("allow-all".into());
        } else if current.as_deref() == Some(AUTOPILOT) {
            current = Some(AGENT.into());
        }
    }
    (modes, current)
}
pub fn features(family: &str, state: &Value, config: &Value) -> Vec<Value> {
    let mut result = vec![
        json!({"type":"toggle","id":"auto_accept","label":"Auto Accept","description":"Automatically approves ACP permission prompts.","tooltip":"Auto accept permission prompts","icon":"shield-check","value":config["featureValues"]["auto_accept"]==true}),
    ];
    let definition = match family {
        "cursor" => Some((
            "fast",
            "Fast",
            "Cursor fast mode",
            "Select Cursor fast mode",
        )),
        "copilot" => Some((
            "agent",
            "Agent",
            "Use a Copilot custom agent profile",
            "Select Copilot agent",
        )),
        _ => None,
    };
    if let Some((id, label, description, tooltip)) = definition {
        if let Some(o) = option_id(&state["configOptions"], id).or_else(|| {
            if id == "agent" {
                option(&state["configOptions"], "_agent")
            } else {
                None
            }
        }) {
            let values: Vec<Value> = choice_rows(o).map(|(c,group)| {
                let name=text(&c["name"]).trim();
                let label=if !name.is_empty(){name}else if id=="agent"&&c["value"]==""{"Default"}else{text(&c["value"])};
                let mut value=json!({"id":c["value"],"label":label,"isDefault":c["value"]==o["currentValue"]});
                if let Some(description)=c.get("description").filter(|v|!v.is_null()){value["description"]=description.clone();}
                if let Some(group)=group.filter(|v|!v.is_null()&&v.as_str()!=Some("")){value["metadata"]=json!({"group":group});}
                value
            }).collect();
            let mut feature = json!({"type":"select","id":id,"label":label,"description":description,"tooltip":tooltip,"value":o["currentValue"],"options":values});
            if id == "fast" {
                feature["icon"] = json!("zap");
            }
            result.push(feature);
        }
    }
    result
}
pub fn mcp(servers: &Value) -> Vec<Value> {
    servers.as_object().into_iter().flatten().map(|(name, server)| {
        let pairs = |value: &Value| -> Vec<Value> { value.as_object().into_iter().flatten().map(|(k,v)| json!({"name":k,"value":v})).collect() };
        if server["type"] == "stdio" || server.get("command").is_some() {
            json!({"name":name,"command":server["command"],"args":server.get("args").cloned().unwrap_or_else(||json!([])),"env":pairs(&server["env"])})
        } else {json!({"name":name,"type":if server["type"]=="http" {"http"} else {"sse"},"url":server["url"],"headers":pairs(&server["headers"])})}
    }).collect()
}
pub fn content_text(content: &Value) -> String {
    match text(&content["type"]) {
        "text" => text(&content["text"]).to_owned(),
        "resource_link" => content["title"]
            .as_str()
            .unwrap_or_else(|| text(&content["uri"]))
            .to_owned(),
        "resource" => content["resource"]["text"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| {
                format!(
                    "[resource:{}]",
                    content["resource"]["mimeType"].as_str().unwrap_or("binary")
                )
            }),
        "image" => "[image]".into(),
        "audio" => "[audio]".into(),
        _ => String::new(),
    }
}
pub fn tool_text(snapshot: &Value) -> Option<String> {
    let parts: Vec<String> = array(&snapshot["content"])
        .iter()
        .filter(|c| c["type"] == "content")
        .map(|c| content_text(&c["content"]))
        .filter(|s| !s.is_empty())
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}
fn first<'a>(v: &'a Value, keys: &[&str]) -> Value {
    keys.iter()
        .find_map(|k| v.get(*k).filter(|v| !v.is_null()).cloned())
        .unwrap_or(Value::Null)
}
pub fn tool_detail(snapshot: &Value, terminal: Option<Value>) -> Value {
    let input = &snapshot["rawInput"];
    let output = &snapshot["rawOutput"];
    let title = snapshot
        .get("title")
        .cloned()
        .unwrap_or_else(|| json!("Tool"));
    let file = snapshot["locations"][0]
        .get("path")
        .cloned()
        .unwrap_or_else(|| {
            let f = first(input, &["path", "filePath", "file"]);
            if f.is_null() { title.clone() } else { f }
        });
    let text_content = tool_text(snapshot)
        .map(Value::String)
        .unwrap_or(Value::Null);
    let content = if text_content.is_null() {
        first(output, &["content", "text"])
    } else {
        text_content.clone()
    };
    let mut detail = match text(&snapshot["kind"]) {
        "read" => {
            json!({"type":"read","filePath":file,"content":content,"offset":first(input,&["offset","line"]),"limit":input["limit"]})
        }
        "edit" | "delete" => {
            let diff = array(&snapshot["content"])
                .iter()
                .find(|c| c["type"] == "diff");
            json!({"type":"edit","filePath":file,"oldString":diff.and_then(|v|v.get("oldText")).cloned().unwrap_or_else(||first(input,&["oldText","oldString"])),"newString":if snapshot["kind"]=="delete" {json!("")} else {diff.and_then(|v|v.get("newText")).cloned().unwrap_or_else(||first(input,&["newText","newString"]))},"unifiedDiff":text_content})
        }
        "search" => {
            json!({"type":"search","query":input.get("query").or_else(||input.get("pattern")).unwrap_or(&title),"toolName":"search","content":content,"filePaths":array(&snapshot["locations"]).iter().map(|l|l["path"].clone()).collect::<Vec<_>>()})
        }
        "execute" => shell_detail(snapshot, terminal),
        "fetch" => {
            json!({"type":"fetch","url":input.get("url").unwrap_or(&title),"prompt":input["prompt"],"result":if text_content.is_null(){first(output,&["result","text","content"])}else{text_content},"code":first(output,&["status","code"])})
        }
        "think" | "switch_mode" => {
            json!({"type":"plain_text","label":title,"icon":if snapshot["kind"]=="think" {"brain"} else {"sparkles"},"text":if text_content.is_null(){if snapshot["kind"]=="think" {stringify(output)} else {stringify(input)}}else{text_content}})
        }
        _ if terminal.is_some() => shell_detail(snapshot, terminal),
        _ if !text_content.is_null() => {
            json!({"type":"plain_text","label":title,"text":text_content,"icon":"wrench"})
        }
        _ => json!({"type":"unknown","input":input,"output":output}),
    };
    if detail["type"] != "unknown" {
        if let Some(o) = detail.as_object_mut() {
            o.retain(|_, v| !v.is_null());
        }
    }
    detail
}
fn stringify(v: &Value) -> Value {
    if v.is_null() {
        Value::Null
    } else if v.is_string() {
        v.clone()
    } else {
        json!(v.to_string())
    }
}
fn shell_detail(s: &Value, terminal: Option<Value>) -> Value {
    let input = &s["rawInput"];
    let output = &s["rawOutput"];
    let mut command = input
        .get("command")
        .cloned()
        .unwrap_or_else(|| s["title"].clone());
    if let Some(args) = input["args"].as_array() {
        command = json!(
            std::iter::once(text(&command))
                .chain(args.iter().filter_map(Value::as_str))
                .collect::<Vec<_>>()
                .join(" ")
        );
    }
    let mut value = json!({"type":"shell","command":command,"cwd":input["cwd"],"output":tool_text(s).map(Value::String).unwrap_or_else(||first(output,&["output","text"])),"exitCode":output["exitCode"]});
    if let Some(terminal) = terminal {
        value["output"] = terminal["output"].clone();
        value["exitCode"] = terminal["exitStatus"]["exitCode"].clone();
    }
    value
}
pub fn tool_item(snapshot: &Value, terminal: Option<Value>) -> Value {
    let status = match text(&snapshot["status"]) {
        "completed" => "completed",
        "failed" => "failed",
        _ => "running",
    };
    json!({"type":"tool_call","callId":snapshot["toolCallId"],"name":snapshot.get("kind").filter(|v| !v.is_null()).unwrap_or(&snapshot["title"]),"detail":tool_detail(snapshot,terminal),"metadata":{"kind":snapshot["kind"],"title":snapshot["title"]},"status":status,"error":if status=="failed" {json!({"message":snapshot["rawOutput"].as_str().or_else(||snapshot["rawOutput"]["message"].as_str()).or_else(||snapshot["rawOutput"]["error"].as_str()).unwrap_or("Tool call failed")})} else {Value::Null}})
}
pub fn chooser(options: &Value) -> bool {
    let mut seen = HashSet::new();
    array(options).iter().any(|o| {
        let kind = text(&o["kind"]);
        kind.starts_with("allow") && !seen.insert(kind)
    })
}
pub fn permission_option(options: &Value, response: &Value) -> Result<Option<Value>> {
    let behavior = text(&response["behavior"]);
    if !matches!(behavior, "allow" | "deny") {
        bail!("Invalid ACP permission behavior");
    }
    if let Some(id) = response.get("selectedActionId") {
        return array(options)
            .iter()
            .find(|o| {
                &o["optionId"] == id
                    && (if text(&o["kind"]).starts_with("allow") {
                        "allow"
                    } else {
                        "deny"
                    }) == behavior
            })
            .cloned()
            .map(Some)
            .ok_or_else(|| {
                anyhow::anyhow!("ACP permission action does not exist or does not match behavior")
            });
    }
    for kind in if behavior == "allow" {
        ["allow_once", "allow_always"]
    } else {
        ["reject_once", "reject_always"]
    } {
        if let Some(o) = array(options).iter().find(|o| o["kind"] == kind) {
            return Ok(Some(o.clone()));
        }
    }
    Ok(None)
}
pub fn permission(provider: &str, id: &str, params: &Value, snapshot: &Value) -> Value {
    let detail = if chooser(&params["options"]) {
        tool_text(&params["toolCall"])
            .map(
                |s| json!({"type":"plain_text","label":snapshot["title"],"text":s,"icon":"wrench"}),
            )
            .unwrap_or_else(|| tool_detail(snapshot, None))
    } else {
        tool_detail(snapshot, None)
    };
    json!({"id":id,"provider":provider,"name":snapshot.get("kind").unwrap_or(&snapshot["title"]),"kind":if snapshot["kind"]=="switch_mode"{"mode"}else{"tool"},"title":params["toolCall"].get("title").unwrap_or(&snapshot["title"]),"detail":detail,"actions":array(&params["options"]).iter().map(|o|json!({"id":o["optionId"],"label":o["name"],"behavior":if text(&o["kind"]).starts_with("allow"){"allow"}else{"deny"}})).collect::<Vec<_>>(),"metadata":{"toolCallId":params["toolCall"]["toolCallId"],"rawRequest":params,"options":params["options"]}})
}
pub fn commands(params: &Value, kiro: bool) -> Vec<Value> {
    if !kiro {
        return array(&params["availableCommands"]).iter().map(|c|json!({"name":c["name"],"description":c["description"],"argumentHint":text(&c["input"]["hint"]),"kind":"command"})).collect();
    }
    let mut seen = HashSet::new();
    let source = array(&params["commands"])
        .iter()
        .chain(array(&params["prompts"]));
    source.filter_map(|c| {
        let name=text(&c["name"]).trim().trim_start_matches('/');
        if name.is_empty()||!seen.insert(name.to_owned()){return None;}
        Some(json!({"name":name,"description":text(&c["description"]),"argumentHint":text(&c["meta"]["hint"]),"kind":if text(&c["serverName"]).starts_with("skill:"){"skill"}else{"command"}}))
    }).collect()
}
pub fn usage(v: &Value) -> Value {
    let mut result = json!({});
    for (target, source) in [
        ("inputTokens", "inputTokens"),
        ("outputTokens", "outputTokens"),
        ("cachedInputTokens", "cachedReadTokens"),
    ] {
        if let Some(value) = v.get(source) {
            result[target] = value.clone();
        }
    }
    result
}

pub fn create_config(input: &Value, provider: &str) -> Result<Value> {
    let unattended = input["unattended"] == true || input["parent"]["isUnattended"] == true;
    let mut result = json!({});
    if let Some(features) = input.get("featureValues") {
        result["featureValues"] = features.clone();
    }
    if unattended && input["featureValues"].get("auto_accept").is_none() {
        if !result["featureValues"].is_object() {
            result["featureValues"] = json!({});
        }
        result["featureValues"]["auto_accept"] = json!(true);
    }
    let requested = input["requestedMode"].as_str();
    let parent = input.get("parent").filter(|parent| !parent.is_null());
    if requested.is_none()
        && unattended
        && parent.is_some_and(|parent| parent["provider"] != provider)
    {
        return Ok(result);
    }
    let mut adjusted = input.clone();
    if adjusted.get("provider").is_none() {
        adjusted["provider"] = json!(provider);
    }
    if let Some(features) = result.get("featureValues") {
        adjusted["featureValues"] = features.clone();
    }
    crate::providers::create_config::resolve_default(&adjusted)
}
