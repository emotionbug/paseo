use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{ErrorKind, Write},
    path::{Component, Path, PathBuf},
};

pub(super) const MANAGED: &str = ".paseo-managed-files.json";
const LEGACY: [&str; 4] = [
    "paseo-chat",
    "paseo-epic",
    "paseo-orchestrate",
    "paseo-orchestrator",
];

#[derive(Clone)]
pub(super) struct Targets {
    pub source: PathBuf,
    pub roots: [PathBuf; 3],
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct Op {
    pub kind: String,
    pub name: String,
}

#[derive(Debug, Serialize)]
pub(super) struct Status {
    pub state: &'static str,
    pub ops: Vec<Op>,
    pub available: Vec<String>,
    pub installed: Vec<String>,
}

pub(super) fn compare_names(a: &str, b: &str) -> std::cmp::Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

pub(super) fn names(value: &Value) -> Vec<String> {
    let mut result: Vec<String> = value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|s| {
            s.trim_matches(|c: char| {
                matches!(c,
                    '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' |
                    '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' |
                    '\u{205f}' | '\u{3000}' | '\u{feff}'
                )
            })
            .to_owned()
        })
        .filter(|s| !s.is_empty())
        .collect();
    result.sort_by(|a, b| compare_names(a, b));
    result.dedup();
    result
}

pub(super) fn selection(value: &Value) -> Value {
    if value.get("mode").and_then(Value::as_str) == Some("custom") {
        json!({"mode":"custom", "skills":names(&value["skills"])})
    } else {
        json!({"mode":"all"})
    }
}

pub(super) fn available(targets: &Targets) -> Result<Vec<String>> {
    // As in the source, an unavailable bundle yields an empty catalog.
    let mut result = Vec::new();
    if let Ok(entries) = fs::read_dir(&targets.source) {
        for entry in entries {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                result.push(
                    entry
                        .file_name()
                        .into_string()
                        .map_err(|_| anyhow::anyhow!("Invalid skill name"))?,
                );
            }
        }
    }
    result.sort_by(|a, b| compare_names(a, b));
    Ok(result)
}

pub(super) fn managed(targets: &Targets) -> Result<Vec<String>> {
    Ok(managed_names(available(targets)?))
}

fn managed_names(mut result: Vec<String>) -> Vec<String> {
    result.extend(LEGACY.into_iter().map(str::to_owned));
    result.sort_by(|a, b| compare_names(a, b));
    result.dedup();
    result
}

