mod build;
pub(crate) mod catalog;
pub(crate) mod client;
mod wire;

use super::{
    api::{AgentProvider, ProviderSpec},
    create_config,
    muse::MuseProvider,
    options,
};
use crate::{state::AppState, transport::RequestContext};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Arc, RwLock, Weak},
};
use tokio::sync::Mutex;

pub(crate) struct ProviderEntry {
    pub definition: Arc<Value>,
    pub spec: ProviderSpec,
    pub client: Arc<dyn AgentProvider>,
    pub enabled: bool,
    pub source: &'static str,
    pub profile_models: Vec<Value>,
    pub additional_models: Vec<Value>,
    pub profile_models_are_additive: bool,
    pub identity: Value,
}

#[derive(Default)]
struct RegistryState {
    entries: Vec<Arc<ProviderEntry>>,
    owned: Vec<Arc<dyn AgentProvider>>,
    muse: Vec<Arc<MuseProvider>>,
}

pub struct ProviderRegistry {
    app: Weak<AppState>,
    state: RwLock<RegistryState>,
    transaction: Mutex<()>,
    pub(crate) catalogs: catalog::CatalogService,
}

impl ProviderRegistry {
    pub fn new(app: Weak<AppState>) -> Self {
        Self {
            catalogs: catalog::CatalogService::new(app.clone()),
            app,
            state: RwLock::new(RegistryState::default()),
            transaction: Mutex::new(()),
        }
    }

    pub async fn initialize(&self) -> Result<()> {
        let app = self.app.upgrade().context("Daemon is shutting down")?;
        self.apply_config(&app.config.snapshot(), &json!({})).await
    }

    pub async fn apply_config(&self, next: &Value, _previous: &Value) -> Result<()> {
        let _transaction = self.transaction.lock().await;
        let app = self.app.upgrade().context("Daemon is shutting down")?;
        if app.cancel.is_cancelled() {
            bail!("Daemon is shutting down");
        }
        let previous = self.entries();
        let (entries, muse) = build::entries(&app, next, &previous)?;
        {
            let mut state = self.state.write().unwrap();
            for entry in &entries {
                if !previous.iter().any(|old| Arc::ptr_eq(old, entry)) {
                    state.owned.push(entry.client.clone());
                }
            }
            state.entries = entries;
            state.muse.extend(muse);
        }
        self.catalogs.registry_changed().await;
        Ok(())
    }

    pub(crate) fn entries(&self) -> Vec<Arc<ProviderEntry>> {
        self.state.read().unwrap().entries.clone()
    }
    pub(crate) fn entry(&self, id: &str) -> Result<Arc<ProviderEntry>> {
        self.state
            .read()
            .unwrap()
            .entries
            .iter()
            .find(|entry| entry.spec.id == id)
            .cloned()
            .ok_or_else(|| anyhow!("Unknown provider '{id}'"))
    }
    pub fn get(&self, id: &str) -> Result<Arc<dyn AgentProvider>> {
        Ok(self.entry(id)?.client.clone())
    }
    pub fn definition(&self, id: &str) -> Option<Arc<Value>> {
        self.entry(id).ok().map(|entry| entry.definition.clone())
    }
    pub fn spec(&self, id: &str) -> Option<ProviderSpec> {
        self.entry(id).ok().map(|entry| entry.spec.clone())
    }
    pub fn specs(&self) -> Vec<ProviderSpec> {
        self.state
            .read()
            .unwrap()
            .entries
            .iter()
            .map(|entry| entry.spec.clone())
            .collect()
    }
    pub fn ids(&self) -> Vec<String> {
        self.state
            .read()
            .unwrap()
            .entries
            .iter()
            .map(|entry| entry.spec.id.clone())
            .collect()
    }
    pub fn muse_instances(&self) -> Vec<Arc<MuseProvider>> {
        self.state.read().unwrap().muse.clone()
    }

    pub async fn catalog(&self, provider: &str, cwd: Option<&Path>, wait: bool) -> Result<Value> {
        self.catalogs.catalog(provider, cwd, wait).await
    }
    pub async fn list(&self, cwd: Option<&Path>, wait: bool) -> Result<Vec<Value>> {
        self.catalogs.list(cwd, wait).await
    }
    pub async fn handle(&self, context: &RequestContext, message: &Value) -> Result<bool> {
        self.catalogs.handle(context, message).await
    }

    pub fn validate_options(&self, provider: &str, input: &Value) -> Result<Value> {
        let entry = self.entry(provider)?;
        options::validate(provider, &entry.spec.family, input)
    }

    pub fn apply_tool_policy(
        &self,
        provider: &str,
        mut config: Value,
        policy: &Value,
    ) -> Result<Value> {
        let entry = self.entry(provider)?;
        let object = config
            .as_object_mut()
            .context("Agent configuration must be an object")?;
        if policy.is_null() {
            object.remove("toolPolicy");
            return Ok(config);
        }
        if entry.definition["supportsExactMcpPreapproval"] != true {
            return Err(options::ToolPolicyUnsupportedError::new(provider, None).into());
        }
        if provider == "hub-e2e" && entry.spec.family == "acp" {
            let grants = policy["preapproved"]
                .as_array()
                .context("Tool policy preapproved must be an array")?;
            for grant in grants {
                let tool = grant["tool"].as_str().unwrap_or("");
                let mut chars = tool.chars();
                let valid = chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
                    && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
                if grant["kind"] != "mcp" || grant["server"] != "hub" || !valid {
                    return Err(options::ToolPolicyUnsupportedError::new(provider,Some(format!("Provider '{provider}' accepts only exact MCP tool grants for the injected 'hub' server"))).into());
                }
            }
            object.insert("toolPolicy".into(), json!({"preapproved":grants}));
        } else {
            object.insert("toolPolicy".into(), policy.clone());
        }
        Ok(config)
    }

    /// provider-snapshot-manager.ts resolveCreateConfig: the target must be a
    /// ready snapshot entry, whose modes (or an explicit empty list) drive the
    /// provider policy.
    pub async fn resolve_create_config(&self, input: &Value) -> Result<Value> {
        let provider = input["provider"]
            .as_str()
            .context("Missing target provider")?;
        let cwd = input.get("cwd").and_then(Value::as_str).map(Path::new);
        let snapshot = self.catalogs.ready(provider, cwd).await?;
        let entry = self
            .entry(provider)
            .map_err(|_| anyhow!("Provider {provider} is not configured"))?;
        let mut resolved = input.clone();
        resolved["availableModes"] = snapshot
            .get("modes")
            .filter(|modes| !modes.is_null())
            .cloned()
            .unwrap_or_else(|| json!([]));
        entry
            .client
            .resolve_create_config(&resolved)
            .unwrap_or_else(|| create_config::resolve_default(&resolved))
    }

    pub fn is_create_config_unattended(&self, provider: &str, input: &Value) -> Result<bool> {
        Ok(self
            .entry(provider)?
            .client
            .is_create_config_unattended(input)
            .unwrap_or_else(|| create_config::is_default_unattended(input)))
    }

    pub async fn shutdown(&self) -> Result<()> {
        self.catalogs.shutdown().await;
        let clients = std::mem::take(&mut self.state.write().unwrap().owned);
        let results =
            futures_util::future::join_all(clients.iter().map(|client| client.shutdown())).await;
        let mut first = None;
        for result in results {
            if let Err(error) = result {
                tracing::warn!(error=%error,"Provider shutdown failed");
                if first.is_none() {
                    first = Some(error);
                }
            }
        }
        match first {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}
