mod cron;
mod execution;
pub mod skills;
mod store;
#[cfg(test)]
mod tests;

use crate::{state::AppState, transport::RequestContext};
use anyhow::{Result, anyhow, bail, ensure};
use chrono::{DateTime, Utc};
use cron::Cadence;
use rand::RngCore;
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    sync::{Arc, Mutex as StdMutex, OnceLock, Weak},
};
use store::{Run, Schedule, Store, Target, date, timestamp};
use tokio::{sync::Mutex, task::JoinHandle};
use tokio_util::sync::CancellationToken;

#[derive(Debug, thiserror::Error)]
#[error("Schedule not found: {0}")]
pub struct ScheduleNotFoundError(pub String);
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ScheduleStateConflictError(pub String);
pub fn is_input_error(error: &anyhow::Error) -> bool {
    error.is::<ScheduleNotFoundError>() || error.is::<ScheduleStateConflictError>()
}
pub fn validate_cadence(value: &Value) -> Result<()> {
    parse_cadence(value).map(|_| ())
}
pub fn next_run_at(value: &Value, after: DateTime<Utc>) -> Result<DateTime<Utc>> {
    parse_cadence(value)?.next(after)
}

pub struct AutomationService {
    app: Weak<AppState>,
    store: OnceLock<Store>,
    clock: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>,
    running: Arc<StdMutex<HashSet<String>>>,
    stop: CancellationToken,
    timer: Mutex<Option<JoinHandle<()>>>,
    skills: skills::SkillsService,
}

struct Reservation {
    running: Arc<StdMutex<HashSet<String>>>,
    id: String,
}
impl Drop for Reservation {
    fn drop(&mut self) {
        self.running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);
    }
}

pub fn schedule_summary(mut value: Value) -> Value {
    if let Some(o) = value.as_object_mut() {
        o.remove("runs");
    }
    value
}
fn string<'a>(input: &'a Value, key: &str) -> Result<&'a str> {
    input[key].as_str().ok_or_else(|| anyhow!("Missing {key}"))
}
fn name(input: &Value) -> Result<Option<String>> {
    if input.is_null() {
        return Ok(None);
    }
    let value = input
        .as_str()
        .ok_or_else(|| anyhow!("Invalid schedule name"))?
        .trim();
    Ok((!value.is_empty()).then(|| value.to_owned()))
}
fn prompt(input: &Value) -> Result<String> {
    let value = string(input, "prompt")?.trim();
    ensure!(!value.is_empty(), "Schedule prompt is required");
    Ok(value.to_owned())
}
fn max_runs(value: &Value) -> Result<Option<u64>> {
    if value.is_null() {
        return Ok(None);
    }
    let n = value
        .as_u64()
        .filter(|n| *n > 0)
        .ok_or_else(|| anyhow!("maxRuns must be a positive integer"))?;
    Ok(Some(n))
}
fn nullable_string(value: &Value, key: &str) -> Result<Option<String>> {
    if value.is_null() {
        return Ok(None);
    }
    Ok(Some(
        value
            .as_str()
            .ok_or_else(|| anyhow!("Invalid {key}"))?
            .to_owned(),
    ))
}
fn parse_cadence(value: &Value) -> Result<Cadence> {
    if value["type"] == "cron" {
        ensure!(
            value.get("timezone").is_none_or(Value::is_string),
            "Invalid cron time zone"
        );
    }
    let mut c: Cadence = serde_json::from_value(value.clone())?;
    if let Cadence::Cron {
        expression,
        timezone,
    } = &mut c
    {
        *expression = expression.trim().to_owned();
        if let Some(t) = timezone {
            *t = t.trim().to_owned();
            ensure!(!t.is_empty(), "Invalid cron time zone: {t}");
        }
    }
    c.validate()?;
    Ok(c)
}
fn parse_target(value: &Value) -> Result<Target> {
    let mut value = value.clone();
    if value["type"] == "self" {
        value["type"] = json!("agent");
    }
    if let Some(config) = value.get_mut("config").and_then(Value::as_object_mut) {
        for key in ["cwd", "modeId", "model", "thinkingOptionId", "title"] {
            if let Some(Value::String(s)) = config.get_mut(key) {
                *s = s.trim().to_owned();
            }
        }
    }
    let target: Target = serde_json::from_value(value)?;
    target.validate()?;
    Ok(target)
}

