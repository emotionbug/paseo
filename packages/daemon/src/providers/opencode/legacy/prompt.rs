use super::projection::s;
use anyhow::Result;
use serde_json::{Value, json};

use crate::providers::prompt::render_attachment as render_prompt_attachment;
pub fn prompt_parts(prompt: &Value) -> Result<Vec<Value>> {
    if let Some(text) = prompt.as_str() {
        return Ok(vec![json!({"type":"text","text":text})]);
    }
    let blocks = prompt
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("OpenCode prompt must be text or content blocks"))?;
    let mut ordinal = 0;
    let mut result = Vec::with_capacity(blocks.len());
    for p in blocks {
        if s(&p["type"]) == "image" {
            ordinal += 1;
            let data = s(&p["data"]);
            let mut mime = s(&p["mimeType"]);
            let url = if let Some((header, payload)) = data
                .strip_prefix("data:")
                .and_then(|s| s.split_once(";base64,"))
            {
                if !payload.is_empty() && !header.contains([',', ';']) {
                    mime = header;
                    data.to_owned()
                } else {
                    format!("data:{mime};base64,{data}")
                }
            } else {
                format!("data:{mime};base64,{data}")
            };
            let ext = match mime {
                "image/png" => "png",
                "image/jpeg" => "jpg",
                "image/webp" => "webp",
                "image/gif" => "gif",
                "image/svg+xml" => "svg",
                _ => "bin",
            };
            result.push(json!({"type":"file","mime":mime,"filename":format!("attachment-{ordinal}.{ext}"),"url":url}));
        } else {
            result.push(json!({"type":"text","text":render_prompt_attachment(p)?}));
        }
    }
    Ok(result)
}
pub fn prompt_text(prompt: &Value) -> Result<String> {
    if let Some(s) = prompt.as_str() {
        return Ok(s.into());
    }
    let blocks = prompt
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("OpenCode prompt must be text or content blocks"))?;
    let mut parts = Vec::with_capacity(blocks.len());
    for p in blocks {
        let text = if s(&p["type"]) == "image" {
            "[Image]".into()
        } else {
            render_prompt_attachment(p)?
        };
        if !text.trim().is_empty() {
            parts.push(text);
        }
    }
    Ok(parts.join("\n"))
}
