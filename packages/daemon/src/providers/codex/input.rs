use crate::providers::prompt;
use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
pub(super) async fn turn_input(prompt: Value) -> Result<Value> {
    if let Some(text) = prompt.as_str() {
        return Ok(json!([{"type":"text","text":text,"text_elements":[]}]));
    }
    let blocks = prompt
        .as_array()
        .ok_or_else(|| anyhow!("Codex prompt must be text or content blocks"))?;
    let mut output = Vec::new();
    let mut previous = false;
    for block in blocks {
        match block["type"].as_str() {
            Some("text") => {
                let text = block["text"]
                    .as_str()
                    .ok_or_else(|| anyhow!("Text block is missing text"))?;
                output.push(json!({"type":"text","text":text,"text_elements":[]}));
                previous = !text.is_empty();
            }
            Some("skill") => {
                output.push(block.clone());
                previous = false;
            }
            Some("image") => {
                let data = block["data"]
                    .as_str()
                    .ok_or_else(|| anyhow!("Image block is missing data"))?;
                match prompt::materialize_image(data, block["mimeType"].as_str()) {
                    Ok(path) => output.push(json!({"type":"localImage","path":path})),
                    Err(error) => output.push(json!({"type":"text",
                        "text":format!("User attached image (failed to write temp file): {error}"),
                        "text_elements":[]})),
                }
                previous = false;
            }
            _ => {
                let attachment = prompt::render_attachment(block)?;
                let text = if previous {
                    format!("\n\n{attachment}")
                } else {
                    attachment
                };
                output.push(json!({"type":"text","text":text,"text_elements":[]}));
                previous = true;
            }
        }
    }
    Ok(json!(output))
}
pub(super) fn output_schema(schema: &Value) -> Result<Value> {
    fn object(v: &Value) -> bool {
        v["type"] == "object"
            || v["type"]
                .as_array()
                .is_some_and(|types| types.iter().any(|t| t == "object"))
            || v["properties"].is_object()
    }
    fn normalize(v: &Value, path: &str) -> Result<Value> {
        let Some(map) = v.as_object() else {
            return Ok(v.clone());
        };
        let mut result = map.clone();
        for keyword in [
            "properties",
            "patternProperties",
            "$defs",
            "definitions",
            "dependentSchemas",
        ] {
            if let Some(children) = map.get(keyword).and_then(Value::as_object) {
                let mut normalized = serde_json::Map::new();
                for (name, child) in children {
                    normalized.insert(
                        name.clone(),
                        normalize(child, &format!("{path}.{keyword}.{name}"))?,
                    );
                }
                result.insert(keyword.into(), Value::Object(normalized));
            }
        }
        for keyword in [
            "items",
            "prefixItems",
            "additionalItems",
            "contains",
            "additionalProperties",
            "unevaluatedProperties",
            "propertyNames",
            "not",
            "if",
            "then",
            "else",
            "allOf",
            "anyOf",
            "oneOf",
        ] {
            if let Some(child) = map.get(keyword) {
                let normalized = if let Some(array) = child.as_array() {
                    let mut values = Vec::new();
                    for (i, child) in array.iter().enumerate() {
                        values.push(normalize(child, &format!("{path}.{keyword}[{i}]"))?);
                    }
                    Value::Array(values)
                } else {
                    normalize(child, &format!("{path}.{keyword}"))?
                };
                result.insert(keyword.into(), normalized);
            }
        }
        let mut result = Value::Object(result);
        if object(&result) {
            if result
                .get("additionalProperties")
                .is_some_and(|v| v != false)
            {
                bail!(
                    "Codex structured outputs require {path} to set additionalProperties to false for object schemas."
                )
            }
            result["additionalProperties"] = json!(false);
            if let Some(properties) = result["properties"].as_object() {
                let mut required = result["required"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter(|v| v.is_string())
                            .cloned()
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                for key in properties.keys() {
                    if !required.iter().any(|v| v == key) {
                        required.push(json!(key));
                    }
                }
                result["required"] = json!(required);
            }
        }
        Ok(result)
    }
    if !schema.is_object() || !object(schema) {
        bail!("Codex structured outputs require a root object schema.")
    }
    normalize(schema, "$")
}
