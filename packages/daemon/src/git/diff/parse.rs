use super::{Change, Stat};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(super) fn changes(bytes: &[u8]) -> Result<Vec<Change>> {
    let mut fields = bytes.split(|b| *b == 0);
    let mut result = Vec::new();
    while let Some(raw) = fields.next() {
        if raw.is_empty() {
            continue;
        }
        // show/log may insert one separator newline before a diff inventory.
        let raw = raw.strip_prefix(b"\n").unwrap_or(raw);
        let status = *raw.first().context("Empty git status")? as char;
        let first = fields.next().context("Missing git status path")?;
        let (old, path) = if status == 'R' || status == 'C' {
            (
                Some(String::from_utf8_lossy(first).into_owned()),
                fields.next().context("Missing rename destination")?,
            )
        } else {
            (None, first)
        };
        result.push(Change {
            path: String::from_utf8_lossy(path).into_owned(),
            old_path: old,
            status,
            untracked: false,
        });
    }
    Ok(result)
}

pub(super) fn stats(bytes: &[u8]) -> Result<BTreeMap<String, Stat>> {
    let mut result = BTreeMap::new();
    let mut fields = bytes.split(|b| *b == 0);
    while let Some(record) = fields.next() {
        if record.is_empty() {
            continue;
        }
        let record = record.strip_prefix(b"\n").unwrap_or(record);
        let mut parts = record.splitn(3, |b| *b == b'\t');
        let additions = parts.next().context("Missing numstat additions")?;
        let deletions = parts.next().context("Missing numstat deletions")?;
        let mut path = parts.next().context("Missing numstat path")?;
        if path.is_empty() {
            fields.next().context("Missing numstat rename source")?;
            path = fields
                .next()
                .context("Missing numstat rename destination")?;
        }
        let binary = additions == b"-" || deletions == b"-";
        let stat = if binary {
            Stat {
                binary: true,
                ..Stat::default()
            }
        } else {
            Stat {
                additions: std::str::from_utf8(additions)?.parse()?,
                deletions: std::str::from_utf8(deletions)?.parse()?,
                binary: false,
            }
        };
        result.insert(String::from_utf8_lossy(path).into_owned(), stat);
    }
    Ok(result)
}

fn unquote(value: &str) -> Result<String> {
    if !value.starts_with('"') {
        return Ok(value.to_owned());
    }
    let raw = value
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .context("Malformed quoted Git path")?
        .as_bytes();
    let mut decoded = Vec::with_capacity(raw.len());
    let mut index = 0;
    while index < raw.len() {
        let byte = raw[index];
        index += 1;
        if byte != b'\\' {
            decoded.push(byte);
            continue;
        }
        let escaped = *raw.get(index).context("Unterminated Git path escape")?;
        index += 1;
        if (b'0'..=b'7').contains(&escaped) {
            let mut value = (escaped - b'0') as u16;
            for _ in 0..2 {
                if let Some(&next) = raw.get(index).filter(|&&b| (b'0'..=b'7').contains(&b)) {
                    value = value * 8 + (next - b'0') as u16;
                    index += 1;
                } else {
                    break;
                }
            }
            decoded.push(value.try_into().context("Invalid Git path octal escape")?);
        } else {
            decoded.push(match escaped {
                b'a' => 7,
                b'b' => 8,
                b't' => 9,
                b'n' => 10,
                b'v' => 11,
                b'f' => 12,
                b'r' => 13,
                b'"' => b'"',
                b'\\' => b'\\',
                _ => escaped,
            });
        }
    }
    Ok(String::from_utf8_lossy(&decoded).into_owned())
}