pub(super) fn list_files(root: &Path, include_manifest: bool) -> Result<Vec<PathBuf>> {
    fn walk(
        root: &Path,
        current: &Path,
        include_manifest: bool,
        out: &mut Vec<PathBuf>,
    ) -> Result<()> {
        for entry in fs::read_dir(current)? {
            let entry = entry?;
            let path = entry.path();
            let rel = path.strip_prefix(root)?;
            if !include_manifest && rel == Path::new(MANAGED) {
                continue;
            }
            let kind = entry.file_type()?;
            if kind.is_dir() {
                walk(root, &path, include_manifest, out)?;
            } else if kind.is_file() {
                out.push(rel.to_owned());
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(root, root, include_manifest, &mut out)?;
    Ok(out)
}

pub(super) fn hash(path: &Path) -> Result<String> {
    Ok(hex::encode(Sha256::digest(fs::read(path)?)))
}

fn hash_dir(path: &Path) -> Result<Option<BTreeMap<PathBuf, String>>> {
    if !fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false) {
        return Ok(None);
    }
    let mut result = BTreeMap::new();
    for rel in list_files(path, false)? {
        result.insert(rel.clone(), hash(&path.join(rel))?);
    }
    Ok(Some(result))
}

pub(super) fn legacy_copy(targets: &Targets, name: &str) -> bool {
    let Ok(codex) = fs::canonicalize(targets.roots[2].join(name)) else {
        return true;
    };
    !targets.roots[..2]
        .iter()
        .any(|root| fs::canonicalize(root.join(name)).ok().as_ref() == Some(&codex))
}

pub(super) fn manifest(root: &Path) -> Option<Map<String, Value>> {
    let value: Value = serde_json::from_slice(&fs::read(root.join(MANAGED)).ok()?).ok()?;
    if value["version"] != 1 {
        return None;
    }
    value["files"].as_object().cloned()
}

pub(super) fn status(targets: &Targets, selection: &Value) -> Result<Status> {
    let available = available(targets)?;
    let names = managed_names(available.clone());
    let chosen: BTreeSet<&str> = selection["skills"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let all = selection["mode"] == "all";
    let mut ops = Vec::new();
    let mut installed = Vec::new();
    for name in &names {
        let desired = available.contains(name) && (all || chosen.contains(name.as_str()));
        let bundle = if available.contains(name) {
            hash_dir(&targets.source.join(name))?
        } else {
            None
        };
        let bundle = bundle.filter(|_| desired);
        let disks = targets
            .roots
            .iter()
            .map(|root| hash_dir(&root.join(name)))
            .collect::<Result<Vec<_>>>()?;
        if disks.iter().any(Option::is_some) {
            installed.push(name.clone());
        }
        let mut kind = if let Some(bundle) = bundle {
            if disks[..2].iter().any(Option::is_none) {
                Some("add")
            } else if disks[..2]
                .iter()
                .flatten()
                .any(|disk| bundle.iter().any(|(rel, sha)| disk.get(rel) != Some(sha)))
            {
                Some("update")
            } else {
                None
            }
        } else if disks[..2].iter().any(Option::is_some) {
            Some("delete")
        } else {
            None
        };
        if kind.is_none() && disks[2].is_some() && legacy_copy(targets, name) {
            if !desired {
                kind = Some("delete");
            } else if manifest(&targets.roots[2].join(name)).is_some() {
                kind = Some("update");
            }
        }
        if let Some(kind) = kind {
            ops.push(Op {
                kind: kind.into(),
                name: name.clone(),
            });
        }
    }
    let state = if installed.is_empty() {
        "not-installed"
    } else if ops.is_empty() {
        "up-to-date"
    } else {
        "drift"
    };
    Ok(Status {
        state,
        ops,
        available,
        installed,
    })
}

pub(super) fn metadata(path: &Path) -> Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(info) => Ok(Some(info)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub(super) fn safe_paths(root: &Path, paths: impl IntoIterator<Item = PathBuf>) -> Result<()> {
    for rel in paths {
        let mut normalized = PathBuf::new();
        for component in rel.components() {
            match component {
                Component::Normal(c) => normalized.push(c),
                Component::CurDir => {}
                Component::ParentDir if normalized.pop() => {}
                _ => bail!(
                    "Cannot sync managed path outside skill directory: {}",
                    rel.display()
                ),
            }
        }
        let mut current = root.to_owned();
        for segment in normalized.components() {
            current.push(segment);
            let Some(info) = metadata(&current)? else {
                break;
            };
            if info.file_type().is_symlink() {
                bail!("Cannot sync through symbolic link: {}", current.display());
            }
        }
    }
    Ok(())
}

pub(super) fn remove(path: &Path) -> Result<()> {
    let Some(info) = metadata(path)? else {
        return Ok(());
    };
    if info.is_dir() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}

pub(super) fn manifest_bytes(files: Map<String, Value>) -> Result<Vec<u8>> {
    #[derive(Serialize)]
    struct Manifest {
        version: u8,
        files: Map<String, Value>,
    }
    let mut bytes = serde_json::to_vec_pretty(&Manifest { version: 1, files })?;
    bytes.push(b'\n');
    Ok(bytes)
}

pub(super) fn expected_files(source: &Path) -> Result<BTreeMap<PathBuf, Vec<u8>>> {
    let mut files = BTreeMap::new();
    let mut hashes = Map::new();
    for rel in list_files(source, false)? {
        let bytes = fs::read(source.join(&rel))?;
        hashes.insert(
            rel.to_string_lossy().into_owned(),
            Value::String(hex::encode(Sha256::digest(&bytes))),
        );
        files.insert(rel, bytes);
    }
    files.insert(PathBuf::from(MANAGED), manifest_bytes(hashes)?);
    Ok(files)
}

fn prune(root: &Path, paths: &[PathBuf]) {
    let mut dirs = BTreeSet::new();
    for rel in paths {
        let mut parent = rel.parent();
        while let Some(p) = parent {
            if p.as_os_str().is_empty() {
                break;
            }
            dirs.insert(p.to_owned());
            parent = p.parent();
        }
    }
    let mut dirs: Vec<_> = dirs.into_iter().collect();
    dirs.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    for dir in dirs {
        let _ = fs::remove_dir(root.join(dir));
    }
}

pub(super) fn durable_write(path: &Path, bytes: &[u8]) -> Result<()> {
    fs::create_dir_all(path.parent().context("Missing parent directory")?)?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn sync_directory(source: &Path, target: &Path) -> Result<()> {
    let files = expected_files(source)?;
    safe_paths(target, [PathBuf::from(MANAGED)])?;
    let previous = manifest(target).unwrap_or_default();
    safe_paths(
        target,
        files
            .keys()
            .cloned()
            .chain(previous.keys().map(PathBuf::from)),
    )?;
    for (rel, bytes) in &files {
        if rel == Path::new(MANAGED) {
            continue;
        }
        let path = target.join(rel);
        if fs::read(&path).ok().as_deref() != Some(bytes.as_slice()) {
            durable_write(&path, bytes)?;
        }
    }
    let mut removed = Vec::new();
    for (rel, old_hash) in previous {
        if files.contains_key(Path::new(&rel)) {
            continue;
        }
        let Some(previous_hash) = old_hash.as_str() else {
            continue;
        };
        if hash(&target.join(&rel)).ok().as_deref() != Some(previous_hash) {
            continue;
        }
        match fs::remove_file(target.join(&rel)) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        removed.push(PathBuf::from(rel));
    }
    prune(target, &removed);
    let bytes = &files[Path::new(MANAGED)];
    let path = target.join(MANAGED);
    if fs::read(&path).ok().as_deref() != Some(bytes.as_slice()) {
        durable_write(&path, bytes)?;
    }
    Ok(())
}

fn retire(target: &Path) -> Result<()> {
    if !metadata(target)?.is_some_and(|m| m.is_dir()) {
        return Ok(());
    }
    let Some(previous) = manifest(target) else {
        return Ok(());
    };
    safe_paths(
        target,
        std::iter::once(PathBuf::from(MANAGED)).chain(previous.keys().map(PathBuf::from)),
    )?;
    let mut removed = Vec::new();
    for (rel, old_hash) in previous {
        let Some(previous_hash) = old_hash.as_str() else {
            continue;
        };
        if hash(&target.join(&rel)).ok().as_deref() != Some(previous_hash) {
            continue;
        }
        fs::remove_file(target.join(&rel))?;
        removed.push(PathBuf::from(rel));
    }
    prune(target, &removed);
    fs::remove_file(target.join(MANAGED))?;
    match fs::remove_dir(target) {
        Ok(()) => {}
        Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::DirectoryNotEmpty) => {}
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

pub(super) fn apply(targets: &Targets, selection: &Value, ops: &[Op]) -> Result<Status> {
    for op in ops.iter().filter(|op| op.kind != "delete") {
        let source = targets.source.join(&op.name);
        if !source.is_dir() {
            continue;
        }
        for root in &targets.roots[..2] {
            sync_directory(&source, &root.join(&op.name))?;
        }
        if legacy_copy(targets, &op.name) {
            retire(&targets.roots[2].join(&op.name))?;
        }
    }
    for op in ops.iter().filter(|op| op.kind == "delete") {
        for root in &targets.roots {
            remove(&root.join(&op.name))?;
        }
    }
    status(targets, selection)
}

pub(super) fn uninstall(targets: &Targets, selection: &Value) -> Result<Status> {
    for name in managed(targets)? {
        for root in &targets.roots {
            remove(&root.join(&name))?;
        }
    }
    status(targets, selection)
}
