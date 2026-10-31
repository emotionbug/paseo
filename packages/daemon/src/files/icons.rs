use anyhow::Result;
use base64::Engine;
use serde_json::{Value, json};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";
const PATTERNS: &[&str] = &[
    "favicon.svg",
    "favicon.png",
    "favicon-*.svg",
    "favicon-*.png",
    "favico.svg",
    "favico.png",
    "icon.svg",
    "icon.png",
    "app-icon.svg",
    "app-icon.png",
    "apple-touch-icon.png",
    "apple-touch-icon-*.png",
    "icon-*.png",
    "android-chrome-*.png",
    "safari-pinned-tab.svg",
    "mstile-*.png",
    "logo.svg",
    "logo.png",
    "favicon.ico",
    "favico.ico",
];
const PRIORITY: &[&str] = &["public", "static", "priv/static", "assets", "images", "img"];
const IGNORED: &[&str] = &[
    ".git",
    "node_modules",
    "dist",
    "build",
    ".next",
    ".nuxt",
    ".output",
    "coverage",
    ".cache",
    "vendor",
    "src",
    "lib",
    "test",
    "tests",
    "__tests__",
];
fn u16le(b: &[u8], offset: usize) -> Option<u32> {
    Some(u16::from_le_bytes(b.get(offset..offset + 2)?.try_into().ok()?) as u32)
}
fn u16be(b: &[u8], offset: usize) -> Option<u32> {
    Some(u16::from_be_bytes(b.get(offset..offset + 2)?.try_into().ok()?) as u32)
}
fn u32le(b: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        b.get(offset..offset + 4)?.try_into().ok()?,
    ))
}
fn u32be(b: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        b.get(offset..offset + 4)?.try_into().ok()?,
    ))
}
pub fn image_dimensions(bytes: &[u8], mime: &str) -> Option<(u32, u32)> {
    match mime {
        "image/png" if bytes.len() >= 24 && bytes.starts_with(b"\x89PNG") => {
            Some((u32be(bytes, 16)?, u32be(bytes, 20)?))
        }
        "image/jpeg" if bytes.len() >= 4 && bytes.starts_with(b"\xff\xd8") => {
            let mut offset = 2usize;
            while offset + 8 < bytes.len() {
                if bytes[offset] != 0xff {
                    offset += 1;
                    continue;
                }
                if (0xc0..=0xc2).contains(&bytes[offset + 1]) {
                    return Some((u16be(bytes, offset + 7)?, u16be(bytes, offset + 5)?));
                }
                offset = offset.checked_add(2 + u16be(bytes, offset + 2)? as usize)?;
            }
            None
        }
        "image/gif" if bytes.len() >= 10 && bytes.starts_with(b"GIF") => {
            Some((u16le(bytes, 6)?, u16le(bytes, 8)?))
        }
        "image/webp"
            if bytes.len() >= 30
                && bytes.starts_with(b"RIFF")
                && bytes.get(8..12) == Some(b"WEBP") =>
        {
            match bytes.get(12..16)? {
                b"VP8 " => Some((u16le(bytes, 26)? & 0x3fff, u16le(bytes, 28)? & 0x3fff)),
                b"VP8L" => {
                    let bits = u32le(bytes, 21)?;
                    Some(((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1))
                }
                _ => None,
            }
        }
        "image/x-icon" | "image/svg+xml" => Some((1, 1)),
        _ => None,
    }
}
pub fn extract_ico_png_frame(bytes: &[u8]) -> Option<&[u8]> {
    if u16le(bytes, 0)? != 0 || u16le(bytes, 2)? != 1 {
        return None;
    }
    let count = u16le(bytes, 4)?;
    let mut best = None;
    let mut width = 0;
    for index in 0..count as usize {
        let offset = 6 + index * 16;
        if offset + 16 > bytes.len() {
            break;
        }
        let candidate_width = if bytes[offset] == 0 {
            256
        } else {
            u32::from(bytes[offset])
        };
        let size = u32le(bytes, offset + 8)? as usize;
        let start = u32le(bytes, offset + 12)? as usize;
        let Some(end) = start.checked_add(size) else {
            continue;
        };
        let Some(frame) = bytes.get(start..end) else {
            continue;
        };
        if candidate_width > width && frame.starts_with(PNG) {
            best = Some(frame);
            width = candidate_width;
        }
    }
    best
}
fn sniff(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(PNG) {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.len() >= 6 && bytes.starts_with(b"GIF") {
        Some("image/gif")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    } else if u16le(bytes, 0) == Some(0)
        && u16le(bytes, 2) == Some(1)
        && u16le(bytes, 4).is_some_and(|n| n > 0)
    {
        Some("image/x-icon")
    } else {
        None
    }
}
fn extension_mime(path: &Path) -> &'static str {
    match path
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase()
        .as_str()
    {
        "ico" => "image/x-icon",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => "application/octet-stream",
    }
}
fn entries(path: &Path) -> Vec<PathBuf> {
    let mut entries: Vec<_> = fs::read_dir(path)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    entries.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
    entries
}
fn matches(name: &str, pattern: &str) -> bool {
    match pattern.split_once('*') {
        Some((start, end)) => {
            name.starts_with(start) && name.ends_with(end) && name.len() >= start.len() + end.len()
        }
        None => name == pattern,
    }
}
fn in_directory(path: &Path) -> Option<PathBuf> {
    let entries = entries(path);
    for pattern in PATTERNS {
        for entry in &entries {
            if matches(&entry.file_name()?.to_string_lossy(), pattern) && entry.is_file() {
                return Some(entry.clone());
            }
        }
    }
    None
}
fn recursive(path: &Path, depth: usize) -> Option<PathBuf> {
    if let Some(found) = in_directory(path) {
        return Some(found);
    }
    if depth == 0 {
        return None;
    }
    for entry in entries(path) {
        if IGNORED.contains(&entry.file_name()?.to_string_lossy().as_ref()) {
            continue;
        }
        if entry.is_dir() {
            if let Some(found) = recursive(&entry, depth - 1) {
                return Some(found);
            }
        }
    }
    None
}
fn priority(path: &Path) -> Option<PathBuf> {
    for directory in PRIORITY {
        let directory = path.join(directory);
        if directory.is_dir() {
            if let Some(found) = recursive(&directory, 2) {
                return Some(found);
            }
        }
    }
    None
}
fn find(path: &Path) -> Option<PathBuf> {
    if let Some(found) = priority(path) {
        return Some(found);
    }
    for directory in ["packages", "apps"] {
        for package in entries(&path.join(directory)) {
            if package.is_dir() {
                if let Some(found) = priority(&package).or_else(|| in_directory(&package)) {
                    return Some(found);
                }
            }
        }
    }
    in_directory(path)
}
pub async fn project_icon(cwd: &Path) -> Result<Option<Value>> {
    let cwd = cwd.to_owned();
    tokio::task::spawn_blocking(move||{
        let Some(path)=find(&cwd)else{return Ok(None);};
        let read=(||->std::io::Result<Option<Value>>{
            let file=fs::File::open(&path)?;if file.metadata()?.len()>32*1024{return Ok(None);}
            let mut bytes=Vec::new();file.take(32*1024+1).read_to_end(&mut bytes)?;if bytes.len()>32*1024{return Ok(None);}
            let mut mime=sniff(&bytes).unwrap_or_else(||extension_mime(&path));
            let data=if mime=="image/x-icon"{if let Some(frame)=extract_ico_png_frame(&bytes){mime="image/png";frame}else{&bytes}}else{&bytes};
            if !image_dimensions(data,mime).is_some_and(|(w,h)|w==h){return Ok(None);}
            Ok(Some(json!({"data":base64::engine::general_purpose::STANDARD.encode(data),"mimeType":mime})))
        })();
        // Discovery is an optional decoration: the source explicitly returns
        // null for unreadable or invalid candidates rather than an RPC error.
        Ok(read.unwrap_or(None))
    }).await?
}
#[cfg(test)]
mod tests {
    use super::*;
    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = vec![0; 24];
        bytes[..8].copy_from_slice(PNG);
        bytes[16..20].copy_from_slice(&width.to_be_bytes());
        bytes[20..24].copy_from_slice(&height.to_be_bytes());
        bytes
    }
    #[test]
    fn dimensions_and_ico_png() {
        let frame = png(64, 64);
        assert_eq!(image_dimensions(&frame, "image/png"), Some((64, 64)));
        let mut ico = vec![0; 22];
        ico[2] = 1;
        ico[4] = 1;
        ico[6] = 64;
        ico[7] = 64;
        ico[14..18].copy_from_slice(&(frame.len() as u32).to_le_bytes());
        ico[18..22].copy_from_slice(&22u32.to_le_bytes());
        ico.extend_from_slice(&frame);
        assert_eq!(extract_ico_png_frame(&ico), Some(frame.as_slice()));
        assert_eq!(image_dimensions(&[0; 10], "image/png"), None);
    }
    #[tokio::test]
    async fn priority_and_magic_override_extension() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("public")).unwrap();
        fs::write(dir.path().join("favicon.svg"), "<svg/>").unwrap();
        fs::write(dir.path().join("public/favicon.ico"), png(32, 32)).unwrap();
        let icon = project_icon(dir.path()).await.unwrap().unwrap();
        assert_eq!(icon["mimeType"], "image/png");
    }
}
