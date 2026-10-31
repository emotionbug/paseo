use anyhow::{Result, anyhow, bail, ensure};
use base64::Engine;
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Value, json};
use std::{
    fs::{self, File, Metadata, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
};
use tokio_util::sync::CancellationToken;

pub const EDIT_LIMIT: u64 = 1024 * 1024;
pub const CHUNK: usize = 256 * 1024;
const OUTSIDE: &str = "Access outside of workspace is not allowed";

pub struct Scoped {
    pub root: PathBuf,
    pub requested: PathBuf,
    pub resolved: PathBuf,
    pub relative: String,
}
pub struct Opened {
    pub scope: Scoped,
    pub file: File,
    pub metadata: Metadata,
    pub kind: &'static str,
    pub mime: &'static str,
}

pub fn expand(path: &str, home: &Path) -> PathBuf {
    if path == "~" {
        home.to_owned()
    } else if let Some(rest) = path.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(path)
    }
}
pub fn normalize(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            part => result.push(part.as_os_str()),
        }
    }
    result
}
pub fn scoped(root: &str, relative: &str, home: &Path) -> Result<Scoped> {
    ensure!(!root.trim().is_empty(), "cwd is required");
    let root = expand(root, home);
    let root = normalize(&if root.is_absolute() {
        root
    } else {
        std::env::current_dir()?.join(root)
    });
    let path = expand(relative, home);
    let requested = normalize(&if path.is_absolute() {
        path
    } else {
        root.join(path)
    });
    ensure!(requested.starts_with(&root), OUTSIDE);
    let canonical_root = fs::canonicalize(&root)?;
    // Validate the nearest existing ancestor too: a missing leaf below an escaping
    // symlink must not become an authorized create/write target.
    let resolved = match fs::canonicalize(&requested) {
        Ok(path) => {
            ensure!(path.starts_with(&canonical_root), OUTSIDE);
            path
        }
        Err(error) if missing(&error) => {
            let mut ancestor = requested.as_path();
            loop {
                if let Ok(path) = fs::canonicalize(ancestor) {
                    ensure!(path.starts_with(&canonical_root), OUTSIDE);
                    break;
                }
                ancestor = ancestor.parent().ok_or_else(|| anyhow!(OUTSIDE))?;
            }
            requested.clone()
        }
        Err(error) => return Err(error.into()),
    };
    let relative = relative_name(&root, &requested)?;
    Ok(Scoped {
        root,
        requested,
        resolved,
        relative,
    })
}
fn relative_name(root: &Path, path: &Path) -> Result<String> {
    let value = path
        .strip_prefix(root)?
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/");
    Ok(if value.is_empty() { ".".into() } else { value })
}
pub fn missing(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::NotFound
        || error.kind() == std::io::ErrorKind::NotADirectory
        || {
            #[cfg(unix)]
            {
                error.raw_os_error() == Some(libc::ELOOP)
            }
            #[cfg(not(unix))]
            {
                false
            }
        }
}
pub fn open_no_follow(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    Ok(options.open(path)?)
}
pub fn revision(metadata: &Metadata) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        return format!(
            "{}:{}:{}:{}",
            metadata.dev(),
            metadata.ino(),
            metadata.len(),
            i128::from(metadata.mtime()) * 1_000_000_000 + i128::from(metadata.mtime_nsec())
        );
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        return format!(
            "{}:{}:{}:{}",
            metadata.volume_serial_number().unwrap_or(0),
            metadata.file_index().unwrap_or(0),
            metadata.len(),
            (i128::from(metadata.last_write_time()) - 116444736000000000) * 100
        );
    }
    #[cfg(not(any(unix, windows)))]
    {
        format!(
            "0:0:{}:{}",
            metadata.len(),
            metadata
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        )
    }
}
pub fn modified(metadata: &Metadata) -> Result<String> {
    Ok(DateTime::<Utc>::from(metadata.modified()?).to_rfc3339_opts(SecondsFormat::Millis, true))
}
fn extension(path: &Path) -> String {
    path.extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase()
}
fn image_mime(ext: &str) -> Option<&'static str> {
    match ext {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "svg" => Some("image/svg+xml"),
        _ => None,
    }
}
fn binary_mime(ext: &str) -> Option<&'static str> {
    match ext {
        "pdf" => Some("application/pdf"),
        "zip" => Some("application/zip"),
        "gz" => Some("application/gzip"),
        "mp3" => Some("audio/mpeg"),
        "mp4" => Some("video/mp4"),
        "woff2" => Some("font/woff2"),
        _ => None,
    }
}
fn suspicious(bytes: &[u8]) -> bool {
    bytes.contains(&0)
        || bytes
            .iter()
            .filter(|&&b| (b < 32 && !matches!(b, 9 | 10 | 13)) || b == 127)
            .count() as f64
            > bytes.len() as f64 * 0.3
}
fn classify(ext: &str, bytes: &[u8], prefix: bool) -> (&'static str, &'static str) {
    if let Some(mime) = image_mime(ext) {
        return ("image", mime);
    }
    let utf8 = match std::str::from_utf8(bytes) {
        Ok(_) => true,
        Err(error) => prefix && error.error_len().is_none(),
    };
    if binary_mime(ext).is_some() || suspicious(bytes) || !utf8 {
        (
            "binary",
            binary_mime(ext).unwrap_or("application/octet-stream"),
        )
    } else {
        (
            "text",
            if ext == "json" {
                "application/json"
            } else {
                "text/plain"
            },
        )
    }
}
pub fn open(root: &str, path: &str, home: &Path) -> Result<Opened> {
    let scope = scoped(root, path, home)?;
    let mut file = open_no_follow(&scope.resolved)?;
    let metadata = file.metadata()?;
    ensure!(metadata.is_file(), "Requested path is not a file");
    let mut sample = [0u8; 8192];
    let count = file.read(&mut sample)?;
    file.rewind()?;
    let (kind, mime) = classify(&extension(&scope.resolved), &sample[..count], true);
    Ok(Opened {
        scope,
        file,
        metadata,
        kind,
        mime,
    })
}
pub fn version(root: &str, path: &str, home: &Path) -> Value {
    match open(root, path, home) {
        Ok(file) => {
            json!({"status":"ready","cwd":expand(root,home),"path":file.scope.relative,"size":file.metadata.len(),"modifiedAt":modified(&file.metadata).unwrap_or_default(),"revision":revision(&file.metadata),"kind":file.kind,"mimeType":file.mime})
        }
        Err(error) if error.downcast_ref::<std::io::Error>().is_some_and(missing) => {
            json!({"status":"missing","cwd":expand(root,home),"path":path})
        }
        Err(error) => {
            json!({"status":"error","cwd":expand(root,home),"path":path,"error":error.to_string()})
        }
    }
}
pub fn list(root: &str, path: &str, home: &Path, cancel: &CancellationToken) -> Result<Value> {
    let scope = scoped(root, path, home)?;
    ensure!(
        fs::metadata(&scope.resolved)?.is_dir(),
        "Requested path is not a directory"
    );
    let mut entries = Vec::new();
    for entry in fs::read_dir(&scope.resolved)? {
        ensure!(!cancel.is_cancelled(), "File request cancelled");
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let requested = scope.requested.join(&name);
        let result = (|| -> Result<Value> {
            let child = scoped(root, &requested.to_string_lossy(), home)?;
            let metadata = fs::metadata(child.resolved)?;
            Ok(
                json!({"name":name,"path":child.relative,"kind":if metadata.is_dir(){"directory"}else{"file"},"size":metadata.len(),"modifiedAt":modified(&metadata)?}),
            )
        })();
        match result {
            Ok(entry) => entries.push(entry),
            Err(error)
                if error.to_string() == OUTSIDE
                    || error.downcast_ref::<std::io::Error>().is_some_and(|e| {
                        missing(e) || e.kind() == std::io::ErrorKind::PermissionDenied
                    }) => {}
            Err(error) => return Err(error),
        }
    }
    entries.sort_by(|a, b| {
        b["modifiedAt"]
            .as_str()
            .cmp(&a["modifiedAt"].as_str())
            .then_with(|| {
                crate::json::locale_compare(
                    a["name"].as_str().unwrap_or(""),
                    b["name"].as_str().unwrap_or(""),
                )
            })
    });
    Ok(json!({"path":scope.relative,"entries":entries}))
}
pub fn read(
    root: &str,
    path: &str,
    home: &Path,
    max: Option<u64>,
    cancel: &CancellationToken,
) -> Result<Value> {
    let mut file = open(root, path, home)?;
    if let Some(max) = max.filter(|m| *m > 0) {
        ensure!(file.metadata.len() <= max, "File is too large to display");
    }
    let mut bytes = Vec::new();
    let mut block = vec![0; CHUNK];
    loop {
        ensure!(!cancel.is_cancelled(), "File request cancelled");
        let n = file.file.read(&mut block)?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&block[..n]);
    }
    ensure!(
        revision(&file.file.metadata()?) == revision(&file.metadata),
        "File changed during transfer"
    );
    let (kind, mime) = classify(&extension(&file.scope.resolved), &bytes, false);
    let mut result = json!({"path":file.scope.relative,"kind":kind,"encoding":match kind {"image"=>"base64","binary"=>"none",_=>"utf-8"},"mimeType":mime,"size":file.metadata.len(),"modifiedAt":modified(&file.metadata)?,"revision":revision(&file.metadata)});
    if kind == "image" {
        result["content"] = Value::String(base64::engine::general_purpose::STANDARD.encode(bytes));
    } else if kind == "text" {
        result["content"] = Value::String(String::from_utf8(bytes)?);
    }
    Ok(result)
}
/// Classify the full stream without allocating the whole file. UTF-8 tails span chunks.
pub fn stream_open(
    root: &str,
    path: &str,
    home: &Path,
    max: Option<u64>,
    cancel: &CancellationToken,
) -> Result<Opened> {
    let mut file = open(root, path, home)?;
    if let Some(max) = max.filter(|m| *m > 0) {
        ensure!(file.metadata.len() <= max, "File is too large to display");
    }
    let ext = extension(&file.scope.resolved);
    if image_mime(&ext).is_none() && binary_mime(&ext).is_none() {
        let mut block = vec![0; CHUNK + 4];
        let mut tail = 0;
        let mut total = 0u64;
        let mut controls = 0u64;
        let mut binary = false;
        while total < file.metadata.len() {
            ensure!(!cancel.is_cancelled(), "File request cancelled");
            let length = (file.metadata.len() - total).min(CHUNK as u64) as usize;
            let n = file.file.read(&mut block[tail..tail + length])?;
            ensure!(n > 0, "File changed during transfer");
            for &b in &block[tail..tail + n] {
                if b == 0 {
                    binary = true;
                }
                if (b < 32 && !matches!(b, 9 | 10 | 13)) || b == 127 {
                    controls += 1;
                }
            }
            total += n as u64;
            let used = tail + n;
            match std::str::from_utf8(&block[..used]) {
                Ok(_) => tail = 0,
                Err(e) => {
                    if e.error_len().is_some() {
                        binary = true;
                        tail = 0;
                    } else {
                        tail = used - e.valid_up_to();
                        block.copy_within(e.valid_up_to()..used, 0);
                    }
                }
            }
        }
        binary |= tail > 0 || controls as f64 > total as f64 * 0.3;
        file.kind = if binary { "binary" } else { "text" };
        file.mime = if binary {
            "application/octet-stream"
        } else if ext == "json" {
            "application/json"
        } else {
            "text/plain"
        };
        file.file.seek(SeekFrom::Start(0))?;
    }
    ensure!(
        revision(&file.file.metadata()?) == revision(&file.metadata),
        "File changed during transfer"
    );
    Ok(file)
}
fn matches(metadata: &Metadata, request: &Value) -> bool {
    if let Some(rev) = request["expectedRevision"]
        .as_str()
        .filter(|s| !s.is_empty())
    {
        revision(metadata) == rev
    } else {
        modified(metadata).ok().as_deref() == request["expectedModifiedAt"].as_str()
    }
}
fn decode_base64(content: &str) -> Result<Vec<u8>> {
    let mut clean = String::new();
    for c in content
        .chars()
        .filter_map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '+' | '/' => Some(c),
            '-' => Some('+'),
            '_' => Some('/'),
            '=' => Some('='),
            _ => None,
        })
        .take_while(|c| *c != '=')
    {
        ensure!(
            clean.len() < ((EDIT_LIMIT as usize + 2) / 3) * 4,
            "File is too large to edit"
        );
        clean.push(c);
    }
    let usable = if clean.len() % 4 == 1 {
        &clean[..clean.len() - 1]
    } else {
        &clean
    };
    Ok(base64::engine::general_purpose::STANDARD.decode(format!(
        "{}{}",
        usable,
        "=".repeat((4 - usable.len() % 4) % 4)
    ))?)
}
pub fn write(request: &Value, home: &Path, cancel: &CancellationToken) -> Value {
    let result = (|| -> Result<Value> {
        let root = field(request, "cwd")?;
        let path = field(request, "path")?;
        let content = field(request, "content")?;
        let base64 = request["contentEncoding"] == "base64";
        ensure!(
            base64 || content.len() as u64 <= EDIT_LIMIT,
            "File is too large to edit"
        );
        let bytes = if base64 {
            decode_base64(content)?
        } else {
            content.as_bytes().to_vec()
        };
        ensure!(
            bytes.len() as u64 <= EDIT_LIMIT,
            "File is too large to edit"
        );
        let scope = scoped(root, path, home)?;
        let conflict = || json!({"status":"conflict","version":version(root,path,home)});
        let mut current = match open_no_follow(&scope.resolved) {
            Ok(file) => file,
            Err(error) if error.downcast_ref::<std::io::Error>().is_some_and(missing) => {
                return Ok(conflict());
            }
            Err(error) => return Err(error),
        };
        let metadata = current.metadata()?;
        ensure!(metadata.is_file(), "Requested path is not a file");
        ensure!(metadata.len() <= EDIT_LIMIT, "File is too large to edit");
        if !base64 {
            let mut old = Vec::new();
            current.read_to_end(&mut old)?;
            ensure!(
                !suspicious(&old) && std::str::from_utf8(&old).is_ok(),
                "Binary files cannot be edited"
            );
        }
        if !matches(&metadata, request) {
            return Ok(conflict());
        }
        let parent = scope.resolved.parent().ok_or_else(|| anyhow!(OUTSIDE))?;
        let mut temporary = tempfile::Builder::new()
            .prefix(".paseo-")
            .suffix(".tmp")
            .tempfile_in(parent)?;
        temporary
            .as_file()
            .set_permissions(metadata.permissions())?;
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        ensure!(!cancel.is_cancelled(), "File request cancelled");
        let latest_scope = scoped(root, path, home)?;
        if latest_scope.resolved != scope.resolved {
            return Ok(conflict());
        }
        let latest = match open_no_follow(&scope.resolved) {
            Ok(file) => file.metadata()?,
            Err(error) if error.downcast_ref::<std::io::Error>().is_some_and(missing) => {
                return Ok(conflict());
            }
            Err(error) => return Err(error),
        };
        if !matches(&latest, request) || revision(&latest) != revision(&metadata) {
            return Ok(conflict());
        }
        // Like the source daemon, this is compare/recheck + atomic rename, not an
        // OS-wide compare-and-swap lock against unrelated writers.
        temporary
            .persist(&scope.resolved)
            .map_err(|error| error.error)?;
        let final_metadata = fs::metadata(&scope.resolved)?;
        Ok(
            json!({"status":"written","modifiedAt":modified(&final_metadata)?,"size":final_metadata.len(),"revision":revision(&final_metadata)}),
        )
    })();
    result.unwrap_or_else(|error| json!({"status":"error","error":error.to_string()}))
}
pub fn field<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .ok_or_else(|| anyhow!("{key} is required"))
}
fn name(value: &str) -> Result<&str> {
    let value = value.trim();
    ensure!(
        !value.is_empty() && value != "." && value != "..",
        "Invalid name"
    );
    ensure!(
        !value.contains(['/', '\\']),
        "Name cannot contain path separators"
    );
    Ok(value)
}
fn copy_exclusive(source: &Path, target: &Path, cancel: &CancellationToken) -> Result<()> {
    ensure!(!cancel.is_cancelled(), "File request cancelled");
    let metadata = fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() {
        let link = fs::read_link(source)?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(link, target)?;
        #[cfg(windows)]
        {
            if fs::metadata(source)?.is_dir() {
                std::os::windows::fs::symlink_dir(link, target)?;
            } else {
                std::os::windows::fs::symlink_file(link, target)?;
            }
        }
    } else if metadata.is_dir() {
        fs::create_dir(target)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_exclusive(&entry.path(), &target.join(entry.file_name()), cancel)?;
        }
        fs::set_permissions(target, metadata.permissions())?;
        let file = File::open(target)?;
        file.set_times(
            std::fs::FileTimes::new()
                .set_accessed(metadata.accessed()?)
                .set_modified(metadata.modified()?),
        )?;
    } else {
        let mut input = open_no_follow(source)?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(target)?;
        let mut buffer = vec![0; CHUNK];
        loop {
            ensure!(!cancel.is_cancelled(), "File request cancelled");
            let n = input.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            output.write_all(&buffer[..n])?;
        }
        output.set_permissions(metadata.permissions())?;
        output.set_times(
            std::fs::FileTimes::new()
                .set_accessed(metadata.accessed()?)
                .set_modified(metadata.modified()?),
        )?;
    }
    Ok(())
}
fn remove(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}
/// Maps filesystem races the way the source explorer does: an entry that already
/// exists names the requested entry, and a vanished entry is reported uniformly.
pub fn mutate(request: &Value, home: &Path, cancel: &CancellationToken) -> Result<String> {
    let operation = request["type"].as_str().unwrap_or("");
    mutate_entry(request, home, cancel).map_err(|error| {
        let Some(io) = error.downcast_ref::<std::io::Error>() else {
            return error;
        };
        let names_entry = matches!(
            operation,
            "fs.entry.create.request" | "fs.entry.rename.request"
        );
        if names_entry && io.kind() == std::io::ErrorKind::AlreadyExists {
            let name = request["name"].as_str().unwrap_or("").trim();
            return anyhow!("\"{name}\" already exists");
        }
        if operation != "fs.entry.create.request" && missing(io) {
            return anyhow!("File or folder no longer exists");
        }
        error
    })
}
fn mutate_entry(request: &Value, home: &Path, cancel: &CancellationToken) -> Result<String> {
    ensure!(!cancel.is_cancelled(), "File request cancelled");
    let root = field(request, "cwd")?;
    let operation = field(request, "type")?;
    if operation == "fs.entry.create.request" {
        let name = name(field(request, "name")?)?;
        let parent = scoped(root, field(request, "parentPath")?, home)?;
        ensure!(
            fs::metadata(&parent.resolved)?.is_dir(),
            "Parent path is not a directory"
        );
        let target = parent.resolved.join(name);
        let result = if request["kind"] == "directory" {
            fs::create_dir(target)
        } else {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(target)
                .map(|_| ())
        };
        if let Err(error) = result {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                bail!("\"{name}\" already exists");
            }
            return Err(error.into());
        }
        return relative_name(&parent.root, &parent.requested.join(name));
    }
    let scope = scoped(root, field(request, "path")?, home)?;
    let verb = if operation == "fs.entry.delete.request" {
        "delete"
    } else if operation == "fs.entry.rename.request" {
        "rename"
    } else {
        "duplicate"
    };
    ensure!(
        scope.resolved != fs::canonicalize(&scope.root)?,
        "Cannot {verb} the workspace root"
    );
    let metadata = fs::symlink_metadata(&scope.requested).map_err(|error| {
        if missing(&error) {
            anyhow!("File or folder no longer exists")
        } else {
            error.into()
        }
    })?;
    if verb == "delete" {
        remove(&scope.requested)?;
        return Ok(scope.relative);
    }
    let parent = scope.requested.parent().ok_or_else(|| anyhow!(OUTSIDE))?;
    if verb == "duplicate" {
        let original = scope.requested.file_name().unwrap().to_string_lossy();
        let extension = if metadata.is_dir() {
            None
        } else {
            scope.requested.extension().and_then(|s| s.to_str())
        };
        let (stem, extension) = if let Some(ext) = extension {
            (
                &original[..original.len() - ext.len() - 1],
                format!(".{ext}"),
            )
        } else {
            (original.as_ref(), String::new())
        };
        for number in 1u64.. {
            let suffix = if number == 1 {
                " copy".into()
            } else {
                format!(" copy {number}")
            };
            let target = parent.join(format!("{stem}{suffix}{extension}"));
            match fs::symlink_metadata(&target) {
                Ok(_) => continue,
                Err(e) if missing(&e) => {}
                Err(e) => return Err(e.into()),
            }
            copy_exclusive(&scope.requested, &target, cancel)?;
            return relative_name(&scope.root, &target);
        }
        unreachable!()
    }
    let name = name(field(request, "name")?)?;
    let target = parent.join(name);
    let relative = relative_name(&scope.root, &target)?;
    if target == scope.requested {
        return Ok(relative);
    }
    let mut case_only = false;
    if let Ok(target_metadata) = fs::symlink_metadata(&target) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            case_only = metadata.dev() == target_metadata.dev()
                && metadata.ino() == target_metadata.ino()
                && target.to_string_lossy().to_lowercase()
                    == scope.requested.to_string_lossy().to_lowercase();
        }
        #[cfg(not(unix))]
        {
            let _ = target_metadata;
            case_only = fs::canonicalize(&target)? == scope.resolved
                && target.to_string_lossy().to_lowercase()
                    == scope.requested.to_string_lossy().to_lowercase();
        }
        ensure!(case_only, "\"{name}\" already exists");
    }
    // The native process supervisor supplies bounded pipes, cancellation and
    // process-tree teardown; this filesystem transaction runs on a blocking worker.
    let repository = run_git(
        &scope.root,
        vec!["rev-parse".into(), "--is-inside-work-tree".into()],
        cancel,
    )?;
    ensure!(
        matches!(repository.status, 0 | 128),
        "{}",
        repository.stderr
    );
    let tracked = if repository.status == 0 {
        let output = run_git(
            &scope.root,
            vec![
                "ls-files".into(),
                "-z".into(),
                "--".into(),
                format!(":(literal){}", scope.relative),
            ],
            cancel,
        )?;
        ensure!(output.status == 0, "{}", output.stderr);
        !output.stdout.is_empty()
    } else {
        false
    };
    ensure!(!cancel.is_cancelled(), "File request cancelled");
    if tracked {
        let output = run_git(
            &scope.root,
            vec![
                "mv".into(),
                "--".into(),
                scope.relative.clone(),
                relative.clone(),
            ],
            cancel,
        )?;
        ensure!(output.status == 0, "{}", output.stderr);
    } else if case_only {
        fs::rename(&scope.requested, &target)?;
    } else if metadata.is_file() {
        fs::hard_link(&scope.requested, &target)?;
        fs::remove_file(&scope.requested)?;
    } else {
        copy_exclusive(&scope.requested, &target, cancel)?;
        remove(&scope.requested)?;
    }
    Ok(relative)
}

