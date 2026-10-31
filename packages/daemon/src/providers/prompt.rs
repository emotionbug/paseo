use std::{
    fmt::Write as _,
    fs::{self, OpenOptions},
    io::Write as _,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Missing prompt field {key}"))
}

fn js_space(c: char) -> bool {
    matches!(c, '\u{0009}'..='\u{000d}' | ' ' | '\u{00a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}')
}

fn nonempty(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .map(|s| s.trim_matches(js_space))
        .filter(|s| !s.is_empty())
}

/// ACP's raw command-text extraction, not a generalized attachment renderer.
pub fn text(prompt: &Value) -> Result<String> {
    if let Some(text) = prompt.as_str() {
        return Ok(text.to_owned());
    }
    let blocks = prompt
        .as_array()
        .ok_or_else(|| anyhow!("Prompt must be a string or content blocks"))?;
    let mut result = String::new();
    for block in blocks {
        if block.get("type").and_then(Value::as_str) == Some("text") {
            result.push_str(string(block, "text")?);
        }
    }
    Ok(result)
}

pub fn to_acp_blocks(prompt: &Value) -> Result<Vec<Value>> {
    if let Some(text) = prompt.as_str() {
        return Ok(vec![json!({"type":"text","text":text})]);
    }
    prompt.as_array().ok_or_else(|| anyhow!("Prompt must be a string or content blocks"))?.iter().map(|block| {
        match string(block, "type")? {
            "text" => Ok(json!({"type":"text","text":string(block,"text")?})),
            "image" => Ok(json!({"type":"image","data":string(block,"data")?,"mimeType":string(block,"mimeType")?})),
            _ => Ok(json!({"type":"text","text":render_attachment(block)?})),
        }
    }).collect()
}

pub fn build(text: &str, images: &[Value], attachments: &[Value]) -> Value {
    let text = text.trim_matches(js_space);
    if images.is_empty() && attachments.is_empty() {
        return Value::String(text.to_owned());
    }
    let history = |a: &&Value| a["type"] == "text" && a["contextKind"] == "chat_history";
    let mut blocks =
        Vec::with_capacity(images.len() + attachments.len() + usize::from(!text.is_empty()));
    blocks.extend(attachments.iter().filter(history).cloned());
    if !text.is_empty() {
        blocks.push(json!({"type":"text","text":text}));
    }
    blocks.extend(
        images
            .iter()
            .map(|image| json!({"type":"image","data":image["data"],"mimeType":image["mimeType"]})),
    );
    blocks.extend(attachments.iter().filter(|a| !history(a)).cloned());
    Value::Array(blocks)
}

pub fn render_attachment(attachment: &Value) -> Result<String> {
    match string(attachment, "type")? {
        "text" => Ok(string(attachment, "text")?.to_owned()),
        "forge_change_request" | "github_pr" | "forge_issue" | "github_issue" => {
            render_forge(attachment)
        }
        "uploaded_file" => Ok(format!(
            "Uploaded file: {}\nPath: {}\nMIME: {}\nSize: {} bytes",
            string(attachment, "fileName")?,
            string(attachment, "path")?,
            string(attachment, "mimeType")?,
            attachment
                .get("size")
                .ok_or_else(|| anyhow!("Missing uploaded file size"))?
        )),
        "review" => {
            let mut output = format!(
                "Paseo review attachment ({})\nCWD: {}",
                string(attachment, "mode")?,
                string(attachment, "cwd")?
            );
            if let Some(base) = attachment
                .get("baseRef")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                write!(output, "\nBase: {base}")?;
            }
            for (index, comment) in attachment
                .get("comments")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow!("Missing review comments"))?
                .iter()
                .enumerate()
            {
                write!(
                    output,
                    "\n\nComment {}: {}:{}:{}\n{}\n{}",
                    index + 1,
                    string(comment, "filePath")?,
                    string(comment, "side")?,
                    comment["lineNumber"],
                    string(comment, "body")?,
                    string(&comment["context"], "hunkHeader")?
                )?;
                let target = &comment["context"]["targetLine"];
                for line in comment["context"]
                    .get("lines")
                    .and_then(Value::as_array)
                    .ok_or_else(|| anyhow!("Missing review context"))?
                {
                    let is_target = ["oldLineNumber", "newLineNumber", "type", "content"]
                        .iter()
                        .all(|key| line.get(*key) == target.get(*key));
                    let marker = match string(line, "type")? {
                        "add" => '+',
                        "remove" => '-',
                        "context" => ' ',
                        other => bail!("Unknown review line type {other}"),
                    };
                    let number = |key: &str| {
                        line.get(key)
                            .filter(|v| !v.is_null())
                            .map(Value::to_string)
                            .unwrap_or_else(|| "-".to_owned())
                    };
                    write!(
                        output,
                        "\n{}{:>2} {:>2} {marker}{}",
                        if is_target { "> " } else { "  " },
                        number("oldLineNumber"),
                        number("newLineNumber"),
                        string(line, "content")?
                    )?;
                }
            }
            Ok(output)
        }
        other => bail!("Unknown prompt attachment type {other}"),
    }
}

