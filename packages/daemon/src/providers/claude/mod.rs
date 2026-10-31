mod catalog;
mod options;
mod projection;
mod prompt;
mod storage;
mod transport;

use super::api::{
    AgentProvider, AgentSession, EventSink, ImportedSession, LaunchContext, ProviderSpec,
    RewindTarget,
};
use crate::process::{CommandSpec, run};
use crate::providers::registry::catalog::refresh_activity;
use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use projection::Projection;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use tokio_util::sync::CancellationToken;
use transport::Transport;

const CONTROL_TIMEOUT: Duration = Duration::from_secs(60);
fn capabilities() -> Value {
    json!({"supportsStreaming":true,"supportsSessionPersistence":true,"supportsSessionListing":true,"supportsDynamicModes":true,"supportsMcpServers":true,"supportsReasoningStream":true,"supportsToolInvocations":true,"supportsRewindConversation":true,"supportsRewindFiles":true,"supportsRewindBoth":true})
}
fn merged_env(spec: &ProviderSpec, context: Option<&LaunchContext>) -> BTreeMap<String, String> {
    options::provider_env(spec, context.map(|context| &context.env))
}

pub struct ClaudeProvider {
    spec: ProviderSpec,
    capabilities: Value,
    sessions: Mutex<Vec<Weak<ClaudeSession>>>,
}
impl ClaudeProvider {
    pub fn new(spec: ProviderSpec) -> Self {
        Self {
            spec,
            capabilities: capabilities(),
            sessions: Mutex::new(Vec::new()),
        }
    }

    async fn session(
        &self,
        mut config: Value,
        context: LaunchContext,
        options: Value,
        persistence: Option<Value>,
        sink: EventSink,
    ) -> Result<Arc<ClaudeSession>> {
        if let Some(handle) = &persistence {
            let mut merged = handle["metadata"].as_object().cloned().unwrap_or_default();
            if let Some(overrides) = config.as_object() {
                merged.extend(overrides.clone());
            }
            config = Value::Object(merged);
        }
        if !config.is_object() {
            bail!("Claude session configuration must be an object");
        }
        config["provider"] = json!(self.spec.id);
        if let Some(model) = config["model"].as_str() {
            let model = model.trim().to_owned();
            if model.is_empty() {
                config.as_object_mut().unwrap().remove("model");
            } else {
                config["model"] = json!(model);
            }
        }
        if let Some(provider_options) = config.get("providerOptions") {
            options::validate_provider_options(provider_options)?;
        }
        let tools = context
            .tool_catalog
            .as_array()
            .or_else(|| context.tool_catalog["tools"].as_array());
        if tools.is_some_and(|tools| !tools.is_empty()) && context.tool_executor.is_none() {
            bail!("Claude runtime tool catalog requires a native tool executor");
        }
        let cwd = config["cwd"]
            .as_str()
            .ok_or_else(|| anyhow!("Claude requires the original working directory"))?
            .to_owned();
        let env = merged_env(&self.spec, Some(&context));
        let mode = config["modeId"]
            .as_str()
            .filter(|mode| !mode.is_empty())
            .unwrap_or("default")
            .to_owned();
        options::validate_mode(&mode, &env)?;
        catalog::validate_thinking(
            config["model"].as_str(),
            config["thinkingOptionId"].as_str(),
        )?;
        let native_id = persistence
            .as_ref()
            .and_then(|p| p["sessionId"].as_str())
            .map(str::to_owned);
        if persistence.is_some() && native_id.is_none() {
            bail!("Cannot resume: persistence handle has no sessionId");
        }
        let mut history_events = Vec::new();
        let mut anchors = Vec::new();
        if let Some(id) = &native_id {
            let loaded = async {
                let path = storage::transcript_path(&cwd, id, &env).await?;
                storage::read_history(&path).await
            }
            .await;
            match loaded {
                Ok(history) => {
                    for entry in &history.parent_entries {
                        remember_anchor(&mut anchors, entry);
                    }
                    history_events = projection::project_history(&self.spec.id, &history);
                }
                Err(error) => {
                    tracing::warn!(error=%error,"Claude persisted history could not be loaded; retaining native resume handle")
                }
            }
        }
        let features = Arc::<[Value]>::from(catalog::features(&config));
        let provider = self.spec.id.clone();
        let session = Arc::new_cyclic(|weak| ClaudeSession {
            weak: weak.clone(),
            spec: self.spec.clone(),
            context,
            sink,
            capabilities: capabilities(),
            history_events: Mutex::new(history_events),
            env,
            persist: options["persistSession"].as_bool(),
            usage_key: uuid::Uuid::new_v4().to_string(),
            gate: tokio::sync::Mutex::new(()),
            state: Mutex::new(State {
                config,
                features,
                native_id,
                fresh_id: None,
                mode: mode.clone(),
                plan_resume: if mode == "plan" { None } else { Some(mode) },
                runtime: None,
                initialized: Value::Null,
                restart: false,
                closed: false,
                active_turn: None,
                active_generation: None,
                main_running: false,
                interrupt_pending: false,
                unstarted: HashSet::new(),
                clearing_steers: HashSet::new(),
                permissions: HashMap::new(),
                anchors,
                projection: Projection::new(provider),
                runtime_model: None,
            }),
        });
        self.sessions.lock().unwrap().push(Arc::downgrade(&session));
        Ok(session)
    }

    fn command(&self, args: &[&str]) -> Result<CommandSpec> {
        let (program, prefix) = self
            .spec
            .command
            .split_first()
            .ok_or_else(|| anyhow!("Claude executable command is empty"))?;
        let mut argv = prefix.to_vec();
        argv.extend(args.iter().map(|v| (*v).to_owned()));
        Ok(CommandSpec {
            program: program.clone(),
            args: argv,
            cwd: None,
            env: merged_env(&self.spec, None),
            clear_env: true,
        })
    }

