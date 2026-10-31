use super::files::{self, Op, Targets};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

const OWNER: &str = "paseo-skills-transaction";
const PREFIX: &str = ".paseo-skills-transaction-";
const RECOVERED: &str = ".paseo-skills-recovered-";
const JOURNAL: &str = "transaction.json";

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    live_path: PathBuf,
    kind: String,
    backup_path: Option<PathBuf>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    owner: String,
    version: u8,
    phase: String,
    previous_selection: Value,
    next_selection: Value,
    entries: Vec<Entry>,
}

pub(super) struct Transaction {
    directory: PathBuf,
    manifest: Manifest,
}

fn is_directory(path: &Path) -> bool {
    fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false)
}
fn backup_path(transaction: &Path, backup: &Option<PathBuf>) -> Option<PathBuf> {
    backup.as_ref().map(|p| {
        if p.is_absolute() {
            p.clone()
        } else {
            transaction.join(p)
        }
    })
}

fn write_manifest(directory: &Path, manifest: &Manifest) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(manifest)?;
    bytes.push(b'\n');
    let temporary = directory.join("transaction.json.tmp");
    files::durable_write(&temporary, &bytes)?;
    fs::rename(temporary, directory.join(JOURNAL))?;
    sync_dir(directory)?;
    Ok(())
}

fn sync_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    fs::File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn copy_entry(source: &Path, destination: &Path) -> Result<()> {
    let info = fs::symlink_metadata(source)?;
    fs::create_dir_all(destination.parent().context("Missing backup parent")?)?;
    if info.file_type().is_symlink() {
        let link = fs::read_link(source)?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(link, destination)?;
        #[cfg(windows)]
        if is_directory(source) {
            std::os::windows::fs::symlink_dir(link, destination)?;
        } else {
            std::os::windows::fs::symlink_file(link, destination)?;
        }
        #[cfg(not(any(unix, windows)))]
        bail!("Skill backup symlinks are unsupported on this platform");
    } else if info.is_dir() {
        fs::create_dir_all(destination)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_entry(&entry.path(), &destination.join(entry.file_name()))?;
        }
        fs::set_permissions(destination, info.permissions())?;
        preserve_times(destination, &info)?;
        sync_dir(destination)?;
    } else if info.is_file() {
        fs::copy(source, destination)?;
        preserve_times(destination, &info)?;
        fs::File::open(destination)?.sync_all()?;
    } else {
        bail!("Cannot capture special skill file: {}", source.display());
    }
    Ok(())
}

fn preserve_times(path: &Path, info: &fs::Metadata) -> Result<()> {
    let mut times = fs::FileTimes::new();
    if let Ok(accessed) = info.accessed() {
        times = times.set_accessed(accessed);
    }
    if let Ok(modified) = info.modified() {
        times = times.set_modified(modified);
    }
    #[cfg(unix)]
    fs::File::open(path)?.set_times(times)?;
    #[cfg(windows)]
    if info.is_file() {
        fs::OpenOptions::new()
            .write(true)
            .open(path)?
            .set_times(times)?;
    }
    Ok(())
}

fn modes_equal(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        a.permissions().mode() & 0o777 == b.permissions().mode() & 0o777
    }
    #[cfg(not(unix))]
    {
        a.permissions().readonly() == b.permissions().readonly()
    }
}

fn merge_missing(source: &Path, destination: &Path) -> Result<bool> {
    let mut conflict = false;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source = entry.path();
        let target = destination.join(entry.file_name());
        let source_info = fs::symlink_metadata(&source)?;
        let Some(target_info) = files::metadata(&target)? else {
            copy_entry(&source, &target)?;
            continue;
        };
        if source_info.is_dir() && target_info.is_dir() {
            if !modes_equal(&source_info, &target_info) {
                conflict = true;
            }
            conflict |= merge_missing(&source, &target)?;
        } else if source_info.is_file() && target_info.is_file() {
            if fs::read(&source)? != fs::read(&target)? || !modes_equal(&source_info, &target_info)
            {
                conflict = true;
            }
        } else if source_info.file_type().is_symlink() && target_info.file_type().is_symlink() {
            if fs::read_link(&source)? != fs::read_link(&target)? {
                conflict = true;
            }
        } else {
            conflict = true;
        }
    }
    Ok(conflict)
}

fn merge_backup(live: &Path, backup: Option<&Path>) -> Result<bool> {
    let Some(backup) = backup else {
        return Ok(false);
    };
    let destination = if files::metadata(live)?.is_some_and(|m| m.file_type().is_symlink()) {
        fs::canonicalize(live)?
    } else {
        live.to_owned()
    };
    fs::create_dir_all(&destination)?;
    merge_missing(backup, &destination)
}