fn section_path(section: &str) -> Result<Option<String>> {
    // Metadata is unambiguous even when the diff --git line has spaces or
    // embedded ' b/' in a filename. Never split a path on whitespace.
    for line in section.lines().take_while(|l| !l.starts_with("@@ ")) {
        if let Some(path) = line
            .strip_prefix("rename to ")
            .or_else(|| line.strip_prefix("copy to "))
        {
            return Ok(Some(unquote(path)?));
        }
    }
    let mut old = None;
    for line in section.split('\n').take_while(|l| !l.starts_with("@@ ")) {
        if let Some(path) = line.strip_prefix("+++ ") {
            let path = unquote(path.split('\t').next().unwrap_or(path))?;
            if path != "/dev/null" {
                return Ok(Some(path.strip_prefix("b/").unwrap_or(&path).to_owned()));
            }
        }
        if let Some(path) = line.strip_prefix("--- ") {
            let path = unquote(path.split('\t').next().unwrap_or(path))?;
            if path != "/dev/null" {
                old = Some(path.strip_prefix("a/").unwrap_or(&path).to_owned());
            }
        }
    }
    Ok(old)
}

fn range(value: &str, prefix: char) -> Result<(u64, u64)> {
    let value = value
        .strip_prefix(prefix)
        .context("Invalid diff hunk range")?;
    let (start, count) = value.split_once(',').unwrap_or((value, "1"));
    Ok((start.parse()?, count.parse()?))
}

/// Apply only the requested path's patch; multiple sections can arise when a
/// rename destination is also the source of a separate change in this commit.
pub(super) fn apply_patch(file: &mut Value, bytes: &[u8], path: &str) -> Result<bool> {
    let text = String::from_utf8_lossy(bytes);
    let sections: Vec<_> = text.split("\ndiff --git ").collect();
    for section in &sections {
        let named = section_path(section)?;
        if let Some(named) = named {
            if named != path {
                continue;
            }
        } else if sections.len() != 1 {
            let first = section
                .lines()
                .next()
                .unwrap_or("")
                .strip_prefix("diff --git ")
                .unwrap_or(section.lines().next().unwrap_or(""));
            let expected = format!(
                "{} {}",
                quote(&format!("a/{path}"), true),
                quote(&format!("b/{path}"), true)
            );
            let unquoted = format!("a/{path} b/{path}");
            if first != expected && first != unquoted {
                continue;
            }
        }
        if section
            .lines()
            .any(|line| line.starts_with("Binary files ") || line == "GIT binary patch")
        {
            file["status"] = json!("binary");
            return Ok(true);
        }
        let mut hunks = Vec::new();
        let mut hunk: Option<Value> = None;
        let mut additions = 0u64;
        let mut deletions = 0u64;
        for line in section.split('\n') {
            if line.starts_with("@@ ") {
                if let Some(previous) = hunk.take() {
                    hunks.push(previous);
                }
                let mut fields = line.splitn(4, ' ');
                fields.next();
                let (old_start, old_count) =
                    range(fields.next().context("Missing old hunk range")?, '-')?;
                let (new_start, new_count) =
                    range(fields.next().context("Missing new hunk range")?, '+')?;
                let end = line[3..]
                    .find(" @@")
                    .map(|n| n + 6)
                    .context("Missing hunk header end")?;
                hunk = Some(
                    json!({"oldStart":old_start,"oldCount":old_count,"newStart":new_start,"newCount":new_count,"lines":[{"type":"header","content":&line[..end]}]}),
                );
            } else if let Some(current) = hunk.as_mut() {
                let (kind, content) = match line.as_bytes().first() {
                    Some(b'+') => {
                        additions += 1;
                        ("add", &line[1..])
                    }
                    Some(b'-') => {
                        deletions += 1;
                        ("remove", &line[1..])
                    }
                    Some(b' ') => ("context", &line[1..]),
                    Some(b'\\') | None => continue,
                    _ => ("context", line),
                };
                current["lines"]
                    .as_array_mut()
                    .context("Invalid parsed hunk")?
                    .push(json!({"type":kind,"content":content}));
            }
        }
        if let Some(previous) = hunk {
            hunks.push(previous);
        }
        file["hunks"] = Value::Array(hunks);
        file["additions"] = json!(additions);
        file["deletions"] = json!(deletions);
        return Ok(true);
    }
    if bytes.is_empty() {
        return Ok(false);
    }
    bail!("Git patch did not contain requested path: {path}")
}