    async fn discover(&self, cancel: CancellationToken) -> Result<Value> {
        let home = dirs::home_dir().ok_or_else(|| anyhow!("Cannot resolve home directory"))?;
        let launch = options::discovery_launch(&self.spec, &home)?;
        let (transport, mut incoming) = Transport::spawn(launch.command, cancel).await?;
        let responder = transport.clone();
        let pump = tokio::spawn(async move {
            while let Some(frame) = incoming.recv().await {
                let frame = frame?;
                if frame["type"] == "control_request" {
                    let id = frame["request_id"]
                        .as_str()
                        .ok_or_else(|| anyhow!("Claude control request has no id"))?;
                    responder
                        .reply(
                            id,
                            Err(anyhow!(
                                "Model discovery does not grant tool or hook execution"
                            )),
                        )
                        .await?;
                }
            }
            Ok::<(), anyhow::Error>(())
        });
        let result = transport
            .request(launch.initialize, Duration::from_secs(5))
            .await;
        pump.abort();
        let _ = pump.await;
        let closed = transport.close().await;
        closed?;
        result
    }
}

#[async_trait]
impl AgentProvider for ClaudeProvider {
    fn id(&self) -> &str {
        &self.spec.id
    }
    fn capabilities(&self) -> &Value {
        &self.capabilities
    }
    fn resolve_configured_model(&self, model: Value) -> Result<Value> {
        Ok(catalog::resolve_configured_model(model))
    }
    async fn resolve_default_mode(
        &self,
        input: Value,
        _cancel: CancellationToken,
    ) -> Result<Option<String>> {
        let overrides = input["config"]["env"].as_object().map(|values| {
            values
                .iter()
                .filter_map(|(key, value)| {
                    value.as_str().map(|value| (key.clone(), value.to_owned()))
                })
                .collect::<BTreeMap<_, _>>()
        });
        let env = options::provider_env(&self.spec, overrides.as_ref());
        Ok(Some(
            if catalog::auto_mode_transport(&env).is_some() {
                "default"
            } else {
                "auto"
            }
            .to_owned(),
        ))
    }
    async fn create_session(
        &self,
        config: Value,
        context: LaunchContext,
        options: Value,
        sink: EventSink,
    ) -> Result<Arc<dyn AgentSession>> {
        Ok(self.session(config, context, options, None, sink).await?)
    }
    async fn resume_session(
        &self,
        persistence: Value,
        config: Value,
        context: LaunchContext,
        options: Value,
        sink: EventSink,
    ) -> Result<Arc<dyn AgentSession>> {
        Ok(self
            .session(config, context, options, Some(persistence), sink)
            .await?)
    }
    async fn catalog_cache_key(
        &self,
        _options: Value,
        _cancel: CancellationToken,
    ) -> Result<Option<String>> {
        Ok(Some("host".to_owned()))
    }
    async fn fetch_catalog(&self, _options: Value, cancel: CancellationToken) -> Result<Value> {
        let version = refresh_activity("version", async {
            run(
                self.command(&["--version"])?,
                65536,
                Duration::from_secs(5),
                cancel.clone(),
            )
            .await
        })
        .await
        .ok()
        .and_then(|output| {
            let value = format!(
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                output.stderr
            );
            catalog::executable_version(&value)
                .map(|version| format!("{}.{}.{}", version[0], version[1], version[2]))
        });
        let initialize = match refresh_activity("models", self.discover(cancel.clone())).await {
            Ok(value) => value,
            Err(error) => {
                if cancel.is_cancelled() {
                    return Err(error);
                }
                tracing::warn!(error=%error,"Claude model discovery failed; omitting discovery-required models");
                Value::Null
            }
        };
        let env = merged_env(&self.spec, None);
        let config_dir = storage::config_dir(&env)?;
        let mut result = refresh_activity("settings", async {
            Ok(catalog::catalog(&config_dir, version.as_deref(), &initialize, &env).await)
        })
        .await?;
        if cancel.is_cancelled() {
            bail!("Claude catalog refresh canceled");
        }
        if let Some(models) = result["models"].as_array_mut() {
            for model in &self.spec.configured_models {
                let model = catalog::resolve_configured_model(model.clone());
                if let Some(index) = models.iter().position(|m| m["id"] == model["id"]) {
                    models[index] = model;
                } else {
                    models.push(model);
                }
            }
            for model in models {
                model["provider"] = json!(self.spec.id);
            }
        }
        Ok(result)
    }
    async fn list_features(&self, config: &Value) -> Result<Vec<Value>> {
        Ok(catalog::features(config))
    }
    async fn list_commands(&self, _config: &Value) -> Result<Vec<Value>> {
        Ok(catalog::commands(
            &self.discover(CancellationToken::new()).await?,
        ))
    }
    async fn list_importable_sessions(&self, options: Value) -> Result<Vec<Value>> {
        storage::list_importable_sessions(&merged_env(&self.spec, None), &options).await
    }
    async fn import_session(
        &self,
        input: Value,
        mut config: Value,
        mut stored_config: Value,
        context: LaunchContext,
        sink: EventSink,
    ) -> Result<ImportedSession> {
        let id = input["providerHandleId"]
            .as_str()
            .ok_or_else(|| anyhow!("Claude import requires providerHandleId"))?;
        if !config.is_object() {
            config = json!({});
        }
        config["cwd"] = input["cwd"].clone();
        stored_config["cwd"] = input["cwd"].clone();
        stored_config["provider"] = json!(self.spec.id);
        config["provider"] = json!(self.spec.id);
        let cwd = config["cwd"]
            .as_str()
            .ok_or_else(|| anyhow!("Claude import requires cwd"))?;
        if let Some(title) =
            storage::renamed_title(cwd, id, &merged_env(&self.spec, Some(&context))).await?
        {
            config["title"] = json!(title);
            stored_config["title"] = json!(title);
        }
        let persistence = json!({"provider":self.spec.id,"sessionId":id,"nativeHandle":id,"metadata":stored_config});
        let session = self
            .session(config, context, json!({}), Some(persistence.clone()), sink)
            .await?;
        let timeline = session
            .history_events
            .lock()
            .unwrap()
            .iter()
            .filter(|v| v["type"] == "timeline")
            .map(|v| json!({"item":v["item"],"timestamp":v["timestamp"]}))
            .collect();
        let provider_subagent_events = session
            .history_events
            .lock()
            .unwrap()
            .iter()
            .filter(|v| v["type"] == "provider_subagent")
            .cloned()
            .collect();
        Ok(ImportedSession {
            session,
            config: stored_config,
            persistence,
            timeline,
            provider_subagent_events,
        })
    }
    async fn is_available(&self, _options: Value, cancel: CancellationToken) -> Result<bool> {
        if cancel.is_cancelled() {
            bail!("Claude availability probe canceled");
        }
        let Some(program) = self.spec.command.first() else {
            return Ok(false);
        };
        let env = merged_env(&self.spec, None);
        Ok(which::which_in(program, env.get("PATH"), std::env::current_dir()?).is_ok())
    }
    async fn diagnostic(&self) -> Result<String> {
        match run(
            self.command(&["auth", "status"])?,
            65536,
            Duration::from_secs(5),
            CancellationToken::new(),
        )
        .await
        {
            Ok(output) => Ok(format!(
                "Claude Code\nCommand: {}\nAuth: {}{}",
                self.spec
                    .command
                    .first()
                    .map(String::as_str)
                    .unwrap_or("claude"),
                String::from_utf8_lossy(&output.stdout),
                output.stderr
            )),
            Err(error) => Ok(format!("Claude Code\nAuth diagnostic failed: {error}")),
        }
    }
    // Claude has no native archive mutation; archival belongs to Paseo's store.
    async fn archive_native_session(&self, _persistence: &Value) -> Result<()> {
        Ok(())
    }
    async fn unarchive_native_session(&self, _persistence: &Value) -> Result<()> {
        Ok(())
    }
    async fn shutdown(&self) -> Result<()> {
        let sessions: Vec<_> = self
            .sessions
            .lock()
            .unwrap()
            .drain(..)
            .filter_map(|s| s.upgrade())
            .collect();
        let mut failure = None;
        for session in sessions {
            if let Err(error) = session.close().await {
                failure = Some(error);
            }
        }
        if let Some(error) = failure {
            Err(error)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone)]
struct Permission {
    request: Value,
    native_id: String,
}

struct State {
    config: Value,
    features: Arc<[Value]>,
    native_id: Option<String>,
    fresh_id: Option<String>,
    mode: String,
    plan_resume: Option<String>,
    runtime: Option<Arc<Transport>>,
    initialized: Value,
    restart: bool,
    closed: bool,
    active_turn: Option<String>,
    active_generation: Option<String>,
    main_running: bool,
    interrupt_pending: bool,
    unstarted: HashSet<String>,
    clearing_steers: HashSet<String>,
    permissions: HashMap<String, Permission>,
    anchors: Vec<(String, Option<String>)>,
    projection: Projection,
    runtime_model: Option<String>,
}
struct ClaudeSession {
    weak: Weak<Self>,
    spec: ProviderSpec,
    context: LaunchContext,
    sink: EventSink,
    capabilities: Value,
    history_events: Mutex<Vec<Value>>,
    env: BTreeMap<String, String>,
    persist: Option<bool>,
    usage_key: String,
    gate: tokio::sync::Mutex<()>,
    state: Mutex<State>,
}

fn remember_anchor(anchors: &mut Vec<(String, Option<String>)>, frame: &Value) {
    if frame["parent_tool_use_id"].as_str().is_some() || frame["isSidechain"] == true {
        return;
    }
    let Some(id) = frame["uuid"].as_str() else {
        return;
    };
    if frame["type"] == "user" && frame["isMeta"] != true && frame["isSynthetic"] != true {
        let tool_result = frame["message"]["content"]
            .as_array()
            .is_some_and(|blocks| blocks.iter().any(|v| v["type"] == "tool_result"));
        if !tool_result && !anchors.iter().any(|a| a.0 == id) {
            anchors.push((id.to_owned(), None));
        }
    } else if frame["type"] == "assistant"
        || (frame["type"] == "stream_event" && frame["event"]["type"] == "message_start")
    {
        if let Some(anchor) = anchors.last_mut() {
            anchor.1 = Some(id.to_owned());
        }
    }
}

impl ClaudeSession {
    async fn emit(&self, events: Vec<Value>) -> Result<()> {
        for event in events {
            self.sink.emit(event).await?;
        }
        Ok(())
    }

