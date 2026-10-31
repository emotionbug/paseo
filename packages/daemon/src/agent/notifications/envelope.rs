use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}
fn unescape(mut text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    while let Some(index) = text.find(['&', '<', '>']) {
        out.push_str(&text[..index]);
        text = &text[index..];
        let (entity, ch) = [
            ("&amp;", '&'),
            ("&lt;", '<'),
            ("&gt;", '>'),
            ("&quot;", '"'),
            ("&apos;", '\''),
        ]
        .into_iter()
        .find(|(entity, _)| text.starts_with(entity))?;
        out.push(ch);
        text = &text[entity.len()..];
    }
    out.push_str(text);
    Some(out)
}
fn source_valid(source: &Value) -> bool {
    let kind = source["kind"].as_str();
    matches!(kind, Some("agent-message" | "agent-notification"))
        && source["agentId"].as_str().is_some_and(|s| !s.is_empty())
        && ["serverId", "title"].iter().all(|key| {
            source
                .get(*key)
                .is_none_or(|v| v.as_str().is_some_and(|s| !s.is_empty()))
        })
        && (kind != Some("agent-notification")
            || matches!(
                source["event"].as_str(),
                Some("finished" | "errored" | "permission-required" | "closed")
            ))
}
#[derive(Debug, PartialEq)]
struct Message {
    id: String,
    source: Option<Value>,
    text: String,
}
fn format(message: &Message) -> String {
    let mut attributes = match &message.source {
        Some(source) => format!(
            "kind=\"{}\" source-agent-id=\"{}\"",
            source["kind"].as_str().unwrap(),
            escape(source["agentId"].as_str().unwrap())
        ),
        None => "kind=\"user-message\"".to_owned(),
    };
    attributes.push_str(&format!(" message-id=\"{}\"", escape(&message.id)));
    if let Some(source) = &message.source {
        for (key, name) in [
            ("serverId", "source-server-id"),
            ("title", "source-agent-title"),
        ] {
            if let Some(value) = source[key].as_str() {
                attributes.push_str(&format!(" {name}=\"{}\"", escape(value)));
            }
        }
        if source["kind"] == "agent-notification" {
            attributes.push_str(&format!(" event=\"{}\"", source["event"].as_str().unwrap()));
        }
    }
    format!(
        "<paseo-system version=\"1\" {attributes}>\n{}\n</paseo-system>",
        escape(&message.text)
    )
}
fn parse(text: &str) -> Option<Message> {
    let text = text.strip_prefix("<paseo-system")?;
    let (mut attrs, text) = text.split_once(">\n")?;
    let body = unescape(text.strip_suffix("\n</paseo-system>")?)?;
    let mut values = BTreeMap::new();
    while !attrs.is_empty() {
        if !attrs.starts_with(char::is_whitespace) {
            return None;
        }
        attrs = attrs.trim_start_matches(char::is_whitespace);
        let (key, rest) = attrs.split_once("=\"")?;
        if key.is_empty() || !key.bytes().all(|b| b.is_ascii_lowercase() || b == b'-') {
            return None;
        }
        let (value, rest) = rest.split_once('"')?;
        if values.insert(key, unescape(value)?).is_some() {
            return None;
        }
        attrs = rest;
    }
    if values.get("version")?.as_str() != "1" {
        return None;
    }
    let id = values.get("message-id")?.clone();
    if id.is_empty() {
        return None;
    }
    let source = if values.get("kind")?.as_str() == "user-message" {
        if values.contains_key("source-agent-id") || values.contains_key("event") {
            return None;
        }
        None
    } else {
        let mut source =
            json!({"kind":values.get("kind")?,"agentId":values.get("source-agent-id")?});
        for (attribute, key) in [
            ("source-server-id", "serverId"),
            ("source-agent-title", "title"),
            ("event", "event"),
        ] {
            if let Some(value) = values.get(attribute) {
                source[key] = json!(value);
            }
        }
        if !source_valid(&source) {
            return None;
        }
        if source["kind"] == "agent-message" {
            source.as_object_mut()?.remove("event");
        }
        Some(source)
    };
    Some(Message {
        id,
        source,
        text: body,
    })
}
fn parse_many(mut text: &str) -> Option<Vec<Message>> {
    let mut messages = Vec::new();
    loop {
        let end = text.find("</paseo-system>")? + "</paseo-system>".len();
        messages.push(parse(&text[..end])?);
        text = &text[end..];
        if text.is_empty() {
            return Some(messages);
        }
        if !text.starts_with('\n') {
            return None;
        }
        text = text.trim_start_matches('\n');
        if !text.starts_with("<paseo-system ") {
            return None;
        }
    }
}
fn legacy(text: &str) -> bool {
    text.starts_with("<paseo-system>\n") && text.ends_with("\n</paseo-system>")
}

