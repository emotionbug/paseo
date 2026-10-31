use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::path::{Component, Path, PathBuf};

pub fn registry(source: &str, default_url: &str) -> Result<Option<Value>> {
    let parts: Vec<_> = source.split('/').collect();
    if parts.len() == 2 && registry_id(source) {
        return Ok(Some(
            json!({"url":default_url.trim_end_matches('/'),"id":source}),
        ));
    }
    if parts.len() == 3
        && parts[0].contains(['.', ':'])
        && !parts[0].contains(['@', '?', '#', '\\'])
    {
        let id = format!("{}/{}", parts[1], parts[2]);
        if registry_id(&id) {
            let url = url::Url::parse(&format!("https://{}", parts[0]))?;
            return Ok(Some(
                json!({"url":url.origin().ascii_serialization(),"id":id}),
            ));
        }
    }
    Ok(None)
}

pub fn registry_id(id: &str) -> bool {
    let mut parts = id.split('/');
    let valid = |part: &str| {
        !part.is_empty()
            && part.split('-').all(|word| {
                !word.is_empty()
                    && word
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            })
    };
    matches!((parts.next(), parts.next(), parts.next()), (Some(a), Some(b), None) if valid(a) && valid(b))
}

pub fn split(source: &str) -> (String, Option<String>) {
    let (prefix, rest) = if let Some(rest) = source.strip_prefix("npm:") {
        ("npm:", rest)
    } else if let Some(rest) = source.strip_prefix("github:") {
        ("github:", rest)
    } else if source.starts_with("git:") && !source.starts_with("git://") {
        ("git:", &source[4..])
    } else {
        ("", source)
    };
    if let Some((base, path)) = rest.rsplit_once(':') {
        let separator = base.len();
        let scheme_ok = if let Some(scheme) = rest.find("://") {
            rest[scheme + 3..]
                .find('/')
                .is_some_and(|start| separator >= scheme + 3 + start)
        } else {
            prefix == "npm:" || !base.contains('@') || base.contains(':') || base.contains('/')
        };
        if portable(path) && scheme_ok {
            return (format!("{prefix}{base}"), Some(path.to_owned()));
        }
    }
    (source.to_owned(), None)
}

fn portable(path: &str) -> bool {
    path == "."
        || (!path.is_empty()
            && !path.contains(':')
            && path
                .split(['/', '\\'])
                .all(|p| !p.is_empty() && p != "." && p != ".."))
}

pub fn plugin_path(path: Option<&str>) -> Result<PathBuf> {
    let path = Path::new(path.filter(|p| !p.is_empty()).unwrap_or("."));
    let mut result = PathBuf::new();
    for part in path.components() {
        match part {
            Component::Normal(part) => result.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                if !result.pop() {
                    bail!("Plugin path must stay inside the repository");
                }
            }
            _ => bail!("Plugin path must be relative to the repository"),
        }
    }
    Ok(result)
}

pub fn absolute(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut result = PathBuf::new();
    for part in absolute.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            _ => result.push(part.as_os_str()),
        }
    }
    Ok(result)
}

pub fn string<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .with_context(|| format!("Missing or invalid {field}"))
}

pub fn is_commit(value: &str) -> bool {
    (40..=64).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn redact(remote: &str) -> String {
    if let Ok(mut url) = url::Url::parse(remote) {
        if matches!(url.scheme(), "http" | "https" | "ssh" | "git" | "file") {
            let _ = url.set_username("");
            let _ = url.set_password(None);
            return url.into();
        }
    }
    remote.to_owned()
}

pub fn links(current: &Value, target: &Value) -> Vec<String> {
    let identity = &current["identity"];
    let revision = current["currentRevision"].as_str().unwrap_or_default();
    if identity["kind"] == "npm" && target["kind"] == "npm" {
        if url::Url::parse(target["resolved"].as_str().unwrap_or_default())
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
            .as_deref()
            == Some("registry.npmjs.org")
        {
            return [revision, target["version"].as_str().unwrap_or_default()]
                .iter()
                .map(|v| {
                    format!(
                        "https://www.npmjs.com/package/{}/v/{}",
                        identity["packageName"].as_str().unwrap_or_default(),
                        percent_encoding::utf8_percent_encode(
                            v,
                            percent_encoding::NON_ALPHANUMERIC
                        )
                    )
                })
                .collect();
        }
    }
    if identity["kind"] == "git" && target["kind"] == "git" {
        let remote = identity["remote"].as_str().unwrap_or_default();
        let remote = if let Some(tail) = remote.strip_prefix("git@") {
            format!("https://{}", tail.replacen(':', "/", 1))
        } else if let Some(tail) = remote.strip_prefix("ssh://") {
            format!("https://{}", tail.strip_prefix("git@").unwrap_or(tail))
        } else {
            remote.to_owned()
        };
        if let Ok(url) = url::Url::parse(remote.trim_end_matches(".git")) {
            let separator = match url.host_str() {
                Some("github.com") => "/compare/",
                Some("gitlab.com") => "/-/compare/",
                _ => return Vec::new(),
            };
            return vec![format!(
                "{}{}{separator}{revision}...{}",
                url.origin().ascii_serialization(),
                url.path(),
                target["commit"].as_str().unwrap_or_default()
            )];
        }
    }
    Vec::new()
}
