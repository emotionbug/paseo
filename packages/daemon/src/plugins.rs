//! Native extension lifecycle. JavaScript runs only for explicitly configured
//! third-party backend extensions in an optional, externally supplied Node runtime.
mod lifecycle;
mod managed;
mod manifest;
mod operations;
mod provider;
#[cfg(test)]
mod tests;
mod worker;

use crate::{
    providers::api::{AgentProvider, ProviderSpec},
    state::AppState,
    transport::RequestContext,
};
use anyhow::{Context, Result, anyhow, bail};
use managed::ManagedSources;
pub(crate) use managed::split_source_reference;
pub(crate) use lifecycle::WORKSPACE_CREATE_FIELDS;
pub use provider::StaleProviderSessionError;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, RwLock, Weak},
};
use tokio::sync::{Mutex as AsyncMutex, Semaphore};
use tokio_util::sync::CancellationToken;
use worker::PluginWorker;

const BUILTINS: &[(&str, &str)] = &[
    ("antigravity-provider", ">=0.10.0"),
    ("claude-usage-source", ">=0.9.2"),
    ("codex-usage-source", ">=0.11.0"),
    ("copilot-usage-source", ">=0.9.2"),
    ("cursor-usage-source", ">=0.9.2"),
    ("grok-usage-source", ">=0.9.2"),
    ("hoplite-provider", ">=0.11.0"),
    ("kimi-usage-source", ">=0.9.2"),
    ("minimax-usage-source", ">=0.9.2"),
    ("muse-provider", ">=0.11.0"),
    ("opencode-go-usage-source", ">=0.9.2"),
    ("zai-usage-source", ">=0.9.2"),
];
const RESERVED_PROVIDERS: &[&str] = &[
    "claude",
    "codex",
    "copilot",
    "opencode",
    "pi",
    "omp",
    "mock",
    "mock-slow",
    "antigravity",
    "hoplite",
    "muse",
];
const RESERVED_USAGE: &[&str] = &[
    "claude",
    "codex",
    "copilot",
    "cursor",
    "grok",
    "kimi",
    "minimax",
    "opencode-go",
    "zai",
];

struct LoadedPlugin {
    worker: Option<Arc<PluginWorker>>,
    manifest: Value,
    generation: String,
    providers: Vec<Value>,
    usage: Vec<Value>,
}
#[derive(Default)]
struct LogTail {
    entries: VecDeque<Value>,
    bytes: usize,
    sequence: u64,
}

pub struct PluginService {
    app: Weak<AppState>,
    current: RwLock<Value>,
    loaded: RwLock<BTreeMap<String, LoadedPlugin>>,
    errors: RwLock<BTreeMap<String, String>>,
    logs: Mutex<BTreeMap<String, LogTail>>,
    managed: OnceLock<Arc<ManagedSources>>,
    lifecycle: AsyncMutex<()>,
    operations: AsyncMutex<()>,
    events: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    event_slots: Arc<Semaphore>,
    stopping: CancellationToken,
}

