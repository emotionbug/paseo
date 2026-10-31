use anyhow::Result;
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path};
use tokio::fs;

fn front_matter(text: &str) -> (BTreeMap<String, String>, &str) {
    let mut meta = BTreeMap::new();
    let mut lines = text.split_inclusive('\n');
    let Some(first) = lines.next() else {
        return (meta, text);
    };
    if first.trim() != "---" {
        return (meta, text);
    }
    let mut offset = first.len();
    for line in lines {
        offset += line.len();
        if line.trim() == "---" {
            return (meta, &text[offset..]);
        }
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            let value = value
                .trim()
                .trim_start_matches(['\'', '"'])
                .trim_end_matches(['\'', '"']);
            if !key.trim().is_empty() && !value.is_empty() {
                meta.insert(key.trim().into(), value.into());
            }
        }
    }
    (BTreeMap::new(), text)
}

pub async fn custom_prompts(home: &Path) -> Result<Vec<Value>> {
    let mut result = Vec::new();
    let Ok(mut entries) = fs::read_dir(home.join("prompts")).await else {
        return Ok(result);
    };
    while let Some(entry) = entries.next_entry().await? {
        if !entry.file_type().await?.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(name) = name.strip_suffix(".md").filter(|s| !s.is_empty()) else {
            continue;
        };
        let Ok(text) = fs::read_to_string(entry.path()).await else {
            continue;
        };
        let (meta, _) = front_matter(&text);
        result.push(json!({"name":format!("prompts:{name}"),"description":meta.get("description").map(String::as_str).unwrap_or("Custom prompt"),"argumentHint":meta.get("argument-hint").or_else(||meta.get("argument_hint")).map(String::as_str).unwrap_or(""),"kind":"command"}));
    }
    result.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    Ok(result)
}

pub async fn fallback_skills(home: &Path, cwd: &Path) -> Result<Vec<Value>> {
    let mut candidates = vec![cwd.join(".codex/skills")];
    // Both .git directories and worktree .git files identify repository roots.
    let mut root = None;
    for ancestor in cwd.ancestors() {
        if fs::metadata(ancestor.join(".git")).await.is_ok() {
            root = Some(ancestor);
            break;
        }
    }
    if let Some(root) = root {
        if let Some(parent) = cwd.parent() {
            candidates.push(parent.join(".codex/skills"));
        }
        candidates.push(root.join(".codex/skills"));
    }
    candidates.push(home.join("skills"));
    let mut found = BTreeMap::new();
    for dir in candidates {
        let Ok(mut entries) = fs::read_dir(dir).await else {
            continue;
        };
        while let Some(entry) = entries.next_entry().await? {
            let kind = entry.file_type().await?;
            if !kind.is_dir() && !kind.is_symlink() {
                continue;
            }
            let Ok(text) = fs::read_to_string(entry.path().join("SKILL.md")).await else {
                continue;
            };
            let (meta, _) = front_matter(&text);
            if let (Some(name), Some(description)) = (meta.get("name"), meta.get("description")) {
                found.entry(name.clone()).or_insert_with(||json!({"name":name,"description":description,"argumentHint":"","kind":"skill"}));
            }
        }
    }
    Ok(found.into_values().collect())
}

pub fn expand_template(template: &str, args: &str) -> String {
    let args = args.trim();
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut chars = args.chars().peekable();
    while let Some(ch) = chars.next() {
        if let Some(q) = quote {
            if ch == q {
                quote = None;
                continue;
            }
            if ch == '\\' {
                if let Some(&next) = chars.peek() {
                    if next == q || matches!(next, '\\' | 'n' | 't') {
                        chars.next();
                        current.push(match next {
                            'n' => '\n',
                            't' => '\t',
                            other => other,
                        });
                        continue;
                    }
                }
            }
            current.push(ch);
        } else if matches!(ch, '\'' | '"') {
            quote = Some(ch);
        } else if ch.is_whitespace() {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
        } else {
            current.push(ch);
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    let mut named = BTreeMap::new();
    let mut positional = Vec::new();
    for token in &tokens {
        if let Some((key, value)) = token.split_once('=').filter(|(k, _)| !k.is_empty()) {
            named.insert(key, value);
        } else {
            positional.push(token.as_str());
        }
    }
    let mut keys: Vec<_> = named.keys().copied().collect();
    keys.sort_by_key(|key| std::cmp::Reverse(key.len()));
    let mut output = String::with_capacity(template.len());
    let mut remaining = template;
    while let Some(index) = remaining.find('$') {
        output.push_str(&remaining[..index]);
        remaining = &remaining[index + 1..];
        if let Some(rest) = remaining.strip_prefix('$') {
            output.push('$');
            remaining = rest;
        } else if let Some(rest) = remaining.strip_prefix("ARGUMENTS") {
            output.push_str(args);
            remaining = rest;
        } else if let Some(digit @ b'1'..=b'9') = remaining.as_bytes().first().copied() {
            output.push_str(
                positional
                    .get((digit - b'1') as usize)
                    .copied()
                    .unwrap_or(""),
            );
            remaining = &remaining[1..];
        } else if let Some(key) = keys.iter().find(|key| {
            remaining.strip_prefix(**key).is_some_and(|rest| {
                rest.chars()
                    .next()
                    .is_none_or(|ch| !ch.is_ascii_alphanumeric() && ch != '_')
            })
        }) {
            output.push_str(named[key]);
            remaining = &remaining[key.len()..];
        } else {
            output.push('$');
        }
    }
    output.push_str(remaining);
    output
}

/// `home` is the resolved CODEX_HOME, not the operating-system home directory.
/// Object invocations are already resolved commands; unknown slash strings stay literal.
pub async fn expand(home: &Path, prompt: &Value, skills: &[Value]) -> Result<Value> {
    let invocation = if let Some(text) = prompt.as_str() {
        let Some(text) = text.trim().strip_prefix('/') else {
            return Ok(prompt.clone());
        };
        let split = text.find(char::is_whitespace).unwrap_or(text.len());
        Some((&text[..split], text[split..].trim(), false))
    } else {
        prompt["commandName"]
            .as_str()
            .map(|name| (name, prompt["args"].as_str().unwrap_or(""), true))
    };
    let Some((name, args, explicit)) = invocation else {
        return Ok(prompt.clone());
    };
    if let Some(prompt_name) = name.strip_prefix("prompts:") {
        if explicit
            || custom_prompts(home)
                .await?
                .iter()
                .any(|p| p["name"] == name)
        {
            let text =
                fs::read_to_string(home.join("prompts").join(format!("{prompt_name}.md"))).await?;
            return Ok(Value::String(expand_template(front_matter(&text).1, args)));
        }
    }
    if let Some(skill) = skills
        .iter()
        .find(|skill| skill["name"] == name && skill["enabled"] != false)
    {
        let text = if args.trim().is_empty() {
            format!("${name}")
        } else {
            format!("${name} {}", args.trim())
        };
        if let Some(path) = skill["path"].as_str() {
            return Ok(
                json!([{"type":"skill","name":name,"path":path},{"type":"text","text":text}]),
            );
        }
        return Ok(json!(text));
    }
    if explicit {
        return Ok(json!(if args.is_empty() {
            format!("${name}")
        } else {
            format!("${name} {args}")
        }));
    }
    Ok(prompt.clone())
}