    async fn ensure_runtime(&self) -> Result<Arc<Transport>> {
        let _gate = self.gate.lock().await;
        let (old, config, resume, fresh) = {
            let mut state = self.state.lock().unwrap();
            if state.closed {
                bail!("Claude session is closed");
            }
            if let Some(runtime) = &state.runtime {
                if !state.restart
                    || state.active_generation.as_deref() == Some(runtime.generation.as_str())
                    || state.projection.is_autonomous()
                {
                    return Ok(runtime.clone());
                }
            }
            (
                state.runtime.take(),
                state.config.clone(),
                state.native_id.clone(),
                state.fresh_id.clone(),
            )
        };
        if let Some(old) = old {
            old.close().await?;
        }
        let launch = options::build_launch(
            &self.spec,
            &self.context,
            &config,
            resume.as_deref(),
            fresh.as_deref(),
            self.persist,
        )?;
        let (runtime, mut incoming) =
            Transport::spawn(launch.command, self.context.cancel.clone()).await?;
        let accepted = {
            let mut state = self.state.lock().unwrap();
            if state.closed {
                false
            } else {
                state.runtime = Some(runtime.clone());
                state.restart = false;
                true
            }
        };
        if !accepted {
            runtime.close().await?;
            bail!("Claude session closed during initialization");
        }
        let weak = self.weak.clone();
        let active = runtime.clone();
        tokio::spawn(async move {
            while let Some(frame) = incoming.recv().await {
                let Some(session) = weak.upgrade() else {
                    let _ = active.close().await;
                    return;
                };
                if !session.is_current(&active) {
                    return;
                }
                match frame {
                    Ok(frame) => {
                        if let Err(error) = session.receive(&active, frame).await {
                            session.runtime_failed(&active, error.to_string()).await;
                            return;
                        }
                    }
                    Err(error) => {
                        session.runtime_failed(&active, error.to_string()).await;
                        return;
                    }
                }
            }
        });
        let initialized = runtime.request(launch.initialize, CONTROL_TIMEOUT).await;
        match initialized {
            Ok(initialized) => self.state.lock().unwrap().initialized = initialized,
            Err(error) => {
                {
                    let mut state = self.state.lock().unwrap();
                    state.runtime = None;
                }
                let _ = runtime.close().await;
                return Err(error);
            }
        }
        if let Some(enabled) = launch.fast_mode {
            runtime
                .request(
                    json!({"subtype":"apply_flag_settings","settings":{"fastMode":enabled}}),
                    CONTROL_TIMEOUT,
                )
                .await?;
        }
        Ok(runtime)
    }