impl AutomationService {
    pub fn new(app: Weak<AppState>) -> Self {
        Self::with_clock(app, Arc::new(Utc::now))
    }
    /// Inject wall time while retaining the same persistence and claim state machine.
    pub fn with_clock(
        app: Weak<AppState>,
        clock: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>,
    ) -> Self {
        Self {
            skills: skills::SkillsService::new(app.clone()),
            app,
            store: OnceLock::new(),
            clock,
            running: Arc::new(StdMutex::new(HashSet::new())),
            stop: CancellationToken::new(),
            timer: Mutex::new(None),
        }
    }
    fn app(&self) -> Result<Arc<AppState>> {
        self.app
            .upgrade()
            .ok_or_else(|| anyhow!("Daemon is shutting down"))
    }
    fn store(&self) -> Result<&Store> {
        self.store
            .get()
            .ok_or_else(|| anyhow!("Automation service is not initialized"))
    }
    fn now(&self) -> DateTime<Utc> {
        (self.clock)()
    }
    pub async fn initialize(&self) -> Result<()> {
        let mut timer = self.timer.lock().await;
        if timer.is_some() {
            return Ok(());
        }
        ensure!(!self.stop.is_cancelled(), "Automation service is stopped");
        if self.store.get().is_none() {
            let home = self.app()?.home.join("schedules");
            self.store
                .set(Store::open(home).await?)
                .map_err(|_| anyhow!("Automation initialization raced"))?;
        }
        self.recover().await?;
        self.sweep_orphans().await?;
        self.skills.initialize().await?;
        let app = self.app.clone();
        let stop = self.stop.clone();
        *timer = Some(tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            interval.tick().await;
            let mut ticks = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    biased;
                    _=stop.cancelled()=>break,
                    result=ticks.join_next(), if !ticks.is_empty()=>{
                        if let Some(Err(error))=result {tracing::error!(%error,"Schedule tick task failed");}
                        continue;
                    },
                    _=interval.tick()=>{}
                }
                let Some(state) = app.upgrade() else {
                    break;
                };
                let service = state.automation.clone();
                let cancel = state.cancel.clone();
                drop(state);
                if cancel.is_cancelled() {
                    break;
                }
                let tick_stop = stop.clone();
                ticks.spawn(async move {
                    tokio::select! {
                        biased;
                        _=tick_stop.cancelled()=>{},
                        _=cancel.cancelled()=>{},
                        result=service.tick()=>if let Err(e)=result {tracing::error!(error=%e,"Failed to process schedule tick");}
                    }
                });
            }
            ticks.abort_all();
            while ticks.join_next().await.is_some() {}
        }));
        Ok(())
    }
    pub async fn shutdown(&self) -> Result<()> {
        self.stop.cancel();
        if let Some(task) = self.timer.lock().await.take() {
            task.await?;
        }
        Ok(())
    }
    pub async fn create(&self, input: &Value) -> Result<Value> {
        self.create_internal(input, false).await
    }
    pub async fn create_or_replace(&self, input: &Value) -> Result<Value> {
        self.create_internal(input, true).await
    }
    async fn create_internal(&self, input: &Value, upsert: bool) -> Result<Value> {
        let prompt = prompt(input)?;
        let name = name(&input["name"])?;
        let mut cadence = parse_cadence(&input["cadence"])?;
        let target = parse_target(&input["target"])?;
        let max_runs = max_runs(&input["maxRuns"])?;
        let expires_at = nullable_string(&input["expiresAt"], "expiresAt")?;
        if let Some(v) = input.get("runOnCreate") {
            ensure!(v.is_null() || v.is_boolean(), "Invalid runOnCreate");
        }
        let store = self.store()?;
        let _writer = store.writer.lock().await;
        let existing = if upsert && name.is_some() {
            let identity = serde_json::to_value(&target)?;
            store.list().await?.into_iter().find(|s| {
                s.status != "completed"
                    && s.name.as_deref().map(str::trim) == name.as_deref()
                    && serde_json::to_value(&s.target).is_ok_and(|t| t == identity)
            })
        } else {
            None
        };
        if let Some(s) = &existing {
            cadence.inherit_timezone(&s.cadence);
        }
        let now = self.now();
        let next = if input["runOnCreate"]
            .as_bool()
            .unwrap_or(cadence.run_on_create())
        {
            now
        } else {
            cadence.next(now)?
        };
        let mut schedule = if let Some(s) = existing {
            s
        } else {
            let id = loop {
                let mut bytes = [0u8; 4];
                rand::thread_rng().fill_bytes(&mut bytes);
                let id = hex::encode(bytes);
                if store.get(&id).await?.is_none() {
                    break id;
                }
            };
            Schedule {
                id,
                name: None,
                prompt: String::new(),
                cadence: cadence.clone(),
                target: target.clone(),
                status: "active".into(),
                created_at: timestamp(now),
                updated_at: timestamp(now),
                next_run_at: None,
                last_run_at: None,
                paused_at: None,
                expires_at: None,
                max_runs: None,
                runs: Vec::new(),
            }
        };
        schedule.name = name;
        schedule.prompt = prompt;
        schedule.cadence = cadence;
        schedule.target = target;
        schedule.status = "active".into();
        schedule.paused_at = None;
        schedule.next_run_at = Some(timestamp(next));
        schedule.updated_at = timestamp(now);
        schedule.max_runs = max_runs;
        schedule.expires_at = expires_at;
        store.write(&schedule).await?;
        schedule.value()
    }
    pub async fn list(&self) -> Result<Vec<Value>> {
        self.store()?
            .list()
            .await?
            .iter()
            .map(Schedule::value)
            .collect()
    }
    pub async fn inspect(&self, id: &str) -> Result<Value> {
        self.store()?.require(id).await?.value()
    }
    pub async fn logs(&self, id: &str) -> Result<Vec<Value>> {
        let mut runs = self.store()?.require(id).await?.runs;
        runs.sort_by(|a, b| a.started_at.cmp(&b.started_at));
        runs.iter().map(|r| Ok(serde_json::to_value(r)?)).collect()
    }
    pub async fn pause(&self, id: &str) -> Result<Value> {
        self.set_paused(id, true).await
    }
    pub async fn resume(&self, id: &str) -> Result<Value> {
        self.set_paused(id, false).await
    }
    async fn set_paused(&self, id: &str, paused: bool) -> Result<Value> {
        let store = self.store()?;
        let _writer = store.writer.lock().await;
        let mut s = store.require(id).await?;
        if s.status == "completed" {
            return Err(
                ScheduleStateConflictError(format!("Schedule {id} is already completed")).into(),
            );
        }
        let status = if paused { "paused" } else { "active" };
        if s.status != status {
            let now = self.now();
            s.status = status.into();
            s.updated_at = timestamp(now);
            s.paused_at = paused.then(|| timestamp(now));
            s.next_run_at = if paused {
                None
            } else {
                Some(timestamp(s.cadence.next(now)?))
            };
            store.write(&s).await?;
        }
        s.value()
    }
    pub async fn update(&self, input: &Value) -> Result<Value> {
        let id = string(input, "id")?;
        let store = self.store()?;
        let _writer = store.writer.lock().await;
        let mut s = store.require(id).await?;
        let now = self.now();
        if input.get("prompt").is_some() {
            s.prompt = prompt(input)?;
        }
        if let Some(v) = input.get("name") {
            s.name = name(v)?;
        }
        if let Some(v) = input.get("cadence") {
            let mut cadence = parse_cadence(v)?;
            cadence.inherit_timezone(&s.cadence);
            s.next_run_at = if s.status == "active" {
                Some(timestamp(cadence.next(now)?))
            } else {
                None
            };
            s.cadence = cadence;
        }
        if let Some(patch) = input.get("newAgentConfig") {
            let Target::NewAgent { config } = &mut s.target else {
                bail!("new-agent config updates are only valid for new-agent target schedules");
            };
            ensure!(patch.is_object(), "Invalid newAgentConfig");
            for key in ["provider", "cwd"] {
                if let Some(v) = patch.get(key) {
                    let value = v.as_str().ok_or_else(|| anyhow!("Invalid {key}"))?.trim();
                    ensure!(!value.is_empty(), "{key} cannot be empty");
                    config[key] = json!(value);
                }
            }
            for key in ["model", "modeId", "thinkingOptionId"] {
                if let Some(v) = patch.get(key) {
                    let value = nullable_string(v, key)?;
                    if let Some(value) =
                        value.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
                    {
                        config[key] = json!(value);
                    } else {
                        config.as_object_mut().unwrap().remove(key);
                    }
                }
            }
            for key in ["archiveOnFinish", "isolation"] {
                if let Some(v) = patch.get(key) {
                    config[key] = v.clone();
                }
            }
            s.target.validate()?;
        }
        if let Some(v) = input.get("maxRuns") {
            s.max_runs = max_runs(v)?;
        }
        if let Some(v) = input.get("expiresAt") {
            s.expires_at = nullable_string(v, "expiresAt")?;
        }
        s.updated_at = timestamp(now);
        store.write(&s).await?;
        s.value()
    }
    pub async fn delete(&self, id: &str) -> Result<()> {
        let store = self.store()?;
        let _writer = store.writer.lock().await;
        store.delete(id).await
    }
    pub async fn complete_for_agent(&self, id: &str) -> Result<usize> {
        let store = self.store()?;
        let _writer = store.writer.lock().await;
        let mut completed = 0;
        for mut s in store.list().await? {
            if s.status != "completed"
                && matches!(&s.target,Target::Agent {agent_id} if agent_id==id)
            {
                s.complete(self.now());
                match store.write(&s).await {
                    Ok(()) => completed += 1,
                    Err(e) => {
                        tracing::warn!(error=%e,schedule_id=%s.id,"Failed to complete schedule for archived agent; continuing")
                    }
                }
            }
        }
        Ok(completed)
    }
    pub async fn run_once(&self, id: &str) -> Result<Value> {
        self.run(id, true, self.now()).await?;
        self.inspect(id).await
    }
    pub async fn tick(&self) -> Result<()> {
        let now = self.now();
        let store = self.store()?;
        for s in store.list().await? {
            if s.status != "active"
                || s.next_run_at.is_none()
                || self
                    .running
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .contains(&s.id)
            {
                continue;
            }
            if s.should_complete(now) {
                let _writer = store.writer.lock().await;
                if let Some(mut current) = store.get(&s.id).await? {
                    if current.status == "active"
                        && current.next_run_at.is_some()
                        && current.should_complete(now)
                    {
                        current.complete(now);
                        store.write(&current).await?;
                    }
                }
            } else if date(s.next_run_at.as_deref().unwrap())? <= now {
                self.run(&s.id, false, now).await?;
            }
        }
        Ok(())
    }
    async fn run(&self, id: &str, manual: bool, now: DateTime<Utc>) -> Result<()> {
        ensure!(!self.stop.is_cancelled(), "Automation service is stopped");
        let reservation = {
            let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
            if !running.insert(id.to_owned()) {
                if manual {
                    return Err(ScheduleStateConflictError(format!(
                        "Schedule {id} is already running"
                    ))
                    .into());
                } else {
                    return Ok(());
                }
            }
            Reservation {
                running: self.running.clone(),
                id: id.to_owned(),
            }
        };
        let Some((schedule, run_id)) = self.claim(id, manual, now).await? else {
            return Ok(());
        };
        // The durable claim precedes every external effect. Cancellation never replays it.
        let result = tokio::select! {biased; _=self.stop.cancelled()=>return Err(anyhow!("Automation service stopped during scheduled run")), result=self.execute(&schedule,&run_id)=>result};
        self.finish(id, &run_id, manual, result).await?;
        drop(reservation);
        Ok(())
    }
    async fn claim(
        &self,
        id: &str,
        manual: bool,
        now: DateTime<Utc>,
    ) -> Result<Option<(Schedule, String)>> {
        let store = self.store()?;
        let _writer = store.writer.lock().await;
        let mut s = store.require(id).await?;
        if manual {
            if s.status == "completed" {
                return Err(ScheduleStateConflictError(format!(
                    "Schedule {id} is already completed"
                ))
                .into());
            }
        } else if s.status != "active"
            || s.next_run_at.is_none()
            || s.should_complete(now)
            || date(s.next_run_at.as_deref().unwrap())? > now
        {
            return Ok(None);
        }
        // Persisted running records also fence effects after a failed finalization.
        if s.runs.iter().any(|r| r.status == "running") {
            return Err(
                ScheduleStateConflictError(format!("Schedule {id} is already running")).into(),
            );
        }
        let scheduled_for = if manual {
            timestamp(now)
        } else {
            s.next_run_at.clone().unwrap()
        };
        if !manual {
            s.next_run_at = if s.max_runs.is_some_and(|n| s.completed_runs() + 1 >= n) {
                None
            } else {
                Some(timestamp(
                    s.cadence
                        .advance_past(s.cadence.next(date(&scheduled_for)?)?, now)?,
                ))
            };
        }
        let run_id = uuid::Uuid::new_v4().to_string();
        s.runs.push(Run {
            id: run_id.clone(),
            scheduled_for,
            started_at: timestamp(now),
            ended_at: None,
            status: "running".into(),
            agent_id: None,
            workspace_id: None,
            output: None,
            error: None,
        });
        s.updated_at = timestamp(now);
        store.write(&s).await?;
        Ok(Some((s, run_id)))
    }
    async fn finish(
        &self,
        id: &str,
        run_id: &str,
        manual: bool,
        result: Result<execution::ExecutionResult>,
    ) -> Result<()> {
        let store = self.store()?;
        let _writer = store.writer.lock().await;
        let mut s = store.require(id).await?;
        let now = self.now();
        let run = s
            .runs
            .iter_mut()
            .find(|r| r.id == run_id)
            .ok_or_else(|| anyhow!("Schedule run not found: {run_id}"))?;
        let target_gone = result
            .as_ref()
            .err()
            .is_some_and(|e| e.is::<execution::TargetGone>());
        match result {
            Ok(result) => {
                run.status = "succeeded".into();
                if result.agent_id.is_some() {
                    run.agent_id = result.agent_id;
                }
                run.output = result.output;
                run.error = None;
            }
            Err(e) => {
                run.status = "failed".into();
                run.output = None;
                run.error = Some(e.to_string());
            }
        }
        run.ended_at = Some(timestamp(now));
        let scheduled_for = run.scheduled_for.clone();
        s.last_run_at = Some(timestamp(now));
        s.updated_at = timestamp(now);
        if target_gone {
            s.complete(now);
        } else if s.status != "completed" && !manual {
            if s.should_complete(now) {
                s.complete(now);
            } else if s.status == "paused" {
                s.next_run_at = None;
            } else {
                let next = if let Some(next) = &s.next_run_at {
                    date(next)?
                } else {
                    s.cadence.next(date(&scheduled_for)?)?
                };
                s.next_run_at = Some(timestamp(s.cadence.advance_past(next, now)?));
            }
        }
        store.write(&s).await
    }
    async fn record_workspace(
        &self,
        id: &str,
        run_id: &str,
        workspace_id: &str,
        agent_id: Option<&str>,
    ) -> Result<()> {
        let store = self.store()?;
        let _writer = store.writer.lock().await;
        let mut s = store.require(id).await?;
        let run = s
            .runs
            .iter_mut()
            .find(|r| r.id == run_id && r.status == "running")
            .ok_or_else(|| anyhow!("Schedule run not running: {run_id}"))?;
        run.workspace_id = Some(json!(workspace_id));
        run.agent_id = agent_id.map(str::to_owned);
        s.updated_at = timestamp(self.now());
        store.write(&s).await
    }
    async fn recover(&self) -> Result<()> {
        let store = self.store()?;
        let now = self.now();
        let mut cleanup = Vec::new();
        {
            let _writer = store.writer.lock().await;
            for mut s in store.list().await? {
                let mut dirty = false;
                for run in &mut s.runs {
                    if run.status != "running" {
                        continue;
                    }
                    if let Target::NewAgent { config } = &s.target {
                        if run.agent_id.is_none()
                            || config["archiveOnFinish"].as_bool().unwrap_or(true)
                        {
                            if let Some(id) = run.workspace_id.as_ref().and_then(Value::as_str) {
                                cleanup.push(id.to_owned());
                            }
                        }
                    }
                    run.status = "failed".into();
                    run.ended_at = Some(timestamp(now));
                    run.error = Some("Daemon restarted before the scheduled run completed".into());
                    dirty = true;
                }
                if dirty && s.should_complete(now) {
                    s.complete(now);
                }
                if s.status == "active" {
                    if let Some(next) = &s.next_run_at {
                        let next = date(next)?;
                        if next <= now {
                            s.next_run_at = Some(timestamp(
                                s.cadence.advance_past(s.cadence.next(next)?, now)?,
                            ));
                            dirty = true;
                        }
                    }
                }
                if dirty {
                    s.updated_at = timestamp(now);
                    store.write(&s).await?;
                }
            }
        }
        for id in cleanup {
            self.archive_workspace_best_effort(&id).await;
        }
        Ok(())
    }
    async fn sweep_orphans(&self) -> Result<()> {
        let app = self.app()?;
        let store = self.store()?;
        for s in store.list().await? {
            let Target::Agent { agent_id } = &s.target else {
                continue;
            };
            if s.status == "completed" {
                continue;
            }
            let record = app.agents.agent_snapshot(agent_id).await?;
            if record.as_ref().is_none_or(|r| !r["archivedAt"].is_null()) {
                let _writer = store.writer.lock().await;
                if let Some(mut current) = store.get(&s.id).await? {
                    if current.status != "completed"
                        && matches!(&current.target,Target::Agent {agent_id:id} if id==agent_id)
                    {
                        current.complete(self.now());
                        store.write(&current).await?;
                    }
                }
            }
        }
        Ok(())
    }
    pub async fn handle(&self, context: &RequestContext, message: &Value) -> Result<bool> {
        let kind = message["type"].as_str().unwrap_or("");
        if !matches!(
            kind,
            "schedule/create"
                | "schedule/list"
                | "schedule/inspect"
                | "schedule/logs"
                | "schedule/pause"
                | "schedule/resume"
                | "schedule/delete"
                | "schedule/run-once"
                | "schedule/update"
        ) {
            return self.skills.handle(context, message).await;
        }
        let result: Result<Value> = async {
            let id = || string(message, "scheduleId");
            let mut payload = json!({"requestId":message["requestId"],"error":null});
            match kind {
                "schedule/create" => {
                    payload["schedule"] = schedule_summary(self.create(message).await?)
                }
                "schedule/list" => {
                    payload["schedules"] = Value::Array(
                        self.list()
                            .await?
                            .into_iter()
                            .map(schedule_summary)
                            .collect(),
                    )
                }
                "schedule/inspect" => payload["schedule"] = self.inspect(id()?).await?,
                "schedule/logs" => payload["runs"] = json!(self.logs(id()?).await?),
                "schedule/pause" => {
                    payload["schedule"] = schedule_summary(self.pause(id()?).await?)
                }
                "schedule/resume" => {
                    payload["schedule"] = schedule_summary(self.resume(id()?).await?)
                }
                "schedule/delete" => {
                    self.delete(id()?).await?;
                    payload["scheduleId"] = message["scheduleId"].clone();
                }
                "schedule/run-once" => payload["schedule"] = self.run_once(id()?).await?,
                "schedule/update" => {
                    let mut input = message.clone();
                    input["id"] = message["scheduleId"].clone();
                    payload["schedule"] = self.update(&input).await?;
                }
                _ => unreachable!(),
            }
            Ok(json!({"type":format!("{kind}/response"),"payload":payload}))
        }
        .await;
        let response = match result {
            Ok(v) => v,
            Err(e) => {
                json!({"type":"rpc_error","payload":{"requestId":message["requestId"],"requestType":kind,"error":e.to_string(),"code":"schedule_request_failed"}})
            }
        };
        context.send(response).await?;
        Ok(true)
    }
}
