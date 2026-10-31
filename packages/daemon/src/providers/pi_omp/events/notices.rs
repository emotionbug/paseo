use super::tools::content_text;
use super::{array, first, put, string};
use serde_json::{Value, json};
use sha1::{Digest, Sha1};
fn hash(parts: &[&str], separated: bool) -> String {
    let mut h = Sha1::new();
    for p in parts {
        h.update(p.as_bytes());
        if separated {
            h.update([0]);
        }
    }
    hex::encode(h.finalize())[..12].to_owned()
}
pub fn custom_item(family: &str, message: &Value, call_id: &str) -> Option<Value> {
    if message["display"] == false {
        return None;
    }
    let text = content_text(&message["content"]);
    if text.is_empty() {
        return None;
    }
    let kind = first(&[&message["customType"]]).unwrap_or("custom-message");
    if family == "omp" {
        if kind == "skill-prompt" && message["attribution"] == "user" {
            if let Some(name) = first(&[&message["details"]["name"]]) {
                let args = string(&message["details"]["args"]).trim();
                return Some(
                    json!({"type":"user_message","text":format!("/skill:{name}{}",if args.is_empty(){String::new()}else{format!(" {args}")})}),
                );
            }
        }
        if kind == "advisor" {
            let notes: Vec<&Value> = array(&message["details"]["notes"])
                .iter()
                .filter(|n| first(&[&n["note"]]).is_some())
                .collect();
            let blockers = notes.iter().filter(|n| n["severity"] == "blocker").count();
            let mut label = if notes.is_empty() {
                "Advisor".into()
            } else {
                format!(
                    "Advisor · {} {}",
                    notes.len(),
                    if notes.len() == 1 { "note" } else { "notes" }
                )
            };
            if blockers > 0 {
                label.push_str(&format!(
                    " · {blockers} {}",
                    if blockers == 1 { "blocker" } else { "blockers" }
                ));
            }
            let body = if notes.is_empty() {
                text.clone()
            } else {
                notes
                    .iter()
                    .map(|n| {
                        let mut p = Vec::new();
                        if matches!(string(&n["severity"]), "nit" | "concern" | "blocker") {
                            p.push(format!("[{}]", string(&n["severity"])));
                        }
                        if let Some(a) = first(&[&n["advisor"]]) {
                            p.push(format!("[{}]", a.trim()));
                        }
                        p.push(string(&n["note"]).trim().to_owned());
                        p.join(" ")
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n")
            };
            return Some(
                json!({"type":"tool_call","callId":format!("omp-advisor:{}",first(&[&message["id"]]).map(str::trim).map(str::to_owned).unwrap_or_else(||hash(&[text.trim()],false))),"name":"advisor","status":"completed","detail":{"type":"plain_text","label":label,"text":body,"icon":"brain"},"metadata":{"synthetic":true,"source":"omp_advisor","noteCount":notes.len(),"blockerCount":blockers},"error":null}),
            );
        }
        if let Some(item) = system_notice(&text) {
            return Some(item);
        }
    }
    let mut metadata = json!({"synthetic":true,"customType":kind});
    if let Some(v) = message.get("details") {
        metadata["details"] = v.clone();
    }
    Some(
        json!({"type":"tool_call","callId":call_id,"name":kind,"status":"completed","detail":{"type":"plain_text","text":text},"metadata":metadata,"error":null}),
    )
}
pub fn system_notice(text: &str) -> Option<Value> {
    if !text.trim_start().starts_with("<system-notice>") {
        return None;
    }
    let tag = cached_regex!(r"(?i)<task-result\b([^>]*)>");
    let mut attrs = std::collections::HashMap::new();
    if let Some(c) = tag.captures(text) {
        let re = cached_regex!(r#"([\w-]+)=["'“‘]([^"'“”‘’]*)["'”’]"#);
        for a in re.captures_iter(&c[1]) {
            attrs.insert(a[1].to_owned(), a[2].trim().to_owned());
        }
    }
    let status = attrs
        .get("status")
        .map(String::as_str)
        .unwrap_or("completed");
    let level = match status.to_lowercase().as_str() {
        "failed" | "error" => "error",
        "canceled" | "cancelled" | "stopped" => "warning",
        _ => "info",
    };
    let label = if let Some(id) = attrs.get("id").filter(|s| !s.is_empty()) {
        format!("Background job {id} {status}")
    } else {
        text.split_once("<system-notice>")
            .map(|(_, b)| b.split("</system-notice>").next().unwrap_or(b))
            .unwrap_or("")
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with('<'))
            .unwrap_or("System notice")
            .to_owned()
    };
    Some(json!({"type":"notification","level":level,"message":label}))
}
fn status(
    id: String,
    name: &str,
    label: String,
    text: String,
    icon: &str,
    state: &str,
    error: Value,
    metadata: Value,
) -> Value {
    let mut detail = json!({"type":"plain_text","label":label,"icon":icon});
    if !text.is_empty() {
        detail["text"] = json!(text);
    }
    json!({"type":"tool_call","callId":id,"name":name,"detail":detail,"metadata":metadata,"status":state,"error":error})
}
pub fn runtime(event: &Value) -> Option<Value> {
    let t = string(&event["type"]);
    match t {
        "notice"
            if matches!(string(&event["level"]), "info" | "warning" | "error")
                && event["message"].is_string() =>
        {
            Some(json!({"type":"notification","level":event["level"],"message":event["message"]}))
        }
        "auto_compaction_start" | "auto_compaction_end" => {
            let mut item = json!({"type":"compaction","status":if t.ends_with("start"){"loading"}else{"completed"},"trigger":"auto"});
            if event["result"]["tokensBefore"].is_number() {
                item["preTokens"] = event["result"]["tokensBefore"].clone();
            }
            Some(item)
        }
        "goal_updated" => {
            let goal = event
                .get("goal")
                .filter(|v| !v.is_null())
                .unwrap_or(&event["state"]["goal"]);
            let mut lines = Vec::new();
            if goal.is_null() {
                lines.push("OMP goal cleared.".to_owned());
            } else {
                lines.push(
                    first(&[&goal["objective"]])
                        .map(str::trim)
                        .unwrap_or("OMP goal updated.")
                        .into(),
                );
                for (key, label, suffix) in [
                    ("status", "Status", ""),
                    ("tokensUsed", "Tokens used", ""),
                    ("tokenBudget", "Token budget", ""),
                    ("timeUsedSeconds", "Time used", "s"),
                ] {
                    if let Some(v) = goal.get(key) {
                        lines.push(format!(
                            "{label}: {}{suffix}",
                            v.as_str()
                                .map(str::to_owned)
                                .unwrap_or_else(|| v.to_string())
                        ));
                    }
                }
                if let Some(mode) = first(&[&event["state"]["mode"]]) {
                    lines.push(format!("Mode: {mode}"));
                }
            }
            let text = lines.join("\n");
            let mut meta = json!({"synthetic":true,"source":"omp_goal_updated"});
            put(&mut meta, "goalId", first(&[&goal["id"]]));
            put(&mut meta, "goalStatus", first(&[&goal["status"]]));
            Some(status(
                format!(
                    "omp-goal:{}",
                    first(&[&goal["id"]])
                        .map(str::to_owned)
                        .unwrap_or_else(|| hash(&[&text], true))
                ),
                "omp_goal_updated",
                first(&[&goal["status"]])
                    .map(|s| format!("OMP goal {s}"))
                    .unwrap_or_else(|| "OMP goal updated".into()),
                text,
                "brain",
                "completed",
                Value::Null,
                meta,
            ))
        }
        "auto_retry_start"
            if event["attempt"].is_number()
                && event["maxAttempts"].is_number()
                && event["delayMs"].is_number()
                && event["errorMessage"].is_string() =>
        {
            let delay = event["delayMs"].as_f64().unwrap_or(0.);
            let delay = if delay < 1000. {
                format!("{delay}ms")
            } else if delay % 1000. == 0. {
                format!("{}s", delay / 1000.)
            } else {
                format!("{:.1}s", delay / 1000.)
            };
            let mut m = json!({"synthetic":true,"source":"omp_auto_retry_start","attempt":event["attempt"],"maxAttempts":event["maxAttempts"],"delayMs":event["delayMs"]});
            if let Some(v) = event.get("errorId") {
                m["errorId"] = v.clone();
            }
            Some(status(
                format!("omp-auto-retry:{}", event["attempt"]),
                "omp_auto_retry",
                format!("OMP retry {}/{}", event["attempt"], event["maxAttempts"]),
                format!("Retrying in {delay}: {}", string(&event["errorMessage"])),
                "sparkles",
                "running",
                Value::Null,
                m,
            ))
        }
        "auto_retry_end" if event["attempt"].is_number() && event["success"].is_boolean() => {
            let ok = event["success"] == true;
            Some(status(
                format!("omp-auto-retry:{}", event["attempt"]),
                "omp_auto_retry",
                format!(
                    "OMP retry {} {}",
                    event["attempt"],
                    if ok { "recovered" } else { "failed" }
                ),
                event["finalError"]
                    .as_str()
                    .unwrap_or(if ok {
                        "Retry recovered."
                    } else {
                        "Retry failed."
                    })
                    .into(),
                "sparkles",
                if ok { "completed" } else { "failed" },
                if ok {
                    Value::Null
                } else {
                    json!(event["finalError"].as_str().unwrap_or("OMP retry failed"))
                },
                json!({"synthetic":true,"source":"omp_auto_retry_end","attempt":event["attempt"],"success":ok}),
            ))
        }
        "retry_fallback_applied"
            if event["role"].is_string()
                && event["from"].is_string()
                && event["to"].is_string() =>
        {
            let role = string(&event["role"]);
            let from = string(&event["from"]);
            let to = string(&event["to"]);
            Some(status(
                format!("omp-retry-fallback:{}", hash(&[role, from, to], true)),
                "omp_retry_fallback",
                format!("OMP fallback applied for {role}"),
                format!("{from} -> {to}"),
                "sparkles",
                "completed",
                Value::Null,
                json!({"synthetic":true,"source":"omp_retry_fallback_applied","role":role,"from":from,"to":to}),
            ))
        }
        "retry_fallback_succeeded" if event["role"].is_string() && event["model"].is_string() => {
            let role = string(&event["role"]);
            let model = string(&event["model"]);
            Some(status(
                format!(
                    "omp-retry-fallback-succeeded:{}",
                    hash(&[role, model], true)
                ),
                "omp_retry_fallback",
                format!("OMP fallback succeeded for {role}"),
                format!("Using {model}"),
                "sparkles",
                "completed",
                Value::Null,
                json!({"synthetic":true,"source":"omp_retry_fallback_succeeded","role":role,"model":model}),
            ))
        }
        _ => None,
    }
}