    fn is_current(&self, runtime: &Transport) -> bool {
        self.state
            .lock()
            .unwrap()
            .runtime
            .as_ref()
            .is_some_and(|r| r.generation == runtime.generation)
    }

    async fn runtime_failed(&self, runtime: &Transport, error: String) {
        let mut events = {
            let mut state = self.state.lock().unwrap();
            if !state
                .runtime
                .as_ref()
                .is_some_and(|r| r.generation == runtime.generation)
            {
                return;
            }
            state.runtime = None;
            state.active_turn = None;
            state.active_generation = None;
            state.main_running = false;
            state.interrupt_pending = false;
            state.unstarted.clear();
            state.clearing_steers.clear();
            let mut events: Vec<_> = state.permissions.drain().map(|(id,_)|json!({"type":"permission_resolved","provider":self.spec.id,"requestId":id,"resolution":{"behavior":"deny","message":error}})).collect();
            events.extend(state.projection.runtime_failed(&error));
            events
        };
        let _ = runtime.close().await;
        let diagnostic = runtime.process.stderr_tail();
        if !diagnostic.trim().is_empty() {
            for event in &mut events {
                if event["type"] == "turn_failed" {
                    event["diagnostic"] = json!(diagnostic);
                }
            }
        }
        let _ = self.emit(events).await;
    }

    async fn receive(&self, runtime: &Transport, frame: Value) -> Result<()> {
        if frame["type"] == "control_request" {
            let id = frame["request_id"]
                .as_str()
                .ok_or_else(|| anyhow!("Claude control request has no request_id"))?;
            let request = &frame["request"];
            match request["subtype"].as_str() {
                Some("can_use_tool") => {
                    let public_id = format!("permission-{}", uuid::Uuid::new_v4());
                    let (permission, superseded) = {
                        let mut state = self.state.lock().unwrap();
                        let request = projection::permission_request(
                            &self.spec.id,
                            &public_id,
                            request,
                            state.plan_resume.as_deref(),
                        );
                        let superseded = !state.clearing_steers.is_empty();
                        state.permissions.insert(
                            public_id.clone(),
                            Permission {
                                request: request.clone(),
                                native_id: id.to_owned(),
                            },
                        );
                        (request, superseded)
                    };
                    self.sink.emit(json!({"type":"permission_requested","provider":self.spec.id,"request":permission})).await?;
                    if superseded {
                        self.respond_to_permission(&public_id,json!({"behavior":"deny","message":"The user answered with a message instead of approving. Their message follows."})).await?;
                    }
                    return Ok(());
                }
                Some("hook_callback") => {
                    let (events, response) = {
                        let mut state = self.state.lock().unwrap();
                        let events = state.projection.observe_hook(&request["input"]);
                        let response = options::hook_response(
                            request["callback_id"].as_str().unwrap_or(""),
                            &request["input"],
                            &state.config,
                        );
                        (events, response)
                    };
                    runtime.reply(id, response).await?;
                    return self.emit(events).await;
                }
                _ => {
                    return runtime
                        .reply(
                            id,
                            Err(anyhow!(
                                "Unsupported Claude control request '{}'; no authority granted",
                                request["subtype"].as_str().unwrap_or("unknown")
                            )),
                        )
                        .await;
                }
            }
        }
        if frame["type"] == "control_cancel_request" {
            if let Some(id) = frame["request_id"].as_str() {
                let public_id = {
                    let mut state = self.state.lock().unwrap();
                    let public_id = state
                        .permissions
                        .iter()
                        .find(|(_, permission)| permission.native_id == id)
                        .map(|(id, _)| id.clone());
                    if let Some(id) = &public_id {
                        state.permissions.remove(id);
                    }
                    public_id
                };
                if let Some(id) = public_id {
                    self.sink.emit(json!({"type":"permission_resolved","provider":self.spec.id,"requestId":id,"resolution":{"behavior":"deny","message":"Permission request canceled"}})).await?;
                }
            }
            return Ok(());
        }
        let events = {
            let mut state = self.state.lock().unwrap();
            if frame["parent_tool_use_id"].as_str().is_some() {
                state.projection.project(&frame)
            } else {
                if let Some(id) = frame["session_id"].as_str().filter(|id| !id.is_empty()) {
                    state.native_id = Some(id.into());
                    state.fresh_id = None;
                }
                if frame["type"] == "system" {
                    match frame["subtype"].as_str() {
                        Some("init") => {
                            state.main_running = true;
                            state.runtime_model = frame["model"].as_str().map(str::to_owned);
                            state.interrupt_pending = false;
                            if let Some(mode) = frame["permissionMode"].as_str() {
                                state.mode = mode.to_owned();
                                state.config["modeId"] = json!(mode);
                                if mode != "plan" {
                                    state.plan_resume = Some(mode.to_owned());
                                }
                            }
                            state.projection.set_context_window(catalog::context_window(
                                frame["model"].as_str(),
                            ));
                        }
                        Some("commands_changed") => {
                            state.initialized["commands"] = frame["commands"].clone()
                        }
                        Some("session_state_changed") if frame["state"] == "idle" => {
                            state.main_running = false
                        }
                        _ => {}
                    }
                }
                if frame["type"] == "command_lifecycle" {
                    if frame["state"] != "queued" {
                        if let Some(id) = frame["command_uuid"].as_str() {
                            state.unstarted.remove(id);
                            state.clearing_steers.remove(id);
                        }
                    }
                    if frame["state"] == "started" {
                        state.main_running = true;
                    }
                }
                remember_anchor(&mut state.anchors, &frame);
                if frame["type"] == "result" && frame["parent_tool_use_id"].as_str().is_none() {
                    state.main_running = false;
                    if state.interrupt_pending {
                        state.interrupt_pending = false;
                        if frame["subtype"] != "success" {
                            return Ok(());
                        }
                    }
                    let aborted = frame["errors"].as_array().is_some_and(|errors| {
                        errors.iter().filter_map(Value::as_str).any(|text| {
                            text.split(|ch: char| !ch.is_alphanumeric() && ch != '_')
                                .any(|word| word.eq_ignore_ascii_case("aborted"))
                        })
                    });
                    if frame["subtype"] != "success" && aborted {
                        return Ok(());
                    }
                    state.active_turn = None;
                    state.active_generation = None;
                }
                state.projection.project(&frame)
            }
        };
        self.emit(events).await
    }

