use super::Timeline;
use anyhow::{Context, Result};
use serde_json::Value;
use std::fs::{self, File};
use std::io::{BufReader, Write};
use std::path::Path;

/// `path` is the complete checkpoint filename, not a directory or database.
/// Missing checkpoints recover an empty timeline; invalid existing data is an error.
pub(super) fn load(path: &Path, agent_id: &str) -> Result<Timeline> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Timeline::new()),
        Err(error) => return Err(error.into()),
    };
    let checkpoint: Value = serde_json::from_reader(BufReader::new(file))
        .with_context(|| format!("Invalid timeline checkpoint {}", path.display()))?;
    anyhow::ensure!(
        checkpoint["version"] == 1,
        "Unsupported timeline checkpoint version"
    );
    anyhow::ensure!(
        checkpoint["agentId"] == agent_id,
        "Timeline checkpoint agent mismatch"
    );
    anyhow::ensure!(
        checkpoint["rows"].is_array()
            && checkpoint["epoch"].is_string()
            && checkpoint["nextSeq"].as_i64().is_some(),
        "Invalid timeline checkpoint"
    );
    Timeline::seed(&checkpoint)
}

pub(super) fn persist(timeline: &Timeline, path: &Path, agent_id: &str) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    // Serialize borrowed projection rows directly rather than cloning the entire history.
    file.write_all(b"{\"version\":1,\"agentId\":")?;
    serde_json::to_writer(&mut file, agent_id)?;
    file.write_all(b",\"epoch\":")?;
    serde_json::to_writer(&mut file, &timeline.epoch)?;
    file.write_all(b",\"nextSeq\":")?;
    serde_json::to_writer(&mut file, &timeline.next_seq)?;
    file.write_all(b",\"rows\":")?;
    serde_json::to_writer(&mut file, &timeline.rows)?;
    file.write_all(b"}\n")?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|e| e.error)?;
    #[cfg(unix)]
    File::open(parent)?.sync_all()?;
    Ok(())
}

/// Persist the changed projection before mutating the live store. Only the changed
/// row is cloned; the unchanged rows are serialized from the current checkpoint.
pub(super) fn append(
    timeline: &mut Timeline,
    path: &Path,
    agent_id: &str,
    item: Value,
    options: &Value,
) -> Result<Value> {
    // Build a one-row mutation and rollback its small affected state if storage fails.
    let old_next = timeline.next_seq;
    let old_min = timeline.min_seq;
    let old_len = timeline.rows.len();
    let id = super::identity(&item);
    let old_identity = id
        .as_ref()
        .and_then(|id| timeline.identities.get(id).copied());
    let candidate = old_identity.or_else(|| old_len.checked_sub(1));
    let old_row = candidate.map(|index| (index, timeline.rows[index].clone()));
    let submitted_id = (item["type"] == "user_message")
        .then(|| item["clientMessageId"].as_str().map(str::to_owned))
        .flatten();
    let old_submitted = submitted_id
        .as_ref()
        .and_then(|id| timeline.submitted.get(id).copied());
    let row = timeline.append(item, options);
    if let Err(error) = persist(timeline, path, agent_id) {
        timeline.rows.truncate(old_len);
        if let Some((index, row)) = old_row {
            timeline.rows[index] = row;
        }
        timeline.next_seq = old_next;
        timeline.min_seq = old_min;
        if let Some(id) = id {
            if let Some(index) = old_identity {
                timeline.identities.insert(id, index);
            } else {
                timeline.identities.remove(&id);
            }
        }
        if let Some(id) = submitted_id {
            if let Some(index) = old_submitted {
                timeline.submitted.insert(id, index);
            } else {
                timeline.submitted.remove(&id);
            }
        }
        return Err(error);
    }
    Ok(row)
}

pub(super) fn enrich(
    timeline: &mut Timeline,
    path: &Path,
    agent_id: &str,
    message_id: &str,
    provider_id: &str,
) -> Result<Option<Value>> {
    let Some(&index) = timeline.submitted.get(message_id) else {
        return Ok(None);
    };
    let previous = timeline.rows[index].get("providerMessageId").cloned();
    let row = timeline.enrich_submitted_user_message(message_id, provider_id);
    if let Err(error) = persist(timeline, path, agent_id) {
        if let Some(previous) = previous {
            timeline.rows[index]["providerMessageId"] = previous;
        } else {
            timeline.rows[index]
                .as_object_mut()
                .unwrap()
                .remove("providerMessageId");
        }
        return Err(error);
    }
    Ok(row)
}