impl PluginService {
    pub fn new(app: Weak<AppState>) -> Self {
        Self {
            app,
            current: RwLock::new(json!({})),
            loaded: RwLock::new(BTreeMap::new()),
            errors: RwLock::new(BTreeMap::new()),
            logs: Mutex::new(BTreeMap::new()),
            managed: OnceLock::new(),
            lifecycle: AsyncMutex::new(()),
            operations: AsyncMutex::new(()),
            events: Mutex::new(Vec::new()),
            event_slots: Arc::new(Semaphore::new(256)),
            stopping: CancellationToken::new(),
        }
    }
    pub async fn initialize(&self) -> Result<()> {
        let app = self.app.upgrade().context("Daemon is shutting down")?;
        let config = app.config.snapshot();
        let managed = Arc::new(ManagedSources::new(
            app.home.clone(),
            app.config.environment(),
        ));
        drop(app);
        managed.initialize().await?;
        managed.configure_registry(&config)?;
        self.managed
            .set(managed)
            .map_err(|_| anyhow!("Plugin service is already initialized"))?;
        self.apply_config(&config, &json!({})).await
    }
    pub async fn apply_config(&self, next: &Value, _previous: &Value) -> Result<()> {
        let _guard = self.lifecycle.lock().await;
        if self.stopping.is_cancelled() {
            bail!("Plugin service is shutting down");
        }
        if let Some(managed) = self.managed.get() {
            managed.configure_registry(next)?;
        }
        let previous = self.current.read().unwrap().clone();
        *self.current.write().unwrap() = next.clone();
        let sources = next.get("plugins").and_then(Value::as_object);
        let current_ids: Vec<String> = self.loaded.read().unwrap().keys().cloned().collect();
        for id in current_ids {
            let desired = sources.and_then(|sources| sources.get(&id));
            let changed = previous.get("plugins").and_then(|sources| sources.get(&id)) != desired;
            if next["pluginsEnabled"] != true
                || desired.is_none_or(|source| source["enabled"] == false)
                || changed
            {
                self.stop_plugin(&id).await?;
            }
        }
        if next["pluginsEnabled"] == true {
            if let Some(sources) = sources {
                for (id, source) in sources {
                    if source["enabled"] == false || self.loaded.read().unwrap().contains_key(id) {
                        continue;
                    }
                    if let Err(error) = self.start_plugin(id, source).await {
                        self.record_failure(id, error.to_string());
                    }
                    self.changed(id).await;
                }
            }
        }
        Ok(())
    }
    async fn start_plugin(&self, id: &str, source: &Value) -> Result<()> {
        manifest::validate_id(id)?;
        if is_builtin(id) {
            bail!("Plugin ID \"{id}\" is reserved for a built-in plugin");
        }
        let app = self.app.upgrade().context("Daemon is shutting down")?;
        let directory = manifest::resolve_directory(
            source["path"]
                .as_str()
                .context("Plugin source requires path")?,
            &app.user_home,
        )?;
        let settings = app.home.join("plugin-settings").join(id);
        let env = app.config.environment();
        drop(app);
        let manifest = manifest::read(&directory).await?;
        manifest::check_compatibility(&manifest)?;
        self.append_log(id, "stdout", "[paseo] Loading plugin");
        manifest::require_server_entry(&directory).await?;
        let worker = PluginWorker::start(self.app.clone(), id, &directory, &settings, env).await?;
        let result = self
            .publish(id, directory, manifest, Some(worker.clone()))
            .await;
        if let Err(error) = result {
            let _ = worker.shutdown().await;
            return Err(error);
        }
        self.errors.write().unwrap().remove(id);
        self.append_log(id, "stdout", "[paseo] Plugin ready");
        Ok(())
    }
    async fn publish(
        &self,
        id: &str,
        directory: PathBuf,
        manifest: Value,
        worker: Option<Arc<PluginWorker>>,
    ) -> Result<()> {
        let metadata = worker
            .as_ref()
            .map(|worker| worker.metadata.read().unwrap().clone())
            .unwrap_or_else(|| json!({}));
        let mut providers = metadata["providers"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let usage = metadata["usageSources"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let existing_providers: BTreeSet<String> = self
            .registered_providers()
            .into_iter()
            .filter_map(|p| p["id"].as_str().map(str::to_owned))
            .collect();
        let existing_usage: BTreeSet<String> = self
            .registered_usage_sources()
            .into_iter()
            .filter_map(|p| p["id"].as_str().map(str::to_owned))
            .collect();
        let mut seen = BTreeSet::new();
        for provider in &mut providers {
            let provider_id = provider["id"]
                .as_str()
                .context("Plugin provider requires id")?;
            manifest::validate_contribution_id(provider_id)?;
            if RESERVED_PROVIDERS.contains(&provider_id) {
                bail!("Plugin {id} cannot register builtin provider ID \"{provider_id}\"");
            }
            if !seen.insert(provider_id.to_owned()) || existing_providers.contains(provider_id) {
                bail!("Plugin {id} cannot register provider ID \"{provider_id}\" twice");
            }
            if let Some(icon) = provider.get("iconPath").and_then(Value::as_str) {
                let svg = manifest::icon(&directory, icon).await?;
                provider["iconSvg"] = json!(svg);
            }
        }
        seen.clear();
        for source in &usage {
            let source_id = source["id"].as_str().context("Usage source requires id")?;
            manifest::validate_contribution_id(source_id)?;
            if RESERVED_USAGE.contains(&source_id)
                || existing_usage.contains(source_id)
                || !seen.insert(source_id.to_owned())
            {
                bail!("Duplicate usage source ID: {source_id}");
            }
        }
        if worker.as_ref().is_some_and(|worker| !worker.is_live()) {
            bail!("Plugin worker closed before publication: {id}");
        }
        self.loaded.write().unwrap().insert(
            id.to_owned(),
            LoadedPlugin {
                worker,
                manifest,
                generation: uuid::Uuid::new_v4().to_string(),
                providers,
                usage,
            },
        );
        Ok(())
    }
    pub(crate) async fn worker_failed(&self, worker: &Arc<PluginWorker>) {
        let removed = {
            let mut loaded = self.loaded.write().unwrap();
            let current = loaded
                .get(&worker.id)
                .and_then(|plugin| plugin.worker.as_ref())
                .is_some_and(|current| Arc::ptr_eq(current, worker));
            if current {
                loaded.remove(&worker.id);
            }
            current
        };
        if !removed {
            return;
        }
        let error = worker.metadata.read().unwrap()["error"]
            .as_str()
            .unwrap_or("Plugin worker closed")
            .to_owned();
        self.record_failure(&worker.id, error);
        self.changed(&worker.id).await;
        if let Some(app) = self.app.upgrade() {
            let config = self.current.read().unwrap().clone();
            if let Err(error) = app.providers.apply_config(&config, &config).await {
                tracing::warn!(%error,"Failed to remove stopped plugin provider registrations");
            }
        }
    }
    async fn stop_plugin(&self, id: &str) -> Result<()> {
        let loaded = self.loaded.write().unwrap().remove(id);
        if let Some(loaded) = loaded {
            if let Some(worker) = loaded.worker {
                let result = worker.shutdown().await;
                let diagnostics = worker.logs();
                if !diagnostics.is_empty() {
                    self.append_log(id, "stderr", &diagnostics);
                }
                result?;
            }
        }
        self.changed(id).await;
        Ok(())
    }
    pub fn registered_providers(&self) -> Vec<Value> {
        let mut definitions: Vec<Value> = self
            .loaded
            .read()
            .unwrap()
            .iter()
            .filter(|(_, loaded)| {
                loaded
                    .worker
                    .as_ref()
                    .is_some_and(|worker| worker.is_live())
            })
            .flat_map(|(plugin_id, loaded)| {
                loaded.providers.iter().map(move |provider| {
                    let mut definition = provider.clone();
                    definition["pluginId"] = json!(plugin_id);
                    definition["generation"] = json!(loaded.generation);
                    definition["description"] =
                        provider.get("description").cloned().unwrap_or_else(|| {
                            json!(format!(
                                "Plugin provider {}",
                                provider["label"].as_str().unwrap_or("")
                            ))
                        });
                    definition["defaultModeId"] = Value::Null;
                    definition["modes"] = json!([]);
                    definition["supportsExactMcpPreapproval"] = json!(true);
                    definition
                })
            })
            .collect();
        definitions.sort_by(compare_ids);
        definitions
    }
    pub fn create_provider(&self, id: &str, spec: ProviderSpec) -> Result<Arc<dyn AgentProvider>> {
        let loaded = self.loaded.read().unwrap();
        for plugin in loaded.values() {
            if let Some(metadata) = plugin
                .providers
                .iter()
                .find(|provider| provider["id"] == id)
            {
                let worker = plugin
                    .worker
                    .as_ref()
                    .filter(|worker| worker.is_live())
                    .context("Plugin is not available")?
                    .clone();
                return Ok(Arc::new(provider::PluginProvider::new(
                    spec,
                    worker,
                    metadata.clone(),
                )));
            }
        }
        bail!("Unknown plugin provider: {id}")
    }
    pub fn registered_usage_sources(&self) -> Vec<Value> {
        self.loaded
            .read()
            .unwrap()
            .iter()
            .filter(|(_, loaded)| {
                loaded
                    .worker
                    .as_ref()
                    .is_some_and(|worker| worker.is_live())
            })
            .flat_map(|(plugin_id, loaded)| {
                loaded.usage.iter().map(move |source| {
                    let mut source = source.clone();
                    source["pluginId"] = json!(plugin_id);
                    source["generation"] = json!(loaded.generation);
                    source
                })
            })
            .collect()
    }
    fn usage_worker(&self, source_id: &str) -> Result<Arc<PluginWorker>> {
        self.loaded
            .read()
            .unwrap()
            .values()
            .find(|loaded| loaded.usage.iter().any(|source| source["id"] == source_id))
            .and_then(|loaded| loaded.worker.clone())
            .filter(|worker| worker.is_live())
            .with_context(|| format!("Unknown usage source: {source_id}"))
    }
    pub async fn discover_usage(
        &self,
        source_id: &str,
        scope: Value,
        cancel: CancellationToken,
    ) -> Result<Value> {
        let output = self
            .usage_worker(source_id)?
            .request(
                json!({"type":"usage.discover","sourceId":source_id,"scope":scope}),
                cancel,
            )
            .await?;
        if !output.is_array() {
            bail!("Invalid usage discovery from {source_id}");
        }
        Ok(output)
    }
    pub async fn fetch_usage(
        &self,
        source_id: &str,
        input: Value,
        cancel: CancellationToken,
    ) -> Result<Value> {
        self.usage_worker(source_id)?
            .request(
                json!({"type":"usage.fetch","sourceId":source_id,"input":input}),
                cancel,
            )
            .await
    }
    pub async fn invoke(
        &self,
        id: &str,
        method: &str,
        input: Value,
        cancel: CancellationToken,
    ) -> Result<Value> {
        let worker = self
            .loaded
            .read()
            .unwrap()
            .get(id)
            .and_then(|plugin| plugin.worker.clone())
            .filter(|worker| worker.is_live())
            .with_context(|| format!("Plugin is not available: {id}"))?;
        if !worker.metadata.read().unwrap()["methods"]
            .as_array()
            .is_some_and(|methods| methods.iter().any(|candidate| candidate == method))
        {
            bail!("Plugin {id} does not contribute RPC {method}");
        }
        worker
            .request(
                json!({"type":"invoke","method":method,"input":input}),
                cancel,
            )
            .await
    }
    pub fn catalog(&self) -> Vec<Value> {
        let mut entries: Vec<Value> = BUILTINS.iter().map(|(id, requirement)| json!({"id":id,"clientBundle":"","requirements":{"paseo":requirement}})).collect();
        entries.extend(
            self.loaded
                .read()
                .unwrap()
                .iter()
                .filter(|(_, plugin)| plugin.worker.as_ref().is_none_or(|worker| worker.is_live()))
                .map(|(id, plugin)| {
                    let mut value = json!({"id":id,"clientBundle":""});
                    if let Some(requirements) = plugin.manifest.get("requirements") {
                        value["requirements"] = requirements.clone();
                    }
                    value
                }),
        );
        entries.sort_by(compare_ids);
        entries
    }
    pub async fn list_plugins(&self) -> Result<Vec<Value>> {
        let config = self.current.read().unwrap().clone();
        let mut result = Vec::new();
        if let Some(sources) = config["plugins"].as_object() {
            for (id, source) in sources {
                let enabled = source["enabled"] != false;
                let live = self.loaded.read().unwrap().get(id).is_some_and(|plugin| {
                    plugin.worker.as_ref().is_none_or(|worker| worker.is_live())
                });
                let status = if !enabled || config["pluginsEnabled"] != true {
                    "disabled"
                } else if live {
                    "running"
                } else {
                    "failed"
                };
                let mut item =
                    json!({"id":id,"path":source["path"],"enabled":enabled,"status":status});
                let path = Path::new(source["path"].as_str().unwrap_or(""));
                if let Ok(manifest) = manifest::read(path).await {
                    for field in ["name", "description", "icon", "media"] {
                        if let Some(value) = manifest.get(field) {
                            item[field] = value.clone();
                        }
                    }
                }
                if let Some(managed) = self.managed.get() {
                    if let Ok(installation) = managed.describe(id, path).await {
                        item["installation"] = installation;
                    }
                }
                if item["installation"]["identity"]["kind"] == "git" {
                    item["source"] = json!("git");
                    item["remote"] = item["installation"]["identity"]["remote"].clone();
                    item["commit"] = item["installation"]["currentRevision"].clone();
                }
                if status == "failed" {
                    if let Some(error) = self.errors.read().unwrap().get(id).cloned() {
                        item["error"] = json!(error);
                    } else if let Some(error) = self
                        .loaded
                        .read()
                        .unwrap()
                        .get(id)
                        .and_then(|plugin| plugin.worker.as_ref())
                        .and_then(|worker| worker.metadata.read().unwrap().get("error").cloned())
                    {
                        item["error"] = error;
                    }
                }
                result.push(item);
            }
        }
        result.sort_by(compare_ids);
        Ok(result)
    }
    fn source(&self, id: &str) -> Result<Value> {
        manifest::validate_id(id)?;
        self.current.read().unwrap()["plugins"]
            .get(id)
            .cloned()
            .with_context(|| format!("Plugin is not configured: {id}"))
    }
    fn managed(&self) -> Result<&Arc<ManagedSources>> {
        self.managed
            .get()
            .context("Plugin service is not initialized")
    }
    async fn item(&self, id: &str) -> Result<Value> {
        self.list_plugins()
            .await?
            .into_iter()
            .find(|item| item["id"] == id)
            .with_context(|| format!("Plugin is not configured: {id}"))
    }
    async fn changed(&self, id: &str) {
        if let Some(app) = self.app.upgrade() {
            let _ = app.transport.publish_event(json!({"type":"status","payload":{"status":"plugin_catalog_changed","pluginId":id}})).await;
        }
    }
    pub(crate) async fn settings_changed(&self, id: &str, settings_id: &str) {
        if let Some(app) = self.app.upgrade() {
            let _ = app.transport.publish_event(json!({"type":"status","payload":{"status":"plugin_settings_changed","pluginId":id,"settingsId":settings_id}})).await;
        }
    }
    fn record_failure(&self, id: &str, error: String) {
        self.append_log(
            id,
            "stderr",
            &format!("[paseo] Plugin failed to load: {error}"),
        );
        self.errors.write().unwrap().insert(id.to_owned(), error);
    }
    fn append_log(&self, id: &str, stream: &str, message: &str) {
        let mut logs = self.logs.lock().unwrap();
        let tail = logs.entry(id.to_owned()).or_default();
        tail.sequence += 1;
        let mut end = message.len().min(16 * 1024);
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        tail.bytes += end;
        tail.entries.push_back(json!({"sequence":tail.sequence,"timestamp":chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis,true),"stream":stream,"message":&message[..end]}));
        while tail.entries.len() > 500 || tail.bytes > 256 * 1024 {
            if let Some(entry) = tail.entries.pop_front() {
                tail.bytes -= entry["message"].as_str().map_or(0, str::len);
            }
        }
    }
    pub async fn shutdown(&self) -> Result<()> {
        self.stopping.cancel();
        self.drain_events().await?;
        let _guard = self.lifecycle.lock().await;
        let ids: Vec<String> = self.loaded.read().unwrap().keys().cloned().collect();
        let mut errors = Vec::new();
        for id in ids {
            if let Err(error) = self.stop_plugin(&id).await {
                errors.push(error.to_string());
            }
        }
        if !errors.is_empty() {
            bail!("{}", errors.join("; "));
        }
        Ok(())
    }
}
fn is_builtin(id: &str) -> bool {
    BUILTINS.iter().any(|(builtin, _)| *builtin == id)
}
fn compare_ids(left: &Value, right: &Value) -> std::cmp::Ordering {
    crate::json::locale_compare(
        left["id"].as_str().unwrap_or(""),
        right["id"].as_str().unwrap_or(""),
    )
}