    async fn deny_permissions(&self, message: &str) -> Result<()> {
        let ids: Vec<_> = self
            .state
            .lock()
            .unwrap()
            .permissions
            .keys()
            .cloned()
            .collect();
        for id in ids {
            self.respond_to_permission(&id, json!({"behavior":"deny","message":message}))
                .await?;
        }
        Ok(())
    }
}

#[async_trait]
impl AgentSession for ClaudeSession {
    fn provider(&self) -> &str {
        &self.spec.id
    }
    fn native_id(&self) -> Option<String> {
        self.state.lock().unwrap().native_id.clone()
    }
    fn capabilities(&self) -> &Value {
        &self.capabilities
    }
    fn features(&self) -> Arc<[Value]> {
        self.state.lock().unwrap().features.clone()
    }
    fn initial_timeline(&self) -> &[Value] {
        &[]
    }
    fn describe_persistence(&self) -> Option<Value> {
        let state = self.state.lock().unwrap();
        let id = state.native_id.as_ref()?;
        let mut metadata = state.config.clone();
        if let Some(object) = metadata.as_object_mut() {
            object.remove("providerOptions");
            object.remove("mcpServers");
            object.remove("toolPolicy");
            object.remove("daemonAppendSystemPrompt");
        }
        Some(json!({"provider":self.spec.id,"sessionId":id,"nativeHandle":id,"metadata":metadata}))
    }
    fn pending_permissions(&self) -> Vec<Value> {
        self.state
            .lock()
            .unwrap()
            .permissions
            .values()
            .map(|permission| permission.request.clone())
            .collect()
    }
    fn usage_session(&self) -> Option<Value> {
        let state = self.state.lock().unwrap();
        if state.closed {
            return None;
        }
        Some(
            json!({"provider":self.spec.id,"model":state.config["model"],"env":self.env,"sessionKey":self.usage_key}),
        )
    }

    async fn start_turn(&self, prompt: Value, options: Value) -> Result<Value> {
        if prompt
            .as_str()
            .is_some_and(|text| text.trim().split_whitespace().next() == Some("/rewind"))
        {
            return self.rewind_command(&prompt).await;
        }
        let (message, turn_id, events) = {
            let mut state = self.state.lock().unwrap();
            if state.closed {
                bail!("Claude session is closed");
            }
            if state.active_turn.is_some() {
                bail!("A foreground turn is already active");
            }
            let message = prompt::user_message(&prompt, state.native_id.as_deref())?;
            let turn_id = state.projection.next_foreground_turn_id();
            state.active_turn = Some(turn_id.clone());
            remember_anchor(&mut state.anchors, &message);
            let events = state.projection.begin_turn(turn_id.clone());
            (message, turn_id, events)
        };
        self.emit(events).await?;
        let runtime = match self.ensure_runtime().await {
            Ok(runtime) => runtime,
            Err(error) => {
                let events = {
                    let mut state = self.state.lock().unwrap();
                    state.active_turn = None;
                    state.projection.runtime_failed(&error.to_string())
                };
                self.emit(events).await?;
                return Ok(json!({"turnId":turn_id}));
            }
        };
        {
            let mut state = self.state.lock().unwrap();
            if state.active_turn.as_deref() != Some(&turn_id) {
                return Ok(json!({"turnId":turn_id}));
            }
            state.active_generation = Some(runtime.generation.clone());
            state
                .unstarted
                .insert(message["uuid"].as_str().unwrap().into());
        }
        if let Err(error) = runtime.process.write_json(&message).await {
            self.runtime_failed(&runtime, error.to_string()).await;
            return Err(error);
        }
        let mut user = message.clone();
        if let Some(id) = options["clientMessageId"].as_str() {
            user["clientMessageId"] = json!(id);
        }
        let events = self.state.lock().unwrap().projection.project(&user);
        self.emit(events).await?;
        Ok(json!({"turnId":turn_id}))
    }

