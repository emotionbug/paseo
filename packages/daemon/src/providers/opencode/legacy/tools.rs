use regex::Regex;
use serde_json::{Value, json};
use std::sync::LazyLock;

pub fn text(value: &Value) -> Option<&str> {
    value.as_str().filter(|s| !s.is_empty())
}
fn first<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|key| text(&value[*key]))
}
pub fn output_text(value: &Value) -> Option<String> {
    if let Some(s) = text(value) {
        return Some(s.to_owned());
    }
    if let Some(items) = value.as_array() {
        let parts: Vec<_> = items.iter().filter_map(output_text).collect();
        return (!parts.is_empty()).then(|| parts.join("\n"));
    }
    [
        "output",
        "text",
        "content",
        "result",
        "aggregated_output",
        "aggregatedOutput",
        "structuredContent",
        "structured_content",
    ]
    .iter()
    .find_map(|key| value.get(*key).and_then(output_text))
}
fn log_text(value: &Value) -> Option<String> {
    if value.is_null() {
        None
    } else {
        Some(
            output_text(value)
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| serde_json::to_string_pretty(value).unwrap_or_default()),
        )
    }
}
fn normalized(value: &Value) -> Option<String> {
    text(value)
        .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|s| !s.is_empty())
}
fn task_session(value: &Value) -> Option<String> {
    static ID: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)\btask_id:\s*(ses_[A-Za-z0-9]+)").unwrap());
    if let Some(s) = output_text(value) {
        if let Some(c) = ID.captures(&s) {
            return Some(c[1].to_owned());
        }
    }
    ["task_id", "taskId", "sessionID", "sessionId"]
        .iter()
        .find_map(|k| normalized(&value[*k]))
        .or_else(|| {
            ["output", "text", "content", "result"]
                .iter()
                .find_map(|k| value.get(*k).and_then(task_session))
        })
}
fn put_text(result: &mut Value, key: &str, value: Option<&str>) {
    if let Some(value) = value.filter(|s| !s.is_empty()) {
        result[key] = json!(value);
    }
}
fn truncate_diff(s: &str) -> String {
    let mut units = 0;
    let end = s.char_indices().find_map(|(i, c)| {
        units += c.len_utf16();
        (units > 12_000).then_some(i)
    });
    match end {
        Some(end) => format!(
            "{}\n...[truncated {} chars]",
            &s[..end],
            s.encode_utf16().count() - 12_000
        ),
        None => s.to_owned(),
    }
}
fn patch_diff(patch: &str) -> (Option<String>, String) {
    let mut path = None;
    let mut lines = Vec::new();
    for line in patch.lines() {
        let trim = line.trim();
        let directive = [
            ("*** Add File:", "add"),
            ("*** Update File:", "update"),
            ("*** Delete File:", "delete"),
        ]
        .iter()
        .find_map(|(prefix, kind)| {
            trim.strip_prefix(prefix)
                .map(|p| (*kind, p.trim().trim_matches(['\'', '"'])))
        });
        if let Some((kind, p)) = directive {
            if path.is_none() && !p.is_empty() {
                path = Some(p.to_owned());
            }
            if !p.is_empty() {
                if !lines.is_empty() {
                    lines.push(String::new());
                }
                lines.push(format!("diff --git a/{p} b/{p}"));
                lines.push(format!(
                    "--- {}",
                    if kind == "add" {
                        "/dev/null".into()
                    } else {
                        format!("a/{p}")
                    }
                ));
                lines.push(format!(
                    "+++ {}",
                    if kind == "delete" {
                        "/dev/null".into()
                    } else {
                        format!("b/{p}")
                    }
                ));
            }
        } else if !trim.starts_with("***")
            && (line.starts_with(['+', '-', ' '])
                || line.starts_with("@@")
                || line.starts_with("\\ No newline at end of file"))
        {
            lines.push(line.to_owned());
        }
    }
    let diff = lines.join("\n");
    (
        path,
        truncate_diff(if diff.trim().is_empty() {
            patch
        } else {
            diff.trim()
        }),
    )
}
fn strip_gutter(content: &str) -> Option<(String, u64)> {
    static GUTTER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*(\d+)\t(.*)$").unwrap());
    let normalized = content.replace("\r\n", "\n");
    let mut out = Vec::new();
    let mut count = 0;
    let mut matches = 0;
    let mut first = None;
    let mut previous = None;
    for line in normalized.split('\n') {
        if line.is_empty() {
            out.push(String::new());
            continue;
        }
        count += 1;
        if let Some(c) = GUTTER.captures(line) {
            let n = c[1].parse::<u64>().ok()?;
            if previous.is_some_and(|p| n != p + 1) {
                return None;
            }
            first.get_or_insert(n);
            previous = Some(n);
            matches += 1;
            out.push(c[2].to_owned());
        } else {
            if first.is_none() {
                return None;
            }
            out.push(line.to_owned());
        }
    }
    if matches * 2 < count {
        return None;
    }
    first.map(|n| (out.join("\n"), n))
}

