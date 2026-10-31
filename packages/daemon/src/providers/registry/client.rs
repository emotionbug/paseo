mod events;
mod models;
mod session;
#[cfg(test)]
mod tests;

use std::sync::Arc;

use anyhow::{Result, bail};
use async_trait::async_trait;
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use crate::providers::{
    api::{AgentProvider, AgentSession, EventSink, ImportedSession, LaunchContext},
    options,
};
use events::EventBridge;
pub(super) use models::{merge_models, resolve_configured_models};
use session::Session;

pub(crate) fn wrap_client(
    inner: Arc<dyn AgentProvider>,
    provider: String,
    profile_models: Vec<Value>,
    additional_models: Vec<Value>,
    profile_models_are_additive: bool,
    default_options: Value,
) -> Result<Arc<dyn AgentProvider>> {
    let profile_models = resolve_configured_models(&provider, inner.as_ref(), &profile_models)?;
    let additional_models =
        resolve_configured_models(&provider, inner.as_ref(), &additional_models)?;
    let configured_model_ids = profile_models
        .iter()
        .chain(&additional_models)
        .map(|model| model["id"].clone())
        .collect();
    Ok(Arc::new(Client {
        inner,
        provider,
        profile_models,
        additional_models,
        profile_models_are_additive,
        default_options,
        configured_model_ids,
    }))
}

struct Client {
    inner: Arc<dyn AgentProvider>,
    provider: String,
    profile_models: Vec<Value>,
    additional_models: Vec<Value>,
    profile_models_are_additive: bool,
    default_options: Value,
    configured_model_ids: Vec<Value>,
}

fn object(value: &mut Value) -> &mut Map<String, Value> {
    if !value.is_object() {
        *value = Value::Object(Map::new());
    }
    value.as_object_mut().expect("object initialized")
}

fn with_provider(mut value: Value, provider: &str) -> Value {
    object(&mut value).insert("provider".into(), Value::String(provider.to_owned()));
    value
}

fn merge_provider_options(base: &Value, overrides: Option<&Value>) -> Option<Value> {
    if base.is_null() && overrides.is_none() {
        return None;
    }
    let empty = Value::Object(Map::new());
    Some(options::merge(
        if base.is_object() { base } else { &empty },
        overrides
            .filter(|value| value.is_object())
            .unwrap_or(&empty),
    ))
}

fn restore_stored_options(config: &mut Value, stored_options: Option<Value>) {
    let config = object(config);
    if let Some(options) = stored_options {
        config.insert("providerOptions".into(), options);
    } else {
        config.remove("providerOptions");
    }
}

impl Client {
    fn config(&self, config: Value) -> Value {
        let mut config = with_provider(config, self.inner.id());
        let options = merge_provider_options(&self.default_options, config.get("providerOptions"));
        restore_stored_options(&mut config, options);
        config
    }

    fn discovery_options(&self, mut options: Value) -> Value {
        let fields = object(&mut options);
        if self.default_options.is_null() {
            fields.remove("providerOptions");
        } else {
            fields.insert("providerOptions".into(), self.default_options.clone());
        }
        options
    }

    fn session_options(&self, mut options: Value) -> Value {
        object(&mut options).insert(
            "configuredModelIds".into(),
            Value::Array(self.configured_model_ids.clone()),
        );
        options
    }

    fn session(&self, inner: Arc<dyn AgentSession>, bridge: EventBridge) -> Arc<dyn AgentSession> {
        Arc::new(Session {
            inner,
            provider: self.provider.clone(),
            bridge,
        })
    }
}

#[async_trait]
impl AgentProvider for Client {
    fn id(&self) -> &str {
        &self.provider
    }
    fn capabilities(&self) -> &Value {
        self.inner.capabilities()
    }
    fn resolve_create_config(&self, input: &Value) -> Option<Result<Value>> {
        self.inner.resolve_create_config(input)
    }
    fn is_create_config_unattended(&self, input: &Value) -> Option<bool> {
        self.inner.is_create_config_unattended(input)
    }
    fn resolve_configured_model(&self, model: Value) -> Result<Value> {
        self.inner.resolve_configured_model(model)
    }