    async fn steer_active_turn(&self, prompt: Value, options: Value) -> Result<Value> {
        if prompt.as_str().is_some_and(|p| p.trim().starts_with('/')) {
            return Ok(json!({"status":"unavailable"}));
        }
        let (runtime, message) = {
            let mut state = self.state.lock().unwrap();
            let active_turn = state
                .active_turn
                .as_deref()
                .or_else(|| state.projection.active_turn_id());
            if active_turn != options["expectedTurnId"].as_str()
                || active_turn.is_none()
                || state.projection.is_compacting()
            {
                return Ok(json!({"status":"unavailable"}));
            }
            let Some(runtime) = state.runtime.clone() else {
                return Ok(json!({"status":"unavailable"}));
            };
            let mut message = prompt::user_message(&prompt, state.native_id.as_deref())?;
            message["priority"] = json!("next");
            state
                .unstarted
                .insert(message["uuid"].as_str().unwrap().into());
            if options["clearPendingPermissions"] == true {
                state
                    .clearing_steers
                    .insert(message["uuid"].as_str().unwrap().into());
            }
            (runtime, message)
        };
        runtime.process.write_json(&message).await?;
        if options["clearPendingPermissions"] == true {
            self.deny_permissions(
                "The user answered with a message instead of approving. Their message follows.",
            )
            .await?;
        }
        Ok(json!({"status":"accepted"}))
    }

    async fn stream_history(&self, sink: EventSink) -> Result<()> {
        let events = std::mem::take(&mut *self.history_events.lock().unwrap());
        for event in events {
            sink.emit(event).await?;
        }
        Ok(())
    }
    async fn runtime_info(&self) -> Result<Value> {
        let state = self.state.lock().unwrap();
        let model = catalog::normalize_runtime_model_id(state.runtime_model.as_deref())
            .map(Value::from)
            .unwrap_or_else(|| state.config["model"].clone());
        let mut info = json!({"provider":self.spec.id,"sessionId":state.native_id,"model":model,"modeId":state.mode});
        if let Some(model) = &state.runtime_model {
            info["extra"] = json!({"runtimeModel":model});
        }
        Ok(info)
    }
    async fn available_modes(&self) -> Result<Vec<Value>> {
        Ok(catalog::mode_catalog(&self.env)["modes"]
            .as_array()
            .cloned()
            .unwrap_or_default())
    }
    async fn current_mode(&self) -> Result<Option<String>> {
        Ok(Some(self.state.lock().unwrap().mode.clone()))
    }
    async fn set_mode(&self, mode: &str) -> Result<Option<Value>> {
        options::validate_mode(mode, &self.env)?;
        let runtime = self.ensure_runtime().await?;
        runtime
            .request(
                json!({"subtype":"set_permission_mode","mode":mode}),
                CONTROL_TIMEOUT,
            )
            .await?;
        let mut state = self.state.lock().unwrap();
        if mode == "plan" {
            if state.mode != "plan" {
                state.plan_resume = Some(state.mode.clone());
            }
        } else {
            state.plan_resume = Some(mode.into());
        }
        state.mode = mode.into();
        state.config["modeId"] = json!(mode);
        Ok(None)
    }

    async fn respond_to_permission(
        &self,
        request_id: &str,
        response: Value,
    ) -> Result<Option<Value>> {
        let (permission, runtime, resume) = {
            let state = self.state.lock().unwrap();
            let request =
                state.permissions.get(request_id).cloned().ok_or_else(|| {
                    anyhow!("No pending permission request with id '{request_id}'")
                })?;
            let runtime = state
                .runtime
                .clone()
                .ok_or_else(|| anyhow!("Claude permission transport closed"))?;
            (request, runtime, state.plan_resume.clone())
        };
        let request = permission.request;
        let native_response = projection::permission_response(&request, &response)?;
        self.state
            .lock()
            .unwrap()
            .permissions
            .remove(request_id)
            .ok_or_else(|| {
                anyhow!("Permission request '{request_id}' was already resolved or canceled")
            })?;
        if request["kind"] == "plan" && response["behavior"] == "allow" {
            let prior = resume
                .as_deref()
                .filter(|m| matches!(*m, "bypassPermissions" | "auto"));
            let mode = if response["selectedActionId"] == "implement_resume" {
                prior.unwrap_or("acceptEdits")
            } else {
                "acceptEdits"
            };
            self.set_mode(mode).await?;
        }
        runtime
            .reply(&permission.native_id, Ok(native_response))
            .await?;
        self.emit(projection::permission_resolution_events(
            &self.spec.id,
            &request,
            &response,
        ))
        .await?;
        Ok(None)
    }

    async fn interrupt(&self) -> Result<()> {
        let (runtime, unstarted, started, events) = {
            let mut state = self.state.lock().unwrap();
            let runtime = state.runtime.clone();
            let unstarted = state.unstarted.drain().collect::<Vec<_>>();
            let started = state.main_running;
            state.active_turn = None;
            state.active_generation = None;
            state.clearing_steers.clear();
            let events = state.projection.interrupt();
            (runtime, unstarted, started, events)
        };
        self.emit(events).await?;
        self.deny_permissions("Permission request canceled").await?;
        let Some(runtime) = runtime else {
            return Ok(());
        };
        let mut withdrew_all = true;
        for id in unstarted {
            match runtime
                .request(
                    json!({"subtype":"cancel_async_message","message_uuid":id}),
                    Duration::from_secs(3),
                )
                .await
            {
                Ok(response) if response["cancelled"] == true => {}
                _ => withdrew_all = false,
            }
        }
        // An idle main session can still own running helpers. Do not send an
        // interrupt unless a prompt was taken by the main session.
        if !started && withdrew_all {
            return Ok(());
        }
        self.state.lock().unwrap().interrupt_pending = true;
        runtime
            .request(json!({"subtype":"interrupt"}), Duration::from_secs(3))
            .await?;
        Ok(())
    }

