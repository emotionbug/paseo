mod catalog;
mod events;
mod history;
mod host_tools;
mod launch;
mod prompt;
mod session;
mod transport;

use crate::providers::registry::catalog::refresh_activity;
use crate::{
    process::{self, CommandSpec},
    providers::api::{
        AgentProvider, AgentSession, EventSink, ImportedSession, LaunchContext, ProviderSpec,
    },
};
use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use serde_json::{Value, json};
use session::Session;
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

pub struct JsonlProvider {
    spec: ProviderSpec,
    capabilities: Value,
    sessions: Mutex<Vec<Weak<Session>>>,
}
impl JsonlProvider {
    pub fn new(spec: ProviderSpec) -> Self {
        Self {
            capabilities: catalog::capabilities(spec.family == "omp", spec.family == "omp"),
            spec,
            sessions: Mutex::new(Vec::new()),
        }
    }
    fn environment(&self) -> BTreeMap<String, String> {
        let mut env = std::env::vars().collect::<BTreeMap<_, _>>();
        env.extend(self.spec.env.clone());
        env
    }
    fn context(&self, cancel: CancellationToken) -> LaunchContext {
        LaunchContext {
            agent_id: String::new(),
            home: dirs::home_dir().unwrap_or_default(),
            executable: std::env::current_exe().unwrap_or_default(),
            env: BTreeMap::new(),
            mcp_servers: json!({}),
            tool_catalog: json!([]),
            tool_manifest: json!([]),
            tool_executor: None,
            cancel,
        }
    }
    fn config(&self, mut config: Value) -> Value {
        if !config.is_object() {
            config = json!({});
        }
        if config
            .get("providerOptions")
            .is_none_or(|value| value.is_null() || value.is_object())
        {
            let mut options = self.spec.options.as_object().cloned().unwrap_or_default();
            if let Some(override_options) = config["providerOptions"].as_object() {
                options.extend(override_options.clone());
            }
            config["providerOptions"] = Value::Object(options);
        }
        config["provider"] = json!(self.spec.id);
        config
    }
    fn remember(&self, session: &Arc<Session>) {
        let mut sessions = self.sessions.lock().unwrap();
        sessions.retain(|s| s.strong_count() > 0);
        sessions.push(Arc::downgrade(session));
    }
    async fn probe(
        &self,
        config: &Value,
        cancel: CancellationToken,
    ) -> Result<(Arc<transport::Transport>, tokio::task::JoinHandle<()>)> {
        let context = self.context(cancel);
        let (runtime, mut events) = transport::Transport::spawn(
            launch::command(&self.spec, config, &context, None, None)?,
            launch::timeout(&self.spec, config),
            context.cancel.child_token(),
        )
        .await?;
        let drain = tokio::spawn(async move { while events.recv().await.is_some() {} });
        if self.spec.family == "omp" {
            if let Err(error) = runtime
                .negotiate(launch::ready_timeout(&self.spec, config))
                .await
            {
                runtime.close().await.ok();
                drain.abort();
                return Err(error);
            }
        }
        Ok((runtime, drain))
    }
    fn probe_config(&self, options: &Value) -> Value {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let cwd = if options["scope"] == "global" {
            home.to_string_lossy().into_owned()
        } else {
            options["cwd"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| home.to_string_lossy().into_owned())
        };
        let mut config = options.clone();
        if !config.is_object() {
            config = json!({});
        }
        config["cwd"] = json!(cwd);
        self.config(config)
    }
    fn executable_prefix(&self) -> Vec<String> {
        if self.spec.command.is_empty() {
            let env = self.environment();
            vec![
                env.get(if self.spec.family == "omp" {
                    "OMP_COMMAND"
                } else {
                    "PI_COMMAND"
                })
                .cloned()
                .or_else(|| {
                    if self.spec.family == "pi" {
                        env.get("PI_ACP_PI_COMMAND").cloned()
                    } else {
                        None
                    }
                })
                .unwrap_or_else(|| self.spec.family.clone()),
            ]
        } else {
            self.spec.command.clone()
        }
    }
}
#[async_trait]
impl AgentProvider for JsonlProvider {
    fn id(&self) -> &str {
        &self.spec.id
    }
    fn capabilities(&self) -> &Value {
        &self.capabilities
    }
    async fn create_session(
        &self,
        config: Value,
        context: LaunchContext,
        options: Value,
        sink: EventSink,
    ) -> Result<Arc<dyn AgentSession>> {
        let session = Session::open(
            self.spec.clone(),
            self.config(config),
            context,
            None,
            options,
            sink,
        )
        .await?;
        self.remember(&session);
        Ok(session)
    }
    async fn resume_session(
        &self,
        persistence: Value,
        config: Value,
        context: LaunchContext,
        options: Value,
        sink: EventSink,
    ) -> Result<Arc<dyn AgentSession>> {
        if persistence["nativeHandle"].as_str().is_none() {
            bail!(
                "{} resume requires a native session file handle",
                self.spec.family
            );
        }
        let mut merged = persistence["metadata"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        if let Some(config) = config.as_object() {
            merged.extend(config.clone());
        }
        if !merged.get("cwd").is_some_and(Value::is_string) {
            return Err(anyhow!("{} resume requires cwd", self.spec.family));
        }
        // Credentials, MCP bindings and tool authority are exclusively in LaunchContext.
        for key in ["mcpServers", "toolCatalog", "paseoTools"] {
            merged.remove(key);
        }
        let session = Session::open(
            self.spec.clone(),
            self.config(Value::Object(merged)),
            context,
            Some(persistence),
            options,
            sink,
        )
        .await?;
        self.remember(&session);
        Ok(session)
    }
    async fn fetch_catalog(&self, options: Value, cancel: CancellationToken) -> Result<Value> {
        let config = self.probe_config(&options);
        let (runtime, drain) =
            refresh_activity("runtime.start", self.probe(&config, cancel)).await?;
        let result = async {
            let (_, available) = refresh_activity(
                "get_available_models",
                runtime.request_with_timeout(json!({"type":"get_available_models"}), None),
            )
            .await?;
            let state = if self.spec.family == "pi" {
                refresh_activity("get_state", runtime.request(json!({"type":"get_state"}))).await?
            } else {
                Value::Null
            };
            let models = available["models"]
                .as_array()
                .ok_or_else(|| anyhow!("Provider returned no model list"))?
                .iter()
                .map(|model| {
                    let mut mapped =
                        catalog::model(model, &self.spec.id, self.spec.family == "omp");
                    if self.spec.family == "pi"
                        && model["id"] == state["model"]["id"]
                        && model["provider"] == state["model"]["provider"]
                    {
                        mapped["isDefault"] = json!(true);
                    }
                    mapped
                })
                .collect::<Vec<_>>();
            Ok(json!({"models":models,"modes":catalog::modes(self.spec.family=="omp")}))
        }
        .await;
        let close = runtime.close().await;
        drain.abort();
        match result {
            Ok(value) => {
                close?;
                Ok(value)
            }
            Err(error) => Err(error),
        }
    }
    async fn list_features(&self, _config: &Value) -> Result<Vec<Value>> {
        Ok(vec![])
    }
    async fn list_commands(&self, config: &Value) -> Result<Vec<Value>> {
        let config = self.probe_config(config);
        let (runtime, drain) = self.probe(&config, CancellationToken::new()).await?;
        let result=runtime.request(json!({"type":if self.spec.family=="omp"{"get_available_commands"}else{"get_commands"}})).await;
        let close = runtime.close().await;
        drain.abort();
        let response = result?;
        close?;
        Ok(catalog::commands(
            response["commands"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or(&[]),
            self.spec.family == "omp",
        ))
    }
    async fn list_importable_sessions(&self, options: Value) -> Result<Vec<Value>> {
        let options = self.config(options);
        launch::validate_options(&self.spec.family, &options["providerOptions"])?;
        let mut sessions =
            history::discover(&self.spec.family, &self.environment(), &options).await?;
        for session in &mut sessions {
            session["provider"] = json!(self.spec.id);
        }
        Ok(sessions)
    }
    async fn import_session(
        &self,
        input: Value,
        config: Value,
        stored_config: Value,
        context: LaunchContext,
        sink: EventSink,
    ) -> Result<ImportedSession> {
        let handle = input["providerHandleId"]
            .as_str()
            .or(input["nativeHandle"].as_str())
            .ok_or_else(|| anyhow!("Import requires providerHandleId"))?;
        let mut config = self.config(config);
        let environment = self.environment();
        let path = history::resolve_session_file(&self.spec.family, handle, &environment, &config)
            .await?
            .ok_or_else(|| anyhow!("Native provider session not found"))?;
        let descriptor = history::describe(&self.spec.family, &path)
            .await?
            .ok_or_else(|| anyhow!("Invalid provider session descriptor"))?;
        let mut stored = match stored_config {
            Value::Object(stored) => stored,
            _ => serde_json::Map::new(),
        };
        if let Some(import) = descriptor["importConfig"].as_object() {
            stored.extend(import.clone());
            config
                .as_object_mut()
                .ok_or_else(|| anyhow!("Import launch config must be an object"))?
                .extend(import.clone());
        }
        let cwd = input["cwd"].clone();
        stored.insert("provider".into(), json!(self.spec.id));
        stored.insert("cwd".into(), cwd.clone());
        config["provider"] = json!(self.spec.id);
        config["cwd"] = cwd;
        let stored_config = Value::Object(stored);
        let persistence = history::import_persistence(
            &self.spec.id,
            handle,
            stored_config["cwd"]
                .as_str()
                .ok_or_else(|| anyhow!("Import requires cwd"))?,
            &stored_config,
        );
        let session = self
            .resume_session(persistence.clone(), config, context, json!({}), sink)
            .await?;
        let cancel = CancellationToken::new();
        let (history_sink, mut receiver) = EventSink::channel(256, cancel.clone());
        let collect = tokio::spawn(async move {
            let mut timeline = Vec::new();
            let mut subagents = Vec::new();
            while let Some(event) = receiver.recv().await {
                if event["type"] == "timeline" {
                    let mut item = json!({"item":event["item"]});
                    if let Some(timestamp) = event.get("timestamp") {
                        item["timestamp"] = timestamp.clone();
                    }
                    timeline.push(item);
                } else if event["type"] == "provider_subagent" {
                    subagents.push(event);
                }
            }
            (timeline, subagents)
        });
        if let Err(error) = session.stream_history(history_sink).await {
            cancel.cancel();
            collect.abort();
            session.close().await.ok();
            return Err(error);
        }
        let (timeline, provider_subagent_events) = collect.await?;
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
            bail!("Availability check cancelled");
        }
        let prefix = self.executable_prefix();
        let env = self.environment();
        let paths = env.get("PATH").or_else(|| env.get("Path"));
        Ok(which::which_in(
            &prefix[0],
            paths.map(std::ffi::OsString::from),
            std::env::current_dir()?,
        )
        .is_ok())
    }
    async fn diagnostic(&self) -> Result<String> {
        let mut prefix = self.executable_prefix();
        let program = prefix.remove(0);
        let mut args = prefix;
        args.push("--version".into());
        let output = process::run(
            CommandSpec {
                program: program.clone(),
                args,
                cwd: None,
                env: self.spec.env.clone(),
                clear_env: false,
            },
            1024 * 1024,
            Duration::from_secs(5),
            CancellationToken::new(),
        )
        .await;
        let version = match output {
            Ok(output) if output.status == 0 => {
                String::from_utf8_lossy(&output.stdout).trim().to_owned()
            }
            Ok(output) => format!(
                "version command exited {}: {}",
                output.status, output.stderr
            ),
            Err(error) => error.to_string(),
        };
        let root = history::sessions_dir(
            &self.spec.family,
            &self.environment(),
            &json!({"providerOptions":self.spec.options}),
        )
        .await?;
        let support = if self.spec.family == "omp" {
            let regex = regex::Regex::new(r"(\d+)\.(\d+)\.(\d+)")?;
            let version = regex
                .captures(&version)
                .and_then(|c| semver::Version::parse(&c[0]).ok());
            format!(
                "\nVersion support: {} (minimum 16.3.9)",
                version.map_or("unknown", |v| if v >= semver::Version::new(16, 3, 9) {
                    "supported"
                } else {
                    "unsupported"
                })
            )
        } else {
            String::new()
        };
        Ok(format!(
            "{}\nCommand: {program}\nVersion: {version}{support}\nSession directory: {}",
            if self.spec.family == "omp" {
                "Oh My Pi (OMP)"
            } else {
                "Pi"
            },
            root.display()
        ))
    }
    async fn archive_native_session(&self, _persistence: &Value) -> Result<()> {
        bail!(
            "{} does not expose native session archiving",
            self.spec.family
        )
    }
    async fn unarchive_native_session(&self, _persistence: &Value) -> Result<()> {
        bail!(
            "{} does not expose native session unarchiving",
            self.spec.family
        )
    }
    async fn shutdown(&self) -> Result<()> {
        let sessions = self
            .sessions
            .lock()
            .unwrap()
            .drain(..)
            .filter_map(|s| s.upgrade())
            .collect::<Vec<_>>();
        let mut error = None;
        for session in sessions {
            if let Err(e) = session.close().await {
                error = Some(e);
            }
        }
        match error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}