fn recovery_base(transaction: &Path, live: &Path, backup: &Path) -> Result<PathBuf> {
    let transaction_id = transaction
        .file_name()
        .and_then(|s| s.to_str())
        .context("Invalid transaction path")?
        .strip_prefix(PREFIX)
        .context("Invalid transaction prefix")?;
    let parent = if backup.starts_with(transaction) {
        transaction.parent()
    } else {
        live.parent()
    }
    .context("Missing recovery parent")?;
    Ok(parent.join(format!(
        "{RECOVERED}{}-{transaction_id}",
        live.file_name()
            .context("Missing skill name")?
            .to_string_lossy()
    )))
}

fn quarantine(transaction: &Path, live: &Path, backup: &Path) -> Result<()> {
    let base = recovery_base(transaction, live, backup)?;
    let mut destination = base.clone();
    let mut suffix = 1;
    while files::metadata(&destination)?.is_some() {
        destination = PathBuf::from(format!("{}-{suffix}", base.display()));
        suffix += 1;
    }
    fs::rename(backup, &destination)?;
    sync_dir(destination.parent().context("Missing quarantine parent")?)?;
    Ok(())
}

fn find_quarantine(
    transaction: &Path,
    live: &Path,
    backup: &Path,
    claimed: &BTreeSet<PathBuf>,
) -> Result<Option<PathBuf>> {
    let base = recovery_base(transaction, live, backup)?;
    let name = base
        .file_name()
        .context("Missing recovery name")?
        .to_string_lossy();
    let mut matches = Vec::new();
    if let Ok(entries) = fs::read_dir(base.parent().context("Missing recovery parent")?) {
        for entry in entries {
            let entry = entry?;
            let entry_name = entry.file_name();
            let entry_name = entry_name.to_string_lossy();
            if (entry_name == name || entry_name.starts_with(&format!("{name}-")))
                && !claimed.contains(&entry.path())
            {
                matches.push(entry.path());
            }
        }
    }
    matches.sort();
    Ok(matches.into_iter().next())
}

fn restore_deleted(transaction: &Path, live: &Path, backup: Option<&Path>) -> Result<()> {
    let Some(backup) = backup else {
        return Ok(());
    };
    if files::metadata(live)?.is_none() {
        fs::create_dir_all(live.parent().context("Missing live parent")?)?;
        fs::rename(backup, live)?;
        sync_dir(live.parent().context("Missing live parent")?)?;
    } else if fs::symlink_metadata(backup)?.is_dir() && is_directory(live) {
        if merge_backup(live, Some(backup))? {
            quarantine(transaction, live, backup)?;
        }
    } else {
        quarantine(transaction, live, backup)?;
    }
    Ok(())
}

fn restore_entry(live: &Path, backup: Option<&Path>, rel: &Path) -> Result<()> {
    let target = live.join(rel);
    files::safe_paths(live, [rel.to_owned()])?;
    files::remove(&target)?;
    if let Some(source) = backup.map(|p| p.join(rel)) {
        if files::metadata(&source)?.is_some() {
            copy_entry(&source, &target)?;
        }
    }
    Ok(())
}