fn quote(path: &str, non_ascii: bool) -> String {
    let mut encoded = String::new();
    let mut quoted = false;
    for character in path.chars() {
        let escape = match character {
            '\u{7}' => Some("\\a"),
            '\u{8}' => Some("\\b"),
            '\t' => Some("\\t"),
            '\n' => Some("\\n"),
            '\u{b}' => Some("\\v"),
            '\u{c}' => Some("\\f"),
            '\r' => Some("\\r"),
            '"' => Some("\\\""),
            '\\' => Some("\\\\"),
            _ => None,
        };
        if let Some(escape) = escape {
            encoded.push_str(escape);
            quoted = true;
        } else if character < ' ' || character == '\u{7f}' || (non_ascii && !character.is_ascii()) {
            let mut bytes = [0; 4];
            for byte in character.encode_utf8(&mut bytes).as_bytes() {
                encoded.push_str(&format!("\\{byte:03o}"));
            }
            quoted = true;
        } else {
            encoded.push(character);
        }
    }
    if quoted {
        format!("\"{encoded}\"")
    } else {
        encoded
    }
}

pub(super) struct PatchFormat {
    pub quote_non_ascii: bool,
    pub abbreviation: usize,
}
impl Default for PatchFormat {
    fn default() -> Self {
        Self {
            quote_non_ascii: true,
            abbreviation: 7,
        }
    }
}
pub(super) fn patch_format(bytes: &[u8]) -> PatchFormat {
    let text = String::from_utf8_lossy(bytes);
    let mut result = PatchFormat::default();
    let mut full = false;
    let mut hash_length = 40;
    for record in text.split('\0') {
        let Some((key, value)) = record.split_once('\n') else {
            continue;
        };
        match key.to_lowercase().as_str() {
            "core.quotepath" => match value.trim().to_lowercase().as_str() {
                "false" | "no" | "off" | "0" => result.quote_non_ascii = false,
                "true" | "yes" | "on" | "1" => result.quote_non_ascii = true,
                _ => {}
            },
            "extensions.objectformat" if value.trim() == "sha256" => hash_length = 64,
            "core.abbrev" => {
                if value.trim().eq_ignore_ascii_case("no") {
                    full = true;
                } else if let Ok(n) = value.trim().parse::<usize>() {
                    if (4..=64).contains(&n) {
                        result.abbreviation = n;
                    }
                }
            }
            _ => {}
        }
    }
    result.abbreviation = if full {
        hash_length
    } else {
        result.abbreviation.min(hash_length)
    };
    result
}

/// Exact byte length of the source daemon's native added-file patch. Hash bytes
/// need not be computed: the hash is not transmitted and its width is known.
pub(super) fn added_patch_len(
    path: &str,
    content: &[u8],
    count: usize,
    hunk: &str,
    format: &PatchFormat,
) -> usize {
    let old = quote(&format!("a/{path}"), format.quote_non_ascii);
    let new = quote(&format!("b/{path}"), format.quote_non_ascii);
    let metadata = "diff --git ".len()
        + old.len()
        + 1
        + new.len()
        + 1
        + "new file mode 100644\n".len()
        + "index ".len()
        + format.abbreviation * 2
        + "..\n".len();
    if count == 0 {
        return metadata;
    }
    let tab = usize::from(!new.starts_with('"') && path.contains(' '));
    metadata
        + "--- /dev/null\n+++ ".len()
        + new.len()
        + tab
        + 1
        + hunk.len()
        + 1
        + content.len()
        + count
        + if content.ends_with(b"\n") {
            0
        } else {
            1 + "\\ No newline at end of file\n".len()
        }
}