    async fn close(&self) -> Result<()> {
        let (runtime, id, cwd, events) = {
            let mut state = self.state.lock().unwrap();
            if state.closed {
                return Ok(());
            }
            state.closed = true;
            state.active_turn = None;
            state.active_generation = None;
            let mut events: Vec<_> = state.permissions.drain().map(|(id,_)|json!({"type":"permission_resolved","provider":self.spec.id,"requestId":id,"resolution":{"behavior":"deny","message":"Claude session closed"}})).collect();
            events.extend(state.projection.interrupt());
            (
                state.runtime.take(),
                state.native_id.clone(),
                state.config["cwd"].as_str().unwrap_or("").to_owned(),
                events,
            )
        };
        if let Some(runtime) = runtime {
            runtime.close().await?;
        }
        let _ = self.emit(events).await;
        if self.persist == Some(false) {
            if let Some(id) = id {
                if let Ok(path) = storage::transcript_path(&cwd, &id, &self.env).await {
                    match tokio::fs::remove_file(path).await {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error.into()),
                    }
                }
            }
        }
        Ok(())
    }
    async fn list_commands(&self) -> Result<Vec<Value>> {
        self.ensure_runtime().await?;
        Ok(catalog::commands(&self.state.lock().unwrap().initialized))
    }
    async fn set_model(&self, model: Option<&str>) -> Result<()> {
        let model = model.map(str::trim).filter(|m| !m.is_empty());
        let runtime = self.ensure_runtime().await?;
        runtime
            .request(
                json!({"subtype":"set_model","model":model}),
                CONTROL_TIMEOUT,
            )
            .await?;
        let (disable_fast, thinking_changed) = {
            let mut state = self.state.lock().unwrap();
            state.config["model"] = json!(model);
            state.runtime_model = None;
            let changed =
                catalog::validate_thinking(model, state.config["thinkingOptionId"].as_str())
                    .is_err();
            if changed {
                state.config["thinkingOptionId"] =
                    json!(catalog::disabled_thinking_fallback(model));
                state.restart = true;
            }
            state
                .projection
                .set_context_window(catalog::context_window(model));
            state.features = Arc::from(catalog::features(&state.config));
            (
                !catalog::supports_fast(model)
                    && state.config["featureValues"]["fast_mode"] == true,
                changed.then(|| state.config["thinkingOptionId"].clone()),
            )
        };
        if let Some(option) = thinking_changed {
            self.sink.emit(json!({"type":"thinking_option_changed","provider":self.spec.id,"thinkingOptionId":option})).await?;
        }
        if disable_fast {
            self.set_feature("fast_mode", json!(false)).await?;
        }
        Ok(())
    }
    async fn set_thinking_option(&self, option: Option<&str>) -> Result<Option<Value>> {
        let option = option.filter(|o| !o.is_empty() && *o != "default");
        let mut state = self.state.lock().unwrap();
        catalog::validate_thinking(state.config["model"].as_str(), option)?;
        state.config["thinkingOptionId"] = json!(option);
        state.restart = true;
        Ok(state
            .projection
            .active_turn_id()
            .map(|_| json!({"type":"warning","message":"Thinking level applies next turn"})))
    }
    async fn set_feature(&self, feature: &str, value: Value) -> Result<()> {
        if feature != "fast_mode" {
            bail!("Unknown Claude feature: {feature}");
        }
        let enabled = value
            .as_bool()
            .ok_or_else(|| anyhow!("Claude fast_mode must be a boolean"))?;
        let runtime = {
            let state = self.state.lock().unwrap();
            if enabled && !catalog::supports_fast(state.config["model"].as_str()) {
                bail!("Claude fast mode is not available for this model");
            }
            state.runtime.clone()
        };
        if let Some(runtime) = runtime {
            runtime
                .request(
                    json!({"subtype":"apply_flag_settings","settings":{"fastMode":enabled}}),
                    CONTROL_TIMEOUT,
                )
                .await?;
        }
        let mut state = self.state.lock().unwrap();
        state.config["featureValues"]["fast_mode"] = json!(enabled);
        state.features = Arc::from(catalog::features(&state.config));
        Ok(())
    }

    async fn rewind(&self, target: RewindTarget, message_id: &str) -> Result<()> {
        {
            if self.state.lock().unwrap().active_turn.is_some() {
                bail!("Cannot rewind while a Claude foreground turn is active");
            }
        }
        if matches!(target, RewindTarget::Files | RewindTarget::Both) {
            let runtime = self.ensure_runtime().await?;
            let result = runtime
                .request(
                    json!({"subtype":"rewind_files","user_message_id":message_id,"dry_run":false}),
                    CONTROL_TIMEOUT,
                )
                .await?;
            if result["canRewind"] != true {
                bail!(
                    "{}",
                    result["error"]
                        .as_str()
                        .unwrap_or("No file checkpoint available")
                );
            }
        }
        if matches!(target, RewindTarget::Conversation | RewindTarget::Both) {
            let (id, cwd, cut) = {
                let state = self.state.lock().unwrap();
                let index = state
                    .anchors
                    .iter()
                    .position(|a| a.0 == message_id)
                    .ok_or_else(|| {
                        anyhow!(
                            "Claude rewind target {message_id} is not in the tracked conversation"
                        )
                    })?;
                let cut = state.anchors[..index]
                    .iter()
                    .rev()
                    .find_map(|a| a.1.clone());
                (
                    state.native_id.clone(),
                    state.config["cwd"].as_str().unwrap_or("").to_owned(),
                    cut,
                )
            };
            let fresh = cut.is_none();
            let next = if let Some(cut) = cut {
                let id = id
                    .as_deref()
                    .ok_or_else(|| anyhow!("Cannot fork Claude session without native ID"))?;
                let path = storage::transcript_path(&cwd, id, &self.env).await?;
                storage::fork_session(&path, id, Some(&cut)).await?
            } else {
                uuid::Uuid::new_v4().to_string()
            };
            let old = {
                let mut state = self.state.lock().unwrap();
                state.native_id = Some(next.clone());
                state.fresh_id = fresh.then(|| next.clone());
                state.restart = true;
                state.anchors.clear();
                state.initialized = Value::Null;
                state.projection.reset_conversation();
                state.runtime.take()
            };
            if let Some(old) = old {
                old.close().await?;
            }
            self.history_events.lock().unwrap().clear();
            if !fresh {
                let path = storage::transcript_path(&cwd, &next, &self.env).await?;
                let history = storage::read_history(&path).await?;
                *self.history_events.lock().unwrap() =
                    projection::project_history(&self.spec.id, &history);
                let mut state = self.state.lock().unwrap();
                for entry in history.parent_entries {
                    remember_anchor(&mut state.anchors, &entry);
                }
            }
            self.sink
                .emit(json!({"type":"thread_started","provider":self.spec.id,"sessionId":next}))
                .await?;
        }
        Ok(())
    }
    async fn try_out_of_band(&self, _prompt: &Value) -> Result<bool> {
        Ok(false)
    }
}

