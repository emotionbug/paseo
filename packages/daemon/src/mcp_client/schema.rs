use std::sync::Arc;

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};

#[derive(Clone)]
pub(super) struct ToolMetadata {
    pub output: Option<Arc<jsonschema::Validator>>,
    pub task_required: bool,
}

pub(super) fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .with_context(|| format!("MCP {key} must be a string"))
}

fn object(value: &Value) -> Result<&Map<String, Value>> {
    value.as_object().context("MCP value must be an object")
}

fn optional(value: &Value, key: &str, predicate: fn(&Value) -> bool) -> Result<()> {
    if value.get(key).is_some_and(|field| !predicate(field)) {
        bail!("MCP {key} has an invalid type");
    }
    Ok(())
}

fn string_array(value: &Value) -> bool {
    value
        .as_array()
        .is_some_and(|items| items.iter().all(Value::is_string))
}

pub(super) fn result(value: &Value) -> Result<()> {
    object(value)?;
    request_meta(value)
}

fn request_meta(value: &Value) -> Result<()> {
    if let Some(meta) = value.get("_meta") {
        object(meta)?;
        if let Some(token) = meta.get("progressToken") {
            if !(token.is_string()
                || token
                    .as_i64()
                    .is_some_and(|id| id.unsigned_abs() <= 9_007_199_254_740_991))
            {
                bail!("MCP progressToken must be a string or safe integer");
            }
        }
        if let Some(task) = meta.get("io.modelcontextprotocol/related-task") {
            object(task)?;
            string(task, "taskId")?;
        }
    }
    Ok(())
}

pub(super) fn rpc(value: &Value) -> Result<()> {
    let fields = object(value)?;
    let allowed: &[&str] = if value.get("method").is_some() {
        &["jsonrpc", "id", "method", "params"]
    } else {
        &["jsonrpc", "id", "result", "error"]
    };
    if fields.keys().any(|key| !allowed.contains(&key.as_str())) {
        bail!("MCP JSON-RPC envelope contains an unknown field");
    }
    if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        bail!("MCP JSON-RPC version must be 2.0");
    }
    if let Some(id) = value.get("id") {
        if !(id.is_string()
            || id
                .as_i64()
                .is_some_and(|id| id.unsigned_abs() <= 9_007_199_254_740_991))
        {
            bail!("MCP JSON-RPC id must be a string or safe integer");
        }
    }
    if value.get("method").is_some() {
        string(value, "method")?;
        optional(value, "params", Value::is_object)?;
        if let Some(params) = value.get("params") {
            request_meta(params)?;
        }
        if value.get("result").is_some() || value.get("error").is_some() {
            bail!("MCP JSON-RPC request contains a response");
        }
    } else {
        match (value.get("result"), value.get("error")) {
            (Some(response), None) => {
                if value.get("id").is_none() {
                    bail!("MCP JSON-RPC result has no id")
                }
                result(response)?;
            }
            (None, Some(error)) => {
                object(error)?;
                if error.get("code").and_then(Value::as_i64).is_none() {
                    bail!("MCP JSON-RPC error code must be an integer")
                }
                string(error, "message")?;
            }
            _ => bail!("MCP JSON-RPC response must have exactly one result or error"),
        }
    }
    Ok(())
}

pub(super) fn initialize<'a>(value: &'a Value, versions: &[&str]) -> Result<&'a str> {
    result(value)?;
    let version = string(value, "protocolVersion")?;
    if !versions.contains(&version) {
        bail!("MCP server selected an unsupported protocol version")
    }
    let capabilities = value
        .get("capabilities")
        .context("MCP initialize result has no capabilities")?;
    object(capabilities)?;
    for field in [
        "experimental",
        "logging",
        "completions",
        "prompts",
        "resources",
        "tools",
        "tasks",
    ] {
        optional(capabilities, field, Value::is_object)?;
    }
    for field in ["prompts", "resources", "tools"] {
        if let Some(capability) = capabilities.get(field) {
            optional(capability, "listChanged", Value::is_boolean)?;
        }
    }
    if let Some(resources) = capabilities.get("resources") {
        optional(resources, "subscribe", Value::is_boolean)?;
    }
    let server = value
        .get("serverInfo")
        .context("MCP initialize result has no serverInfo")?;
    metadata(server)?;
    string(server, "version")?;
    optional(value, "instructions", Value::is_string)?;
    Ok(version)
}

fn metadata(value: &Value) -> Result<()> {
    object(value)?;
    string(value, "name")?;
    optional(value, "title", Value::is_string)?;
    if let Some(icons) = value.get("icons") {
        for icon in icons.as_array().context("MCP icons must be an array")? {
            string(icon, "src")?;
            optional(icon, "mimeType", Value::is_string)?;
            optional(icon, "sizes", string_array)?;
            if let Some(theme) = icon.get("theme") {
                if !matches!(theme.as_str(), Some("light" | "dark")) {
                    bail!("MCP icon theme must be light or dark")
                }
            }
        }
    }
    Ok(())
}

fn object_schema(value: &Value) -> Result<()> {
    object(value)?;
    if value.get("type").and_then(Value::as_str) != Some("object") {
        bail!("MCP tool schema must have object type")
    }
    if let Some(properties) = value.get("properties") {
        for value in object(properties)?.values() {
            // The SDK AssertObjectSchema admits arrays as well as objects.
            if !value.is_object() && !value.is_array() {
                bail!("MCP schema property must be an object")
            }
        }
    }
    optional(value, "required", string_array)
}

