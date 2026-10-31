use chrono::{DateTime, SecondsFormat, Utc};
use regex::Regex;
use serde_json::{Value, json};
use std::{collections::HashMap, sync::LazyLock};

fn string<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| v.get(*k).and_then(Value::as_str))
}
fn nonempty<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| v.get(*k).and_then(Value::as_str).filter(|s| !s.is_empty()))
}
fn put(v: &mut Value, key: &str, text: Option<&str>) {
    if let Some(text) = text.filter(|s| !s.is_empty()) {
        v[key] = text.into();
    }
}
fn kind(v: &Value) -> &str {
    match v["type"].as_str().unwrap_or("") {
        "UserMessage" => "userMessage",
        "AgentMessage" => "agentMessage",
        "Reasoning" => "reasoning",
        "Plan" => "plan",
        "CommandExecution" => "commandExecution",
        "FileChange" => "fileChange",
        "McpToolCall" => "mcpToolCall",
        "WebSearch" => "webSearch",
        "CollabAgentToolCall" => "collabAgentToolCall",
        "SubAgentActivity" => "subAgentActivity",
        "ImageView" => "imageView",
        "ImageGeneration" => "imageGeneration",
        other => other,
    }
}
pub fn status(raw: Option<&str>, error: &Value, output: &Value) -> &'static str {
    if !error.is_null() {
        return "failed";
    }
    match raw.unwrap_or("").trim().to_ascii_lowercase().as_str() {
        "failed" | "failure" | "error" | "errored" | "rejected" | "denied" | "declined" => "failed",
        "canceled" | "cancelled" | "interrupted" | "aborted" => "canceled",
        "completed" | "complete" | "done" | "success" | "succeeded" => "completed",
        "" if !output.is_null() => "completed",
        _ => "running",
    }
}
fn path(value: &str, cwd: Option<&str>) -> String {
    let value = value.trim();
    if let Some(cwd) = cwd.filter(|s| !s.is_empty()) {
        let normalized = value.replace('\\', "/");
        let base = cwd.replace('\\', "/");
        let base = base.trim_end_matches('/');
        if normalized == base {
            return ".".into();
        }
        if let Some(rest) = normalized
            .strip_prefix(base)
            .and_then(|s| s.strip_prefix('/'))
        {
            return rest.into();
        }
    }
    value.into()
}
fn unquote(s: &str) -> &str {
    if s.len() >= 2
        && ((s.starts_with('\'') && s.ends_with('\'')) || (s.starts_with('"') && s.ends_with('"')))
    {
        &s[1..s.len() - 1]
    } else {
        s
    }
}
pub fn command(v: &Value) -> Option<String> {
    static UNIX: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(?:(?:/[^/\s]+)*/)?(?:zsh|bash|sh)\s+-(?:lc|c)\s+([\s\S]+)$").unwrap()
    });
    static WINDOWS: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?i)^(?:"[^"]*\\)?(?:pwsh|powershell|cmd)(?:\.exe)?"?(?:\s+-[A-Za-z]+(?:\s+[^-\s][^\s]*)?)*\s+(?:-Command|-c|/c)\s+([\s\S]+)$"#).unwrap()
    });
    if let Some(s) = v.as_str() {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        return Some(
            UNIX.captures(s)
                .or_else(|| WINDOWS.captures(s))
                .map(|c| unquote(c.get(1).unwrap().as_str().trim()).to_owned())
                .unwrap_or_else(|| s.into()),
        );
    }
    let parts: Vec<_> = v
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if parts.is_empty() {
        return None;
    }
    if parts.len() >= 3 && matches!(parts[1], "-lc" | "-c") {
        return Some(parts[2].into());
    }
    let shell = unquote(parts[0])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if parts.len() >= 3
        && matches!(
            shell.as_str(),
            "pwsh" | "pwsh.exe" | "powershell" | "powershell.exe" | "cmd" | "cmd.exe"
        )
        && matches!(
            parts[1].to_ascii_lowercase().as_str(),
            "-command" | "-c" | "/c"
        )
    {
        return Some(unquote(&parts[2..].join(" ")).into());
    }
    Some(parts.join(" "))
}
fn shell_output(text: &str) -> String {
    let normalized = text.replace("\r\n", "\n");
    let lines: Vec<_> = normalized.split('\n').collect();
    if !lines[0]
        .trim()
        .to_ascii_lowercase()
        .starts_with("chunk id:")
        || !lines
            .iter()
            .take(8)
            .any(|s| s.trim().to_ascii_lowercase().starts_with("wall time:"))
        || !lines.iter().take(8).any(|s| {
            s.trim()
                .to_ascii_lowercase()
                .starts_with("process exited with code")
        })
    {
        return text.into();
    }
    let start = lines
        .iter()
        .position(|s| s.trim() == "Output:")
        .map(|n| n + 1)
        .or_else(|| {
            lines
                .iter()
                .enumerate()
                .skip(1)
                .find(|(_, s)| {
                    ![
                        "chunk id:",
                        "wall time:",
                        "process exited with code",
                        "original token count:",
                    ]
                    .iter()
                    .any(|p| s.trim().to_ascii_lowercase().starts_with(p))
                })
                .map(|(n, _)| n)
        })
        .unwrap_or(lines.len());
    lines[start..].join("\n")
}
fn truncate(text: &str) -> String {
    let length = text.encode_utf16().count();
    if length <= 12_000 {
        return text.into();
    }
    let prefix: Vec<u16> = text.encode_utf16().take(12_000).collect();
    format!(
        "{}\n...[truncated {} chars]",
        String::from_utf16_lossy(&prefix),
        length - 12_000
    )
}
fn directive(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    [
        ("*** Add File:", "add"),
        ("*** Update File:", "update"),
        ("*** Delete File:", "delete"),
    ]
    .iter()
    .find_map(|(prefix, k)| line.strip_prefix(prefix).map(|p| (*k, p.trim())))
}
fn diff(text: &str) -> Option<String> {
    if ["diff --git", "@@", "--- ", "+++ "]
        .iter()
        .any(|p| text.trim_start().starts_with(p))
    {
        return Some(text.into());
    }
    if !text.trim_start().starts_with("*** Begin Patch")
        && !text.lines().any(|s| directive(s).is_some())
    {
        return None;
    }
    let mut out = Vec::new();
    for line in text.lines() {
        if let Some((k, p)) = directive(line) {
            let p = p.trim_matches(['\'', '"']);
            if !p.is_empty() {
                if !out.is_empty() {
                    out.push(String::new());
                }
                out.push(format!("diff --git a/{p} b/{p}"));
                out.push(format!(
                    "--- {}",
                    if k == "add" {
                        "/dev/null".into()
                    } else {
                        format!("a/{p}")
                    }
                ));
                out.push(format!(
                    "+++ {}",
                    if k == "delete" {
                        "/dev/null".into()
                    } else {
                        format!("b/{p}")
                    }
                ));
            }
        } else if line.starts_with("@@")
            || line.starts_with(['+', '-', ' '])
            || line.starts_with("\\ No newline at end of file")
        {
            out.push(line.into());
        }
    }
    Some(if out.is_empty() {
        text.into()
    } else {
        out.join("\n").trim().into()
    })
}
fn edit_input(input: &Value) -> Value {
    let candidate = input.as_str().or_else(|| {
        nonempty(
            input,
            &["patch", "diff", "unified_diff", "unifiedDiff", "content"],
        )
    });
    let Some(text) = candidate else {
        return input.clone();
    };
    let mut result = if input.is_object() {
        input.clone()
    } else {
        json!({})
    };
    if string(&result, &["path", "file_path", "filePath"]).is_none() {
        if let Some((_, p)) = text.lines().find_map(directive) {
            result["path"] = p.into();
        }
    }
    for key in ["patch", "diff", "unified_diff", "unifiedDiff"] {
        result.as_object_mut().unwrap().remove(key);
    }
    if let Some(d) = diff(text) {
        result.as_object_mut().unwrap().remove("content");
        result["patch"] = truncate(&d).into();
    } else {
        result["content"] = text.into();
    }
    result
}
fn flatten(v: &Value) -> Option<String> {
    if let Some(s) = v.as_str() {
        return Some(s.into());
    }
    if let Some(a) = v.as_array() {
        let parts: Vec<_> = a
            .iter()
            .filter_map(flatten)
            .filter(|s| !s.is_empty())
            .collect();
        return (!parts.is_empty()).then(|| parts.join("\n"));
    }
    string(v, &["text", "content", "output"]).map(str::to_owned)
}
fn xml_read(v: &Value) -> Option<(Option<String>, Option<String>)> {
    static PATH: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?is)<path>(.*?)</path>").unwrap());
    static CONTENT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?is)<content>(.*?)</content>").unwrap());
    static ENTITY: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"&(?:#(\d+)|#x([0-9a-fA-F]+)|amp|lt|gt|quot|apos);").unwrap());
    let text = v.as_str()?.trim();
    if !text.starts_with("<path>") || !text.to_ascii_lowercase().contains("<content>") {
        return None;
    }
    let decode = |s: &str| {
        ENTITY
            .replace_all(s, |c: &regex::Captures<'_>| match &c[0] {
                "&amp;" => "&".into(),
                "&lt;" => "<".into(),
                "&gt;" => ">".into(),
                "&quot;" => "\"".into(),
                "&apos;" => "'".into(),
                _ => c
                    .get(1)
                    .and_then(|n| n.as_str().parse::<u32>().ok())
                    .or_else(|| {
                        c.get(2)
                            .and_then(|n| u32::from_str_radix(n.as_str(), 16).ok())
                    })
                    .and_then(char::from_u32)
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| c[0].into()),
            })
            .into_owned()
    };
    let p = PATH.captures(text).map(|c| decode(c[1].trim()));
    let content = CONTENT.captures(text).map(|c| {
        let s = c.get(1).unwrap().as_str();
        let s = s
            .strip_prefix("\r\n")
            .or_else(|| s.strip_prefix('\n'))
            .unwrap_or(s);
        let s = s
            .strip_suffix("\r\n")
            .or_else(|| s.strip_suffix('\n'))
            .unwrap_or(s);
        decode(s)
    });
    Some((p, content))
}
fn read_content(v: &Value) -> Option<String> {
    if let Some((_, content)) = xml_read(v) {
        return content;
    }
    if v.is_string() || v.is_array() {
        return flatten(v);
    }
    for key in ["content", "text", "output"] {
        if let Some(s) = flatten(&v[key]) {
            return Some(s);
        }
    }
    for key in ["data", "structuredContent", "structured_content"] {
        if let Some(s) = v.get(key).and_then(read_content) {
            return Some(s);
        }
    }
    None
}
fn strip_gutter(text: &str) -> (String, Option<u64>) {
    static GUTTER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*(\d+)\t(.*)$").unwrap());
    let mut out = Vec::new();
    let mut first = None;
    let mut previous = None;
    let mut count = 0;
    let mut matched = 0;
    for line in text.split('\n') {
        if line.is_empty() {
            out.push(String::new());
            continue;
        }
        count += 1;
        if let Some(c) = GUTTER.captures(line) {
            let n = c[1].parse::<u64>().unwrap_or(0);
            if previous.is_some_and(|p| n != p + 1) {
                return (text.into(), None);
            }
            first.get_or_insert(n);
            previous = Some(n);
            matched += 1;
            out.push(c[2].into());
        } else {
            if first.is_none() {
                return (text.into(), None);
            }
            out.push(line.into());
        }
    }
    if matched * 2 < count {
        (text.into(), None)
    } else {
        (out.join("\n"), first)
    }
}
fn detail(name: &str, input: &Value, output: &Value, cwd: Option<&str>) -> Value {
    let unknown = || json!({"type":"unknown","input":input,"output":output});
    if name == "speak" {
        if let Some(text) = input
            .as_str()
            .or_else(|| input["text"].as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return json!({"type":"unknown","input":text,"output":null});
        }
        return unknown();
    }
    if matches!(
        name,
        "shell" | "Bash" | "bash" | "exec" | "exec_command" | "command"
    ) {
        if !input.is_null()
            && string(input, &["command", "cmd"]).is_none()
            && !input["command"].is_array()
            && !input["cmd"].is_array()
        {
            return unknown();
        }
        if !output.is_null() && !output.is_string() && !output.is_object() {
            return unknown();
        }
        let command = input
            .get("command")
            .or_else(|| input.get("cmd"))
            .and_then(|v| {
                if let Some(s) = v.as_str() {
                    Some(s.to_owned())
                } else {
                    v.as_array().map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                }
            })
            .or_else(|| {
                nonempty(output, &["command"])
                    .or_else(|| nonempty(&output["result"], &["command"]))
                    .map(str::to_owned)
            });
        let Some(command) = command.filter(|s| !s.is_empty()) else {
            return unknown();
        };
        let mut d = json!({"type":"shell","command":command});
        put(&mut d, "cwd", nonempty(input, &["cwd", "directory"]));
        let text = output
            .as_str()
            .or_else(|| {
                nonempty(
                    output,
                    &[
                        "output",
                        "text",
                        "content",
                        "aggregated_output",
                        "aggregatedOutput",
                    ],
                )
            })
            .or_else(|| {
                ["structuredContent", "structured_content", "result"]
                    .iter()
                    .find_map(|k| nonempty(&output[*k], &["output", "text", "content"]))
            });
        if let Some(text) = text {
            put(&mut d, "output", Some(&shell_output(text)));
        }
        if let Some(exit) = output
            .get("exitCode")
            .or_else(|| output.get("exit_code"))
            .or_else(|| output["metadata"].get("exitCode"))
            .or_else(|| output["metadata"].get("exit_code"))
            .filter(|v| v.is_number())
        {
            d["exitCode"] = exit.clone();
        }
        return d;
    }
    if matches!(name, "search" | "web_search") {
        return nonempty(input, &["query", "q", "pattern"])
            .map(|q| json!({"type":"search","query":q,"toolName":name}))
            .unwrap_or_else(unknown);
    }
    let t = match name {
        "read" | "read_file" => "read",
        "write" | "write_file" | "create_file" => "write",
        "edit" | "apply_patch" | "apply_diff" => "edit",
        _ => return unknown(),
    };
    if !input.is_null() && string(input, &["path", "file_path", "filePath"]).is_none() {
        return unknown();
    }
    let out = output["files"]
        .as_array()
        .and_then(|a| a.first())
        .unwrap_or(output);
    let xml = if t == "read" { xml_read(output) } else { None };
    let Some(p) = nonempty(input, &["path", "file_path", "filePath"])
        .or_else(|| nonempty(out, &["path", "file_path", "filePath"]))
        .or_else(|| xml.as_ref().and_then(|(p, _)| p.as_deref()))
    else {
        return unknown();
    };
    let mut d = json!({"type":t,"filePath":path(p,cwd)});
    match t {
        "read" => {
            if let Some(content) = read_content(output) {
                let (text, start) = strip_gutter(&content);
                put(&mut d, "content", Some(&text));
                if let Some(n) = start {
                    d["offset"] = n.into();
                }
            }
            for k in ["offset", "limit"] {
                if input[k].is_number() {
                    d[k] = input[k].clone();
                }
            }
        }
        "write" => put(
            &mut d,
            "content",
            nonempty(input, &["content", "new_content", "newContent"])
                .or_else(|| nonempty(output, &["content", "new_content", "newContent"])),
        ),
        _ => {
            put(
                &mut d,
                "oldString",
                nonempty(
                    input,
                    &["old_string", "old_str", "oldContent", "old_content"],
                ),
            );
            put(
                &mut d,
                "newString",
                nonempty(
                    input,
                    &[
                        "new_string",
                        "new_str",
                        "newContent",
                        "new_content",
                        "content",
                    ],
                )
                .or_else(|| nonempty(output, &["newContent", "new_content", "content"])),
            );
            if let Some(s) = nonempty(input, &["patch", "diff", "unified_diff", "unifiedDiff"])
                .or_else(|| nonempty(out, &["patch", "diff", "unified_diff", "unifiedDiff"]))
            {
                d["unifiedDiff"] = truncate(s).into();
            }
        }
    }
    d
}
fn speak(name: &str) -> bool {
    matches!(
        name,
        "speak"
            | "paseo.speak"
            | "paseo_voice.speak"
            | "mcp__paseo__speak"
            | "mcp__paseo_voice__speak"
    )
}
fn tool(
    id: &str,
    name: &str,
    input: &Value,
    output: &Value,
    error: &Value,
    state: &str,
    cwd: Option<&str>,
) -> Value {
    let name = if speak(name) { "speak" } else { name };
    let mut d = detail(name, input, output, cwd);
    if state != "running"
        && d["type"] == "edit"
        && !["unifiedDiff", "newString", "oldString"]
            .iter()
            .any(|k| d[*k].as_str().is_some_and(|s| !s.trim().is_empty()))
    {
        d = json!({"type":"unknown","input":input,"output":output});
    }
    json!({"type":"tool_call","callId":id,"name":name,"status":state,"error":if state=="failed" { if error.is_null(){json!({"message":"Tool call failed"})}else{error.clone()} }else{Value::Null},"detail":d})
}
pub fn tool_envelope(raw: &Value, cwd: Option<&str>) -> Option<Value> {
    let id = nonempty(raw, &["callId"])?.trim();
    let name = nonempty(raw, &["name"])?.trim();
    if id.is_empty() || name.is_empty() {
        return None;
    }
    let input = if matches!(name, "apply_patch" | "apply_diff") {
        edit_input(&raw["input"])
    } else {
        raw["input"].clone()
    };
    Some(tool(
        id,
        name,
        &input,
        &raw["output"],
        &raw["error"],
        status(Some("completed"), &raw["error"], &raw["output"]),
        cwd,
    ))
}
fn file_entries(changes: &Value, cwd: Option<&str>) -> Vec<Value> {
    fn entry(v: &Value, fallback: Option<&str>, cwd: Option<&str>) -> Option<Value> {
        let p = nonempty(v, &["path", "file_path", "filePath"]).or(fallback)?;
        if p.trim().is_empty() {
            return None;
        }
        let mut f = json!({"path":path(p,cwd)});
        put(&mut f, "kind", nonempty(v, &["kind", "type"]));
        put(
            &mut f,
            "diff",
            v.as_str().or_else(|| {
                nonempty(
                    v,
                    &[
                        "diff",
                        "patch",
                        "unified_diff",
                        "unifiedDiff",
                        "content",
                        "newString",
                    ],
                )
            }),
        );
        Some(f)
    }
    if let Some(a) = changes.as_array() {
        return a.iter().filter_map(|v| entry(v, None, cwd)).collect();
    }
    if changes["files"].is_array() {
        return file_entries(&changes["files"], cwd);
    }
    if let Some(f) = entry(changes, None, cwd) {
        return vec![f];
    }
    changes
        .as_object()
        .map(|m| {
            m.iter()
                .filter_map(|(p, v)| entry(v, Some(p), cwd))
                .collect()
        })
        .unwrap_or_default()
}
fn file_text(f: &Value) -> Value {
    let text = f["diff"].as_str().unwrap_or("");
    if f["kind"] == "delete" {
        let d = diff(text).unwrap_or_else(|| {
            let p = f["path"].as_str().unwrap_or("");
            let mut d = format!("diff --git a/{p} b/{p}\n--- a/{p}\n+++ /dev/null");
            let lines: Vec<_> = text.lines().collect();
            if !lines.is_empty() {
                d.push_str(&format!("\n@@ -1,{} +0,0 @@", lines.len()));
                for l in lines {
                    d.push_str("\n-");
                    d.push_str(l);
                }
            }
            d
        });
        return json!({"patch":truncate(&d)});
    }
    if text.is_empty() {
        return json!({});
    }
    if let Some(d) = diff(text) {
        json!({"patch":truncate(&d)})
    } else {
        json!({"content":text})
    }
}
pub fn item(raw: &Value, include_user: bool, cwd: Option<&str>) -> Option<Value> {
    item_with_images(raw, include_user, cwd, &mut |_, _| None)
}
fn item_with_images(
    raw: &Value,
    include_user: bool,
    cwd: Option<&str>,
    materialize: &mut dyn FnMut(&str, Option<&str>) -> Option<String>,
) -> Option<Value> {
    let k = kind(raw);
    match k {
        "userMessage" => {
            if !include_user {
                return None;
            }
            let text = raw["content"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter(|v| v["type"] == "text")
                        .filter_map(|v| v["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            let mut v = json!({"type":"user_message","text":text});
            put(&mut v, "messageId", nonempty(raw, &["id"]));
            put(
                &mut v,
                "clientMessageId",
                nonempty(raw, &["clientId", "client_id", "clientUserMessageId"]),
            );
            Some(v)
        }
        "agentMessage" => {
            if raw["delivery"] == "async" && nonempty(raw, &["id"]).is_some() {
                if let Some(q) = raw["questions"].as_array().filter(|a| {
                    !a.is_empty()
                        && a.iter().all(|q| {
                            q["title"].as_str().is_some_and(|s| !s.trim().is_empty())
                                && (q["options"].is_null()
                                    || q["options"].as_array().is_some_and(|a| {
                                        a.iter().all(|v| v.as_str().is_some_and(|s| !s.is_empty()))
                                    }))
                        })
                }) {
                    let text = q
                        .iter()
                        .map(|q| {
                            let title = q["title"].as_str().unwrap().trim();
                            let options = q["options"]
                                .as_array()
                                .map(|a| {
                                    a.iter()
                                        .filter_map(Value::as_str)
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                })
                                .unwrap_or_default();
                            if options.is_empty() {
                                title.into()
                            } else {
                                format!("{title}\n{options}")
                            }
                        })
                        .collect::<Vec<String>>()
                        .join("\n\n");
                    return Some(
                        json!({"type":"tool_call","callId":raw["id"],"name":"request_user_input_async","status":"completed","error":null,"detail":{"type":"plain_text","icon":"brain","text":text}}),
                    );
                }
            }
            let mut v =
                json!({"type":"assistant_message","text":raw["text"].as_str().unwrap_or("")});
            put(&mut v, "messageId", nonempty(raw, &["id"]));
            Some(v)
        }
        "reasoning" => {
            let joined = |key: &str| {
                raw[key]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .map(|v| {
                                v.as_str()
                                    .map(str::to_owned)
                                    .unwrap_or_else(|| v.to_string())
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default()
            };
            let mut text = joined("summary");
            if text.is_empty() {
                text = joined("content");
            }
            (!text.is_empty()).then(|| json!({"type":"reasoning","text":text}))
        }
        "plan" => {
            let text = raw["text"]
                .as_str()
                .unwrap_or("")
                .lines()
                .map(str::trim_end)
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .to_owned();
            if text.is_empty() {
                return None;
            }
            Some(
                json!({"type":"tool_call","callId":nonempty(raw,&["id","itemId"]).map(str::to_owned).unwrap_or_else(||format!("plan:{text}")),"name":"plan","status":"completed","error":null,"detail":{"type":"plan","text":text}}),
            )
        }
        "contextCompaction" => Some(json!({"type":"compaction","status":"completed"})),
        "imageView" | "imageGeneration" => image_item(raw, materialize),
        "commandExecution" => {
            let id = nonempty(raw, &["id"])?;
            let mut input = json!({});
            if let Some(c) = command(&raw["command"]) {
                input["command"] = c.into();
            }
            put(&mut input, "cwd", string(raw, &["cwd"]));
            let mut output = Value::Null;
            if nonempty(raw, &["aggregatedOutput"]).is_some() || raw.get("exitCode").is_some() {
                output = json!({});
                if let Some(c) = input.get("command") {
                    output["command"] = c.clone();
                }
                if let Some(s) = nonempty(raw, &["aggregatedOutput"]) {
                    put(&mut output, "output", Some(&shell_output(s)));
                }
                if let Some(v) = raw.get("exitCode") {
                    output["exitCode"] = v.clone();
                }
            }
            if input.as_object().unwrap().is_empty() {
                input = Value::Null;
            }
            Some(tool(
                id,
                "shell",
                &input,
                &output,
                &raw["error"],
                status(string(raw, &["status"]), &raw["error"], &output),
                string(raw, &["cwd"]),
            ))
        }
        "fileChange" => {
            let id = nonempty(raw, &["id"])?;
            let files = file_entries(&raw["changes"], cwd);
            let mut input = Value::Null;
            let mut output = Value::Null;
            if !files.is_empty() {
                input = json!({"files":files.iter().map(|f|{let mut v=json!({"path":f["path"]});if let Some(k)=f.get("kind"){v["kind"]=k.clone();}v}).collect::<Vec<_>>()});
                let fields = file_text(&files[0]);
                if !fields.as_object().unwrap().is_empty() {
                    input["path"] = files[0]["path"].clone();
                    for (k, v) in fields.as_object().unwrap() {
                        input[k] = v.clone();
                    }
                }
                output = json!({"files":files.iter().map(|f|{let mut v=f.clone();v.as_object_mut().unwrap().remove("diff");for(k,x)in file_text(f).as_object().unwrap(){v[k]=x.clone();}v}).collect::<Vec<_>>()});
            }
            Some(tool(
                id,
                "apply_patch",
                &input,
                &output,
                &raw["error"],
                status(string(raw, &["status"]), &raw["error"], &output),
                cwd,
            ))
        }
        "mcpToolCall" => {
            let id = nonempty(raw, &["id"])?;
            let name = nonempty(raw, &["tool"])?.trim();
            if name.is_empty() {
                return None;
            }
            let name = match nonempty(raw, &["server"])
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                Some(s) => format!("{s}.{name}"),
                None => name.into(),
            };
            let output = split_mcp_images(&raw["result"]).0;
            Some(tool(
                id,
                &name,
                &raw["arguments"],
                &output,
                &raw["error"],
                status(string(raw, &["status"]), &raw["error"], &output),
                cwd,
            ))
        }
        "webSearch" => {
            let id = nonempty(raw, &["id"])?;
            let input = raw
                .get("query")
                .map(|v| json!({"query":v}))
                .unwrap_or(Value::Null);
            Some(tool(
                id,
                "web_search",
                &input,
                &raw["action"],
                &raw["error"],
                status(
                    Some(string(raw, &["status"]).unwrap_or("completed")),
                    &raw["error"],
                    &raw["action"],
                ),
                cwd,
            ))
        }
        "collabAgentToolCall" | "subAgentActivity" => subagent(raw),
        "toolCall" | "tool_call" => tool_envelope(raw, cwd),
        _ => None,
    }
}
fn subagent(raw: &Value) -> Option<Value> {
    let id = nonempty(raw, &["id"])?;
    let mut state = status(string(raw, &["status"]), &raw["error"], &Value::Null);
    let mut d = json!({"type":"sub_agent","subAgentType":"Sub-agent","log":"","actions":[]});
    if kind(raw) == "subAgentActivity" {
        nonempty(raw, &["agentThreadId"])?;
        let p = string(raw, &["agentPath"])?;
        let k = string(raw, &["kind"])?;
        if !matches!(k, "started" | "interacted" | "interrupted") {
            return None;
        }
        state = if k == "interrupted" {
            "canceled"
        } else {
            "running"
        };
        let p = if p == "/root" {
            ""
        } else if let Some(p) = p.strip_prefix("/root/") {
            p
        } else {
            p.rsplit(['/', '\\']).next().unwrap_or("")
        };
        d["description"] = p.into();
        let name = p
            .split('/')
            .map(|s| s.replace(['_', '-'], " ").trim().to_owned())
            .filter(|s| !s.is_empty())
            .map(|s| {
                let mut c = s.chars();
                format!("{}{}", c.next().unwrap().to_uppercase(), c.as_str())
            })
            .collect::<Vec<_>>()
            .join(" / ");
        if !name.is_empty() {
            d["subAgentType"] = name.into();
        }
    } else {
        put(&mut d, "description", nonempty(raw, &["prompt"]));
        if state != "failed" {
            let states: Vec<_> = raw["agentsStates"]
                .as_object()
                .into_iter()
                .flat_map(|m| m.values())
                .filter_map(|v| nonempty(v, &["status"]))
                .map(|s| match s.trim().to_ascii_lowercase().as_str() {
                    "error" | "errored" => "running",
                    "shutdown" => "canceled",
                    "notfound" => "failed",
                    _ => status(Some(s), &Value::Null, &Value::Null),
                })
                .collect();
            if states.contains(&"failed") {
                state = "failed";
            } else if states.contains(&"canceled") {
                state = "canceled";
            } else if !states.is_empty() {
                state = if states.iter().all(|s| *s == "completed") {
                    "completed"
                } else {
                    "running"
                };
            }
        }
    }
    Some(
        json!({"type":"tool_call","callId":id,"name":"Sub-agent","status":state,"error":if state=="failed"{if raw["error"].is_null(){json!({"message":"Sub-agent failed"})}else{raw["error"].clone()}}else{Value::Null},"detail":d}),
    )
}
pub fn child_threads(raw: &Value) -> Vec<String> {
    if kind(raw) == "subAgentActivity" && raw["kind"] == "started" {
        return nonempty(raw, &["agentThreadId"])
            .map(|s| vec![s.into()])
            .unwrap_or_default();
    }
    if kind(raw) == "collabAgentToolCall" && raw["tool"] == "spawnAgent" {
        return raw["receiverThreadIds"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
    }
    vec![]
}
pub fn split_mcp_images(result: &Value) -> (Value, Vec<Value>) {
    let mut output = result.clone();
    let mut images = Vec::new();
    if let Some(a) = output.get_mut("content").and_then(Value::as_array_mut) {
        for block in a {
            if block["type"] == "image"
                && block["data"].is_string()
                && block["mimeType"].is_string()
            {
                images.push(json!({"data":block["data"],"mimeType":block["mimeType"]}));
                *block = json!({"type":"text","text":"[image]"});
            }
        }
    }
    (output, images)
}
fn render_image(
    image: &Value,
    materialize: &mut dyn FnMut(&str, Option<&str>) -> Option<String>,
) -> Option<Value> {
    let source = nonempty(image, &["path", "url"])
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let data = nonempty(image, &["data"])
        .or_else(|| source.filter(|s| s.to_ascii_lowercase().starts_with("data:image/")));
    let owned;
    let source = if let Some(s) =
        source.filter(|s| !s.to_ascii_lowercase().starts_with("data:image/"))
    {
        s
    } else if let Some(data) = data {
        owned = materialize(data, string(image, &["mimeType"]));
        if let Some(p) = owned
            .as_deref()
            .filter(|p| !p.is_empty() && !p.to_ascii_lowercase().starts_with("data:image/"))
        {
            p
        } else {
            return Some(
                json!({"type":"assistant_message","text":"Image output was omitted because it was not available as a file path or URL."}),
            );
        }
    } else {
        return None;
    };
    let source = if source.starts_with('/')
        || source.starts_with("\\\\")
        || source.as_bytes().get(1) == Some(&b':')
    {
        let normalized = source.replace('\\', "/");
        let encoded = normalized
            .split('/')
            .map(|s| {
                percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC)
                    .to_string()
                    .replace("%3A", ":")
                    .replace("%2D", "-")
                    .replace("%2E", ".")
                    .replace("%5F", "_")
                    .replace("%7E", "~")
            })
            .collect::<Vec<_>>()
            .join("/");
        if normalized.starts_with("//") {
            format!("file:{encoded}")
        } else if normalized.starts_with('/') {
            format!("file://{encoded}")
        } else {
            format!("file:///{encoded}")
        }
    } else {
        source.into()
    };
    Some(
        json!({"type":"assistant_message","text":format!("\n\n![Image]({})\n\n",source.replace('\\',"\\\\").replace(')',"\\)"))}),
    )
}
fn image_item(
    raw: &Value,
    materialize: &mut dyn FnMut(&str, Option<&str>) -> Option<String>,
) -> Option<Value> {
    if kind(raw) == "imageView" {
        return render_image(&json!({"path":raw["path"]}), materialize);
    }
    if raw["status"] == "failed" {
        return tool_envelope(
            &json!({"callId":raw["id"],"name":"image_generation","input":{"prompt":string(raw,&["revisedPrompt","revised_prompt"])},"error":raw.get("failure").cloned().unwrap_or_else(||json!({"message":"Image generation failed"}))}),
            None,
        );
    }
    let result = &raw["result"];
    let mut image = json!({});
    if let Some(s) = result.as_str() {
        let s = s.trim();
        if s.to_ascii_lowercase().starts_with("data:image/")
            || (s.len() > 64
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b)))
        {
            image["data"] = s.into();
        } else {
            image["url"] = s.into();
        }
    } else {
        put(
            &mut image,
            "path",
            string(result, &["path", "savedPath", "saved_path"]),
        );
        put(&mut image, "url", string(result, &["url"]));
        put(&mut image, "data", string(result, &["data"]));
        put(
            &mut image,
            "mimeType",
            string(result, &["mimeType", "mime_type"]),
        );
    }
    if let Some(p) = string(raw, &["savedPath", "saved_path"]) {
        image["path"] = p.into();
    }
    render_image(&image, materialize)
}
pub fn item_entries_with_images(
    raw: &Value,
    include_user: bool,
    cwd: Option<&str>,
    materialize: &mut dyn FnMut(&str, Option<&str>) -> Option<String>,
) -> Vec<Value> {
    let Some(item) = item_with_images(raw, include_user, cwd, materialize) else {
        return vec![];
    };
    let mut items = vec![item];
    if kind(raw) == "mcpToolCall" {
        for image in split_mcp_images(&raw["result"]).1 {
            if let Some(item) = render_image(&image, materialize) {
                items.push(item);
            }
        }
    }
    items
}
pub fn usage(raw: &Value) -> Option<Value> {
    raw.as_object()?;
    let mut u = json!({});
    let last = &raw["last"];
    for key in ["inputTokens", "cachedInputTokens", "outputTokens"] {
        if last[key].is_number() {
            u[key] = last[key].clone();
        }
    }
    for (key, v, aliases) in [
        (
            "contextWindowMaxTokens",
            raw,
            ["model_context_window", "modelContextWindow"],
        ),
        (
            "contextWindowUsedTokens",
            last,
            ["total_tokens", "totalTokens"],
        ),
    ] {
        if let Some(n) = aliases
            .iter()
            .find_map(|k| v[*k].as_f64().filter(|n| n.is_finite() && *n > 0.0))
        {
            u[key] = json!(n);
        }
    }
    Some(u)
}
fn timestamp(v: &Value) -> Option<String> {
    if let Some(s) = v.as_str() {
        let s = s.trim();
        return (DateTime::parse_from_rfc3339(s).is_ok()
            || DateTime::parse_from_rfc2822(s).is_ok()
            || chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok())
        .then(|| s.into());
    }
    let n = v.as_f64()?;
    let ms = if n > 1_000_000_000_000.0 {
        n
    } else {
        n * 1000.0
    };
    if !ms.is_finite() {
        return None;
    }
    DateTime::<Utc>::from_timestamp_millis(ms as i64)
        .map(|t| t.to_rfc3339_opts(SecondsFormat::Millis, true))
}
fn timestamp_fields(v: &Value, fields: &[&str]) -> Option<String> {
    fields.iter().find_map(|k| timestamp(&v[*k]))
}
pub fn history_with_images(
    thread: &Value,
    cwd: Option<&str>,
    materialize: &mut dyn FnMut(&str, Option<&str>) -> Option<String>,
) -> Vec<Value> {
    let thread = thread.get("thread").unwrap_or(thread);
    let mut events: Vec<Value> = Vec::new();
    let mut children: HashMap<String, usize> = HashMap::new();
    for turn in thread["turns"].as_array().into_iter().flatten() {
        for raw in turn["items"].as_array().into_iter().flatten() {
            let activity = kind(raw) == "subAgentActivity"
                && matches!(
                    raw["kind"].as_str(),
                    Some("started" | "interacted" | "interrupted")
                );
            if activity {
                if let Some(index) = raw["agentThreadId"]
                    .as_str()
                    .and_then(|id| children.get(id))
                    .copied()
                {
                    events[index]["item"]["status"] = if raw["kind"] == "interrupted" {
                        "canceled"
                    } else {
                        "completed"
                    }
                    .into();
                    events[index]["item"]["error"] = Value::Null;
                    if let Some(mapped) = item(raw, true, cwd) {
                        if mapped["detail"]["subAgentType"].is_string() {
                            events[index]["item"]["detail"]["subAgentType"] =
                                mapped["detail"]["subAgentType"].clone();
                        }
                    }
                    continue;
                }
            }
            for mut mapped in item_entries_with_images(raw, true, cwd, materialize) {
                if activity && mapped["type"] == "tool_call" {
                    mapped["status"] = if raw["kind"] == "interrupted" {
                        "canceled"
                    } else {
                        "completed"
                    }
                    .into();
                    mapped["error"] = Value::Null;
                }
                let user = mapped["type"] == "user_message";
                let stamp = timestamp_fields(raw, &["timestamp", "createdAt", "created_at"])
                    .or_else(|| {
                        timestamp_fields(
                            turn,
                            if user {
                                &["startedAt", "started_at", "completedAt", "completed_at"]
                            } else {
                                &["completedAt", "completed_at", "startedAt", "started_at"]
                            },
                        )
                    });
                let mut event = json!({"type":"timeline","provider":"codex","item":mapped});
                if let Some(id) = turn["id"].as_str() {
                    event["turnId"] = id.into();
                }
                if let Some(stamp) = stamp {
                    event["timestamp"] = stamp.into();
                }
                events.push(event);
                for id in child_threads(raw) {
                    children.insert(id, events.len() - 1);
                }
            }
        }
    }
    events
}

pub fn plan_todo(steps: &Value) -> Value {
    let items: Vec<Value> = steps.as_array().into_iter().flatten().enumerate().filter_map(|(index,entry)| {
        let text = entry["step"].as_str()?.trim();
        if text.is_empty() { return None; }
        let status = match entry["status"].as_str() {
            Some("completed") => "completed",
            Some("inProgress" | "in_progress") => "in_progress",
            _ => "pending",
        };
        Some(json!({"id":index.to_string(),"text":text,"status":status,"completed":status=="completed"}))
    }).collect();
    json!({"type":"todo","items":items})
}
