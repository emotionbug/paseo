use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub fn validate_id(id: &str) -> Result<()> {
    if id.is_empty()
        || !id.as_bytes()[0].is_ascii_lowercase()
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        bail!("Invalid plugin ID: {id}");
    }
    Ok(())
}
pub fn validate_contribution_id(id: &str) -> Result<()> {
    if id.is_empty()
        || !id.as_bytes()[0].is_ascii_lowercase()
        || !id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
        })
    {
        bail!("Invalid plugin contribution ID: {id}");
    }
    Ok(())
}
pub async fn read(directory: &Path) -> Result<Value> {
    let path = directory.join("paseo-plugin.json");
    let bytes = tokio::fs::read(&path)
        .await
        .with_context(|| format!("Plugin manifest is missing: {}", path.display()))?;
    if bytes.len() > 1024 * 1024 {
        bail!("Plugin manifest exceeds 1 MiB");
    }
    let mut manifest: Value =
        serde_json::from_slice(&bytes).context("Invalid plugin manifest JSON")?;
    let object = manifest
        .as_object()
        .context("Plugin manifest must be an object")?;
    validate_id(
        object
            .get("id")
            .and_then(Value::as_str)
            .context("Plugin manifest requires id")?,
    )?;
    for field in ["name", "description"] {
        if let Some(value) = object.get(field) {
            if value.as_str().is_none_or(|s| s.trim().is_empty()) {
                bail!("Plugin manifest {field} must be a nonempty string");
            }
        }
    }
    if let Some(value) = object.get("icon") {
        let icon = value.as_str().context("Plugin icon must be a path")?;
        validate_asset(icon)?;
        if !icon.to_ascii_lowercase().ends_with(".png") {
            bail!("Plugin icon must be a PNG");
        }
    }
    if let Some(media) = object.get("media") {
        for item in media.as_array().context("Plugin media must be an array")? {
            let item = item.as_str().context("Invalid plugin media path")?;
            if !item.starts_with("https://") {
                validate_asset(item)?;
            } else {
                url::Url::parse(item)?;
            }
        }
    }
    if let Some(build) = object.get("build") {
        let commands = build.as_array().context("Plugin build must be an array")?;
        if commands.is_empty() {
            bail!("Plugin build must contain a command");
        }
        for command in commands {
            let args = command
                .as_array()
                .context("Plugin build command must be an array")?;
            if args.is_empty()
                || args
                    .iter()
                    .any(|arg| arg.as_str().is_none_or(|arg| arg.trim().is_empty()))
            {
                bail!("Invalid plugin build command");
            }
        }
    }
    if let Some(requirements) = object.get("requirements") {
        let requirements = requirements
            .as_object()
            .context("Invalid plugin requirements")?;
        if requirements.keys().any(|key| key != "paseo") {
            bail!("Unknown plugin requirement");
        }
        if requirements
            .get("paseo")
            .is_some_and(|value| value.as_str().is_none_or(|s| s.trim().is_empty()))
        {
            bail!("Invalid requirements.paseo");
        }
    }
    if let Some(range) = manifest["requirements"]["paseo"].as_str() {
        node_semver::Range::parse(range).with_context(|| {
            format!(
                "Invalid requirements.paseo: {range}. Use an npm semver range such as \">=0.8.0\"."
            )
        })?;
    }
    for field in ["name", "description"] {
        if let Some(text) = manifest.get(field).and_then(Value::as_str) {
            let trimmed = text.trim().to_owned();
            manifest[field] = Value::String(trimmed);
        }
    }
    Ok(manifest)
}
pub async fn require_server_entry(directory: &Path) -> Result<()> {
    for name in ["index.server.ts", "index.server.tsx"] {
        let path = directory.join(name);
        match tokio::fs::metadata(&path).await {
            Ok(metadata) if metadata.is_file() => return Ok(()),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("Cannot inspect plugin entry: {}", path.display()));
            }
        }
    }
    bail!(
        "Native Paseo requires index.server.ts or index.server.tsx; client-only plugins are not supported: {}",
        directory.display()
    )
}