fn render_forge(value: &Value) -> Result<String> {
    let kind = string(value, "type")?;
    let legacy = kind.starts_with("github_");
    let forge = if legacy {
        "github"
    } else {
        string(value, "forge")?
    };
    let (label, abbreviation, prefix) = match forge {
        "github" => ("GitHub", "PR", "#"),
        "gitlab" => ("GitLab", "MR", "!"),
        "gitea" => ("Gitea", "PR", "#"),
        "forgejo" => ("Forgejo", "PR", "#"),
        "codeberg" => ("Codeberg", "PR", "#"),
        other => (other, "PR", "#"),
    };
    let issue = kind.ends_with("issue");
    let mut output = format!(
        "{label} {} {}{}: {}\n{}",
        if issue { "Issue" } else { abbreviation },
        if issue { "#" } else { prefix },
        value
            .get("number")
            .ok_or_else(|| anyhow!("Missing forge number"))?,
        string(value, "title")?,
        string(value, "url")?
    );
    let fields = [
        ("projectPath", "Project"),
        ("baseRefName", "Base"),
        ("headRefName", "Head"),
    ];
    for (key, label) in fields {
        if (legacy && key == "projectPath") || (issue && key != "projectPath") {
            continue;
        }
        if let Some(text) = value
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            write!(output, "\n{label}: {text}")?;
        }
    }
    if let Some(body) = value
        .get("body")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        write!(output, "\n\n{body}")?;
    }
    Ok(output)
}

pub fn branch_name_seed(context: Option<&Value>) -> Result<Option<String>> {
    let Some(context) = context else {
        return Ok(None);
    };
    let mut parts = Vec::new();
    if let Some(prompt) = nonempty(context.get("prompt")) {
        parts.push(format!("<user-prompt>\n{prompt}\n</user-prompt>"));
    }
    let mut attachments = Vec::new();
    if let Some(values) = context.get("attachments").and_then(Value::as_array) {
        for value in values {
            let rendered = render_attachment(value)?;
            let rendered = rendered.trim_matches(js_space);
            if !rendered.is_empty() {
                attachments.push(rendered.to_owned());
            }
        }
    }
    if !attachments.is_empty() {
        parts.push(format!(
            "<attachments>\n{}\n</attachments>",
            attachments.join("\n\n")
        ));
    }
    Ok((!parts.is_empty()).then(|| parts.join("\n\n")))
}

static IMAGE_DIRECTORY: Mutex<Option<Arc<PathBuf>>> = Mutex::new(None);

fn private_mode(path: &Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
    Ok(())
}

fn image_directory() -> Result<Arc<PathBuf>> {
    let mut cached = IMAGE_DIRECTORY
        .lock()
        .map_err(|_| anyhow!("Image directory lock poisoned"))?;
    if let Some(path) = cached.as_ref() {
        if fs::symlink_metadata(path.as_path()).is_ok_and(|m| m.file_type().is_dir()) {
            private_mode(path, 0o700)?;
            return Ok(Arc::clone(path));
        }
    }
    // Persist after shutdown: timeline Markdown references these paths on replay.
    let path = tempfile::Builder::new()
        .prefix("paseo-attachments-")
        .tempdir()?
        .keep();
    private_mode(&path, 0o700)?;
    let path = Arc::new(path);
    *cached = Some(Arc::clone(&path));
    Ok(path)
}

fn image_data<'a>(data: &'a str, mime: Option<&'a str>) -> (&'a str, &'a str) {
    if let Some(rest) = data.strip_prefix("data:") {
        if let Some((mime, data)) = rest.split_once(";base64,") {
            if !mime.is_empty()
                && !mime.contains(';')
                && !data.contains(['\n', '\r', '\u{2028}', '\u{2029}'])
            {
                return (data, mime);
            }
        }
    }
    (data, mime.unwrap_or("image/png"))
}

fn image_extension(mime: &str) -> &'static str {
    match mime {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/webp" => "webp",
        "image/gif" => "gif",
        "image/bmp" => "bmp",
        "image/tiff" => "tiff",
        _ => "bin",
    }
}