    async fn create_session(
        &self,
        config: Value,
        context: LaunchContext,
        options: Value,
        sink: EventSink,
    ) -> Result<Arc<dyn AgentSession>> {
        let (bridge, sink) = EventBridge::new(self.provider.clone(), sink);
        let inner = self
            .inner
            .create_session(
                self.config(config),
                context,
                self.session_options(options),
                sink,
            )
            .await?;
        Ok(self.session(inner, bridge))
    }

    async fn resume_session(
        &self,
        persistence: Value,
        config: Value,
        context: LaunchContext,
        options: Value,
        sink: EventSink,
    ) -> Result<Arc<dyn AgentSession>> {
        let (bridge, sink) = EventBridge::new(self.provider.clone(), sink);
        let inner = self
            .inner
            .resume_session(
                with_provider(persistence, self.inner.id()),
                self.config(config),
                context,
                self.session_options(options),
                sink,
            )
            .await?;
        Ok(self.session(inner, bridge))
    }

    async fn fetch_catalog(&self, options: Value, cancel: CancellationToken) -> Result<Value> {
        let mut catalog = self
            .inner
            .fetch_catalog(self.discovery_options(options), cancel)
            .await?;
        let Some(Value::Array(runtime_models)) = object(&mut catalog).remove("models") else {
            bail!("Provider '{}' catalog did not return models", self.provider);
        };
        catalog["models"] = Value::Array(merge_models(
            &self.provider,
            &self.profile_models,
            &self.additional_models,
            runtime_models,
            self.profile_models_are_additive,
        ));
        Ok(catalog)
    }

    async fn catalog_cache_key(
        &self,
        options: Value,
        cancel: CancellationToken,
    ) -> Result<Option<String>> {
        self.inner.catalog_cache_key(options, cancel).await
    }

    async fn resolve_default_mode(
        &self,
        mut input: Value,
        cancel: CancellationToken,
    ) -> Result<Option<String>> {
        let config = object(&mut input).remove("config").unwrap_or(Value::Null);
        input["config"] = self.config(config);
        self.inner.resolve_default_mode(input, cancel).await
    }

    async fn list_features(&self, config: &Value) -> Result<Vec<Value>> {
        self.inner.list_features(&self.config(config.clone())).await
    }

    async fn list_commands(&self, config: &Value) -> Result<Vec<Value>> {
        self.inner.list_commands(&self.config(config.clone())).await
    }

    async fn list_importable_sessions(&self, options: Value) -> Result<Vec<Value>> {
        self.inner
            .list_importable_sessions(self.discovery_options(options))
            .await
    }

    async fn import_session(
        &self,
        input: Value,
        config: Value,
        stored_config: Value,
        context: LaunchContext,
        sink: EventSink,
    ) -> Result<ImportedSession> {
        // Provider-level defaults are launch-only, never persisted overrides.
        let stored_options = stored_config.get("providerOptions").cloned();
        let (bridge, sink) = EventBridge::new(self.provider.clone(), sink);
        let mut imported = self
            .inner
            .import_session(
                input,
                self.config(config),
                with_provider(stored_config, self.inner.id()),
                context,
                sink,
            )
            .await?;
        if imported.persistence.is_null() {
            imported.session.close().await?;
            bail!(
                "Provider '{}' import did not return persistence",
                self.provider
            );
        }
        imported.persistence = with_provider(imported.persistence, &self.provider);
        imported.config = with_provider(imported.config, &self.provider);
        restore_stored_options(&mut imported.config, stored_options);
        for event in &mut imported.provider_subagent_events {
            object(event).insert("provider".into(), Value::String(self.provider.clone()));
        }
        imported.session = self.session(imported.session, bridge);
        Ok(imported)
    }

    async fn is_available(&self, options: Value, cancel: CancellationToken) -> Result<bool> {
        self.inner.is_available(options, cancel).await
    }
    async fn diagnostic(&self) -> Result<String> {
        self.inner.diagnostic().await
    }
    async fn archive_native_session(&self, persistence: &Value) -> Result<()> {
        self.inner
            .archive_native_session(&with_provider(persistence.clone(), self.inner.id()))
            .await
    }
    async fn unarchive_native_session(&self, persistence: &Value) -> Result<()> {
        self.inner
            .unarchive_native_session(&with_provider(persistence.clone(), self.inner.id()))
            .await
    }
    async fn shutdown(&self) -> Result<()> {
        self.inner.shutdown().await
    }
}
