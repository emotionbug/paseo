use super::{array, first, put, string};
use serde_json::{Value, json};

pub fn text(value: &Value) -> Option<String> {
    if let Some(s) = value.as_str() {
        return Some(s.to_owned());
    }
    for key in ["output", "stdout", "text"] {
        if let Some(s) = value[key].as_str() {
            if !s.is_empty() {
                return Some(s.to_owned());
            }
        }
    }
    let parts: Vec<&str> = array(&value["content"])
        .iter()
        .filter(|p| p["type"] == "text")
        .filter_map(|p| p["text"].as_str())
        .collect();
    (!parts.is_empty()).then(|| parts.join("\n"))
}
pub fn content_text(content: &Value) -> String {
    content.as_str().map(str::to_owned).unwrap_or_else(|| {
        array(content)
            .iter()
            .filter(|p| p["type"] == "text")
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n\n")
    })
}
pub fn failure(result: &Value) -> String {
    if let Some(s) = first(&[
        &result["details"]["error"],
        &result["details"]["error"]["message"],
    ]) {
        return s.lines().next().unwrap_or("").chars().take(240).collect();
    }
    if let Some(s) = text(result) {
        if let Some(m) = cached_regex!(r"(?i)(?:Command|Process) exited with code \d+").find(&s) {
            return m.as_str().to_owned();
        }
        if let Some(line) = s.lines().find(|s| !s.trim().is_empty()) {
            return line.trim().chars().take(240).collect();
        }
    }
    if let Some(code) = result
        .get("exitCode")
        .or_else(|| result.get("code"))
        .filter(|v| v.is_number())
    {
        return format!("Tool exited with code {code}");
    }
    "Tool call failed".into()
}
pub fn tool_item(
    family: &str,
    call_id: &str,
    name: &str,
    args: &Value,
    status: &str,
    result: &Value,
    error: &Value,
) -> Value {
    if family == "omp" && name == "todo" {
        return Value::Null;
    }
    let mut display_name = name.to_owned();
    if family == "omp" && matches!(name, "read" | "write") {
        if let Some(path) = args["path"].as_str().and_then(|p| p.strip_prefix("xd://")) {
            display_name = path
                .split(['/', '?', '#'])
                .next()
                .unwrap_or(name)
                .to_owned();
        } else if name == "write" && result["details"]["xdev"]["mode"] == "execute" {
            if let Some(n) = first(&[&result["details"]["xdev"]["tool"]]) {
                display_name = n.into();
            }
        }
    }
    if family != "omp" && name == "mcp" {
        let details = &result["details"];
        if let (Some(server), Some(tool)) =
            (first(&[&details["server"]]), first(&[&details["tool"]]))
        {
            display_name = format!("{}.{}", server.trim(), tool.trim());
        } else if let Some(tool) = first(&[&args["tool"]]) {
            if let Some(server) = first(&[&args["server"]]) {
                let prefix = format!("{}_", server.trim());
                display_name = format!(
                    "{}.{}",
                    server.trim(),
                    tool.trim()
                        .strip_prefix(prefix.as_str())
                        .unwrap_or(tool.trim())
                );
            } else if let Some((server, tool)) = tool.trim().split_once('_') {
                display_name = format!("{server}.{tool}");
            }
        }
    }
    let detail = detail(family, name, &display_name, args, result);
    json!({"type":"tool_call","callId":call_id,"name":display_name,"status":status,"detail":detail,"error":if status == "failed" {error.clone()} else {Value::Null}})
}
fn detail(family: &str, name: &str, display_name: &str, args: &Value, result: &Value) -> Value {
    let omp = family == "omp";
    let output = text(result);
    let details = &result["details"];
    if omp {
        if name == "write" && string(&args["path"]).starts_with("xd://") {
            if opaque(display_name) || display_name == "ast_grep" {
                let decoded =
                    serde_json::from_str::<Value>(string(&args["content"])).unwrap_or(Value::Null);
                return detail(family, display_name, display_name, &decoded, result);
            }
        }
        match name {
            "task" => {
                let task = array(&args["tasks"])
                    .iter()
                    .find(|v| v.is_object())
                    .unwrap_or(&Value::Null);
                let mut d =
                    json!({"type":"sub_agent","log":output.as_deref().unwrap_or("").trim()});
                put(
                    &mut d,
                    "subAgentType",
                    first(&[
                        &task["name"],
                        &args["name"],
                        &args["agent"],
                        &args["subAgentType"],
                        &args["agentType"],
                        &args["type"],
                    ]),
                );
                let instruction = string(&task["task"])
                    .lines()
                    .map(|l| {
                        cached_regex!(r"(?i)^#+\s*(?:Target|Goal|Task)\s*")
                            .replace(l, "")
                            .trim()
                            .to_owned()
                    })
                    .find(|l| !l.is_empty());
                put(
                    &mut d,
                    "description",
                    first(&[&args["i"]]).or(instruction.as_deref()).or(first(&[
                        &args["description"],
                        &args["task"],
                        &args["prompt"],
                        &args["assignment"],
                    ])),
                );
                if let Some(id) = first(&[
                    &details["sessionFile"],
                    &details["session_file"],
                    &details["childSessionId"],
                ]) {
                    d["childSessionId"] = json!(id);
                } else if let Some(s) = output.as_deref() {
                    if let Some(c) =
                        cached_regex!(r"(?i)(?:session|transcript)(?: file)?:\s*(/\S+\.jsonl)")
                            .captures(s)
                    {
                        d["childSessionId"] = json!(&c[1]);
                    }
                }
                return d;
            }
            "yield" => return plain("Submitted subagent result", output.as_deref()),
            "wait" => {
                let job = array(&details["jobs"])
                    .iter()
                    .find(|v| v.is_object())
                    .unwrap_or(&Value::Null);
                let label = if let (Some(t), Some(s), Some(l)) = (
                    first(&[&job["type"]]),
                    first(&[&job["status"]]),
                    first(&[&job["label"], &job["id"]]),
                ) {
                    format!("{t} {s}: {l}")
                } else {
                    output
                        .as_deref()
                        .map(str::trim)
                        .and_then(|s| s.lines().next())
                        .unwrap_or("Waiting for background work")
                        .to_owned()
                };
                return plain(
                    &label,
                    first(&[&job["resultText"], &job["errorText"]])
                        .or(output.as_deref().map(str::trim)),
                );
            }
            "web_search" | "glob" | "ast_grep" => {
                let mut d = json!({"type":"search","query":first(&[&args["query"],&args["pattern"],&args["pat"]]).unwrap_or(if name == "web_search" {"Web search"} else {name})});
                if name != "ast_grep" {
                    d["toolName"] = json!(name);
                }
                put(&mut d, "content", output.as_deref());
                return d;
            }
            "think" => return plain("Thinking", first(&[&args["thoughts"]])),
            "ask" | "ask_user" => {
                let answers = details["results"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or(std::slice::from_ref(details));
                let lines: Vec<String> = answers
                    .iter()
                    .filter_map(|a| {
                        let q = first(&[&a["question"]])?;
                        let mut choices: Vec<&str> = array(&a["selectedOptions"])
                            .iter()
                            .filter_map(Value::as_str)
                            .collect();
                        if let Some(c) = first(&[&a["customInput"]]) {
                            choices.push(c);
                        }
                        Some(format!(
                            "{}\n{}",
                            q,
                            if choices.is_empty() {
                                "No selection".into()
                            } else {
                                choices.join(", ")
                            }
                        ))
                    })
                    .collect();
                let label = first(&[
                    &details["question"],
                    &args["questions"][0]["question"],
                    &args["question"],
                    &args["title"],
                ])
                .unwrap_or("Question");
                let answer_text = (!lines.is_empty()).then(|| lines.join("\n\n"));
                return plain(label, answer_text.as_deref().or(output.as_deref()));
            }
            _ if opaque(name) => {
                let label = [
                    first(&[
                        &args["action"],
                        &args["op"],
                        &args["operation"],
                        &args["command"],
                    ]),
                    first(&[
                        &args["name"],
                        &args["goal"],
                        &args["path"],
                        &args["query"],
                        &args["id"],
                        &args["report"],
                    ]),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" ");
                let label = if label.is_empty() {
                    name.replace('_', " ")
                } else {
                    label
                };
                return plain(&label, output.as_deref());
            }
            _ => {}
        }
    }
    let mut d = match name {
        "bash" if args["command"].is_string() => {
            let mut d = json!({"type":"shell","command":args["command"]});
            put(&mut d, "output", output.as_deref());
            if result.is_object() {
                d["exitCode"] = result
                    .get("exitCode")
                    .or_else(|| result.get("code"))
                    .cloned()
                    .unwrap_or(Value::Null);
            }
            d
        }
        "read" if args["path"].is_string() => {
            let path = string(&args["path"]);
            if omp && path.starts_with("xd://") {
                return plain(display_name, output.as_deref());
            }
            if omp && (path.starts_with("https://") || path.starts_with("http://")) {
                let mut d = json!({"type":"fetch","url":path});
                put(&mut d, "result", output.as_deref());
                return d;
            }
            let mut d = json!({"type":"read","filePath":path});
            put(
                &mut d,
                "content",
                if omp {
                    first(&[&details["displayContent"]["text"]]).or(output.as_deref())
                } else {
                    output.as_deref()
                },
            );
            for k in ["offset", "limit"] {
                if let Some(v) = args.get(k) {
                    d[k] = v.clone();
                }
            }
            d
        }
        "write" if args["path"].is_string() && args["content"].is_string() => {
            if omp && details.get("xdev").is_some() {
                let x = &details["xdev"];
                if x["mode"] == "execute" && first(&[&x["tool"]]).is_some() {
                    let mut r = result.clone();
                    r["details"] = x["inner"].clone();
                    return json!({"type":"unknown","input":x["args"],"output":r});
                }
                return json!({"type":"unknown","input":args,"output":result});
            }
            if omp && string(&args["path"]).starts_with("xd://") {
                return plain(display_name, output.as_deref());
            }
            json!({"type":"write","filePath":args["path"],"content":args["content"]})
        }
        "find" | "grep" if args["pattern"].is_string() => {
            let mut d = json!({"type":"search","query":args["pattern"],"toolName":if name=="grep"{"grep"}else{"search"}});
            put(&mut d, "content", result.as_str());
            d
        }
        "ls" if args.is_object() => {
            let mut d = json!({"type":"search","query":args["path"].as_str().unwrap_or("ls")});
            put(&mut d, "content", result.as_str());
            d
        }
        _ => json!({"type":"unknown","input":args,"output":result}),
    };
    if name == "edit" {
        let first_edit = &args["edits"][0];
        let old = first_edit["oldText"]
            .as_str()
            .or(args["old_string"].as_str())
            .or(args["oldString"].as_str());
        let new = first_edit["newText"]
            .as_str()
            .or(args["new_string"].as_str())
            .or(args["newString"].as_str());
        let patch_path = if omp {
            cached_regex!(r"(?m)^\[(.+?)#[^\]\n]+\]")
                .captures(string(&args["input"]))
                .map(|c| c[1].to_owned())
        } else {
            None
        };
        let override_path = if omp {
            first(&[&details["path"], &details["filePath"]]).or(patch_path.as_deref())
        } else {
            None
        };
        let path = override_path.or_else(|| {
            if args["edits"].is_array() || (old.is_some_and(|s| !s.is_empty()) && new.is_some()) {
                args["path"].as_str()
            } else {
                None
            }
        });
        if let Some(path) = path {
            d = json!({"type":"edit","filePath":path});
            put(
                &mut d,
                "oldString",
                if override_path.is_some() {
                    first(&[&details["oldText"], &details["old_string"]])
                } else {
                    old
                },
            );
            put(
                &mut d,
                "newString",
                if override_path.is_some() {
                    first(&[&details["newText"], &details["new_string"]])
                } else {
                    new
                },
            );
            put(&mut d, "unifiedDiff", details["diff"].as_str());
        }
    }
    if !omp
        && name == "ask_user_question"
        && details["answers"].is_array()
        && details["cancelled"].is_boolean()
    {
        let lines = array(&details["answers"])
            .iter()
            .map(|a| {
                format!(
                    "{}: {}",
                    string(&a["question"]),
                    if a["kind"] == "multi" {
                        array(&a["selected"])
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(", ")
                    } else {
                        string(&a["answer"]).into()
                    }
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        return plain(
            "Questions",
            Some(
                details["error"]
                    .as_str()
                    .unwrap_or(if details["cancelled"] == true {
                        "Cancelled"
                    } else {
                        &lines
                    }),
            ),
        );
    }
    if !omp
        && ((name == "subagent"
            && args["action"].is_null()
            && first(&[&args["agent"]]).is_some()
            && first(&[&args["task"]]).is_some())
            || (matches!(name, "subagent" | "Agent")
                && first(&[&args["subagent_type"]]).is_some()
                && first(&[&args["prompt"]]).is_some()))
    {
        d = json!({"type":"sub_agent","subAgentType":first(&[&args["agent"],&args["subagent_type"]]),"description":first(&[&args["description"],&args["task"],&args["prompt"]]),"log":output.as_deref().unwrap_or("").trim()});
    }
    d
}
pub fn plain(label: &str, text: Option<&str>) -> Value {
    let mut d = json!({"type":"plain_text","label":label});
    put(&mut d, "text", text);
    d
}
fn opaque(name: &str) -> bool {
    matches!(
        name,
        "ast_edit"
            | "debug"
            | "eval"
            | "github"
            | "checkpoint"
            | "rewind"
            | "context_notes"
            | "new_context"
            | "security_scan"
            | "memory_edit"
            | "retain"
            | "recall"
            | "reflect"
            | "learn"
            | "manage_skill"
            | "yield"
            | "goal"
    )
}
