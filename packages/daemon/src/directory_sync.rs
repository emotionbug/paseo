//! Daemon-wide latest-state directory sequencing. Feed complete source
//! projections here, then apply observer visibility to the returned entries.
//! Collections are intentionally in-memory: a new daemon gets a new generation.

mod collection;
mod projection;

#[cfg(test)]
mod tests;

pub use collection::{
    CollectionRead, DEFAULT_MAX_TOMBSTONES, VersionedCollection, VersionedRemoval, VersionedValue,
};
pub use projection::{
    agent_directory_includes, matches_agent_updates_filter, matches_workspace_filter,
    resolve_effective_thinking_option_id, same_json_fingerprint, workspace_directory_includes,
};

use std::sync::{Mutex, MutexGuard};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

struct Collections {
    projects: VersionedCollection,
    workspaces: VersionedCollection,
    agents: VersionedCollection,
}

/// One instance must be shared across sessions, projects and the agent manager.
/// Background observers share the same source state, but use a distinct cursor
/// generation so switching visibility always forces a new snapshot.
pub struct DirectorySyncService {
    generation: String,
    background_generation: String,
    collections: Mutex<Collections>,
}

impl Default for DirectorySyncService {
    fn default() -> Self {
        Self::new()
    }
}

impl DirectorySyncService {
    pub fn new() -> Self {
        Self::with_generation(uuid::Uuid::new_v4().to_string())
    }

    pub fn with_generation(generation: impl Into<String>) -> Self {
        let generation = generation.into();
        Self {
            background_generation: format!("{generation}:background"),
            generation,
            collections: Mutex::new(Collections {
                projects: VersionedCollection::new(project_id),
                workspaces: VersionedCollection::new(workspace_id),
                agents: VersionedCollection::new(agent_id),
            }),
        }
    }

    pub fn generation(&self, include_background: bool) -> &str {
        if include_background {
            &self.background_generation
        } else {
            &self.generation
        }
    }

    pub fn synchronize_projects(&self, snapshot: Vec<Value>, cursor: &Value) -> Result<Value> {
        let mut collections = self.lock()?;
        collections.projects.replace_all(snapshot)?;
        let read = self.read(&collections.projects, cursor, false);
        Ok(json!({"projects": sequenced_values(&read)?, "sync": self.metadata(&read, false)}))
    }

    pub fn synchronize_workspaces(
        &self,
        snapshot: Vec<Value>,
        cursor: &Value,
        include_background: bool,
    ) -> Result<Value> {
        let mut collections = self.lock()?;
        collections.workspaces.replace_all(snapshot)?;
        let read = self.read(&collections.workspaces, cursor, include_background);
        Ok(json!({
            "entries": sequenced_values(&read)?, "emptyProjects": [],
            "pageInfo": {"nextCursor": null, "prevCursor": null, "hasMore": false},
            "sync": self.metadata(&read, include_background),
        }))
    }

    pub fn synchronize_agents(
        &self,
        snapshot: Vec<Value>,
        cursor: &Value,
        include_background: bool,
    ) -> Result<Value> {
        let mut collections = self.lock()?;
        collections.agents.replace_all(snapshot)?;
        let read = self.read(&collections.agents, cursor, include_background);
        Ok(json!({
            "entries": sequenced_values(&read)?,
            "pageInfo": {"nextCursor": null, "prevCursor": null, "hasMore": false},
            "sync": self.metadata(&read, include_background),
        }))
    }

    pub fn sequence_project_update(&self, payload: Value, include_sequence: bool) -> Result<Value> {
        let mut collections = self.lock()?;
        let id = match payload.get("kind").and_then(Value::as_str) {
            Some("upsert") => {
                let project = payload
                    .get("project")
                    .context("Project upsert is missing project")?;
                let id = project_id(project)?;
                collections.projects.replace(project.clone())?;
                id
            }
            Some("remove") => {
                let id = payload
                    .get("projectId")
                    .and_then(Value::as_str)
                    .context("Project removal is missing projectId")?;
                collections.projects.remove(id)?;
                id
            }
            _ => bail!("Invalid project update kind"),
        };
        let seq = collections.projects.sequence_for(id);
        self.with_version(payload, include_sequence, seq, false)
    }

