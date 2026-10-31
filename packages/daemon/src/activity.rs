use crate::agent::timeline::project_rows;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
#[path = "agent/timeline_presentation.rs"]
mod presentation;
pub use presentation::tool_display;

pub(crate) fn utf16_prefix(text: &str, limit: usize) -> String {
    let mut units = 0;
    let mut end = 0;
    for (index, ch) in text.char_indices() {
        if units + ch.len_utf16() > limit {
            break;
        }
        units += ch.len_utf16();
        end = index + ch.len_utf8();
    }
    text[..end].to_owned()
}
fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}
fn utf16_suffix(text: &str, limit: usize) -> &str {
    let mut units = 0;
    let mut start = text.len();
    for (index, ch) in text.char_indices().rev() {
        if units + ch.len_utf16() > limit {
            break;
        }
        units += ch.len_utf16();
        start = index;
    }
    &text[start..]
}
fn projected(items: &[Value]) -> Vec<Value> {
    let rows: Vec<Value> = items
        .iter()
        .enumerate()
        .map(|(i, item)| json!({"seq":i+1,"timestamp":"","item":item}))
        .collect();
    project_rows(&rows)
}
fn push(entries: &mut Vec<Value>, text: String) {
    entries.push(json!({"text":text}));
}
fn str_field<'a>(value: &'a Value, field: &str) -> &'a str {
    value[field].as_str().unwrap_or("")
}
fn curate_projected<'a>(
    items: impl IntoIterator<Item = &'a Value>,
    options: &Value,
) -> Result<Vec<Value>> {
    let mut entries = vec![];
    for item in items {
        let kind = str_field(item, "type");
        if let Some(kinds) = options["includeKinds"].as_array() {
            if !kinds.iter().any(|k| k == kind) {
                continue;
            }
        }
        match kind {
            "user_message" => push(
                &mut entries,
                format!("[User] {}", str_field(item, "text").trim()),
            ),
            "assistant_message" | "reasoning" => {
                let text = str_field(item, "text").trim();
                if text.is_empty() {
                    continue;
                }
                let prefix = if kind == "reasoning" {
                    "[Thought] "
                } else if options["labelAssistantMessages"].as_bool() == Some(true) {
                    "[Assistant] "
                } else {
                    ""
                };
                push(&mut entries, format!("{prefix}{text}"));
            }
            "tool_call" => {
                if item["agentMessage"].is_object() {
                    let message = &item["agentMessage"];
                    let (name, summary) = presentation::agent_message_display(message);
                    push(
                        &mut entries,
                        if summary.is_empty() {
                            format!("[{name}]")
                        } else {
                            format!("[{name}] {summary}")
                        },
                    );
                    let body = str_field(message, "text").trim();
                    if !body.is_empty() {
                        push(&mut entries, body.into());
                    }
                    continue;
                }
                let display = tool_display(item);
                let name = display["displayName"].as_str().unwrap_or("");
                let mut summary = display["summary"]
                    .as_str()
                    .unwrap_or("")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                if utf16_len(&summary) > 200 {
                    summary = utf16_prefix(&summary, 197) + "...";
                }
                let external = options["includeExternalToolInput"]
                    .as_bool()
                    .unwrap_or(true)
                    && presentation::external_tool_name(str_field(item, "name"));
                let input = if item["detail"]["type"] == "unknown" {
                    item["detail"].get("input")
                } else {
                    Some(&Value::Null)
                };
                if let Some(input) = input.filter(|_| external) {
                    let mut encoded = serde_json::to_string(input)?;
                    if utf16_len(&encoded) > 400 {
                        encoded = utf16_prefix(&encoded, 400) + "...";
                    }
                    push(&mut entries, format!("[{name}] {encoded}"));
                } else {
                    push(
                        &mut entries,
                        if summary.is_empty() {
                            format!("[{name}]")
                        } else {
                            format!("[{name}] {summary}")
                        },
                    );
                }
                if item["detail"]["type"] == "sub_agent" {
                    let log = str_field(&item["detail"], "log").trim();
                    if !log.is_empty() {
                        push(&mut entries, log.into());
                    }
                }
            }
            "todo" => {
                push(&mut entries, "[Tasks]".into());
                if let Some(items) = item["items"].as_array() {
                    for item in items {
                        push(
                            &mut entries,
                            format!(
                                "- [{}] {}",
                                if item["completed"].as_bool().unwrap_or(false) {
                                    "x"
                                } else {
                                    " "
                                },
                                str_field(item, "text")
                            ),
                        );
                    }
                }
            }
            "error" => push(
                &mut entries,
                format!("[Error] {}", str_field(item, "message")),
            ),
            "compaction" => push(&mut entries, "[Compacted]".into()),
            _ => {}
        }
    }
    Ok(entries)
}
pub fn curate(items: &[Value], options: &Value) -> Result<Vec<Value>> {
    let rows = projected(items);
    let limit = options["maxItems"].as_f64().unwrap_or(0.0);
    let start = if limit >= 1.0 && (rows.len() as f64) > limit {
        rows.len().saturating_sub(limit.trunc() as usize)
    } else {
        0
    };
    curate_projected(rows[start..].iter().map(|r| &r["item"]), options)
}
pub fn summarize(items: &[Value], limit: Option<f64>) -> Result<Value> {
    let all = projected(items);
    let total = all.len();
    let count = limit.unwrap_or(0.0).floor().max(0.0) as usize;
    let begin = if count == 0 {
        0
    } else {
        total.saturating_sub(count)
    };
    let mut selected: Vec<&Value> = all[begin..].iter().collect();
    // Tool lifecycles can cross display anchors. Include every overlapping source
    // range until the selected canonical window is closed.
    if !selected.is_empty() {
        loop {
            let min = selected
                .iter()
                .filter_map(|r| r["seqStart"].as_i64())
                .min()
                .unwrap();
            let max = selected
                .iter()
                .filter_map(|r| r["seqEnd"].as_i64())
                .max()
                .unwrap();
            let expanded: Vec<&Value> = all
                .iter()
                .filter(|r| {
                    r["seqStart"].as_i64().unwrap_or(0) <= max
                        && r["seqEnd"].as_i64().unwrap_or(0) >= min
                })
                .collect();
            if expanded.len() == selected.len() {
                break;
            }
            selected = expanded;
        }
    }
    let shown = selected.len();
    let noun = if total == 1 { "activity" } else { "activities" };
    let header = if limit.is_some_and(|n| n != 0.0) && shown < total {
        format!(
            "Showing {shown} of {total} {noun} (limited to {})",
            limit.unwrap()
        )
    } else {
        format!("Showing all {total} {noun}")
    };
    let entries = curate_projected(selected.iter().map(|r| &r["item"]), &Value::Null)?;
    let text = if entries.is_empty() {
        "No activity to display.".into()
    } else {
        entries
            .iter()
            .map(|e| str_field(e, "text"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    Ok(json!({"updateCount":items.len(),"content":format!("{header}\n\n{text}")}))
}
pub fn build_fork_context(input: &Value) -> Result<Value> {
    let rows = project_rows(
        input["rows"]
            .as_array()
            .context("Fork context requires timeline rows")?,
    );
    let boundary = input.get("cursorBoundary").filter(|v| !v.is_null());
    let cursor = boundary
        .and_then(|b| b.get("cursor"))
        .filter(|v| !v.is_null());
    let message_id = input["boundaryMessageId"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let selected: Vec<&Value> = if cursor.is_some() || message_id.is_some() {
        if let Some(boundary) = boundary {
            if boundary["cursor"]["epoch"] != boundary["timelineEpoch"] {
                bail!("Selected timeline position is no longer available.");
            }
        }
        let index = if let Some(cursor) = cursor {
            rows.iter().position(|r| r["seqEnd"] == cursor["seq"])
        } else {
            rows.iter().rposition(|r| {
                r["item"]["type"] == "assistant_message"
                    && r["item"]["messageId"].as_str() == message_id
            })
        };
        let index = index.with_context(|| {
            if cursor.is_some() {
                "Selected timeline position is no longer available."
            } else {
                "Selected assistant message is no longer available."
            }
        })?;
        let seq = rows[index]["seqEnd"]
            .as_i64()
            .context("Invalid fork sequence")?;
        if rows.iter().any(|r| {
            r["seqStart"].as_i64().unwrap_or(0) <= seq && r["seqEnd"].as_i64().unwrap_or(0) > seq
        }) {
            bail!(
                "This checkpoint changed after it was created. Fork from a later completed response instead."
            );
        }
        rows.iter()
            .filter(|r| r["seqEnd"].as_i64().unwrap_or(0) <= seq)
            .collect()
    } else {
        rows.iter().collect()
    };
    let entries = curate_projected(
        selected.iter().map(|r| &r["item"]),
        &json!({"labelAssistantMessages":true,"includeKinds":["user_message","assistant_message","tool_call"],"includeExternalToolInput":false}),
    )?;
    let mut header = "Chat history from a previous Paseo agent.".to_owned();
    for (field, label) in [("agentTitle", "Source agent"), ("cwd", "Source directory")] {
        let value = str_field(input, field).trim();
        if !value.is_empty() {
            header.push_str(&format!("\n{label}: {}", utf16_prefix(value, 1000)));
        }
    }
    let prefix = format!("<chat-history-summary>\n{header}\n\n");
    let suffix = "\n</chat-history-summary>";
    let budget = 100_000 - utf16_len(&prefix) - utf16_len(suffix);
    let body = if entries.is_empty() {
        "No chat history to display.".to_owned()
    } else {
        fit_entries(&entries, budget)
    };
    Ok(
        json!({"attachment":{"type":"text","mimeType":"text/plain","contextKind":"chat_history","title":"Chat history","text":format!("{prefix}{body}{suffix}")},"itemCount":selected.len(),"boundaryCursor":cursor,"boundaryMessageId":message_id}),
    )
}
fn fit_entries(entries: &[Value], budget: usize) -> String {
    let length = entries
        .iter()
        .map(|e| utf16_len(str_field(e, "text")))
        .sum::<usize>()
        + entries.len().saturating_sub(1);
    if length <= budget {
        return entries
            .iter()
            .map(|e| str_field(e, "text"))
            .collect::<Vec<_>>()
            .join("\n");
    }
    let omitted = "[Earlier chat history omitted]\n";
    let mut remaining = budget - utf16_len(omitted);
    let mut selected = vec![];
    for entry in entries.iter().rev() {
        let text = str_field(entry, "text");
        let size = utf16_len(text) + usize::from(!selected.is_empty());
        if size > remaining {
            if selected.is_empty() {
                let marker = "\n[Message truncated]\n";
                let available = remaining - utf16_len(marker);
                let head = available.div_ceil(2);
                selected.push(format!(
                    "{}{marker}{}",
                    utf16_prefix(text, head),
                    utf16_suffix(text, available - head)
                ));
            }
            break;
        }
        selected.push(text.to_owned());
        remaining -= size;
    }
    selected.reverse();
    omitted.to_owned() + &selected.join("\n")
}