pub fn prepare_agent_message(
    prompt: Value,
    source: Option<&Value>,
    id: Option<&str>,
) -> Result<(Value, Option<String>)> {
    if let Some(source) = source {
        if !source_valid(source) {
            bail!("Invalid agent message source");
        }
    }
    if source.is_none() {
        let submitted = if let Some(text) = prompt.as_str() {
            text.to_owned()
        } else {
            prompt
                .as_array()
                .context("Prompt must be a string or content blocks")?
                .iter()
                .filter(|b| b["type"] == "text" && b.get("mimeType").is_none())
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .to_owned()
        };
        if parse_many(&submitted).is_none() && !legacy(&submitted) {
            return Ok((prompt, id.map(str::to_owned)));
        }
    }
    let id = id
        .map(str::to_owned)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let text = if let Some(text) = prompt.as_str() {
        text.to_owned()
    } else {
        prompt
            .as_array()
            .context("Prompt must be a string or content blocks")?
            .iter()
            .filter(|b| b["type"] != "image")
            .map(crate::providers::prompt::render_attachment)
            .collect::<Result<Vec<_>>>()?
            .join("\n\n")
    };
    let envelope = format(&Message {
        id: id.clone(),
        source: source.cloned(),
        text,
    });
    let prepared = match prompt {
        Value::String(_) => Value::String(envelope),
        Value::Array(blocks) => {
            let mut result = Vec::with_capacity(blocks.len() + 1);
            result.push(json!({"type":"text","text":envelope}));
            result.extend(blocks.into_iter().filter(|b| b["type"] == "image"));
            Value::Array(result)
        }
        _ => bail!("Prompt must be a string or content blocks"),
    };
    Ok((prepared, Some(id)))
}

pub fn project_agent_messages(item: &Value) -> Vec<Value> {
    if item["type"] != "user_message" {
        return vec![item.clone()];
    }
    let text = item["text"].as_str().unwrap_or_default();
    let Some(messages) = parse_many(text) else {
        return if legacy(text) {
            Vec::new()
        } else {
            vec![item.clone()]
        };
    };
    messages.into_iter().map(|message| match message.source {
        None => { let mut item = item.clone(); item["text"] = json!(message.text); item["clientMessageId"] = json!(message.id); item }
        Some(source) => {
            let mut sender = json!({"id":source["agentId"]});
            for key in ["serverId", "title"] { if let Some(value) = source.get(key) { sender[key] = value.clone(); } }
            json!({"type":"tool_call","callId":format!("paseo-agent-message:{}", message.id),
                "agentMessage":{"event":if source["kind"] == "agent-message" { json!("message") } else { source["event"].clone() },"sender":sender,"text":message.text},
                "name":"agent_message","status":"completed","error":null,"detail":{"type":"plain_text","text":message.text,"icon":"bot"}})
        }
    }).collect()
}

pub fn agent_message_command(prompt: &Value) -> Option<String> {
    let mut messages = parse_many(prompt.as_str()?)?;
    if messages.len() != 1 || messages[0].source.is_none() {
        return None;
    }
    Some(messages.remove(0).text)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn envelope_roundtrip_and_human_provenance() {
        let source = json!({"kind":"agent-message","agentId":"a<&\"","title":"Worker"});
        let (prompt, _) =
            prepare_agent_message(json!("<result> & ready"), Some(&source), Some("id")).unwrap();
        let item = json!({"type":"user_message","text":prompt});
        let projected = project_agent_messages(&item);
        assert_eq!(projected[0]["agentMessage"]["text"], "<result> & ready");
        let (quoted, _) = prepare_agent_message(prompt.clone(), None, Some("human")).unwrap();
        let human = project_agent_messages(&json!({"type":"user_message","text":quoted}));
        assert_eq!(human[0]["text"], prompt);
        assert_eq!(human[0]["clientMessageId"], "human");
        assert!(parse("<paseo-system version=\"1\" version=\"1\" kind=\"user-message\" message-id=\"x\">\nx\n</paseo-system>").is_none());
    }
    #[test]
    fn complete_sequences_only_and_image_order() {
        let source = json!({"kind":"agent-message","agentId":"a"});
        let (one, _) = prepare_agent_message(json!("one"), Some(&source), Some("1")).unwrap();
        let (two, _) = prepare_agent_message(json!("two"), Some(&source), Some("2")).unwrap();
        let joined = format!("{}\n\n{}", one.as_str().unwrap(), two.as_str().unwrap());
        assert_eq!(
            project_agent_messages(&json!({"type":"user_message","text":joined})).len(),
            2
        );
        assert!(parse_many(&format!("prose\n{joined}")).is_none());
        let (blocks, _) = prepare_agent_message(json!([{"type":"text","text":"hi"},{"type":"image","data":"x","mimeType":"image/png"},{"type":"uploaded_file","fileName":"x","path":"/x","mimeType":"text/plain","size":1}]), Some(&source), Some("3")).unwrap();
        assert_eq!(blocks.as_array().unwrap().len(), 2);
        assert_eq!(blocks[1]["type"], "image");
        assert!(
            blocks[0]["text"]
                .as_str()
                .unwrap()
                .contains("Uploaded file: x")
        );
    }
}