pub(super) fn tool(value: &Value) -> Result<ToolMetadata> {
    metadata(value)?;
    optional(value, "description", Value::is_string)?;
    optional(value, "_meta", Value::is_object)?;
    object_schema(
        value
            .get("inputSchema")
            .context("MCP tool has no inputSchema")?,
    )?;
    if let Some(annotations) = value.get("annotations") {
        object(annotations)?;
        optional(annotations, "title", Value::is_string)?;
        for field in [
            "readOnlyHint",
            "destructiveHint",
            "idempotentHint",
            "openWorldHint",
        ] {
            optional(annotations, field, Value::is_boolean)?;
        }
    }
    let task_required = if let Some(execution) = value.get("execution") {
        object(execution)?;
        match execution.get("taskSupport").and_then(Value::as_str) {
            Some("required") => true,
            Some("optional" | "forbidden") => false,
            None if execution.get("taskSupport").is_none() => false,
            _ => bail!("MCP tool taskSupport is invalid"),
        }
    } else {
        false
    };
    let output = value.get("outputSchema").map(|schema| {
        object_schema(schema)?;
        // SDK AjvJsonSchemaValidator uses Ajv's default draft-07 engine with
        // formats enabled, strict=false, and no external schema loader.
        let validator = jsonschema::options()
            .with_draft(jsonschema::Draft::Draft7)
            .should_validate_formats(true)
            .should_ignore_unknown_formats(true)
            .offline()
            .build(schema)
            .map_err(|_| anyhow::anyhow!("MCP output schema could not be compiled (external references are not fetched)"))?;
        Ok::<_, anyhow::Error>(Arc::new(validator))
    }).transpose()?;
    Ok(ToolMetadata {
        output,
        task_required,
    })
}

pub(super) fn call_result(value: &mut Value) -> Result<()> {
    result(value)?;
    optional(value, "isError", Value::is_boolean)?;
    optional(value, "structuredContent", Value::is_object)?;
    // This is the SDK's schema default, not an invented successful fallback:
    // structured-output-only tools are allowed to omit content entirely.
    if value.get("content").is_none() {
        value
            .as_object_mut()
            .expect("validated object")
            .insert("content".into(), Value::Array(Vec::new()));
    }
    for item in value["content"]
        .as_array()
        .context("MCP result content must be an array")?
    {
        content(item)?;
    }
    Ok(())
}

fn content(value: &Value) -> Result<()> {
    object(value)?;
    optional(value, "_meta", Value::is_object)?;
    if let Some(annotations) = value.get("annotations") {
        object(annotations)?;
        if let Some(audience) = annotations.get("audience") {
            if !audience.as_array().is_some_and(|items| {
                items
                    .iter()
                    .all(|item| matches!(item.as_str(), Some("user" | "assistant")))
            }) {
                bail!("MCP content audience must contain user or assistant roles");
            }
        }
        if let Some(priority) = annotations.get("priority") {
            if !priority.as_f64().is_some_and(|n| (0.0..=1.0).contains(&n)) {
                bail!("MCP content priority must be between zero and one")
            }
        }
        if let Some(modified) = annotations.get("lastModified") {
            let text = modified
                .as_str()
                .context("MCP content lastModified must be a timestamp")?;
            chrono::DateTime::parse_from_rfc3339(text)
                .context("MCP content lastModified must be an ISO timestamp with offset")?;
        }
    }
    match string(value, "type")? {
        "text" => {
            string(value, "text")?;
        }
        "image" | "audio" => {
            base64(string(value, "data")?)?;
            string(value, "mimeType")?;
        }
        "resource" => {
            let resource = value
                .get("resource")
                .context("MCP embedded resource is missing")?;
            object(resource)?;
            string(resource, "uri")?;
            optional(resource, "mimeType", Value::is_string)?;
            optional(resource, "_meta", Value::is_object)?;
            // The SDK accepts either branch of the text/blob union; unrelated
            // fields do not invalidate a correctly typed text resource.
            if resource.get("text").is_none_or(|text| !text.is_string()) {
                base64(string(resource, "blob")?)?;
            }
        }
        "resource_link" => {
            metadata(value)?;
            string(value, "uri")?;
            optional(value, "mimeType", Value::is_string)?;
            optional(value, "description", Value::is_string)?;
            optional(value, "size", Value::is_number)?;
        }
        _ => bail!("MCP result contains an unknown content block type"),
    }
    Ok(())
}

// WHATWG forgiving-base64 validation (the source schema uses atob): permit
// ASCII whitespace and omitted padding without allocating decoded image bytes.
fn base64(text: &str) -> Result<()> {
    let mut data = 0usize;
    let mut padding = 0usize;
    for byte in text.bytes() {
        match byte {
            b' ' | b'\t' | b'\n' | b'\r' | 0x0c => {}
            b'=' => {
                padding += 1;
                if padding > 2 {
                    bail!("MCP content is not base64")
                }
            }
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'+' | b'/' if padding == 0 => data += 1,
            _ => bail!("MCP content is not base64"),
        }
    }
    if data % 4 == 1 || (padding > 0 && ((data + padding) % 4 != 0 || data == 0)) {
        bail!("MCP content is not base64");
    }
    Ok(())
}