pub fn check_compatibility(manifest: &Value) -> Result<()> {
    check_version(manifest, env!("CARGO_PKG_VERSION"))
}
pub(super) fn check_version(manifest: &Value, version: &str) -> Result<()> {
    let id = manifest["id"].as_str().unwrap_or("");
    let requirement = manifest["requirements"]["paseo"].as_str();
    let range_text = requirement.unwrap_or("<0.8.0");
    let range = node_semver::Range::parse(range_text).context("Invalid requirements.paseo")?;
    let parsed = node_semver::Version::parse(version).context(
        "Cannot check plugin requirements: Paseo daemon version is unknown. Update the daemon.",
    )?;
    let stable = node_semver::Version::parse(version.split(['-', '+']).next().unwrap_or(version))?;
    if range.satisfies(&parsed) || range.satisfies(&stable) {
        return Ok(());
    }
    let action = if requirement.is_none() {
        "This plugin has no requirements.paseo and targets Paseo before 0.8. Ask its author to migrate it: https://paseo.sh/docs/plugins/migration"
    } else {
        "Use a compatible plugin version or update the daemon."
    };
    bail!("Plugin \"{id}\" requires Paseo {range_text}. Your daemon is {version}. {action}")
}
pub fn validate_asset(path: &str) -> Result<()> {
    if path.is_empty()
        || path != path.trim()
        || path.starts_with('/')
        || path.ends_with('/')
        || path.split('/').any(|part| part == "..")
        || path.split('/').next_back() == Some(".")
        || path
            .chars()
            .any(|ch| ch.is_control() || "\\:?#".contains(ch))
    {
        bail!("Expected a relative file path inside the plugin package");
    }
    Ok(())
}
pub async fn icon(directory: &Path, path: &str) -> Result<String> {
    let directory = tokio::fs::canonicalize(directory).await?;
    let relative = Path::new(path);
    if relative.is_absolute() {
        bail!("Provider icon must be relative to plugin directory");
    }
    let file = tokio::fs::canonicalize(directory.join(relative)).await?;
    if !file.starts_with(&directory) {
        bail!("Provider icon is outside plugin directory");
    }
    let bytes = tokio::fs::read(&file).await?;
    if bytes.len() > 64 * 1024 {
        bail!("Provider icon exceeds 64 KiB");
    }
    let svg = String::from_utf8(bytes).context("Provider icon must be UTF-8 SVG")?;
    validate_svg(&svg)?;
    Ok(svg)
}
fn validate_svg(svg: &str) -> Result<()> {
    static PATTERNS: std::sync::LazyLock<(regex::Regex, regex::Regex, regex::Regex)> =
        std::sync::LazyLock::new(|| {
            (
        regex::Regex::new(r"(?i)^\s*<svg(?:\s|>)").unwrap(),
        regex::Regex::new(r"(?i)<(?:script|foreignObject|style)(?:\s|>)|\son[a-z0-9_-]*\s*=|javascript\s*:").unwrap(),
        regex::Regex::new(r#"(?i)\s(?:href|xlink:href)\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+))"#).unwrap(),
    )
        });
    let (document, unsafe_content, href) = &*PATTERNS;
    if !document.is_match(svg) {
        bail!("Provider icon is not an SVG document");
    }
    if unsafe_content.is_match(svg) {
        bail!("Provider icon contains executable content");
    }
    for captures in href.captures_iter(svg) {
        let value = captures
            .get(1)
            .or_else(|| captures.get(2))
            .or_else(|| captures.get(3))
            .unwrap()
            .as_str();
        if !value.starts_with('#') {
            bail!("Provider icon external href references are not allowed");
        }
    }
    Ok(())
}
pub fn resolve_directory(path: &str, home: &Path) -> Result<PathBuf> {
    let path = if path == "~" {
        home.to_owned()
    } else if let Some(tail) = path.strip_prefix("~/") {
        home.join(tail)
    } else {
        PathBuf::from(path)
    };
    Ok(if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    })
}