// Buffer.from(data,"base64") accepts URL-safe symbols, omitted padding and
// whitespace/irrelevant bytes. Keep that existing provider boundary unchanged.
fn decode_image(data: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(data.len().saturating_mul(3) / 4);
    let mut accumulator = 0u16;
    let mut bits = 0u8;
    for byte in data.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            _ => continue,
        };
        accumulator = (accumulator << 6) | u16::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            bytes.push((accumulator >> bits) as u8);
        }
    }
    bytes
}

fn write_image(path: &Path, bytes: &[u8], unique: bool) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true);
    if unique {
        options.create_new(true);
    } else {
        options.create(true).truncate(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("Cannot materialize provider image {}", path.display()))?;
    file.write_all(bytes)?;
    private_mode(path, 0o600)
}

pub fn materialize_image(data: &str, mime_type: Option<&str>) -> Result<PathBuf> {
    let (data, mime) = image_data(data, mime_type);
    let bytes = decode_image(data);
    let path = image_directory()?.join(format!(
        "{}.{}",
        hex::encode(Sha256::digest(&bytes)),
        image_extension(mime)
    ));
    write_image(&path, &bytes, false)?;
    Ok(path)
}

pub fn materialize_unique_image_in(
    data: &str,
    mime_type: Option<&str>,
    directory: &Path,
) -> Result<PathBuf> {
    let (data, mime) = image_data(data, mime_type);
    let bytes = decode_image(data);
    let mime = mime.to_ascii_lowercase();
    let subtype = mime
        .strip_prefix("image/")
        .filter(|subtype| {
            subtype
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
                && subtype
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b".+-".contains(&byte))
        })
        .ok_or_else(|| anyhow!("Invalid image MIME type"))?;
    let extension = match subtype {
        "jpeg" => "jpg",
        "svg+xml" => "svg",
        other => other,
    };
    let path = directory.join(format!("{}.{}", uuid::Uuid::new_v4(), extension));
    write_image(&path, &bytes, true)?;
    Ok(path)
}

fn encode_path(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()/".contains(&byte) {
            output.push(byte as char);
        } else {
            output.push('%');
            output.push(HEX[(byte >> 4) as usize] as char);
            output.push(HEX[(byte & 15) as usize] as char);
        }
    }
    output
}

fn markdown_source(source: &str) -> String {
    let network = source.starts_with("\\\\");
    let mut normalized = source.replace('\\', "/");
    if normalized
        .get(..8)
        .is_some_and(|s| s.eq_ignore_ascii_case("//?/UNC/"))
    {
        normalized = format!("//{}", &normalized[8..]);
    } else if normalized.starts_with("//?/")
        && normalized
            .as_bytes()
            .get(4)
            .is_some_and(u8::is_ascii_alphabetic)
        && normalized.get(5..7) == Some(":/")
    {
        normalized.drain(..4);
    }
    let uri = if normalized
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphabetic)
        && normalized.get(1..3) == Some(":/")
    {
        format!(
            "file:///{}{}",
            &normalized[..2],
            encode_path(&normalized[2..])
        )
    } else if network && normalized.starts_with("//") {
        format!("file:{}", encode_path(&normalized))
    } else if source.starts_with('/') {
        format!("file://{}", encode_path(source))
    } else {
        source.to_owned()
    };
    uri.replace('\\', "\\\\").replace(')', "\\)")
}

pub fn projected_image(image: &Value) -> Result<Option<Value>> {
    let source = nonempty(image.get("path")).or_else(|| nonempty(image.get("url")));
    let is_inline = |s: &str| {
        s.get(..11)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("data:image/"))
    };
    let materialized;
    let source = if let Some(source) = source.filter(|source| !is_inline(source)) {
        source
    } else {
        let Some(data) = nonempty(image.get("data")).or_else(|| source.filter(|s| is_inline(s)))
        else {
            return Ok(None);
        };
        match materialize_image(data, nonempty(image.get("mimeType"))) {
            Ok(path) => {
                materialized = path.to_string_lossy().into_owned();
                &materialized
            }
            Err(error) => {
                tracing::warn!(error=%error,"Provider image materialization failed");
                return Ok(Some(
                    json!({"type":"assistant_message","text":"Image output was omitted because it was not available as a file path or URL."}),
                ));
            }
        }
    };
    let alt = nonempty(image.get("altText"))
        .unwrap_or("Image")
        .replace('\\', "\\\\")
        .replace(']', "\\]");
    Ok(Some(
        json!({"type":"assistant_message","text":format!("\n\n![{alt}]({})\n\n",markdown_source(source))}),
    ))
}