fn xml_decode(value: &str) -> String {
    static ENTITY: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"&(?:#([0-9]+)|#x([0-9a-fA-F]+)|amp|lt|gt|quot|apos);").unwrap()
    });
    ENTITY
        .replace_all(value, |c: &regex::Captures<'_>| {
            if let Some(n) = c
                .get(1)
                .and_then(|m| m.as_str().parse::<u32>().ok())
                .or_else(|| {
                    c.get(2)
                        .and_then(|m| u32::from_str_radix(m.as_str(), 16).ok())
                })
            {
                return char::from_u32(n)
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| c[0].to_owned());
            }
            match &c[0] {
                "&amp;" => "&",
                "&lt;" => "<",
                "&gt;" => ">",
                "&quot;" => "\"",
                "&apos;" => "'",
                _ => &c[0],
            }
            .to_owned()
        })
        .into_owned()
}
fn read_output(output: &Value) -> (Option<String>, Option<String>) {
    if let Some(raw) = output.as_str() {
        if raw.trim().starts_with("<path>") {
            static PATH: LazyLock<Regex> =
                LazyLock::new(|| Regex::new(r"(?is)<path>(.*?)</path>").unwrap());
            static CONTENT: LazyLock<Regex> =
                LazyLock::new(|| Regex::new(r"(?is)<content>(.*?)</content>").unwrap());
            if let Some(c) = CONTENT.captures(raw) {
                let body = &c[1];
                let body = body
                    .strip_prefix("\r\n")
                    .or_else(|| body.strip_prefix('\n'))
                    .unwrap_or(body);
                let body = body
                    .strip_suffix("\r\n")
                    .or_else(|| body.strip_suffix('\n'))
                    .unwrap_or(body);
                return (
                    PATH.captures(raw).map(|c| xml_decode(c[1].trim())),
                    Some(xml_decode(body)),
                );
            }
        }
    }
    let payload = output
        .get("data")
        .or_else(|| output.get("structuredContent"))
        .or_else(|| output.get("structured_content"))
        .unwrap_or(output);
    let content = if let Some(array) = payload.as_array() {
        let parts: Vec<_> = array
            .iter()
            .filter_map(|p| first(p, &["text", "content", "output"]))
            .collect();
        (!parts.is_empty()).then(|| parts.join("\n"))
    } else {
        payload.as_str().map(str::to_owned).or_else(|| {
            ["content", "text", "output"]
                .iter()
                .find_map(|key| payload.get(*key).and_then(output_text))
        })
    };
    (None, content)
}
fn shell_output(raw: &str) -> Option<String> {
    let normalized = raw.replace("\r\n", "\n");
    let lines: Vec<_> = normalized.split('\n').collect();
    let headers: Vec<_> = lines
        .iter()
        .take(8)
        .map(|s| s.trim().to_ascii_lowercase())
        .collect();
    if !headers.first().is_some_and(|s| s.starts_with("chunk id:"))
        || !headers.iter().any(|s| s.starts_with("wall time:"))
        || !headers
            .iter()
            .any(|s| s.starts_with("process exited with code"))
    {
        return Some(raw.into());
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
                .find(|(_, line)| {
                    ![
                        "chunk id:",
                        "wall time:",
                        "process exited with code",
                        "original token count:",
                    ]
                    .iter()
                    .any(|p| line.trim().to_ascii_lowercase().starts_with(p))
                })
                .map(|(n, _)| n)
        })?;
    let content = lines[start..].join("\n");
    (!content.is_empty()).then_some(content)
}

