//! Native Antigravity CLI adapter. This is agy's stream-json protocol, not ACP.
//! Auth, account selection and native project/global configuration remain owned by agy.
mod catalog;
mod session;
#[cfg(test)]
mod tests;
mod wire;

use std::sync::{Arc, Mutex, RwLock, Weak};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::api::*;
use session::Session;

pub struct AntigravityProvider {
    spec: ProviderSpec,
    capabilities: Value,
    catalog: Arc<RwLock<Value>>,
    catalog_init: tokio::sync::OnceCell<()>,
    sessions: Mutex<Vec<Weak<Session>>>,
}
impl AntigravityProvider {
    pub fn new(spec: ProviderSpec) -> Self {
        Self {
            spec,
            capabilities: catalog::capabilities(),
            catalog: Arc::new(RwLock::new(Value::Null)),
            catalog_init: tokio::sync::OnceCell::new(),
            sessions: Mutex::new(Vec::new()),
        }
    }
    async fn open(
        &self,
        config: Value,
        context: LaunchContext,
        options: Value,
        conversation: Option<String>,
        sink: EventSink,
    ) -> Result<Arc<Session>> {
        if self
            .catalog
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_null()
        {
            self.catalog_init
                .get_or_try_init(|| async {
                    let discovered = catalog::discover(
                        &self.spec,
                        config["cwd"].as_str(),
                        context.cancel.clone(),
                    )
                    .await?;
                    *self.catalog.write().unwrap_or_else(|e| e.into_inner()) = discovered;
                    Ok::<_, anyhow::Error>(())
                })
                .await?;
        }
        let session = Session::open(
            self.spec.clone(),
            config,
            context,
            options,
            conversation,
            self.catalog.clone(),
            sink,
        )
        .await?;
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        sessions.retain(|s| s.strong_count() > 0);
        sessions.push(Arc::downgrade(&session));
        Ok(session)
    }
}
#[async_trait]
impl AgentProvider for AntigravityProvider {
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
        Ok(self.open(config, context, options, None, sink).await?)
    }
    async fn resume_session(
        &self,
        persistence: Value,
        config: Value,
        context: LaunchContext,
        _options: Value,
        sink: EventSink,
    ) -> Result<Arc<dyn AgentSession>> {
        let conversation = catalog::conversation(&persistence)?;
        Ok(self
            .open(
                config,
                context,
                json!({"persistSession":true}),
                Some(conversation),
                sink,
            )
            .await?)
    }
    async fn fetch_catalog(&self, options: Value, cancel: CancellationToken) -> Result<Value> {
        let cwd = if options["scope"] == "workspace" {
            options["cwd"].as_str()
        } else {
            None
        };
        let discovered = catalog::discover(&self.spec, cwd, cancel).await?;
        *self.catalog.write().unwrap_or_else(|e| e.into_inner()) = discovered.clone();
        Ok(discovered)
    }
    async fn list_features(&self, _config: &Value) -> Result<Vec<Value>> {
        Ok(Vec::new())
    }
    async fn list_commands(&self, _config: &Value) -> Result<Vec<Value>> {
        Ok(Vec::new())
    }
    async fn list_importable_sessions(&self, _options: Value) -> Result<Vec<Value>> {
        // The original plugin runtime skips session.list when it was not negotiated.
        Ok(Vec::new())
    }
    async fn import_session(
        &self,
        input: Value,
        mut config: Value,
        _stored_config: Value,
        context: LaunchContext,
        sink: EventSink,
    ) -> Result<ImportedSession> {
        let id = input["providerHandleId"]
            .as_str()
            .ok_or_else(|| anyhow!("Missing Antigravity providerHandleId"))?;
        let conversation = catalog::conversation(&json!(id))?;
        config["provider"] = json!(self.spec.id);
        if let Some(cwd) = input.get("cwd") {
            config["cwd"] = cwd.clone();
        }
        let session = self
            .open(
                config.clone(),
                context,
                json!({"persistSession":true}),
                Some(conversation),
                sink,
            )
            .await?;
        let persistence = session
            .describe_persistence()
            .ok_or_else(|| anyhow!("Antigravity did not persist import"))?;
        let timeline = session.initial_timeline().to_vec();
        Ok(ImportedSession {
            session,
            config,
            persistence,
            timeline,
            provider_subagent_events: Vec::new(),
        })
    }
    async fn is_available(&self, _options: Value, cancel: CancellationToken) -> Result<bool> {
        match catalog::status(&self.spec, cancel.clone()).await {
            Ok(()) => Ok(true),
            Err(error) if cancel.is_cancelled() => Err(error),
            Err(_) => Ok(false),
        }
    }
    async fn diagnostic(&self) -> Result<String> {
        Ok(
            match catalog::status(&self.spec, CancellationToken::new()).await {
                Ok(()) => "Provider is available".into(),
                Err(error) => wire::diagnostic(&error.to_string()),
            },
        )
    }
    async fn archive_native_session(&self, persistence: &Value) -> Result<()> {
        // No native archive protocol: core archive is intentionally independent of agy.
        catalog::decode_persistence(persistence)?;
        Ok(())
    }
    async fn unarchive_native_session(&self, persistence: &Value) -> Result<()> {
        // Matches ProviderRuntime.unarchive's unnegotiated-capability gate.
        catalog::decode_persistence(persistence)?;
        Ok(())
    }
    async fn shutdown(&self) -> Result<()> {
        let sessions =
            std::mem::take(&mut *self.sessions.lock().unwrap_or_else(|e| e.into_inner()));
        let mut error = None;
        for session in sessions.into_iter().filter_map(|s| s.upgrade()) {
            if let Err(next) = session.close().await {
                if error.is_none() {
                    error = Some(next);
                }
            }
        }
        match error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}
