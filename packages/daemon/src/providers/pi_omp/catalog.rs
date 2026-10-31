use serde_json::{Value, json};
pub fn capabilities(omp: bool, mcp: bool) -> Value {
    let mut result = json!({"supportsStreaming":true,"supportsSessionPersistence":true,"supportsSessionListing":true,"supportsDynamicModes":true,"supportsMcpServers":mcp,"supportsReasoningStream":true,"supportsToolInvocations":true,"supportsRewindConversation":true,"supportsRewindFiles":false,"supportsRewindBoth":false});
    if omp {
        result["supportsNativePaseoTools"] = json!(true);
    }
    result
}
pub fn modes(omp: bool) -> Vec<Value> {
    if !omp {
        return vec![];
    }
    vec![
        json!({"id":"full","label":"Full Access","description":"Launches OMP with yolo approval mode so tools run without prompts.","icon":"ShieldOff","colorTier":"dangerous","isUnattended":true}),
        json!({"id":"write","label":"Write Approval","description":"Launches OMP with write approval mode — reads are free, writes require approval.","icon":"ShieldAlert","colorTier":"moderate"}),
        json!({"id":"ask","label":"Always Ask","description":"Launches OMP with always-ask approval mode for write and exec tools.","icon":"ShieldCheck","colorTier":"safe"}),
    ]
}
pub fn features(state: &Value) -> Vec<Value> {
    if !state["fastModeEnabled"].is_boolean() || !state["fastModeActive"].is_boolean() {
        return vec![];
    }
    let mut feature = json!({"type":"toggle","id":"fast_mode","label":"Fast","icon":"zap","value":state["fastModeEnabled"] == true});
    if state["fastModeEnabled"] == true && state["fastModeActive"] != true {
        feature["description"] = json!("Fast is enabled but does not apply to this model.");
        feature["tooltip"] = feature["description"].clone();
    }
    vec![feature]
}
pub fn model_id(model: &Value) -> Option<String> {
    Some(format!(
        "{}/{}",
        model["provider"].as_str()?,
        model["id"].as_str()?
    ))
}
pub fn model(model: &Value, provider: &str, omp: bool) -> Value {
    let native = model["provider"].as_str().unwrap_or("");
    let id = model["id"].as_str().unwrap_or("");
    let name = model["name"]
        .as_str()
        .unwrap_or(id)
        .rsplit('/')
        .find(|v| !v.is_empty())
        .unwrap_or(id);
    let label = name
        .replace('_', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let label = label
        .split_once(": ")
        .map_or(label.as_str(), |(_, label)| label)
        .trim();
    let mut result = json!({"provider":provider,"id":format!("{native}/{id}"),"label":label,"description":format!("{native}/{id}"),"metadata":{"provider":native,"modelId":id}});
    if model["reasoning"] != true {
        return result;
    }
    let levels = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];
    let efforts = model.pointer("/thinking/efforts").and_then(Value::as_array);
    let reported: Vec<&str> = if omp {
        efforts
            .map(|efforts| {
                std::iter::once("auto")
                    .chain(levels.iter().copied())
                    .filter(|level| efforts.iter().any(|v| v == *level))
                    .collect()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let has_reported = !reported.is_empty();
    let mut supported: Vec<&str> = if omp {
        if has_reported {
            reported
        } else {
            levels.to_vec()
        }
    } else {
        levels
            .iter()
            .copied()
            .filter(|level| {
                let mapped = model["thinkingLevelMap"].get(*level);
                mapped != Some(&Value::Null)
                    && (!matches!(*level, "xhigh" | "max") || mapped.is_some())
            })
            .collect()
    };
    let default = if omp && has_reported {
        model
            .pointer("/thinking/defaultLevel")
            .and_then(Value::as_str)
            .filter(|v| supported.contains(v))
            .unwrap_or_else(|| supported.first().copied().unwrap_or("medium"))
    } else {
        supported
            .iter()
            .copied()
            .find(|v| matches!(*v, "medium" | "high" | "xhigh" | "max"))
            .or_else(|| supported.last().copied())
            .unwrap_or("off")
    }
    .to_string();
    if omp {
        supported.retain(|level| *level != "auto");
        supported.insert(0, "auto");
    }
    result["thinkingOptions"] = json!(
        supported
            .iter()
            .map(|level| {
                let (label, description) = match *level {
                    "auto" => ("Auto", "OMP picks the effort each turn"),
                    "off" => ("Off", "No extra reasoning"),
                    "minimal" => ("Minimal", "Light reasoning"),
                    "low" => ("Low", "Faster reasoning"),
                    "medium" => ("Medium", "Balanced reasoning"),
                    "high" => ("High", "Deeper reasoning"),
                    "xhigh" => (
                        "XHigh",
                        if omp {
                            "Extra-high reasoning"
                        } else {
                            "Very deep reasoning"
                        },
                    ),
                    _ => (
                        "Max",
                        if omp {
                            "Maximum reasoning"
                        } else {
                            "Extreme reasoning"
                        },
                    ),
                };
                let mut option = json!({"id":level,"label":label,"description":description});
                if *level == default {
                    option["isDefault"] = json!(true);
                }
                option
            })
            .collect::<Vec<_>>()
    );
    result["defaultThinkingOptionId"] = json!(default);
    result
}
pub fn commands(native: &[Value], omp: bool) -> Vec<Value> {
    let mut result = vec![
        json!({"name":"compact","description":"Manually compact the session context","argumentHint":"[instructions]","kind":"command"}),
        json!({"name":"autocompact","description":"Toggle automatic context compaction","argumentHint":"[on|off|toggle]","kind":"command"}),
    ];
    if omp {
        result.extend([json!({"name":"handoff","description":"Hand off from planning to implementation","argumentHint":"[instructions]","kind":"command"}),json!({"name":"steer","description":"Steer the active OMP turn","argumentHint":"<message>","kind":"command"}),json!({"name":"follow-up","description":"Queue a follow-up message for OMP","argumentHint":"<message>","kind":"command"})]);
    }
    for command in native {
        let Some(name) = command["name"].as_str() else {
            continue;
        };
        let existing = result.iter().position(|v| v["name"] == name);
        let hint = if omp {
            command.pointer("/input/hint").and_then(Value::as_str)
        } else {
            None
        }
        .or_else(|| existing.and_then(|i| result[i]["argumentHint"].as_str()))
        .unwrap_or("");
        let mapped = json!({"name":name,"description":command["description"].as_str().or(command["source"].as_str()).unwrap_or("command"),"argumentHint":hint,"kind":if command["source"]=="skill" {"skill"} else {"command"}});
        if let Some(index) = existing {
            result[index] = mapped;
        } else {
            result.push(mapped);
        }
    }
    result
}