pub fn map_tool(
    name: &str,
    id: &str,
    input: &Value,
    status: &str,
    output: &Value,
    error: &Value,
    metadata: &Value,
) -> Option<Value> {
    let name = name.trim();
    let id = id.trim();
    if name.is_empty() || id.is_empty() {
        return None;
    }
    let status = if !error.is_null() {
        "failed"
    } else {
        match status.trim().to_ascii_lowercase().as_str() {
            "failed" | "failure" | "error" | "errored" | "rejected" | "denied" | "declined" => {
                "failed"
            }
            "canceled" | "cancelled" | "interrupted" | "aborted" => "canceled",
            "completed" | "complete" | "done" | "success" | "succeeded" => "completed",
            "" if !output.is_null() => "completed",
            _ => "running",
        }
    };
    let mut result = json!({"type":"tool_call","callId":id,"name":name,"status":status,"detail":detail(name,input,output,error,metadata),"error":null});
    if status == "failed" {
        result["error"] = if error.is_null() {
            json!({"message":"Tool call failed"})
        } else {
            error.clone()
        };
    }
    if metadata.is_object() {
        result["metadata"] = metadata.clone();
    }
    Some(result)
}

fn detail(name: &str, input: &Value, output: &Value, error: &Value, metadata: &Value) -> Value {
    let fallback = || json!({"type":"unknown","input":input,"output":output});
    if name.eq_ignore_ascii_case("task") {
        let kind =
            normalized(&input["subagent_type"]).or_else(|| normalized(&input["subAgentType"]));
        let description = normalized(&input["description"]);
        if kind.is_some() || description.is_some() {
            let mut result = json!({"type":"sub_agent","log":([log_text(output),log_text(error)].into_iter().flatten().collect::<Vec<_>>().join("\n")),"actions":[]});
            put_text(&mut result, "subAgentType", kind.as_deref());
            put_text(&mut result, "description", description.as_deref());
            put_text(
                &mut result,
                "childSessionId",
                normalized(&metadata["sessionId"])
                    .or_else(|| task_session(output))
                    .as_deref(),
            );
            return result;
        }
    }
    match name {
        "shell" | "bash" | "exec_command" => {
            let command_value = input.get("command").or_else(|| input.get("cmd"));
            if !input.is_null()
                && !command_value.is_some_and(|v| {
                    v.is_string() || v.as_array().is_some_and(|a| a.iter().all(Value::is_string))
                })
            {
                return fallback();
            }
            if !output.is_null() && !output.is_string() && !output.is_object() {
                return fallback();
            }
            let command = command_value
                .and_then(|v| {
                    v.as_str().map(str::to_owned).or_else(|| {
                        v.as_array().map(|a| {
                            a.iter()
                                .filter_map(Value::as_str)
                                .map(str::trim)
                                .filter(|s| !s.is_empty())
                                .collect::<Vec<_>>()
                                .join(" ")
                        })
                    })
                })
                .or_else(|| first(output, &["command"]).map(str::to_owned))
                .or_else(|| text(&output["result"]["command"]).map(str::to_owned));
            let Some(command) = command.filter(|s| !s.is_empty()) else {
                return fallback();
            };
            let mut result = json!({"type":"shell","command":command});
            put_text(&mut result, "cwd", first(input, &["cwd", "directory"]));
            if let Some(out) = output_text(output).and_then(|s| shell_output(&s)) {
                put_text(&mut result, "output", Some(&out));
            }
            if let Some(code) = [
                output.get("exitCode"),
                output.get("exit_code"),
                output["metadata"].get("exitCode"),
                output["metadata"].get("exit_code"),
            ]
            .into_iter()
            .flatten()
            .find(|v| v.is_number())
            {
                result["exitCode"] = code.clone();
            }
            result
        }
        "read" | "read_file" | "write" | "write_file" | "create_file" | "edit" | "apply_patch"
        | "apply_diff" | "patch" => {
            let kind = match name {
                "read" | "read_file" => "read",
                "write" | "write_file" | "create_file" => "write",
                _ => "edit",
            };
            let direct_edit = input["filePath"].is_string()
                || (input["path"].is_string()
                    && input["oldString"].is_string()
                    && input["newString"].is_string());
            let patch = if matches!(name, "apply_patch" | "patch") && !direct_edit {
                text(&input["patchText"]).map(patch_diff)
            } else {
                None
            };
            let input_path = first(input, &["path", "file_path", "filePath"]);
            if !input.is_null() && input_path.is_none() && patch.is_none() {
                return fallback();
            }
            let read = if kind == "read" {
                Some(read_output(output))
            } else {
                None
            };
            let output_file = output["files"]
                .as_array()
                .and_then(|files| files.first())
                .unwrap_or(output);
            let path = input_path
                .or_else(|| {
                    if kind == "read" {
                        read.as_ref().and_then(|r| r.0.as_deref())
                    } else {
                        first(output_file, &["path", "file_path", "filePath"])
                    }
                })
                .or_else(|| patch.as_ref().and_then(|p| p.0.as_deref()));
            let Some(path) = path else {
                return fallback();
            };
            let mut result = json!({"type":kind,"filePath":path});
            if kind == "read" {
                if let Some(content) = read.and_then(|r| r.1) {
                    if let Some((content, start)) = strip_gutter(&content) {
                        result["content"] = json!(content);
                        result["offset"] = json!(start);
                    } else {
                        put_text(&mut result, "content", Some(&content));
                    }
                }
                for key in ["offset", "limit"] {
                    if input[key].is_number() {
                        result[key] = input[key].clone();
                    }
                }
            } else if kind == "write" {
                put_text(
                    &mut result,
                    "content",
                    first(input, &["content", "new_content", "newContent"])
                        .or_else(|| first(output, &["content", "new_content", "newContent"])),
                );
            } else {
                let old = if direct_edit {
                    first(input, &["oldString"])
                } else {
                    first(
                        input,
                        &["old_string", "old_str", "oldContent", "old_content"],
                    )
                };
                let new = if direct_edit {
                    first(input, &["newString"])
                } else {
                    first(
                        input,
                        &[
                            "new_string",
                            "new_str",
                            "newContent",
                            "new_content",
                            "content",
                        ],
                    )
                };
                put_text(&mut result, "oldString", old);
                put_text(
                    &mut result,
                    "newString",
                    new.or_else(|| first(output, &["newContent", "new_content", "content"])),
                );
                let diff = patch
                    .map(|p| p.1)
                    .or_else(|| {
                        if direct_edit {
                            None
                        } else {
                            first(input, &["patch", "diff", "unified_diff", "unifiedDiff"])
                                .map(truncate_diff)
                        }
                    })
                    .or_else(|| {
                        first(
                            output_file,
                            &["patch", "diff", "unified_diff", "unifiedDiff"],
                        )
                        .map(truncate_diff)
                    });
                put_text(&mut result, "unifiedDiff", diff.as_deref());
            }
            result
        }
        "search" | "grep" | "glob" | "web_search" => {
            let Some(query) = first(input, &["query", "q", "pattern"]) else {
                return fallback();
            };
            let mut result = json!({"type":"search","query":query,"toolName":name});
            if name == "grep" && output.is_string() {
                put_text(&mut result, "content", output.as_str());
                result["numFiles"] = json!(0);
            } else if matches!(name, "grep" | "glob")
                && output["numFiles"].is_u64()
                && output["filenames"].is_array()
            {
                if name == "glob"
                    && (!output["durationMs"].is_number() || !output["truncated"].is_boolean())
                {
                    return result;
                }
                if output["filenames"]
                    .as_array()
                    .is_some_and(|v| !v.is_empty())
                {
                    result["filePaths"] = output["filenames"].clone();
                }
                result["numFiles"] = output["numFiles"].clone();
                if name == "grep" {
                    put_text(&mut result, "content", output["content"].as_str());
                    for key in ["numMatches", "mode"] {
                        if let Some(v) = output.get(key) {
                            result[key] = v.clone();
                        }
                    }
                } else {
                    result["durationMs"] = output["durationMs"].clone();
                    result["truncated"] = output["truncated"].clone();
                }
            } else if name == "grep" && !output.is_null() {
                return fallback();
            }
            result
        }
        "skill" => {
            let Some(label) = text(&input["name"])
                .map(str::trim)
                .filter(|s| !s.is_empty())
            else {
                return fallback();
            };
            if !output.is_null() && !output.is_string() && !output["output"].is_string() {
                return fallback();
            }
            let mut result = json!({"type":"plain_text","label":label,"icon":"sparkles"});
            put_text(
                &mut result,
                "text",
                output.as_str().or_else(|| output["output"].as_str()),
            );
            result
        }
        _ => fallback(),
    }
}
