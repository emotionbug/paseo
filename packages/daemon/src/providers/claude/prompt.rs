use anyhow::{Result, bail};
use serde_json::{Value, json};

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}

pub(super) fn user_message(prompt: &Value, session_id: Option<&str>) -> Result<Value> {
    let mut content = Vec::new();
    let mut slash = None;
    if let Some(prompt) = prompt.as_str() {
        content.push(json!({"type":"text","text":prompt}));
    } else if let Some(blocks) = prompt.as_array() {
        for block in blocks {
            match text(block,"type") {
            "image" => {
                let mime = text(block,"mimeType");
                if matches!(mime,"image/jpeg"|"image/png"|"image/gif"|"image/webp") {
                    content.push(json!({"type":"image","source":{"type":"base64","media_type":mime,"data":block["data"]}}));
                }
            }
            "text" => {
                if block.get("mimeType").is_none() && text(block,"text").trim().starts_with('/') { slash=Some(content.len()); }
                content.push(json!({"type":"text","text":block["text"]}));
            }
            _ => content.push(json!({"type":"text","text":crate::providers::prompt::render_attachment(block)?})),
        }
        }
    } else {
        bail!("Claude prompt must be text or content blocks");
    }
    if let Some(index) = slash {
        if index + 1 < content.len() {
            let block = content.remove(index);
            content.push(block);
        }
    }
    Ok(
        json!({"type":"user","message":{"role":"user","content":content},"parent_tool_use_id":null,"uuid":uuid::Uuid::new_v4().to_string(),"session_id":session_id.unwrap_or("")}),
    )
}