fn restore(targets: &Targets, transaction: &Path, manifest: &Manifest) -> Result<()> {
    if manifest.phase == "capturing" {
        return Ok(());
    }
    let mut expected_by_name = std::collections::BTreeMap::new();
    let mut claimed = BTreeSet::new();
    for entry in &manifest.entries {
        let backup = backup_path(transaction, &entry.backup_path);
        if let Some(backup) = backup.as_deref() {
            if files::metadata(backup)?.is_none() {
                if entry.kind == "delete" {
                    continue;
                }
                if let Some(path) =
                    find_quarantine(transaction, &entry.live_path, backup, &claimed)?
                {
                    claimed.insert(path);
                    continue;
                }
                bail!("Skills transaction backup is missing: {}", backup.display());
            }
        }
        if entry.kind == "delete" {
            restore_deleted(transaction, &entry.live_path, backup.as_deref())?;
            continue;
        }
        let live_info = files::metadata(&entry.live_path)?;
        if live_info.is_some() && !is_directory(&entry.live_path) {
            if let Some(backup) = backup.as_deref() {
                quarantine(transaction, &entry.live_path, backup)?;
            }
            continue;
        }
        if let Some(backup) = backup.as_deref() {
            if !fs::symlink_metadata(backup)?.is_dir() {
                if live_info.is_some() && is_directory(&entry.live_path) {
                    quarantine(transaction, &entry.live_path, backup)?;
                } else {
                    restore_entry(&entry.live_path, Some(backup), Path::new("."))?;
                }
                continue;
            }
        }
        let name = entry
            .live_path
            .file_name()
            .context("Missing captured skill name")?
            .to_owned();
        if !expected_by_name.contains_key(&name) {
            expected_by_name.insert(
                name.clone(),
                files::expected_files(&targets.source.join(&name))?,
            );
        }
        let expected = &expected_by_name[&name];
        let live_files = if is_directory(&entry.live_path) {
            files::list_files(&entry.live_path, true)?
        } else {
            Vec::new()
        };
        for rel in live_files {
            let Some(bytes) = expected.get(&rel) else {
                continue;
            };
            if fs::read(entry.live_path.join(&rel)).ok().as_deref() == Some(bytes.as_slice()) {
                restore_entry(&entry.live_path, backup.as_deref(), &rel)?;
            }
        }
        if merge_backup(&entry.live_path, backup.as_deref())? {
            if let Some(backup) = backup.as_deref() {
                quarantine(transaction, &entry.live_path, backup)?;
            }
        }
        if fs::read_dir(&entry.live_path)
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(false)
        {
            files::remove(&entry.live_path)?;
        }
    }
    Ok(())
}

fn discard(directory: &Path, entries: &[Entry]) -> Result<()> {
    let stages: BTreeSet<PathBuf> = entries
        .iter()
        .filter_map(|e| e.backup_path.as_ref())
        .filter(|p| p.is_absolute())
        .filter_map(|p| p.parent().map(Path::to_owned))
        .collect();
    for stage in stages {
        files::remove(&stage)?;
    }
    files::remove(directory)?;
    if let Some(parent) = directory.parent() {
        sync_dir(parent)?;
    }
    Ok(())
}

fn valid_selection(value: &Value) -> bool {
    value["mode"] == "all"
        || (value["mode"] == "custom"
            && value["skills"]
                .as_array()
                .is_some_and(|v| v.iter().all(Value::is_string)))
}

