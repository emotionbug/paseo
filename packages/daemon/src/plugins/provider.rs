mod mapping;
mod runtime;
mod schema;
mod session;
#[cfg(test)]
mod tests;

use super::worker::PluginWorker;
use crate::providers::api::{
    AgentProvider, AgentSession, EventSink, ImportedSession, LaunchContext, ProviderSpec,
};
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use runtime::Connection;
use serde_json::{Value, json};
use session::{PluginSession, SessionCore};
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, Ordering},
};
use tokio_util::sync::CancellationToken;

/// Only this error permits the manager to reopen persistence and retry a prompt.
/// A send rejection or an uncertain live-provider result is never classified stale.
#[derive(Debug, thiserror::Error)]
#[error("Provider session {session_id} is stale after its plugin reloaded")]
pub struct StaleProviderSessionError {
    pub session_id: String,
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct ProviderOperationError {
    pub message: String,
    pub code: Option<String>,
    pub diagnostic: Option<String>,
}
impl ProviderOperationError {
    fn from_value(value: &Value) -> Self {
        Self {
            message: value["message"]
                .as_str()
                .unwrap_or("Provider request failed")
                .to_owned(),
            code: value["code"].as_str().map(str::to_owned),
            diagnostic: value["diagnostic"].as_str().map(str::to_owned),
        }
    }
}

/// A configured provider is bound to one immutable plugin-worker generation.
/// Replacing a plugin creates a new provider, never retargets existing sessions.
pub struct PluginProvider {
    spec: ProviderSpec,
    worker: Arc<PluginWorker>,
    metadata: Value,
    connection: OnceLock<Arc<Connection>>,
    capabilities: OnceLock<Value>,
    initial_capabilities: Value,
    connecting: tokio::sync::Mutex<()>,
    stopped: AtomicBool,
    cancel: CancellationToken,
}
impl PluginProvider {
    pub fn new(spec: ProviderSpec, worker: Arc<PluginWorker>, metadata: Value) -> Self {
        Self {
            spec,
            worker,
            metadata,
            connection: OnceLock::new(),
            capabilities: OnceLock::new(),
            initial_capabilities: mapping::agent_capabilities(&[]),
            connecting: tokio::sync::Mutex::new(()),
            stopped: AtomicBool::new(false),
            cancel: CancellationToken::new(),
        }
    }
    fn registration_id(&self) -> &str {
        self.metadata["id"].as_str().unwrap_or(&self.spec.id)
    }
    async fn launch(&self) -> Result<Option<Value>> {
        // Registration.command is also the declaration that this provider needs
        // an executable. A daemon command override cannot manufacture that hook.
        let Some(default) = self.metadata["command"].as_array() else {
            return Ok(None);
        };
        let command = if self.spec.command.is_empty() {
            default
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_owned)
                        .context("Invalid plugin provider command")
                })
                .collect::<Result<Vec<_>>>()?
        } else {
            self.spec.command.clone()
        };
        let program = command
            .first()
            .context("Invalid empty plugin provider command")?
            .clone();
        let mut env = self.spec.env.clone();
        mapping::strip_parent_environment(&mut env);
        let path = env.get("PATH").map(std::ffi::OsString::from);
        let resolved = tokio::task::spawn_blocking(move || {
            let cwd = std::env::current_dir()?;
            which::which_in(&program, path, cwd)
                .map_err(|_| anyhow::anyhow!("{program} not found on PATH"))
        })
        .await??;
        Ok(Some(
            json!({"command":resolved,"args":&command[1..],"env":env}),
        ))
    }
    async fn get_connection(&self, cancel: &CancellationToken) -> Result<Arc<Connection>> {
        if self.stopped.load(Ordering::Acquire) {
            bail!("Provider runtime is closed");
        }
        if cancel.is_cancelled() {
            bail!("Provider request aborted");
        }
        if let Some(connection) = self.connection.get() {
            return Ok(connection.clone());
        }
        let _gate = tokio::select! {
            guard = self.connecting.lock() => guard,
            _ = cancel.cancelled() => bail!("Provider request aborted"),
            _ = self.cancel.cancelled() => bail!("Provider runtime is closed"),
        };
        if self.stopped.load(Ordering::Acquire) {
            bail!("Provider runtime is closed");
        }
        if let Some(connection) = self.connection.get() {
            return Ok(connection.clone());
        }
        let launch = self.launch().await?;
        let connection = tokio::select! {
            result = Connection::connect(self.worker.clone(),self.registration_id(),launch,self.cancel.clone()) => result?,
            _ = cancel.cancelled() => bail!("Provider request aborted"),
        };
        if self.stopped.load(Ordering::Acquire) {
            let _ = connection.close().await;
            bail!("Provider runtime is closed");
        }
        let _ = self.capabilities.set(mapping::agent_capabilities(
            connection
                .capabilities
                .get()
                .context("Missing provider negotiation")?,
        ));
        let _ = self.connection.set(connection.clone());
        Ok(connection)
    }
    async fn open(
        &self,
        config: Value,
        context: LaunchContext,
        persist: bool,
        persistence: Option<Value>,
        sink: EventSink,
    ) -> Result<Arc<PluginSession>> {
        let mapped = mapping::session_config(&config, &context, persist)?;
        let connection = self.get_connection(&context.cancel).await?;
        let id = uuid::Uuid::new_v4().to_string();
        let core = Arc::new(SessionCore::new(
            id.clone(),
            self.spec.id.clone(),
            connection.clone(),
            context.cancel.clone(),
            sink,
        ));
        connection.register_session(&id, &core)?;
        let mut guard = Opening {
            connection: connection.clone(),
            id: Some(id.clone()),
        };
        let mut input = json!({"type":"session.open","sessionId":id,"config":mapped,"history":if persistence.is_some() {"replay"} else {"skip"}});
        if let Some(persistence) = persistence {
            input["persistence"] = persistence;
        }
        connection.complete(input, &context.cancel).await?;
        let session = Arc::new(PluginSession::new(core));
        session.core.activate().await?;
        guard.id = None;
        Ok(session)
    }
    async fn status(&self, cancel: &CancellationToken) -> Result<Value> {
        if cancel.is_cancelled() {
            bail!("Provider request aborted");
        }
        let result:Result<Value> = async {
            let launch = self.launch().await?;
            if self.metadata["hasStatus"] == true {
                let mut request = json!({}); if let Some(launch) = launch { request["launch"] = launch; }
                let status = self.worker.request(json!({"type":"provider.status","providerId":self.registration_id(),"request":request}),cancel.clone()).await?;
                let available = status["available"].as_bool().context("Invalid provider availability status")?;
                if !available && !status["diagnostic"].as_str().is_some_and(|text|!text.is_empty()) { bail!("Unavailable provider omitted diagnostic"); }
                if status.get("diagnostic").is_some_and(|value|!value.is_string()) { bail!("Invalid provider status diagnostic"); }
                let mut output = json!({"available":available}); mapping::copy(&status,"diagnostic",&mut output,"diagnostic");
                return Ok(output);
            }
            if launch.is_none() { self.get_connection(cancel).await?; }
            Ok(json!({"available":true}))
        }.await;
        Ok(match result {
            Ok(status) => status,
            Err(error) => json!({"available":false,"diagnostic":format!("{error:#}")}),
        })
    }
    pub async fn catalog_cache_key(
        &self,
        mut options: Value,
        cancel: CancellationToken,
    ) -> Result<Option<String>> {
        if self.metadata["hasCatalogCacheKey"] != true {
            return Ok(None);
        }
        let fields = if options["scope"] == "workspace" {
            &["scope", "cwd", "force"][..]
        } else {
            &["scope", "force"][..]
        };
        options
            .as_object_mut()
            .context("Invalid provider catalog options")?
            .retain(|key, _| fields.contains(&key.as_str()));
        if let Some(launch) = self.launch().await? {
            options["launch"] = launch;
        }
        let value = self.worker.request(json!({"type":"provider.catalog_key","providerId":self.registration_id(),"options":options}),cancel).await?;
        if value.is_null() {
            return Ok(None);
        }
        Ok(Some(
            value.as_str().context("Invalid catalogue key")?.to_owned(),
        ))
    }
    async fn archive(&self, persistence: &Value, unarchive: bool) -> Result<()> {
        let persistence = mapping::decode_persistence(persistence)?;
        let cancel = CancellationToken::new();
        let connection = self.get_connection(&cancel).await?;
        let kind = if unarchive {
            "session.unarchive"
        } else {
            "session.archive"
        };
        // These hooks exist on the source adapter even when not negotiated.
        if connection.supports(kind) {
            connection
                .complete(json!({"type":kind,"persistence":persistence}), &cancel)
                .await?;
        }
        Ok(())
    }
}
struct Opening {
    connection: Arc<Connection>,
    id: Option<String>,
}
impl Drop for Opening {
    fn drop(&mut self) {
        if let Some(id) = &self.id {
            self.connection.remove_session(id);
        }
    }
}

