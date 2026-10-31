use super::{array, s};
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

pub(super) const OUTPUT_TOOL: &str = "paseo_structured_output";
#[derive(Default)]
pub(super) struct Timeline {
    content: HashMap<String, String>,
    streams: HashMap<String, String>,
    structured_messages: HashSet<String>,
}
pub(super) fn timestamp(value: &Value) -> String {
    value
        .as_i64()
        .and_then(chrono::DateTime::from_timestamp_millis)
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
fn event(item: Value, time: &str) -> Value {
    json!({"type":"timeline","provider":"opencode","item":item,"timestamp":time})
}
impl Timeline {
    pub fn reset_streams(&mut self) {
        self.streams.clear();
    }
    pub fn start(&mut self, data: &Value, kind: &str) {
        if kind == "text"
            && self
                .structured_messages
                .contains(s(data, "assistantMessageID"))
        {
            return;
        }
        self.streams.insert(
            format!(
                "{}:{}:{}",
                s(data, "assistantMessageID"),
                kind,
                data["ordinal"]
            ),
            String::new(),
        );
    }
    pub fn delta(&mut self, data: &Value, kind: &str) -> Result<Option<Value>> {
        if kind == "text"
            && self
                .structured_messages
                .contains(s(data, "assistantMessageID"))
        {
            return Ok(None);
        }
        let key = format!(
            "{}:{}:{}",
            s(data, "assistantMessageID"),
            kind,
            data["ordinal"]
        );
        let Some(stream) = self.streams.get_mut(&key) else {
            return Ok(None);
        };
        stream.push_str(s(data, "delta"));
        let previous = self.content.get(&key).map(String::as_str).unwrap_or("");
        if stream.len() <= previous.len() {
            return Ok(None);
        }
        if !stream.starts_with(previous) {
            bail!("OpenCode changed previously emitted message content");
        }
        let suffix = stream[previous.len()..].to_owned();
        self.content.insert(key, stream.clone());
        let item = if kind == "text" {
            json!({"type":"assistant_message","text":suffix,"messageId":data["assistantMessageID"]})
        } else {
            json!({"type":"reasoning","text":suffix})
        };
        Ok(Some(event(item, &timestamp(&Value::Null))))
    }
    pub fn messages(&mut self, messages: &[Value], include_client: bool) -> Result<Vec<Value>> {
        let mut events = Vec::new();
        let mut structured = false;
        let mut accepted = false;
        for message in messages {
            let id = s(message, "id");
            let time = timestamp(&message["time"]["created"]);
            match s(message, "type") {
                "user" => {
                    structured = message["metadata"].get("paseoOutputSchema").is_some();
                    accepted = false;
                    if self.content.contains_key(id) {
                        continue;
                    }
                    self.content
                        .insert(id.to_owned(), s(message, "text").to_owned());
                    let mut item =
                        json!({"type":"user_message","text":message["text"],"messageId":id});
                    if include_client && message["metadata"]["paseoClientMessageId"].is_string() {
                        item["clientMessageId"] =
                            message["metadata"]["paseoClientMessageId"].clone();
                    }
                    events.push(event(item, &time));
                }
                "assistant" => {
                    if structured {
                        self.structured_messages.insert(id.to_owned());
                    }
                    let mut text_ordinal = 0;
                    let mut reasoning_ordinal = 0;
                    for (index, part) in array(&message["content"]).iter().enumerate() {
                        let kind = s(part, "type");
                        if kind == "tool" && s(part, "name") == OUTPUT_TOOL {
                            if !structured || accepted || s(&part["state"], "status") != "completed"
                            {
                                continue;
                            }
                            let Some(value) =
                                part["state"]["metadata"].get("paseoStructuredOutput")
                            else {
                                continue;
                            };
                            accepted = true;
                            let key = format!("{id}:{index}");
                            let text = serde_json::to_string(value)?;
                            if self.content.get(&key) == Some(&text) {
                                continue;
                            }
                            self.content.insert(key, text.clone());
                            events.push(event(
                                json!({"type":"assistant_message","text":text,"messageId":id}),
                                &time,
                            ));
                        } else if kind == "text" || kind == "reasoning" {
                            let ordinal = if kind == "text" {
                                let n = text_ordinal;
                                text_ordinal += 1;
                                n
                            } else {
                                let n = reasoning_ordinal;
                                reasoning_ordinal += 1;
                                n
                            };
                            if structured && kind == "text" {
                                continue;
                            }
                            let key = format!("{id}:{kind}:{ordinal}");
                            let text = s(part, "text");
                            let previous = self.content.get(&key).map(String::as_str).unwrap_or("");
                            if text.len() <= previous.len() {
                                continue;
                            }
                            if !text.starts_with(previous) {
                                bail!("OpenCode changed previously emitted message content");
                            }
                            let suffix = text[previous.len()..].to_owned();
                            self.content.insert(key, text.to_owned());
                            let item = if kind == "text" {
                                json!({"type":"assistant_message","text":suffix,"messageId":id})
                            } else {
                                json!({"type":"reasoning","text":suffix})
                            };
                            events.push(event(item, &time));
                        } else if kind == "tool" {
                            let key = format!("{id}:{index}");
                            let serialized = serde_json::to_string(part)?;
                            if self.content.get(&key) == Some(&serialized) {
                                continue;
                            }
                            self.content.insert(key.clone(), serialized);
                            let state = &part["state"];
                            let mut output = Vec::new();
                            let mut images = Vec::new();
                            for item in array(&state["content"]) {
                                if s(item, "type") == "text" {
                                    output.push(s(item, "text"));
                                } else if s(item, "uri").starts_with("data:image/") {
                                    images.push(item);
                                    output.push("[image]");
                                } else {
                                    output.push(s(item, "uri"));
                                }
                            }
                            let output = if state.get("content").is_some() {
                                Value::String(output.join("\n"))
                            } else {
                                Value::Null
                            };
                            let status = if s(state, "status") == "error" {
                                "failed"
                            } else {
                                s(state, "status")
                            };
                            if let Some(item) = super::super::legacy::map_tool(
                                s(part, "name"),
                                s(part, "id"),
                                &state["input"],
                                status,
                                &output,
                                &state["error"],
                                &state["metadata"],
                            ) {
                                events.push(event(item, &time));
                            }
                            for (i, image) in images.iter().enumerate() {
                                let image_key = format!("{key}:image:{i}");
                                if self.content.contains_key(&image_key) {
                                    continue;
                                }
                                self.content.insert(image_key, String::new());
                                if let Some(item) = crate::providers::prompt::projected_image(
                                    &json!({"url":image["uri"],"mimeType":image["mime"]}),
                                )? {
                                    events.push(event(item, &time));
                                }
                            }
                        }
                    }
                }
                "compaction" => {
                    let status = if s(message, "status") == "running" {
                        "loading"
                    } else {
                        "completed"
                    };
                    if self.content.get(id).map(String::as_str) != Some(status) {
                        self.content.insert(id.to_owned(), status.to_owned());
                        events.push(event(json!({"type":"compaction","status":status}), &time));
                    }
                }
                _ => {}
            }
        }
        Ok(events)
    }
}
