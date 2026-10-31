use super::cron::Cadence;
use anyhow::{Result, ensure};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};
use tokio::{io::AsyncWriteExt, sync::Mutex};

pub fn timestamp(now: DateTime<Utc>) -> String {
    now.to_rfc3339_opts(SecondsFormat::Millis, true)
}
pub fn date(value: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(value)?.with_timezone(&Utc))
}

fn nullable<'de, D, T>(deserializer: D) -> std::result::Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}
fn present_value<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Target {
    #[serde(rename = "agent")]
    Agent {
        #[serde(rename = "agentId")]
        agent_id: String,
    },
    #[serde(rename = "new-agent")]
    NewAgent { config: Value },
}
impl Target {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Agent { agent_id } => {
                uuid::Uuid::parse_str(agent_id)?;
            }
            Self::NewAgent { config } => {
                ensure!(config.is_object(), "new-agent config must be an object");
                ensure!(config["provider"].is_string(), "Invalid provider");
                ensure!(
                    config["cwd"].as_str().is_some_and(|s| !s.trim().is_empty()),
                    "cwd cannot be empty"
                );
                for key in ["model", "modeId", "thinkingOptionId"] {
                    if let Some(v) = config.get(key) {
                        ensure!(
                            v.as_str().is_some_and(|s| !s.trim().is_empty()),
                            "Invalid {key}"
                        );
                    }
                }
                if let Some(v) = config.get("title") {
                    ensure!(
                        v.is_null() || v.as_str().is_some_and(|s| !s.trim().is_empty()),
                        "Invalid title"
                    );
                }
                if let Some(v) = config.get("archiveOnFinish") {
                    ensure!(v.is_boolean(), "Invalid archiveOnFinish");
                }
                if let Some(v) = config.get("isolation") {
                    ensure!(
                        matches!(v.as_str(), Some("local" | "worktree")),
                        "Invalid isolation"
                    );
                }
                for key in ["providerOptions", "featureValues", "mcpServers"] {
                    if let Some(v) = config.get(key) {
                        ensure!(v.is_object(), "Invalid {key}");
                    }
                }
                if let Some(v) = config.get("systemPrompt") {
                    ensure!(v.is_string(), "Invalid systemPrompt");
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    pub id: String,
    pub scheduled_for: String,
    pub started_at: String,
    #[serde(deserialize_with = "nullable")]
    pub ended_at: Option<String>,
    pub status: String,
    #[serde(deserialize_with = "nullable")]
    pub agent_id: Option<String>,
    #[serde(
        default,
        deserialize_with = "present_value",
        skip_serializing_if = "Option::is_none"
    )]
    pub workspace_id: Option<Value>,
    #[serde(deserialize_with = "nullable")]
    pub output: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub error: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Schedule {
    pub id: String,
    #[serde(deserialize_with = "nullable")]
    pub name: Option<String>,
    pub prompt: String,
    pub cadence: Cadence,
    pub target: Target,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(deserialize_with = "nullable")]
    pub next_run_at: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub last_run_at: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub paused_at: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub expires_at: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub max_runs: Option<u64>,
    pub runs: Vec<Run>,
}
impl Schedule {
    fn validate(&self) -> Result<()> {
        safe_id(&self.id)?;
        ensure!(!self.prompt.is_empty(), "Schedule prompt is required");
        // Stored cron syntax is validated when used, as in the source schema.
        if let Cadence::Every { every_ms } = &self.cadence {
            ensure!(*every_ms > 0, "Invalid everyMs");
        }
        ensure!(
            matches!(self.status.as_str(), "active" | "paused" | "completed"),
            "Invalid schedule status"
        );
        ensure!(
            self.max_runs != Some(0),
            "maxRuns must be a positive integer"
        );
        self.target.validate()?;
        for run in &self.runs {
            ensure!(
                matches!(run.status.as_str(), "running" | "succeeded" | "failed"),
                "Invalid schedule run status"
            );
            if let Some(id) = &run.agent_id {
                uuid::Uuid::parse_str(id)?;
            }
            ensure!(
                run.workspace_id
                    .as_ref()
                    .is_none_or(|v| v.is_null() || v.is_string()),
                "Invalid workspaceId"
            );
        }
        Ok(())
    }
    pub fn completed_runs(&self) -> u64 {
        self.runs.iter().filter(|r| r.status != "running").count() as u64
    }
    pub fn should_complete(&self, now: DateTime<Utc>) -> bool {
        self.expires_at
            .as_deref()
            .and_then(|s| date(s).ok())
            .is_some_and(|t| t <= now)
            || self.max_runs.is_some_and(|n| self.completed_runs() >= n)
    }
    pub fn complete(&mut self, now: DateTime<Utc>) {
        self.status = "completed".into();
        self.next_run_at = None;
        self.paused_at = None;
        self.updated_at = timestamp(now);
    }
    pub fn value(&self) -> Result<Value> {
        Ok(serde_json::to_value(self)?)
    }
}