#[async_trait]
impl AgentProvider for PluginProvider {
    fn id(&self) -> &str {
        &self.spec.id
    }
    fn capabilities(&self) -> &Value {
        self.capabilities
            .get()
            .unwrap_or(&self.initial_capabilities)
    }
    async fn create_session(
        &self,
        config: Value,
        context: LaunchContext,
        options: Value,
        sink: EventSink,
    ) -> Result<Arc<dyn AgentSession>> {
        Ok(self
            .open(
                config,
                context,
                options["persistSession"] != false,
                None,
                sink,
            )
            .await?)
    }
    async fn resume_session(
        &self,
        persistence: Value,
        config: Value,
        context: LaunchContext,
        _options: Value,
        sink: EventSink,
    ) -> Result<Arc<dyn AgentSession>> {
        if !config["cwd"].is_string() {
            bail!(
                "Plugin provider '{}' requires cwd to resume a session",
                self.spec.id
            );
        }
        Ok(self
            .open(
                config,
                context,
                true,
                Some(mapping::decode_persistence(&persistence)?),
                sink,
            )
            .await?)
    }
    async fn fetch_catalog(&self, options: Value, cancel: CancellationToken) -> Result<Value> {
        let connection = self.get_connection(&cancel).await?;
        let mut input = json!({"type":"catalog"});
        if options["scope"] == "workspace" {
            mapping::copy(&options, "cwd", &mut input, "cwd");
        }
        let event = connection.complete(input, &cancel).await?;
        if event["type"] != "catalog" {
            bail!("Provider returned an invalid catalog response");
        }
        Ok(mapping::catalog(&self.spec.id, &event["catalog"]))
    }
    async fn catalog_cache_key(
        &self,
        options: Value,
        cancel: CancellationToken,
    ) -> Result<Option<String>> {
        PluginProvider::catalog_cache_key(self, options, cancel).await
    }
    // ProviderRegistration has no static feature/command callbacks. Their real
    // discovery happens through session.config and session.commands events.
    async fn list_features(&self, _config: &Value) -> Result<Vec<Value>> {
        Ok(Vec::new())
    }
    async fn list_commands(&self, _config: &Value) -> Result<Vec<Value>> {
        Ok(Vec::new())
    }
    async fn list_importable_sessions(&self, options: Value) -> Result<Vec<Value>> {
        let cancel = CancellationToken::new();
        let connection = self.get_connection(&cancel).await?;
        if !connection.supports("session.list") {
            return Ok(Vec::new());
        }
        let mut input = json!({"type":"sessions"});
        for key in ["query", "cwd", "limit"] {
            mapping::copy(&options, key, &mut input, key);
        }
        let event = connection.complete(input, &cancel).await?;
        if event["type"] != "sessions" {
            bail!("Provider returned an invalid sessions response");
        }
        Ok(event["sessions"].as_array().context("Invalid provider sessions")?.iter().map(|session| {
            let updated = session["updatedAt"].as_str().and_then(parse_date).unwrap_or_else(||"1970-01-01T00:00:00.000Z".to_owned());
            json!({"providerHandleId":mapping::persistence_id(&session["persistence"]),"cwd":session["cwd"],"title":session["title"],"firstPromptPreview":null,"lastPromptPreview":session["description"],"lastActivityAt":updated})
        }).collect())
    }
    async fn import_session(
        &self,
        input: Value,
        mut config: Value,
        _stored_config: Value,
        context: LaunchContext,
        sink: EventSink,
    ) -> Result<ImportedSession> {
        let persistence = mapping::decode_persistence_id(
            input["providerHandleId"]
                .as_str()
                .context("Import requires providerHandleId")?,
        )?;
        config["provider"] = json!(self.spec.id);
        config["cwd"] = input["cwd"].clone();
        let session = self
            .open(config.clone(), context, true, Some(persistence), sink)
            .await?;
        let persistence = session.describe_persistence().with_context(|| {
            format!("Plugin provider '{}' did not persist import", self.spec.id)
        })?;
        let history = session.history();
        let timeline = history
            .iter()
            .filter(|event| event["type"] == "timeline")
            .map(session::timeline_entry)
            .collect();
        let provider_subagent_events = history
            .into_iter()
            .filter(|event| event["type"] == "provider_subagent")
            .collect();
        Ok(ImportedSession {
            session,
            config,
            persistence,
            timeline,
            provider_subagent_events,
        })
    }
    async fn is_available(&self, _options: Value, cancel: CancellationToken) -> Result<bool> {
        Ok(self.status(&cancel).await?["available"] == true)
    }
    async fn diagnostic(&self) -> Result<String> {
        let status = self.status(&CancellationToken::new()).await?;
        Ok(status["diagnostic"]
            .as_str()
            .unwrap_or("Provider is available")
            .to_owned())
    }
    async fn archive_native_session(&self, persistence: &Value) -> Result<()> {
        self.archive(persistence, false).await
    }
    async fn unarchive_native_session(&self, persistence: &Value) -> Result<()> {
        self.archive(persistence, true).await
    }
    async fn shutdown(&self) -> Result<()> {
        self.stopped.store(true, Ordering::Release);
        self.cancel.cancel();
        let _gate = self.connecting.lock().await;
        if let Some(connection) = self.connection.get() {
            connection.close().await?;
        }
        Ok(())
    }
}
fn parse_date(value: &str) -> Option<String> {
    chrono::DateTime::parse_from_rfc3339(value)
        .or_else(|_| chrono::DateTime::parse_from_rfc2822(value))
        .ok()
        .map(|date| {
            date.to_utc()
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        })
        .or_else(|| {
            chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
                .ok()
                .and_then(|date| date.and_hms_opt(0, 0, 0))
                .map(|date| {
                    date.and_utc()
                        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                })
        })
}