    pub fn sequence_workspace_update(
        &self,
        payload: Value,
        value: Option<Value>,
        id: &str,
        include_sequence: bool,
        include_background: bool,
    ) -> Result<Value> {
        let mut collections = self.lock()?;
        if let Some(value) = value {
            collections.workspaces.replace(value)?;
        } else {
            collections.workspaces.remove(id)?;
        }
        self.with_version(
            payload,
            include_sequence,
            collections.workspaces.sequence_for(id),
            include_background,
        )
    }

    pub fn sequence_agent_update(
        &self,
        payload: Value,
        value: Option<Value>,
        id: &str,
        include_sequence: bool,
        include_background: bool,
    ) -> Result<Value> {
        let mut collections = self.lock()?;
        if let Some(value) = value {
            collections.agents.replace(value)?;
        } else {
            collections.agents.remove(id)?;
        }
        self.with_version(
            payload,
            include_sequence,
            collections.agents.sequence_for(id),
            include_background,
        )
    }

    fn lock(&self) -> Result<MutexGuard<'_, Collections>> {
        self.collections
            .lock()
            .map_err(|_| anyhow!("Directory sync lock poisoned"))
    }

    fn read<'a>(
        &self,
        collection: &'a VersionedCollection,
        cursor: &Value,
        include_background: bool,
    ) -> CollectionRead<'a> {
        let generation = cursor.get("generation");
        if generation.and_then(Value::as_str) != Some(self.generation(include_background)) {
            return collection.read_snapshot(if generation.is_none() {
                "no_cursor"
            } else {
                "generation_changed"
            });
        }
        match cursor.get("afterSeq") {
            None => collection.read_snapshot("no_cursor"),
            Some(value) => match value.as_u64().or_else(|| {
                value
                    .as_f64()
                    .filter(|number| {
                        number.is_finite()
                            && *number >= 0.0
                            && number.fract() == 0.0
                            && *number < u64::MAX as f64
                    })
                    .map(|number| number as u64)
            }) {
                Some(seq) => collection.read_after(seq),
                None => collection.read_snapshot("cursor_expired"),
            },
        }
    }

    fn metadata(&self, read: &CollectionRead<'_>, include_background: bool) -> Value {
        let mut metadata = json!({
            "generation": self.generation(include_background), "headSeq": read.head_seq,
            "mode": read.mode, "removals": read.removals,
        });
        if let Some(reason) = read.reason {
            metadata["reason"] = Value::String(reason.to_owned());
        }
        metadata
    }

    fn with_version(
        &self,
        mut payload: Value,
        include_sequence: bool,
        seq: Option<u64>,
        include_background: bool,
    ) -> Result<Value> {
        if include_sequence {
            if let Some(seq) = seq {
                let object = payload
                    .as_object_mut()
                    .context("Directory update payload must be an object")?;
                object.insert(
                    "generation".into(),
                    Value::String(self.generation(include_background).to_owned()),
                );
                object.insert("seq".into(), Value::from(seq));
            }
        }
        Ok(payload)
    }
}

fn project_id(value: &Value) -> Result<&str> {
    value
        .get("projectId")
        .and_then(Value::as_str)
        .context("Directory project is missing projectId")
}

fn workspace_id(value: &Value) -> Result<&str> {
    value
        .get("id")
        .and_then(Value::as_str)
        .context("Directory workspace is missing id")
}

fn agent_id(value: &Value) -> Result<&str> {
    value
        .get("agent")
        .and_then(|agent| agent.get("id"))
        .and_then(Value::as_str)
        .context("Directory agent entry is missing agent.id")
}

fn sequenced_values(read: &CollectionRead<'_>) -> Result<Vec<Value>> {
    read.values
        .iter()
        .map(|entry| {
            let mut value = entry.value.clone();
            value
                .as_object_mut()
                .context("Directory entry must be an object")?
                .insert("syncSeq".into(), Value::from(entry.seq));
            Ok(value)
        })
        .collect()
}