impl ClaudeSession {
    async fn rewind_command(&self, prompt: &Value) -> Result<Value> {
        let explicit = prompt
            .as_str()
            .unwrap_or("")
            .split_whitespace()
            .nth(1)
            .map(str::to_owned);
        let (turn_id, events) = {
            let mut state = self.state.lock().unwrap();
            if state.closed {
                bail!("Claude session is closed");
            }
            if state.active_turn.is_some() {
                bail!("A foreground turn is already active");
            }
            let turn_id = state.projection.next_foreground_turn_id();
            state.active_turn = Some(turn_id.clone());
            state.active_generation = None;
            let events = state.projection.begin_turn(turn_id.clone());
            (turn_id, events)
        };
        self.emit(events).await?;
        let session = self
            .weak
            .upgrade()
            .ok_or_else(|| anyhow!("Claude session released"))?;
        let task_turn = turn_id.clone();
        tokio::spawn(async move {
            let result = session.attempt_file_rewind(explicit.as_deref()).await;
            let owned = {
                let mut state = session.state.lock().unwrap();
                if state.active_turn.as_deref() != Some(task_turn.as_str()) {
                    false
                } else {
                    state.active_turn = None;
                    state.active_generation = None;
                    state.projection.end_turn();
                    true
                }
            };
            if !owned {
                return;
            }
            let events = match result {
                Ok(text) => vec![
                    json!({"type":"timeline","provider":session.spec.id,"turnId":task_turn,"item":{"type":"assistant_message","text":text}}),
                    json!({"type":"turn_completed","provider":session.spec.id,"turnId":task_turn}),
                ],
                Err(error) => vec![
                    json!({"type":"turn_failed","provider":session.spec.id,"turnId":task_turn,"error":error.to_string()}),
                ],
            };
            let _ = session.emit(events).await;
        });
        Ok(json!({"turnId":turn_id}))
    }

    async fn attempt_file_rewind(&self, explicit: Option<&str>) -> Result<String> {
        let candidates = if let Some(id) = explicit {
            uuid::Uuid::parse_str(id).map_err(|_| {
                anyhow!("Invalid message UUID. Usage: /rewind <user_message_uuid> or /rewind")
            })?;
            vec![id.to_owned()]
        } else {
            self.state
                .lock()
                .unwrap()
                .anchors
                .iter()
                .rev()
                .map(|a| a.0.clone())
                .collect::<Vec<_>>()
        };
        if candidates.is_empty() {
            bail!("No prior user message available to rewind. Use /rewind <user_message_uuid>.");
        }
        let mut failure =
            anyhow!("No rewind checkpoints are currently available for this session.");
        for id in candidates {
            self.state.lock().unwrap().restart = true;
            let result = async {
                let runtime = self.ensure_runtime().await?;
                runtime
                    .request(
                        json!({"subtype":"rewind_files","user_message_id":id,"dry_run":false}),
                        CONTROL_TIMEOUT,
                    )
                    .await
            }
            .await;
            match result {
                Ok(result) if result["canRewind"] == true => {
                    let mut stats = Vec::new();
                    if let Some(files) = result["filesChanged"].as_array() {
                        stats.push(format!(
                            "{} file{}",
                            files.len(),
                            if files.len() == 1 { "" } else { "s" }
                        ));
                    }
                    if let Some(count) = result["insertions"].as_u64() {
                        stats.push(format!("{count} insertions"));
                    }
                    if let Some(count) = result["deletions"].as_u64() {
                        stats.push(format!("{count} deletions"));
                    }
                    return Ok(if stats.is_empty() {
                        format!("Rewound tracked files to message {id}.")
                    } else {
                        format!(
                            "Rewound tracked files to message {id} ({}).",
                            stats.join(", ")
                        )
                    });
                }
                Ok(result) if result["canRewind"] == false => {
                    failure = anyhow!(
                        "{}",
                        result["error"]
                            .as_str()
                            .unwrap_or("No file checkpoint found")
                    )
                }
                Ok(_) => {
                    bail!("Claude rewind reply omitted canRewind; mutation outcome is unknown")
                }
                Err(error) => {
                    self.state.lock().unwrap().restart = true;
                    // A lost control reply does not prove that files were left
                    // untouched. Only an explicit canRewind:false may try an
                    // older checkpoint.
                    return Err(error);
                }
            }
            if explicit.is_some() {
                break;
            }
        }
        Err(failure)
    }
}