fn valid_entries(targets: &Targets, transaction: &Path, manifest: &Manifest) -> Result<bool> {
    let names = files::managed(targets)?;
    for entry in &manifest.entries {
        let Some(index) = targets
            .roots
            .iter()
            .position(|root| Some(root.as_path()) == entry.live_path.parent())
        else {
            return Ok(false);
        };
        let Some(name) = entry.live_path.file_name().and_then(|s| s.to_str()) else {
            return Ok(false);
        };
        if !names.iter().any(|n| n == name)
            || !matches!(entry.kind.as_str(), "add" | "update" | "delete")
        {
            return Ok(false);
        }
        if let Some(backup) = &entry.backup_path {
            let expected = PathBuf::from("backup").join(index.to_string()).join(name);
            let stage = targets.roots[index]
                .join(
                    transaction
                        .file_name()
                        .context("Missing transaction name")?,
                )
                .join(name);
            if !(backup == &expected || (entry.kind == "delete" && backup == &stage)) {
                return Ok(false);
            }
            // The captured entry itself can be a symlink. Its parent cannot be:
            // recovery must not traverse a forged backup/staging directory.
            let resolved =
                backup_path(transaction, &entry.backup_path).context("Missing backup")?;
            let base = if backup.is_absolute() {
                &targets.roots[index]
            } else {
                transaction
            };
            let parent = resolved.parent().context("Missing backup parent")?;
            let rel = parent.strip_prefix(base)?;
            if files::safe_paths(base, [rel.to_owned()]).is_err() {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

pub(super) fn recover(targets: &Targets, committed: &Value) -> Result<()> {
    let mut parents = Vec::new();
    for root in &targets.roots {
        if let Some(parent) = root.parent() {
            if !parents.contains(&parent) {
                parents.push(parent);
            }
        }
    }
    for parent in parents {
        let Ok(entries) = fs::read_dir(parent) else {
            continue;
        };
        for entry in entries {
            let entry = entry?;
            if !entry.file_name().to_string_lossy().starts_with(PREFIX)
                || !entry.file_type()?.is_dir()
            {
                continue;
            }
            let directory = entry.path();
            if !files::metadata(&directory.join(JOURNAL))?.is_some_and(|m| m.is_file()) {
                continue;
            }
            let Ok(bytes) = fs::read(directory.join(JOURNAL)) else {
                continue;
            };
            let Ok(manifest) = serde_json::from_slice::<Manifest>(&bytes) else {
                continue;
            };
            if manifest.owner != OWNER
                || manifest.version != 1
                || !matches!(manifest.phase.as_str(), "capturing" | "converging")
                || !valid_selection(&manifest.previous_selection)
                || !valid_selection(&manifest.next_selection)
                || !valid_entries(targets, &directory, &manifest)?
            {
                continue;
            }
            if selections_equal(committed, &manifest.next_selection) {
                discard(&directory, &manifest.entries)?;
                continue;
            }
            if !selections_equal(committed, &manifest.previous_selection) {
                bail!(
                    "Cannot safely recover interrupted skills transaction at {}",
                    directory.display()
                );
            }
            restore(targets, &directory, &manifest)?;
            discard(&directory, &manifest.entries)?;
        }
    }
    Ok(())
}

fn selections_equal(left: &Value, right: &Value) -> bool {
    left["mode"] == right["mode"] && (left["mode"] == "all" || left["skills"] == right["skills"])
}

impl Transaction {
    pub fn begin(targets: &Targets, previous: Value, next: Value, ops: &[Op]) -> Result<Self> {
        let directory = targets.roots[0]
            .parent()
            .context("Missing transaction parent")?
            .join(format!("{PREFIX}{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory)?;
        let mut transaction = Self {
            directory,
            manifest: Manifest {
                owner: OWNER.into(),
                version: 1,
                phase: "capturing".into(),
                previous_selection: previous,
                next_selection: next,
                entries: Vec::new(),
            },
        };
        write_manifest(&transaction.directory, &transaction.manifest)?;
        sync_dir(
            transaction
                .directory
                .parent()
                .context("Missing transaction parent")?,
        )?;
        let capture = (|| -> Result<()> {
            let mut seen = BTreeSet::new();
            for (index, root) in targets.roots.iter().enumerate() {
                let identity = fs::canonicalize(root).unwrap_or_else(|_| root.clone());
                if !seen.insert(identity) {
                    continue;
                }
                for op in ops {
                    let live = root.join(&op.name);
                    let Some(info) = files::metadata(&live)? else {
                        transaction.manifest.entries.push(Entry {
                            live_path: live,
                            kind: op.kind.clone(),
                            backup_path: None,
                        });
                        continue;
                    };
                    let backup = if op.kind == "delete" {
                        root.join(
                            transaction
                                .directory
                                .file_name()
                                .context("Missing transaction name")?,
                        )
                        .join(&op.name)
                    } else {
                        PathBuf::from("backup")
                            .join(index.to_string())
                            .join(&op.name)
                    };
                    if op.kind != "delete" {
                        let source = if info.file_type().is_symlink() && is_directory(&live) {
                            fs::canonicalize(&live)?
                        } else {
                            live.clone()
                        };
                        let target = if backup.is_absolute() {
                            backup.clone()
                        } else {
                            transaction.directory.join(&backup)
                        };
                        copy_entry(&source, &target)?;
                    }
                    transaction.manifest.entries.push(Entry {
                        live_path: live,
                        kind: op.kind.clone(),
                        backup_path: Some(backup),
                    });
                }
            }
            transaction.manifest.phase = "converging".into();
            write_manifest(&transaction.directory, &transaction.manifest)
        })();
        if let Err(error) = capture {
            discard(&transaction.directory, &transaction.manifest.entries)?;
            return Err(error);
        }
        let stage = (|| -> Result<()> {
            for entry in &transaction.manifest.entries {
                if entry.kind != "delete" {
                    continue;
                }
                let Some(backup) = backup_path(&transaction.directory, &entry.backup_path) else {
                    continue;
                };
                fs::create_dir_all(backup.parent().context("Missing stage parent")?)?;
                fs::rename(&entry.live_path, &backup)?;
                sync_dir(backup.parent().context("Missing stage parent")?)?;
                sync_dir(entry.live_path.parent().context("Missing live parent")?)?;
            }
            Ok(())
        })();
        if let Err(error) = stage {
            transaction.rollback(targets)?;
            return Err(error);
        }
        Ok(transaction)
    }

    pub fn commit(self) -> Result<()> {
        discard(&self.directory, &self.manifest.entries)
    }

    pub fn rollback(self, targets: &Targets) -> Result<()> {
        restore(targets, &self.directory, &self.manifest)?;
        discard(&self.directory, &self.manifest.entries)
    }
}