fn safe_id(id: &str) -> Result<()> {
    ensure!(
        !id.is_empty() && id != "." && id != ".." && !id.contains(['/', '\\', '\0']),
        "Invalid schedule id"
    );
    Ok(())
}

pub struct Store {
    dir: PathBuf,
    // A single writer also serializes named identity upserts against ordinary mutations.
    pub writer: Mutex<()>,
    reported_invalid: Mutex<HashSet<PathBuf>>,
}
impl Store {
    pub async fn open(dir: PathBuf) -> Result<Self> {
        tokio::fs::create_dir_all(&dir).await?;
        Ok(Self {
            dir,
            writer: Mutex::new(()),
            reported_invalid: Mutex::new(HashSet::new()),
        })
    }
    fn path(&self, id: &str) -> Result<PathBuf> {
        safe_id(id)?;
        Ok(self.dir.join(format!("{id}.json")))
    }
    pub async fn list(&self) -> Result<Vec<Schedule>> {
        let mut entries = tokio::fs::read_dir(&self.dir).await?;
        let mut records = Vec::new();
        let mut invalid = HashSet::new();
        while let Some(entry) = entries.next_entry().await? {
            if !entry.file_type().await?.is_file()
                || entry.path().extension().is_none_or(|s| s != "json")
            {
                continue;
            }
            let path = entry.path();
            let bytes = match tokio::fs::read(&path).await {
                Ok(b) => b,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            let parsed = serde_json::from_slice::<Schedule>(&bytes)
                .map_err(anyhow::Error::from)
                .and_then(|s| {
                    s.validate()?;
                    ensure!(self.path(&s.id)? == path, "Schedule file identity mismatch");
                    Ok(s)
                });
            match parsed {
                Ok(s) => records.push(s),
                Err(e) => {
                    if !self.reported_invalid.lock().await.contains(&path) {
                        tracing::error!(file=%path.display(),error=%e,"Skipping invalid schedule file");
                    }
                    invalid.insert(path);
                }
            }
        }
        *self.reported_invalid.lock().await = invalid;
        records.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(records)
    }
    pub async fn get(&self, id: &str) -> Result<Option<Schedule>> {
        match tokio::fs::read(self.path(id)?).await {
            Ok(bytes) => {
                let s: Schedule = serde_json::from_slice(&bytes)?;
                s.validate()?;
                ensure!(s.id == id, "Schedule file identity mismatch");
                Ok(Some(s))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
    pub async fn require(&self, id: &str) -> Result<Schedule> {
        self.get(id)
            .await?
            .ok_or_else(|| super::ScheduleNotFoundError(id.to_owned()).into())
    }
    pub async fn write(&self, s: &Schedule) -> Result<()> {
        s.validate()?;
        let path = self.path(&s.id)?;
        let temp = self
            .dir
            .join(format!(".{}.{}.tmp", s.id, uuid::Uuid::new_v4()));
        let bytes = serde_json::to_vec_pretty(s)?;
        let result = async {
            let mut options = tokio::fs::OpenOptions::new();
            options.create_new(true).write(true);
            #[cfg(unix)]
            options.mode(0o600);
            let mut file = options.open(&temp).await?;
            file.write_all(&bytes).await?;
            file.sync_all().await?;
            drop(file);
            tokio::fs::rename(&temp, &path).await?;
            sync_directory(&self.dir).await
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(&temp).await;
        }
        result
    }
    pub async fn delete(&self, id: &str) -> Result<()> {
        match tokio::fs::remove_file(self.path(id)?).await {
            Ok(()) => sync_directory(&self.dir).await,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}
async fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        tokio::fs::File::open(path).await?.sync_all().await?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}
