mod compat;
mod session;
#[cfg(test)]
mod tests;

use crate::providers::api::{
    AgentProvider, AgentSession, EventSink, ImportedSession, LaunchContext, ProviderSpec,
};
use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use serde_json::{Value, json};
use session::{HopliteSession, Inner};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use tokio_util::sync::CancellationToken;

/// The bundled Hoplite adapter runs the native `hoplite acp` protocol, using the
/// shared ACP transport but the plugin adapter's distinct session policy.
pub struct HopliteProvider {
    spec: ProviderSpec,
    capabilities: Value,
    discovered_capabilities: OnceLock<Value>,
    sessions: Mutex<Vec<Weak<Inner>>>,
}
impl HopliteProvider {
    pub fn new(spec: ProviderSpec) -> Self {
        Self {
            spec,
            capabilities: capabilities(None, false),
            discovered_capabilities: OnceLock::new(),
            sessions: Mutex::new(Vec::new()),
        }
    }
    async fn spawn(
        &self,
        context: Option<&LaunchContext>,
        cancel: CancellationToken,
        sink: Option<EventSink>,
        config: Value,
    ) -> Result<Arc<Inner>> {
        let inner = Inner::spawn(&self.spec, context, cancel, sink, config).await?;
        self.discovered_capabilities
            .get_or_init(|| inner.negotiated_capabilities());
        let mut sessions = self.sessions.lock().unwrap();
        sessions.retain(|session| session.strong_count() > 0);
        sessions.push(Arc::downgrade(&inner));
        Ok(inner)
    }
    async fn open(
        &self,
        mut config: Value,
        context: LaunchContext,
        options: Value,
        persistence: Option<Value>,
        sink: EventSink,
    ) -> Result<Arc<HopliteSession>> {
        config["provider"] = json!(self.spec.id);
        config["persist"] = json!(options["persistSession"] != false);
        if config.get("providerOptions").is_none() {
            config["providerOptions"] = self.spec.options.clone();
        }
        if !config["cwd"].is_string() {
            bail!(
                "Plugin provider '{}' requires cwd to open a session",
                self.spec.id
            );
        }
        let inner = self
            .spawn(
                Some(&context),
                context.cancel.clone(),
                Some(sink),
                config.clone(),
            )
            .await?;
        let result = inner.open(&config, &context, persistence).await;
        match result {
            Ok(session) => Ok(session),
            Err(error) => {
                let _ = inner.close().await;
                Err(error)
            }
        }
    }
}

pub(crate) fn capabilities(native: Option<&Value>, configure: bool) -> Value {
    json!({"supportsStreaming":true,"supportsSessionPersistence":native.is_some_and(|v|v["loadSession"]==true),"supportsSessionListing":native.is_some_and(|v|v["sessionCapabilities"].get("list").is_some_and(|v|!v.is_null()&&v!=false)),"supportsDynamicModes":configure,"supportsMcpServers":true,"supportsNativePaseoTools":false,"supportsReasoningStream":true,"supportsToolInvocations":true,"supportsRewindConversation":false,"supportsRewindFiles":false,"supportsRewindBoth":false})
}

// Keep the pre-cutover plugin handle, including JSON field order. Existing
// imports and persisted agents must not silently change their native identity.
pub(crate) fn encode_id(native_id: &str) -> String {
    format!(
        "plugin:{{\"version\":1,\"data\":{{\"sessionId\":{}}}}}",
        serde_json::to_string(native_id).expect("string serialization cannot fail")
    )
}
pub(crate) fn persistence(provider: &str, native_id: &str) -> Value {
    json!({"provider":provider,"sessionId":encode_id(native_id),"metadata":{"pluginProviderPersistence":{"version":1,"data":{"sessionId":native_id}}}})
}
fn decode_id(id: &str) -> Result<Value> {
    let encoded = id
        .strip_prefix("plugin:")
        .ok_or_else(|| anyhow!("Invalid plugin provider persistence handle"))?;
    let value: Value = serde_json::from_str(encoded)?;
    validate_persistence(&value)?;
    Ok(value)
}
fn validate_persistence(value: &Value) -> Result<()> {
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("Invalid plugin provider persistence handle"))?;
    if object.len() != 2 || !object.contains_key("data") || value["version"].as_u64().is_none() {
        bail!("Invalid plugin provider persistence handle");
    }
    Ok(())
}
fn decode_handle(provider: &str, handle: &Value) -> Result<Value> {
    if handle["provider"].as_str().is_some_and(|id| id != provider) {
        bail!("Cannot resume another provider's persistence handle with {provider}");
    }
    if let Some(value) = handle["metadata"].get("pluginProviderPersistence") {
        validate_persistence(value)?;
        return Ok(value.clone());
    }
    decode_id(
        handle["sessionId"]
            .as_str()
            .ok_or_else(|| anyhow!("Invalid plugin provider persistence handle"))?,
    )
}

