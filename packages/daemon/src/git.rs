pub mod command;
pub mod diff;
mod facts;
pub mod forge;
mod metadata;
#[cfg(test)]
mod mutation_tests;
mod mutations;
mod observation;
mod scheduler;

use crate::{state::AppState, transport::RequestContext};
use anyhow::{Result, anyhow, bail};
pub use command::resolve_diff_base;
pub(crate) use facts::current_branch;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::{
        Arc, Weak,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::sync::{Mutex, watch};
use tokio_util::sync::CancellationToken;

struct CachedRead {
    generation: AtomicU64,
    build: Mutex<()>,
    value: Mutex<Option<(u64, Value)>>,
    built_at: std::sync::Mutex<Option<std::time::Instant>>,
}
impl CachedRead {
    fn new() -> Self {
        Self {
            generation: AtomicU64::new(0),
            build: Mutex::new(()),
            value: Mutex::new(None),
            built_at: std::sync::Mutex::new(None),
        }
    }
    fn invalidate(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
    }
    fn fresh(&self) -> bool {
        self.built_at
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some_and(|time| time.elapsed() < std::time::Duration::from_secs(15))
    }
}
struct Checkout {
    snapshot: CachedRead,
    diffs: Mutex<HashMap<String, Arc<CachedRead>>>,
    changes: watch::Sender<u64>,
    mutation: Mutex<()>,
}
impl Checkout {
    fn new() -> Self {
        let (changes, _) = watch::channel(0);
        Self {
            snapshot: CachedRead::new(),
            diffs: Mutex::new(HashMap::new()),
            changes,
            mutation: Mutex::new(()),
        }
    }
}
pub struct GitService {
    app: Weak<AppState>,
    checkouts: Mutex<HashMap<PathBuf, Arc<Checkout>>>,
    aliases: Mutex<HashMap<PathBuf, PathBuf>>,
    pub forge: forge::ForgeEngine,
    observers: Mutex<HashMap<PathBuf, observation::Observation>>,
    forge_observation: Mutex<Option<observation::ForgeObservation>>,
    cancel: CancellationToken,
}
impl GitService {
    pub fn new(app: Weak<AppState>) -> Self {
        Self {
            app,
            checkouts: Mutex::new(HashMap::new()),
            aliases: Mutex::new(HashMap::new()),
            forge: forge::ForgeEngine::new(),
            observers: Mutex::new(HashMap::new()),
            forge_observation: Mutex::new(None),
            cancel: CancellationToken::new(),
        }
    }
    pub async fn initialize(&self) -> Result<()> {
        let app = self
            .app
            .upgrade()
            .ok_or_else(|| anyhow!("Daemon shutting down"))?;
        self.apply_config(&app.config.snapshot(), &Value::Null)
            .await
    }
    pub async fn apply_config(&self, next: &Value, _previous: &Value) -> Result<()> {
        let git = &next["git"];
        scheduler::configure(
            git["maxProcessesPerSecond"].as_u64().unwrap_or(64) as usize,
            git["maxProcessConcurrency"].as_u64().unwrap_or(8) as usize,
        )
    }
    pub async fn shutdown(&self) -> Result<()> {
        self.cancel.cancel();
        self.forge_observation.lock().await.take();
        self.observers.lock().await.clear();
        self.checkouts.lock().await.clear();
        self.aliases.lock().await.clear();
        Ok(())
    }
    fn environment(&self) -> Result<BTreeMap<String, String>> {
        Ok(self
            .app
            .upgrade()
            .ok_or_else(|| anyhow!("Daemon shutting down"))?
            .config
            .environment())
    }
    async fn key(&self, cwd: &Path) -> Result<PathBuf> {
        let canonical = tokio::fs::canonicalize(cwd).await?;
        let known = { self.aliases.lock().await.get(&canonical).cloned() };
        if let Some(root) = known {
            self.aliases
                .lock()
                .await
                .insert(cwd.to_owned(), root.clone());
            return Ok(root);
        }
        if self.checkouts.lock().await.contains_key(&canonical) {
            self.aliases
                .lock()
                .await
                .insert(cwd.to_owned(), canonical.clone());
            return Ok(canonical);
        }
        let env = self.environment()?;
        if let Some(root) = command::optional(
            &canonical,
            &["rev-parse", "--show-toplevel"],
            &env,
            self.cancel.child_token(),
        )
        .await?
        {
            let root = tokio::fs::canonicalize(root).await?;
            let mut aliases = self.aliases.lock().await;
            if aliases.len() >= 50_000 {
                aliases.clear();
            }
            aliases.insert(canonical, root.clone());
            aliases.insert(cwd.to_owned(), root.clone());
            return Ok(root);
        }
        Ok(canonical)
    }
    async fn checkout_entry(&self, cwd: &Path) -> Result<(PathBuf, Arc<Checkout>)> {
        let key = self.key(cwd).await?;
        let mut entries = self.checkouts.lock().await;
        let entry = entries
            .entry(key.clone())
            .or_insert_with(|| Arc::new(Checkout::new()))
            .clone();
        Ok((key, entry))
    }
    pub async fn cached_snapshot(&self, cwd: &Path) -> Option<Value> {
        let key = self
            .aliases
            .lock()
            .await
            .get(cwd)
            .cloned()
            .unwrap_or_else(|| cwd.to_owned());
        let entry = self.checkouts.lock().await.get(&key).cloned()?;
        let value = entry.snapshot.value.lock().await;
        value.as_ref().map(|(_, value)| value.clone())
    }
    pub async fn invalidate(&self, cwd: &Path) -> Result<()> {
        let (key, entry) = self.checkout_entry(cwd).await?;
        entry.snapshot.invalidate();
        for diff in entry.diffs.lock().await.values() {
            diff.invalidate();
        }
        entry.changes.send_modify(|g| *g = g.wrapping_add(1));
        self.forge.invalidate(&key).await;
        Ok(())
    }
    pub fn snapshot<'a>(
        &'a self,
        cwd: &'a Path,
        force: bool,
        cancel: CancellationToken,
    ) -> futures_util::future::BoxFuture<'a, Result<Value>> {
        Box::pin(async move {
            let (key, entry) = self.checkout_entry(cwd).await?;
            if force {
                entry.snapshot.invalidate();
            }
            let generation = entry.snapshot.generation.load(Ordering::Acquire);
            if let Some((g, value)) = entry.snapshot.value.lock().await.as_ref() {
                if *g == generation {
                    return Ok(value.clone());
                }
            }
            let _flight = tokio::select! {_=cancel.cancelled()=>bail!("Git snapshot cancelled"),guard=entry.snapshot.build.lock()=>guard};
            let generation = entry.snapshot.generation.load(Ordering::Acquire);
            if let Some((g, value)) = entry.snapshot.value.lock().await.as_ref() {
                if *g == generation {
                    return Ok(value.clone());
                }
            }
            let env = self.environment()?;
            let (home, config) = {
                let app = self
                    .app
                    .upgrade()
                    .ok_or_else(|| anyhow!("Daemon shutting down"))?;
                (app.home.clone(), app.config.snapshot())
            };
            let git = facts::snapshot(&key, &home, &config, &env, cancel.clone()).await?;
            let previous = entry
                .snapshot
                .value
                .lock()
                .await
                .as_ref()
                .map(|(_, v)| v["forge"].clone());
            let forge=previous.unwrap_or_else(||json!({"featuresEnabled":false,"pullRequest":null,"authState":"no_remote","error":null}));
            let mut value = json!({"cwd":key,"git":git,"forge":forge});
            if value["git"]["isGit"] == true {
                value["git"]["diffStat"] = match diff::shortstat(&key, &env, cancel.clone()).await {
                    Ok(stat) => stat.unwrap_or(Value::Null),
                    Err(error) => {
                        tracing::debug!(%error,"Checkout shortstat unavailable");
                        Value::Null
                    }
                };
                self.ensure_observation(&key, &env, cancel).await?;
            }
            let published = entry.snapshot.generation.load(Ordering::Acquire) == generation;
            if published {
                *entry.snapshot.value.lock().await = Some((generation, value.clone()));
            }
            drop(_flight);
            if published {
                if let Some(app) = self.app.upgrade() {
                    if let Err(error) = app.projects.observe_git_snapshot(&value).await {
                        tracing::warn!(%error,"Checkout snapshot lifecycle notification failed");
                    }
                }
            }
            Ok(value)
        })
    }
    pub async fn checkout(&self, cwd: &Path, cancel: CancellationToken) -> Result<Value> {
        let mut value = self.snapshot(cwd, false, cancel).await?["git"].clone();
        value["worktreeRoot"] = value["repoRoot"].clone();
        Ok(value)
    }
    pub async fn read_diff(
        &self,
        cwd: &Path,
        compare: &Value,
        cancel: CancellationToken,
    ) -> Result<Value> {
        let (key, checkout) = self.checkout_entry(cwd).await?;
        let cache_key = serde_json::to_string(
            &json!({"mode":compare["mode"],"baseRef":compare["baseRef"],"ignoreWhitespace":compare["ignoreWhitespace"].as_bool().unwrap_or(false),"detail":compare["detail"].as_str().unwrap_or("full")}),
        )?;
        let entry = {
            let mut diffs = checkout.diffs.lock().await;
            if diffs.len() >= 64 && !diffs.contains_key(&cache_key) {
                let expired = diffs
                    .iter()
                    .find(|(_, entry)| Arc::strong_count(entry) == 1 && !entry.fresh())
                    .or_else(|| {
                        diffs
                            .iter()
                            .find(|(_, entry)| Arc::strong_count(entry) == 1)
                    })
                    .map(|(key, _)| key.clone());
                if let Some(expired) = expired {
                    diffs.remove(&expired);
                }
            }
            diffs
                .entry(cache_key)
                .or_insert_with(|| Arc::new(CachedRead::new()))
                .clone()
        };
        let generation = entry.generation.load(Ordering::Acquire);
        if entry.fresh() {
            if let Some((g, value)) = entry.value.lock().await.as_ref() {
                if *g == generation {
                    return Ok(value.clone());
                }
            }
        }
        let _flight = tokio::select! {_=cancel.cancelled()=>bail!("Git diff cancelled"),guard=entry.build.lock()=>guard};
        let generation = entry.generation.load(Ordering::Acquire);
        if entry.fresh() {
            if let Some((g, value)) = entry.value.lock().await.as_ref() {
                if *g == generation {
                    return Ok(value.clone());
                }
            }
        }
        let env = self.environment()?;
        let value = diff::read(&key, compare, &env, cancel.clone()).await?;
        self.ensure_observation(&key, &env, cancel).await?;
        if entry.generation.load(Ordering::Acquire) == generation {
            *entry.value.lock().await = Some((generation, value.clone()));
            *entry.built_at.lock().unwrap_or_else(|e| e.into_inner()) =
                Some(std::time::Instant::now());
        }
        Ok(value)
    }
    pub async fn notify_mutation(&self, cwd: &Path) -> Result<()> {
        if let Err(error) = self.invalidate(cwd).await {
            tracing::warn!(%error,cwd=%cwd.display(),"Git mutation succeeded but invalidation failed");
            return Ok(());
        }
        self.request_forge_refresh(cwd).await;
        match self.snapshot(cwd, true, self.cancel.child_token()).await {
            Ok(snapshot) => {
                if let Err(error) = self.publish_checkout_snapshot(cwd, &snapshot).await {
                    tracing::warn!(%error,cwd=%cwd.display(),"Git mutation succeeded but publication failed");
                }
            }
            Err(error) => {
                tracing::warn!(%error,cwd=%cwd.display(),"Git mutation succeeded but refresh failed")
            }
        }
        Ok(())
    }
    pub async fn stage(
        &self,
        cwd: &Path,
        paths: &[String],
        cancel: CancellationToken,
    ) -> Result<()> {
        let (_, entry) = self.checkout_entry(cwd).await?;
        let _lock = entry.mutation.lock().await;
        mutations::stage(cwd, paths, &self.environment()?, cancel).await?;
        self.notify_mutation(cwd).await
    }
    pub async fn unstage(
        &self,
        cwd: &Path,
        paths: &[String],
        cancel: CancellationToken,
    ) -> Result<()> {
        let (_, entry) = self.checkout_entry(cwd).await?;
        let _lock = entry.mutation.lock().await;
        mutations::unstage(cwd, paths, &self.environment()?, cancel).await?;
        self.notify_mutation(cwd).await
    }
    pub async fn revert(&self, cwd: &Path, sha: &str, cancel: CancellationToken) -> Result<()> {
        let (_, entry) = self.checkout_entry(cwd).await?;
        let _lock = entry.mutation.lock().await;
        mutations::revert(cwd, sha, &self.environment()?, cancel).await?;
        self.notify_mutation(cwd).await
    }
    pub async fn rebase(&self, cwd: &Path, base: &str, cancel: CancellationToken) -> Result<()> {
        let (_, entry) = self.checkout_entry(cwd).await?;
        let _lock = entry.mutation.lock().await;
        mutations::rebase(cwd, base, &self.environment()?, cancel).await?;
        self.notify_mutation(cwd).await
    }
    pub async fn handle(&self, context: &RequestContext, message: &Value) -> Result<bool> {
        self.handle_message(context, message).await
    }
}
pub(super) fn status_projection(snapshot: &Value) -> Value {
    let mut status = snapshot["git"].clone();
    if let Some(fields) = status.as_object_mut() {
        fields.remove("diffStat");
        fields.remove("worktreeRoot");
    }
    if status["isGit"] != true {
        if let Some(fields) = status.as_object_mut() {
            fields.remove("mainRepoRoot");
        }
    }
    status
}
/// The source's `toCheckoutError`: only typed not-a-repository and merge-conflict
/// failures carry a specific code; every other failure, including raw git output
/// that mentions a missing repository, is `UNKNOWN`.
fn checkout_error(error: &anyhow::Error) -> Value {
    let code = if error.downcast_ref::<mutations::MergeConflict>().is_some() {
        "MERGE_CONFLICT"
    } else if error.downcast_ref::<command::NotGitRepo>().is_some() {
        "NOT_GIT_REPO"
    } else {
        "UNKNOWN"
    };
    json!({"code":code,"message":error.to_string()})
}
mod session;