fn run_git(
    cwd: &Path,
    args: Vec<String>,
    cancel: &CancellationToken,
) -> Result<crate::process::CommandOutput> {
    let spec = crate::process::CommandSpec {
        program: "git".into(),
        args,
        cwd: Some(cwd.to_owned()),
        env: std::collections::BTreeMap::from([("GIT_LITERAL_PATHSPECS".into(), "0".into())]),
        clear_env: false,
    };
    tokio::runtime::Handle::current().block_on(crate::process::run(
        spec,
        20 * 1024 * 1024,
        std::time::Duration::from_secs(30),
        cancel.clone(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stale_revision_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.md");
        fs::write(&path, "old").unwrap();
        let cwd = dir.path().to_str().unwrap();
        let initial = version(cwd, "file.md", dir.path());
        fs::write(&path, "external").unwrap();
        let result = write(
            &json!({"cwd":cwd,"path":"file.md","content":"new","expectedRevision":initial["revision"],"expectedModifiedAt":initial["modifiedAt"]}),
            dir.path(),
            &CancellationToken::new(),
        );
        assert_eq!(result["status"], "conflict");
        assert_eq!(fs::read_to_string(path).unwrap(), "external");
    }
    #[test]
    fn revision_wins_and_markdown_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("file.md"), "# old").unwrap();
        let cwd = dir.path().to_str().unwrap();
        let initial = version(cwd, "file.md", dir.path());
        let result = write(
            &json!({"cwd":cwd,"path":"file.md","content":"# 文書\n","expectedRevision":initial["revision"],"expectedModifiedAt":"wrong"}),
            dir.path(),
            &CancellationToken::new(),
        );
        assert_eq!(result["status"], "written");
        let file = read(cwd, "file.md", dir.path(), None, &CancellationToken::new()).unwrap();
        assert_eq!(file["content"], "# 文書\n");
        assert_eq!(file["encoding"], "utf-8");
    }
    #[test]
    fn limits_and_missing_are_not_success() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().to_str().unwrap();
        let result = write(
            &json!({"cwd":cwd,"path":"missing","content":"text","expectedModifiedAt":""}),
            dir.path(),
            &CancellationToken::new(),
        );
        assert_eq!(result["status"], "conflict");
        fs::write(
            dir.path().join("large"),
            vec![b'a'; EDIT_LIMIT as usize + 1],
        )
        .unwrap();
        assert!(read(cwd, "large", dir.path(), Some(8), &CancellationToken::new()).is_err());
        let result = write(
            &json!({"cwd":cwd,"path":"large","content":"text","expectedModifiedAt":""}),
            dir.path(),
            &CancellationToken::new(),
        );
        assert_eq!(result["error"], "File is too large to edit");
    }
    #[cfg(unix)]
    #[test]
    fn symlinks_cannot_escape_and_delete_does_not_follow() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret"), "secret").unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).unwrap();
        let cwd = dir.path().to_str().unwrap();
        assert!(scoped(cwd, "escape/secret", dir.path()).is_err());
        assert!(scoped(cwd, "escape/missing", dir.path()).is_err());
        assert!(scoped(cwd, "../secret", dir.path()).is_err());
        fs::write(dir.path().join("target"), "safe").unwrap();
        std::os::unix::fs::symlink("target", dir.path().join("link")).unwrap();
        mutate(
            &json!({"type":"fs.entry.delete.request","cwd":cwd,"path":"link"}),
            dir.path(),
            &CancellationToken::new(),
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("target")).unwrap(),
            "safe"
        );
    }
    #[test]
    fn classification_checks_beyond_sample_and_utf8_chunk_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().to_str().unwrap();
        let cancel = CancellationToken::new();
        let mut bytes = vec![b'a'; CHUNK - 1];
        bytes.extend_from_slice("文".as_bytes());
        bytes.push(b'\n');
        fs::write(dir.path().join("text"), &bytes).unwrap();
        assert_eq!(
            stream_open(cwd, "text", dir.path(), None, &cancel)
                .unwrap()
                .kind,
            "text"
        );
        bytes.push(0xff);
        fs::write(dir.path().join("binary"), &bytes).unwrap();
        assert_eq!(
            stream_open(cwd, "binary", dir.path(), None, &cancel)
                .unwrap()
                .kind,
            "binary"
        );
        cancel.cancel();
        assert!(read(cwd, "text", dir.path(), None, &cancel).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn readonly_file_is_replaced_atomically_with_its_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file");
        let cwd = dir.path().to_str().unwrap();
        fs::write(&path, "old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o440)).unwrap();
        let initial = version(cwd, "file", dir.path());
        let request = json!({"cwd":cwd,"path":"file","content":"new","expectedRevision":initial["revision"],"expectedModifiedAt":initial["modifiedAt"]});
        // The source daemon writes a temporary file with the current mode and renames
        // it over the original, so a read-only mode does not block the edit.
        assert_eq!(
            write(&request, dir.path(), &CancellationToken::new())["status"],
            "written"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "new");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o440
        );
    }
    #[test]
    fn existing_target_and_vanished_source_use_source_errors() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().to_str().unwrap();
        fs::write(dir.path().join("a.txt"), "a").unwrap();
        let missing = mutate(
            &json!({"type":"fs.entry.duplicate.request","cwd":cwd,"path":"gone.txt"}),
            dir.path(),
            &CancellationToken::new(),
        )
        .unwrap_err();
        assert_eq!(missing.to_string(), "File or folder no longer exists");
        let exists = mutate(
            &json!({"type":"fs.entry.create.request","cwd":cwd,"parentPath":".","name":" a.txt ","kind":"file"}),
            dir.path(),
            &CancellationToken::new(),
        )
        .unwrap_err();
        assert_eq!(exists.to_string(), "\"a.txt\" already exists");
    }
}