#[async_trait]
impl AgentProvider for HopliteProvider {
    fn id(&self) -> &str {
        &self.spec.id
    }
    fn capabilities(&self) -> &Value {
        self.discovered_capabilities
            .get()
            .unwrap_or(&self.capabilities)
    }
    async fn create_session(
        &self,
        config: Value,
        context: LaunchContext,
        options: Value,
        sink: EventSink,
    ) -> Result<Arc<dyn AgentSession>> {
        Ok(self.open(config, context, options, None, sink).await?)
    }
    async fn resume_session(
        &self,
        handle: Value,
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
                json!({}),
                Some(decode_handle(&self.spec.id, &handle)?),
                sink,
            )
            .await?)
    }
    async fn fetch_catalog(&self, options: Value, cancel: CancellationToken) -> Result<Value> {
        let inner = self.spawn(None, cancel, None, json!({})).await?;
        let result = async {
            // Hoplite cannot provision a workspace for global discovery. It still
            // initializes the executable, so auth/startup errors remain visible.
            if options["scope"] != "workspace" || !options["cwd"].is_string() {
                return Ok(json!({"models":[],"modes":[]}));
            }
            let response = inner
                .rpc
                .request(
                    "session/new",
                    json!({"cwd":options["cwd"],"mcpServers":[]}),
                    None,
                )
                .await?;
            inner.accept_open(&response)?;
            Ok(compat::catalog(&response, &self.spec.id))
        }
        .await;
        let close = inner.close().await;
        match result {
            Err(error) => Err(error),
            Ok(value) => {
                close?;
                Ok(value)
            }
        }
    }
    // The plugin registration has no static feature or slash-command catalog;
    // these come from the opened session's config/command notifications.
    async fn list_features(&self, _config: &Value) -> Result<Vec<Value>> {
        Ok(Vec::new())
    }
    async fn list_commands(&self, _config: &Value) -> Result<Vec<Value>> {
        Ok(Vec::new())
    }
    async fn list_importable_sessions(&self, options: Value) -> Result<Vec<Value>> {
        let inner = self
            .spawn(None, CancellationToken::new(), None, json!({}))
            .await?;
        let result=async {
            if !inner.supports_listing(){bail!("Provider '{}' does not support session.list",self.spec.id);}
            let mut params=json!({});if let Some(cwd)=options.get("cwd"){params["cwd"]=cwd.clone();}
            let response=inner.rpc.request("session/list",params,None).await?;
            let sessions=response["sessions"].as_array().ok_or_else(||anyhow!("ACP session/list omitted sessions"))?;
            let limit=options["limit"].as_u64().filter(|limit|*limit>0).unwrap_or(u64::MAX).min(usize::MAX as u64) as usize;
            sessions.iter().take(limit).map(|session| {
                let native=session["sessionId"].as_str().ok_or_else(||anyhow!("ACP session/list omitted sessionId"))?;
                let updated=session["updatedAt"].as_str().and_then(|s|chrono::DateTime::parse_from_rfc3339(s).ok()).map(|date|date.to_utc().to_rfc3339_opts(chrono::SecondsFormat::Millis,true)).unwrap_or_else(||"1970-01-01T00:00:00.000Z".into());
                Ok(json!({"providerHandleId":encode_id(native),"cwd":session["cwd"],"title":session["title"],"firstPromptPreview":null,"lastPromptPreview":null,"lastActivityAt":updated}))
            }).collect::<Result<Vec<_>>>()
        }.await;
        let close = inner.close().await;
        match result {
            Err(error) => Err(error),
            Ok(value) => {
                close?;
                Ok(value)
            }
        }
    }
    async fn import_session(
        &self,
        input: Value,
        mut config: Value,
        _stored_config: Value,
        context: LaunchContext,
        sink: EventSink,
    ) -> Result<ImportedSession> {
        let handle = decode_id(
            input["providerHandleId"]
                .as_str()
                .ok_or_else(|| anyhow!("Import requires providerHandleId"))?,
        )?;
        config["provider"] = json!(self.spec.id);
        config["cwd"] = input["cwd"].clone();
        let session = self
            .open(config.clone(), context, json!({}), Some(handle), sink)
            .await?;
        let persistence = session
            .describe_persistence()
            .ok_or_else(|| anyhow!("Plugin provider '{}' did not persist import", self.spec.id))?;
        let timeline = session
            .history()
            .into_iter()
            .filter(|event| event["type"] == "timeline")
            .map(|event| {
                let mut entry = json!({"item":event["item"]});
                if let Some(timestamp) = event.get("timestamp") {
                    entry["timestamp"] = timestamp.clone();
                }
                entry
            })
            .collect();
        Ok(ImportedSession {
            session,
            config,
            persistence,
            timeline,
            provider_subagent_events: Vec::new(),
        })
    }
    async fn is_available(&self, _options: Value, cancel: CancellationToken) -> Result<bool> {
        if cancel.is_cancelled() {
            bail!("Hoplite availability request cancelled");
        }
        let Some(command) = self.spec.command.first() else {
            return Ok(false);
        };
        let path = self
            .spec
            .env
            .get("PATH")
            .map(std::ffi::OsString::from)
            .or_else(|| std::env::var_os("PATH"));
        Ok(which::which_in(command, path, std::env::current_dir()?).is_ok())
    }
    async fn diagnostic(&self) -> Result<String> {
        if self
            .is_available(json!({}), CancellationToken::new())
            .await?
        {
            Ok("Provider is available".into())
        } else {
            Ok(format!(
                "{} not found on PATH",
                self.spec
                    .command
                    .first()
                    .map(String::as_str)
                    .unwrap_or("hoplite")
            ))
        }
    }
    async fn archive_native_session(&self, persistence: &Value) -> Result<()> {
        decode_handle(&self.spec.id, persistence)?;
        bail!("session.archive is not supported by this ACP provider")
    }
    async fn unarchive_native_session(&self, persistence: &Value) -> Result<()> {
        decode_handle(&self.spec.id, persistence)?;
        bail!("session.unarchive is not supported by this ACP provider")
    }
    async fn shutdown(&self) -> Result<()> {
        let sessions: Vec<_> = self
            .sessions
            .lock()
            .unwrap()
            .drain(..)
            .filter_map(|session| session.upgrade())
            .collect();
        let mut failure = None;
        for session in sessions {
            if let Err(error) = session.close().await {
                failure = Some(error);
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
        Ok(())
    }
}
