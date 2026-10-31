use crate::providers::prompt::{materialize_image, render_attachment};
use anyhow::{Result, anyhow};
use serde_json::{Value, json};

pub fn convert(prompt: &Value, model: &Value) -> Result<Value> {
    if let Some(text) = prompt.as_str() {
        return Ok(json!({"message":text}));
    }
    let blocks = prompt
        .as_array()
        .ok_or_else(|| anyhow!("Prompt must be text or content blocks"))?;
    let forward_images = model["input"]
        .as_array()
        .is_some_and(|a| a.iter().any(|v| v == "image"));
    let mut text = Vec::new();
    let mut images = Vec::new();
    for block in blocks {
        if block["type"] == "image" {
            if forward_images {
                images.push(
                    json!({"type":"image","data":block["data"],"mimeType":block["mimeType"]}),
                );
            } else {
                let data = block["data"]
                    .as_str()
                    .ok_or_else(|| anyhow!("Image data missing"))?;
                text.push(match materialize_image(data, block["mimeType"].as_str()) {
                    Ok(path) => format!("[Image available at: {}]", path.display()),
                    Err(error) => {
                        format!("[Image attachment omitted: failed to write local file ({error})]")
                    }
                });
            }
        } else {
            text.push(render_attachment(block)?);
        }
    }
    let mut payload = json!({"message":text.join("\n\n")});
    if !images.is_empty() {
        payload["images"] = json!(images);
    }
    Ok(payload)
}
