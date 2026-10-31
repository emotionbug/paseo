mod refresh;
mod state;
#[cfg(test)]
mod tests;

use super::{client, wire};
use crate::{state::AppState, transport::RequestContext};
use anyhow::{Context, Result, anyhow};
pub(crate) use refresh::refresh_activity;
use serde_json::{Value, json};
use state::{Binding, Cache, GLOBAL, Load, ProviderState, completed, initial};
pub(crate) use state::{Snapshot, SnapshotRecord};
use std::{
    collections::{HashMap, HashSet},
    future::Future,
    path::Path,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use tokio::{
    sync::{Mutex as AsyncMutex, Semaphore, mpsc, watch},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

type Transition = (Snapshot, Snapshot);

struct Inner {
    app: Weak<AppState>,
    cache: Mutex<Cache>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    cancel: CancellationToken,
    transitions: mpsc::UnboundedSender<Transition>,
    receiver: Mutex<Option<mpsc::UnboundedReceiver<Transition>>>,
    delivery: Mutex<HashMap<String, Weak<AsyncMutex<()>>>>,
}

#[derive(Clone)]
pub(crate) struct CatalogService {
    inner: Arc<Inner>,
}

impl CatalogService {
    pub(crate) fn new(app: Weak<AppState>) -> Self {
        let (transitions, receiver) = mpsc::unbounded_channel();
        Self {
            inner: Arc::new(Inner {
                app,
                cache: Mutex::new(Cache::default()),
                tasks: Mutex::new(Vec::new()),
                cancel: CancellationToken::new(),
                transitions,
                receiver: Mutex::new(Some(receiver)),
                delivery: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub(super) fn app(&self) -> Result<Arc<AppState>> {
        self.inner.app.upgrade().context("Daemon is shutting down")
    }
    pub(super) async fn delivery(
        &self,
        context: &RequestContext,
    ) -> tokio::sync::OwnedMutexGuard<()> {
        let gate = {
            let mut gates = self.inner.delivery.lock().unwrap();
            gates.retain(|_, gate| gate.strong_count() > 0);
            if let Some(gate) = gates.get(context.source_id()).and_then(Weak::upgrade) {
                gate
            } else {
                let gate = Arc::new(AsyncMutex::new(()));
                gates.insert(context.source_id().to_owned(), Arc::downgrade(&gate));
                gate
            }
        };
        gate.lock_owned().await
    }

    fn spawn(&self, operation: impl Future<Output = ()> + Send + 'static) {
        let mut tasks = self.inner.tasks.lock().unwrap();
        if self.inner.cancel.is_cancelled() {
            return;
        }
        tasks.retain(|task| !task.is_finished());
        tasks.push(tokio::spawn(operation));
    }

    fn start_dispatcher(&self) {
        let Some(mut receiver) = self.inner.receiver.lock().unwrap().take() else {
            return;
        };
        let service = self.clone();
        self.spawn(async move {
            loop {
                let transition = tokio::select! {
                    biased;
                    _ = service.inner.cancel.cancelled() => break,
                    transition = receiver.recv() => match transition { Some(transition) => transition, None => break },
                };
                let Ok(app) = service.app() else { break; };
                if !app.transport.has_event_demand("providers_snapshot_update") { continue; }
                for source in app.transport.event_sources("providers_snapshot_update") {
                    if wire::same_visible_records(&transition.0, &transition.1, &source) { continue; }
                    let _delivery = service.delivery(&source).await;
                    let message = json!({"type": "providers_snapshot_update", "payload": wire::snapshot_payload(&transition.1, &source, None)});
                    if let Err(error) = source.emit_event(message).await {
                        tracing::debug!(%error, source_id = source.source_id(), "Provider snapshot source detached");
                    }
                }
            }
        });
    }

    fn publish(&self, cache: &mut Cache, cwds: &[String]) {
        for transition in cache.publish(cwds) {
            let _ = self.inner.transitions.send(transition);
        }
    }

    pub(crate) async fn registry_changed(&self) {
        let Ok(app) = self.app() else {
            return;
        };
        self.start_dispatcher();
        let entries = app.providers.entries();
        let (targets, changed) = {
            let mut cache = self.inner.cache.lock().unwrap();
            if cache.stopped {
                return;
            }
            let mut changed = HashSet::new();
            let mut next = HashMap::new();
            let order = entries.iter().map(|entry| entry.spec.id.clone()).collect();
            for entry in entries {
                let id = entry.spec.id.clone();
                let previous = cache.providers.get(&id);
                if let Some(previous) =
                    previous.filter(|previous| Arc::ptr_eq(&previous.entry, &entry))
                {
                    next.insert(id, previous.clone());
                    continue;
                }
                changed.insert(id.clone());
                let limit = previous
                    .map(|previous| previous.limit.clone())
                    .unwrap_or_else(|| Arc::new(Semaphore::new(4)));
                if let Some(previous) = previous {
                    previous.cancel.cancel();
                }
                let replacement_models =
                    if !entry.profile_models.is_empty() && !entry.profile_models_are_additive {
                        Some(
                            (|| {
                                let profiles = client::resolve_configured_models(
                                    &id,
                                    entry.client.as_ref(),
                                    &entry.profile_models,
                                )?;
                                let additions = client::resolve_configured_models(
                                    &id,
                                    entry.client.as_ref(),
                                    &entry.additional_models,
                                )?;
                                Ok::<_, anyhow::Error>(client::merge_models(
                                    &id,
                                    &profiles,
                                    &additions,
                                    Vec::new(),
                                    false,
                                ))
                            })()
                            .map_err(|error| error.to_string()),
                        )
                    } else {
                        None
                    };
                next.insert(
                    id,
                    Arc::new(ProviderState {
                        initial: initial(&entry),
                        entry,
                        limit,
                        cancel: self.inner.cancel.child_token(),
                        replacement_models,
                    }),
                );
            }
            for (id, previous) in &cache.providers {
                if !next.contains_key(id) {
                    previous.cancel.cancel();
                    changed.insert(id.clone());
                }
            }
            cache.providers = next;
            cache.order = order;
            cache.catalogs.retain(|_, catalogs| {
                catalogs.retain(|provider, catalog| {
                    if !changed.contains(provider) {
                        return true;
                    }
                    if let Some(load) = &catalog.load {
                        load.cancel.cancel();
                    }
                    false
                });
                !catalogs.is_empty()
            });
            for target in cache.targets.values_mut() {
                target.bindings.retain(|id, _| !changed.contains(id));
            }
            let targets = cache.target_order.clone();
            self.publish(&mut cache, &targets);
            let changed = cache
                .order
                .iter()
                .filter(|id| changed.contains(*id))
                .cloned()
                .collect::<Vec<_>>();
            (targets, changed)
        };
        for cwd in targets {
            self.begin_loads(&cwd, &changed, false);
        }
    }

    pub(crate) async fn catalog(
        &self,
        provider: &str,
        cwd: Option<&Path>,
        wait: bool,
    ) -> Result<Value> {
        let providers = [provider.to_owned()];
        self.snapshot(cwd, Some(&providers), wait)
            .await?
            .records
            .into_iter()
            .find(|record| record.entry["provider"] == provider)
            .map(|record| record.entry.clone())
            .ok_or_else(|| anyhow!("Provider {provider} is not configured"))
    }

    /// getReadyProvider: reads that need a usable catalog reject with the
    /// snapshot entry's own state instead of proceeding without one.
    pub(crate) async fn ready(&self, provider: &str, cwd: Option<&Path>) -> Result<Value> {
        require_ready(provider, self.catalog(provider, cwd, true).await?)
    }

    pub(crate) async fn list(&self, cwd: Option<&Path>, wait: bool) -> Result<Vec<Value>> {
        Ok(self
            .snapshot(cwd, None, wait)
            .await?
            .records
            .into_iter()
            .map(|record| record.entry.clone())
            .collect())
    }

    pub(crate) async fn handle(&self, context: &RequestContext, message: &Value) -> Result<bool> {
        // Work shared across sources continues; replies/events are still fenced by
        // the exact request/subscription owner and its current permission grant.
        let cancel = context.cancellation();
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Ok(true),
            result = wire::handle(self, context, message) => result,
        }
    }

    pub(super) async fn snapshot(
        &self,
        cwd: Option<&Path>,
        providers: Option<&[String]>,
        wait: bool,
    ) -> Result<Snapshot> {
        let cwd = refresh::target(cwd)?;
        let (selected, current) = {
            let mut cache = self.inner.cache.lock().unwrap();
            let current = cache.target(&cwd).snapshot.clone();
            (cache.selected(providers), current)
        };
        // Key discovery is provider-owned and runs on every read, not on a TTL.
        // Capture the synchronous read before starting work on another thread.
        let pending = self.begin_loads(&cwd, &selected, false);
        if !wait {
            return Ok(current);
        }
        futures_util::future::join_all(pending.into_iter().map(completed)).await;
        Ok(self
            .inner
            .cache
            .lock()
            .unwrap()
            .target(&cwd)
            .snapshot
            .clone())
    }

    pub(super) async fn refresh(
        &self,
        cwd: Option<&Path>,
        providers: Option<&[String]>,
    ) -> Result<()> {
        let cwd = refresh::target(cwd)?;
        let (selected, targets) = {
            let mut cache = self.inner.cache.lock().unwrap();
            cache.target(&cwd);
            let selected = cache.selected(providers);
            if cwd == GLOBAL {
                let selected_set = selected.iter().map(String::as_str).collect::<HashSet<_>>();
                for catalogs in cache.catalogs.values_mut() {
                    for (provider, catalog) in catalogs {
                        if !selected_set.contains(provider.as_str()) {
                            continue;
                        }
                        if let Some(load) = catalog.load.take() {
                            load.cancel.cancel();
                        }
                        catalog.stale = true;
                    }
                }
            }
            (selected, cache.target_order.clone())
        };
        let pending = self.begin_loads(&cwd, &selected, true);
        futures_util::future::join_all(pending.into_iter().map(completed)).await;
        if cwd == GLOBAL {
            let pending = targets
                .into_iter()
                .filter(|target| target != GLOBAL)
                .flat_map(|target| self.begin_loads(&target, &selected, false))
                .collect::<Vec<_>>();
            futures_util::future::join_all(pending.into_iter().map(completed)).await;
        }
        Ok(())
    }

    fn timeout_ms(&self) -> u64 {
        let app = self.inner.app.upgrade();
        let config = app.as_ref().map(|app| app.config.snapshot());
        refresh::deadline(
            config
                .as_ref()
                .and_then(|config| config.get("providerCatalogRefreshTimeoutMs")),
        )
    }

    fn begin_loads(
        &self,
        cwd: &str,
        providers: &[String],
        force: bool,
    ) -> Vec<watch::Receiver<bool>> {
        providers
            .iter()
            .filter_map(|provider| self.begin_load(cwd, provider, force))
            .collect()
    }

    fn begin_load(&self, cwd: &str, provider: &str, force: bool) -> Option<watch::Receiver<bool>> {
        let (sender, done) = watch::channel(false);
        let (state, binding) = {
            let mut cache = self.inner.cache.lock().unwrap();
            if cache.stopped {
                return None;
            }
            let state = cache.providers.get(provider)?.clone();
            let id = cache.next_id();
            let target = cache.target(cwd);
            let previous = target.bindings.get(provider);
            let binding = Binding::superseding(id, previous, force, done.clone());
            target.bindings.insert(provider.to_owned(), binding.clone());
            (state, binding)
        };
        let service = self.clone();
        let cwd = cwd.to_owned();
        let provider = provider.to_owned();
        self.spawn(async move {
            service
                .resolve_binding(&cwd, &provider, state, binding)
                .await;
            let _ = sender.send(true);
        });
        Some(done)
    }

    async fn resolve_binding(
        &self,
        cwd: &str,
        provider: &str,
        state: Arc<ProviderState>,
        binding: Binding,
    ) {
        if !state.entry.enabled {
            return;
        }
        let options = refresh::fetch_options(cwd, binding.force);
        let cancel = state.cancel.child_token();
        let key_result = tokio::select! {
            biased;
            _ = state.cancel.cancelled() => return,
            result = tokio::time::timeout(Duration::from_millis(self.timeout_ms()), state.entry.client.catalog_cache_key(options.clone(), cancel.clone())) => {
                match result {
                    Ok(result) => result,
                    Err(_) => { cancel.cancel(); Err(anyhow!("Timed out resolving {provider} catalogue key")) }
                }
            }
        };
        let mut start = None;
        let wait_for = {
            let mut cache = self.inner.cache.lock().unwrap();
            if cache.stopped
                || !cache
                    .providers
                    .get(provider)
                    .is_some_and(|current| Arc::ptr_eq(current, &state))
            {
                return;
            }
            let current = cache
                .targets
                .get(cwd)
                .and_then(|target| target.bindings.get(provider));
            if !current.is_some_and(|current| current.id == binding.id) {
                current.map(|current| current.done.clone())
            } else {
                match key_result {
                    Err(error) => {
                        let current = cache
                            .targets
                            .get_mut(cwd)
                            .unwrap()
                            .bindings
                            .get_mut(provider)
                            .unwrap();
                        current.fail_key(&state.initial, &error.to_string());
                        self.publish(&mut cache, &[cwd.to_owned()]);
                        None
                    }
                    Ok(shared) => {
                        let key = crate::json::stringify(&match shared {
                            Some(shared) => json!(["provider", shared]),
                            None => json!(["target", cwd]),
                        })
                        .expect("string cache keys are JSON representable");
                        let current = cache
                            .targets
                            .get_mut(cwd)
                            .unwrap()
                            .bindings
                            .get_mut(provider)
                            .unwrap();
                        current.force = false;
                        current.key = Some(key.clone());
                        current.failure = None;
                        let catalog = cache
                            .catalogs
                            .entry(key.clone())
                            .or_default()
                            .entry(provider.to_owned())
                            .or_default();
                        let reuse = catalog.reusable(binding.force);
                        let wait_for = if reuse {
                            catalog.load.as_ref().map(|load| load.done.clone())
                        } else {
                            if let Some(load) = catalog.load.take() {
                                load.cancel.cancel();
                            }
                            let (sender, done) = watch::channel(false);
                            let cancel = state.cancel.child_token();
                            let id = cache.next_id();
                            let catalog = cache
                                .catalogs
                                .get_mut(&key)
                                .unwrap()
                                .get_mut(provider)
                                .unwrap();
                            catalog.stale = false;
                            catalog.load = Some(Load {
                                id,
                                done: done.clone(),
                                cancel: cancel.clone(),
                            });
                            start = Some((key, id, cancel, sender));
                            Some(done)
                        };
                        self.publish(&mut cache, &[cwd.to_owned()]);
                        wait_for
                    }
                }
            }
        };
        if let Some((key, id, cancel, sender)) = start {
            let service = self.clone();
            let provider = provider.to_owned();
            self.spawn(async move {
                service
                    .load_catalog(&key, &provider, id, state, options, cancel)
                    .await;
                let _ = sender.send(true);
            });
        }
        if let Some(wait_for) = wait_for {
            tokio::select! { biased; _ = self.inner.cancel.cancelled() => {}, _ = completed(wait_for) => {} }
        }
    }

    async fn load_catalog(
        &self,
        key: &str,
        provider: &str,
        id: u64,
        state: Arc<ProviderState>,
        options: Value,
        cancel: CancellationToken,
    ) {
        let permit = tokio::select! {
            biased;
            _ = cancel.cancelled() => return,
            permit = state.limit.acquire() => match permit { Ok(permit) => permit, Err(_) => return },
        };
        if cancel.is_cancelled() {
            return;
        }
        let result = refresh::discover(&state, options, self.timeout_ms(), cancel.clone()).await;
        drop(permit);
        let mut cache = self.inner.cache.lock().unwrap();
        if cache.stopped || cancel.is_cancelled() && state.cancel.is_cancelled() {
            return;
        }
        let Some(catalog) = cache
            .catalogs
            .get_mut(key)
            .and_then(|catalogs| catalogs.get_mut(provider))
        else {
            return;
        };
        if !catalog.load.as_ref().is_some_and(|load| load.id == id) {
            return;
        }
        if result.entry["status"] == "error" {
            tracing::warn!(provider, error = ?result.entry["error"], "Failed to refresh provider snapshot");
        }
        catalog.result = Some(result);
        catalog.load = None;
        let targets = cache
            .target_order
            .iter()
            .filter(|cwd| {
                cache.targets[*cwd]
                    .bindings
                    .get(provider)
                    .and_then(|binding| binding.key.as_deref())
                    == Some(key)
            })
            .cloned()
            .collect::<Vec<_>>();
        self.publish(&mut cache, &targets);
    }

    pub(super) async fn diagnostic(&self, provider: &str) -> Result<String> {
        let state = self
            .inner
            .cache
            .lock()
            .unwrap()
            .providers
            .get(provider)
            .cloned();
        let Some(state) = state else {
            return Ok(format!(
                "{provider}\n  Error: Provider {provider} is not configured"
            ));
        };
        let label = state.entry.definition["label"].as_str().unwrap_or(provider);
        let timeout = self.timeout_ms().max(120_000);
        let base = async {
            match tokio::time::timeout(
                Duration::from_millis(timeout),
                state.entry.client.diagnostic(),
            )
            .await
            {
                Ok(Ok(diagnostic)) => diagnostic,
                Ok(Err(error)) => format!("{label}\n  Error: {error:#}"),
                Err(_) => format!(
                    "{label}\n  Error: Timed out collecting {label} diagnostic after {timeout}ms"
                ),
            }
        };
        let snapshot = async {
            let providers = [provider.to_owned()];
            let pending = self.begin_loads(GLOBAL, &providers, true);
            futures_util::future::join_all(pending.into_iter().map(completed)).await;
            let mut cache = self.inner.cache.lock().unwrap();
            cache
                .target(GLOBAL)
                .snapshot
                .records
                .iter()
                .find(|record| record.entry["provider"] == provider)
                .map(|record| record.entry.clone())
                .unwrap_or_else(|| state.initial.entry.clone())
        };
        let (base, entry) = tokio::join!(base, snapshot);
        let count = if entry["status"] == "ready" {
            entry["models"].as_array().map_or(0, Vec::len).to_string()
        } else {
            "—".to_owned()
        };
        let status = match entry["status"].as_str() {
            Some("ready") => "Ready".to_owned(),
            Some("error") => format!(
                "Error: {}",
                entry["error"].as_str().unwrap_or("Unknown error")
            ),
            Some("unavailable") => "Unavailable".to_owned(),
            _ => "Loading".to_owned(),
        };
        Ok(format!("{base}\n  Models: {count}\n  Status: {status}"))
    }

    pub(crate) async fn shutdown(&self) {
        self.inner.cancel.cancel();
        {
            let mut cache = self.inner.cache.lock().unwrap();
            cache.stopped = true;
            for state in cache.providers.values() {
                state.cancel.cancel();
            }
            for catalog in cache
                .catalogs
                .values()
                .flat_map(|catalogs| catalogs.values())
            {
                if let Some(load) = &catalog.load {
                    load.cancel.cancel();
                }
            }
        }
        let tasks = std::mem::take(&mut *self.inner.tasks.lock().unwrap());
        for task in tasks {
            let _ = task.await;
        }
        let mut cache = self.inner.cache.lock().unwrap();
        cache.catalogs.clear();
        cache.targets.clear();
        cache.target_order.clear();
    }
}

/// provider-snapshot-manager.ts getReadyProvider: disabled, failed and not-yet
/// available entries reject with the original messages, in that order.
pub(crate) fn require_ready(provider: &str, entry: Value) -> Result<Value> {
    if entry["enabled"] != true {
        return Err(anyhow!("Provider '{provider}' is disabled"));
    }
    match entry["status"].as_str() {
        Some("ready") => Ok(entry),
        Some("error") => Err(anyhow!(
            "{}",
            entry["error"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| format!("Failed to load provider '{provider}'"))
        )),
        _ => Err(anyhow!("Provider '{provider}' is not available")),
    }
}
