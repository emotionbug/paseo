use super::{GitService, command};
use crate::state::AppState;
use anyhow::{Result, anyhow, bail};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc, LazyLock, Mutex as StdMutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{Notify, Semaphore, mpsc},
    time::Instant,
};
use tokio_util::sync::CancellationToken;

const DEBOUNCE: Duration = Duration::from_secs(1);
const MIN_GAP: Duration = Duration::from_secs(2);
const FETCH_INTERVAL: Duration = Duration::from_secs(180);
const REENSURE: Duration = Duration::from_secs(60);
const POLL_BASE: Duration = Duration::from_secs(5);
const POLL_MAX: Duration = Duration::from_secs(60);
const RECOVERY_BASE: Duration = Duration::from_secs(30);
const RECOVERY_MAX: Duration = Duration::from_secs(300);

fn setup_limit() -> &'static Semaphore {
    static LIMIT: LazyLock<Semaphore> = LazyLock::new(|| Semaphore::new(2));
    &LIMIT
}
fn refresh_limit() -> &'static Semaphore {
    static LIMIT: LazyLock<Semaphore> = LazyLock::new(|| Semaphore::new(4));
    &LIMIT
}

#[derive(Clone)]
struct Demand {
    count: usize,
    root: PathBuf,
    git_dir: PathBuf,
    aliases: HashMap<PathBuf, usize>,
    is_git: bool,
    lifetime: CancellationToken,
}
type Demands = Arc<StdMutex<HashMap<PathBuf, Demand>>>;

/// One actor per common Git directory. Dropping its final repository ownership
/// cancels active work and closes every watcher, timer and background fetch.
pub(super) struct Observation {
    demands: Demands,
    events: mpsc::Sender<Message>,
    cancel: CancellationToken,
}
impl Drop for Observation {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

enum Message {
    DemandChanged,
    Files {
        root: PathBuf,
        event: notify::Result<Event>,
    },
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Worktree,
    Common,
    Private,
}
struct Watch {
    kind: Kind,
    watcher: Option<RecommendedWatcher>,
    failed: Arc<AtomicBool>,
    retry_at: Instant,
    attempts: u32,
    established: Instant,
    poll_at: Instant,
    poll_interval: Duration,
}
#[derive(Default)]
struct Pending {
    paths: HashSet<PathBuf>,
    metadata: bool,
}
struct CheckoutClock {
    due: Option<Instant>,
    last: Option<Instant>,
    pending: Pending,
}
impl Default for CheckoutClock {
    fn default() -> Self {
        Self {
            due: None,
            last: None,
            pending: Pending::default(),
        }
    }
}

impl GitService {
    /// Retain exactly once per workspace registration or diff subscription.
    /// Ordinary snapshot reads do not implicitly create permanent demand.
    pub async fn retain_observation(&self, cwd: &Path) -> Result<()> {
        let _permit = tokio::select! { _ = self.cancel.cancelled() => bail!("Git service stopped"), permit = setup_limit().acquire() => permit? };
        let key = self.key(cwd).await?;
        let env = self.environment()?;
        let (common, root, dir, is_git) =
            observation_roots(&key, &env, self.cancel.child_token()).await?;
        let mut observers = self.observers.lock().await;
        if self.cancel.is_cancelled() {
            bail!("Git service stopped");
        }
        self.install_observation(
            &mut observers,
            common,
            key.clone(),
            Demand {
                count: 1,
                root,
                git_dir: dir,
                aliases: HashMap::from([(cwd.to_owned(), 1)]),
                is_git,
                lifetime: self.cancel.child_token(),
            },
        )?;
        if is_git {
            self.retain_forge_observation(&key, 1).await;
        }
        drop(observers);
        Ok(())
    }

    pub async fn release_observation(&self, cwd: &Path) -> Result<()> {
        // Remembered aliases allow release after a symlink or checkout is removed.
        let key = tokio::fs::canonicalize(cwd)
            .await
            .unwrap_or_else(|_| cwd.to_owned());
        let mut observers = self.observers.lock().await;
        let mut empty = Vec::new();
        let mut released = None;
        for (common, observer) in observers.iter() {
            let mut demands = observer
                .demands
                .lock()
                .map_err(|_| anyhow!("Git observation state poisoned"))?;
            let key = demands
                .iter()
                .find_map(|(stored, demand)| {
                    (stored == &key || demand.aliases.contains_key(cwd)).then(|| stored.clone())
                })
                .unwrap_or_else(|| key.clone());
            if let Some(demand) = demands.get_mut(&key) {
                demand.count -= 1;
                if let Some(count) = demand.aliases.get_mut(cwd) {
                    *count -= 1;
                    if *count == 0 {
                        demand.aliases.remove(cwd);
                    }
                }
                if demand.is_git {
                    released = Some(key.clone());
                }
                if demand.count == 0 {
                    if let Some(demand) = demands.remove(&key) {
                        demand.lifetime.cancel();
                    }
                }
                if demands.is_empty() {
                    empty.push(common.clone());
                } else {
                    let _ = observer.events.try_send(Message::DemandChanged);
                }
                break;
            }
        }
        for common in empty {
            observers.remove(&common);
        }
        if let Some(key) = released {
            self.release_forge_observation(&key).await;
        }
        drop(observers);
        Ok(())
    }

    /// Snapshot refreshes may request watcher reconciliation, but never retain
    /// another reference. In particular refresh -> snapshot cannot leak demand.
    pub(super) async fn ensure_observation(
        &self,
        cwd: &Path,
        env: &BTreeMap<String, String>,
        cancel: CancellationToken,
    ) -> Result<()> {
        let previous = {
            let observers = self.observers.lock().await;
            let mut previous = None;
            for (root, observer) in observers.iter() {
                let demands = observer
                    .demands
                    .lock()
                    .map_err(|_| anyhow!("Git observation state poisoned"))?;
                if let Some(demand) = demands.get(cwd) {
                    if !demand.is_git {
                        previous = Some(root.clone());
                    }
                    let _ = observer.events.try_send(Message::DemandChanged);
                    break;
                }
            }
            previous
        };
        // A directory demand survives `git init`: move the same references to
        // the newly discovered common-dir actor without adding new consumers.
        if let Some(previous) = previous {
            let (common, root, git_dir, is_git) = observation_roots(cwd, env, cancel).await?;
            if is_git {
                let mut observers = self.observers.lock().await;
                let (demand, empty) = if let Some(observer) = observers.get(&previous) {
                    let mut demands = observer
                        .demands
                        .lock()
                        .map_err(|_| anyhow!("Git observation state poisoned"))?;
                    (demands.remove(cwd), demands.is_empty())
                } else {
                    (None, false)
                };
                if empty {
                    observers.remove(&previous);
                }
                if let Some(mut demand) = demand {
                    let consumers = demand.count;
                    demand.root = root;
                    demand.git_dir = git_dir;
                    demand.is_git = true;
                    self.install_observation(&mut observers, common, cwd.to_owned(), demand)?;
                    self.retain_forge_observation(cwd, consumers).await;
                }
            }
        }
        Ok(())
    }

    fn install_observation(
        &self,
        observers: &mut HashMap<PathBuf, Observation>,
        common: PathBuf,
        key: PathBuf,
        incoming: Demand,
    ) -> Result<()> {
        if let Some(observer) = observers.get(&common) {
            let mut demands = observer
                .demands
                .lock()
                .map_err(|_| anyhow!("Git observation state poisoned"))?;
            if let Some(demand) = demands.get_mut(&key) {
                demand.count = demand
                    .count
                    .checked_add(incoming.count)
                    .ok_or_else(|| anyhow!("Too many Git observation consumers"))?;
                for (alias, count) in incoming.aliases {
                    *demand.aliases.entry(alias).or_default() += count;
                }
            } else {
                demands.insert(key, incoming);
            }
            let _ = observer.events.try_send(Message::DemandChanged);
            return Ok(());
        }
        let demands = Arc::new(StdMutex::new(HashMap::from([(key, incoming)])));
        let (events, receiver) = mpsc::channel(256);
        let cancel = self.cancel.child_token();
        observers.insert(
            common.clone(),
            Observation {
                demands: demands.clone(),
                events: events.clone(),
                cancel: cancel.clone(),
            },
        );
        let actor = Actor {
            app: self.app.clone(),
            common,
            demands,
            events,
            receiver,
            cancel,
            overflow: Arc::new(AtomicBool::new(false)),
            watches: BTreeMap::new(),
            clocks: HashMap::new(),
            known_files: HashMap::new(),
            next_fetch: Instant::now(),
            next_reensure: Instant::now(),
            fetch_task: None,
        };
        tokio::spawn(actor.run());
        Ok(())
    }
}

async fn observation_roots(
    cwd: &Path,
    env: &BTreeMap<String, String>,
    cancel: CancellationToken,
) -> Result<(PathBuf, PathBuf, PathBuf, bool)> {
    let root = command::run(
        cwd,
        &["rev-parse".into(), "--show-toplevel".into()],
        env,
        cancel.clone(),
        command::READ_LIMIT,
        command::READ_TIMEOUT,
    )
    .await?;
    if root.status != 0 {
        if root.stderr.to_lowercase().contains("not a git repository") {
            let cwd = tokio::fs::canonicalize(cwd).await?;
            return Ok((cwd.clone(), cwd.clone(), cwd, false));
        }
        bail!("{}", root.stderr);
    }
    let root = PathBuf::from(String::from_utf8(root.stdout)?.trim_end_matches(['\r', '\n']));
    let common = PathBuf::from(
        command::text(
            cwd,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            env,
            cancel.clone(),
        )
        .await?,
    );
    let dir = command::git_dir(cwd, env, cancel).await?;
    Ok((
        tokio::fs::canonicalize(common).await?,
        tokio::fs::canonicalize(root).await?,
        tokio::fs::canonicalize(dir).await?,
        true,
    ))
}

struct Actor {
    app: Weak<AppState>,
    common: PathBuf,
    demands: Demands,
    events: mpsc::Sender<Message>,
    receiver: mpsc::Receiver<Message>,
    cancel: CancellationToken,
    overflow: Arc<AtomicBool>,
    watches: BTreeMap<PathBuf, Watch>,
    clocks: HashMap<PathBuf, CheckoutClock>,
    known_files: HashMap<PathBuf, HashSet<PathBuf>>,
    next_fetch: Instant,
    next_reensure: Instant,
    fetch_task: Option<tokio::task::JoinHandle<bool>>,
}
impl Drop for Actor {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(task) = self.fetch_task.take() {
            task.abort();
        }
    }
}
impl Actor {
    fn demands(&self) -> HashMap<PathBuf, Demand> {
        self.demands
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    async fn run(mut self) {
        self.reconcile().await;
        while !self.cancel.is_cancelled() {
            if self.demands().is_empty() {
                break;
            }
            let now = Instant::now();
            if self.next_reensure <= now {
                self.reconcile().await;
                self.next_reensure = now + REENSURE;
            }
            if self.overflow.swap(false, Ordering::AcqRel) {
                self.queue_all(true);
            }
            let failed: Vec<_> = self
                .watches
                .iter()
                .filter_map(|(root, watch)| {
                    watch.failed.load(Ordering::Acquire).then_some(root.clone())
                })
                .collect();
            for root in failed {
                self.mark_failed(&root);
            }
            let poll: Vec<_> = self
                .watches
                .iter()
                .filter_map(|(root, watch)| {
                    (watch.watcher.is_none() && watch.poll_at <= now).then_some(root.clone())
                })
                .collect();
            for root in &poll {
                self.queue_root(root, true, &[]);
            }
            self.refresh_ready().await;
            // Polling backs off only after an unchanged successful snapshot.
            // refresh_ready updates the interval; each failed root shares one
            // deadline rather than causing one poll per alias or subscriber.
            for root in poll {
                if let Some(watch) = self.watches.get_mut(&root) {
                    watch.poll_at = Instant::now() + watch.poll_interval;
                }
            }
            if self.next_fetch <= Instant::now() && self.fetch_task.is_none() {
                self.start_fetch();
                self.next_fetch = Instant::now() + FETCH_INTERVAL;
            }
            let retry = self
                .watches
                .values()
                .filter(|w| w.watcher.is_none())
                .map(|w| w.retry_at)
                .min();
            if retry.is_some_and(|deadline| deadline <= Instant::now()) {
                self.reconcile().await;
            }
            let mut deadline = if self.fetch_task.is_some() {
                self.next_reensure
            } else {
                self.next_fetch.min(self.next_reensure)
            };
            for clock in self.clocks.values() {
                if let Some(due) = clock.due {
                    deadline = deadline.min(due);
                }
            }
            for watch in self.watches.values().filter(|w| w.watcher.is_none()) {
                deadline = deadline.min(watch.retry_at).min(watch.poll_at);
            }
            tokio::select! {
                _ = self.cancel.cancelled() => break,
                _ = tokio::time::sleep_until(deadline) => {},
                result = async { match self.fetch_task.as_mut() { Some(task)=>task.await, None=>std::future::pending().await } }, if self.fetch_task.is_some() => {
                    self.fetch_task=None;
                    if !matches!(result,Ok(false)){self.queue_all(true);}
                },
                message = self.receiver.recv() => match message {
                    Some(Message::DemandChanged) => self.reconcile().await,
                    Some(Message::Files{root,event}) => match event {
                        Ok(event) if !matches!(event.kind,EventKind::Access(_)) => {
                            let removed=matches!(event.kind,EventKind::Remove(_)) && event.paths.contains(&root);
                            if removed{self.mark_failed(&root);}
                            self.queue_root(&root,removed,&event.paths);
                        },
                        Ok(_) => {},
                        Err(error) => { tracing::debug!(%error,path=%root.display(),"Git watcher degraded to polling"); self.mark_failed(&root); }
                    },
                    None => break,
                },
            }
        }
        self.watches.clear();
    }

    async fn reconcile(&mut self) {
        let demands = self.demands();
        let mut desired = BTreeMap::new();
        if demands.values().any(|demand| demand.is_git) {
            desired.insert(self.common.clone(), Kind::Common);
        }
        for demand in demands.values() {
            desired.insert(demand.root.clone(), Kind::Worktree);
            if demand.is_git && demand.git_dir != self.common {
                desired.insert(demand.git_dir.clone(), Kind::Private);
            }
        }
        self.clocks.retain(|cwd, _| demands.contains_key(cwd));
        self.known_files
            .retain(|root, _| desired.contains_key(root));
        self.watches.retain(|root, _| desired.contains_key(root));
        for (root, kind) in desired {
            let now = Instant::now();
            let existing = self.watches.get(&root);
            if existing.is_some_and(|w| w.watcher.is_some() || w.retry_at > now) {
                continue;
            }
            let attempts = existing.map(|w| w.attempts).unwrap_or(0);
            let poll_interval = existing.map(|w| w.poll_interval).unwrap_or(POLL_BASE);
            let events = self.events.clone();
            let watched = root.clone();
            let overflow = self.overflow.clone();
            let failed = Arc::new(AtomicBool::new(false));
            let failure = failed.clone();
            let permit = tokio::select! {_=self.cancel.cancelled()=>return,permit=setup_limit().acquire()=>match permit{Ok(p)=>p,Err(_)=>return}};
            let open_root = root.clone();
            let open =
                tokio::task::spawn_blocking(move || -> notify::Result<RecommendedWatcher> {
                    use std::io::Write as _;
                    let canary =
                        open_root.join(format!(".paseo-watcher-canary-{}", uuid::Uuid::new_v4()));
                    let watched_canary = canary.clone();
                    let (observed, receipt) = std::sync::mpsc::sync_channel(1);
                    let mut watcher =
                        notify::recommended_watcher(move |mut event: notify::Result<Event>| {
                            if matches!(&event,Ok(e) if matches!(e.kind,EventKind::Access(_))) {
                                return;
                            }
                            if event.is_err() {
                                failure.store(true, Ordering::Release);
                            }
                            if let Ok(event) = &mut event {
                                event.paths.retain(|path| {
                                    if path == &watched_canary {
                                        let _ = observed.try_send(());
                                        false
                                    } else {
                                        true
                                    }
                                });
                                if event.paths.is_empty() {
                                    return;
                                }
                            }
                            if events
                                .try_send(Message::Files {
                                    root: watched.clone(),
                                    event,
                                })
                                .is_err()
                            {
                                overflow.store(true, Ordering::Release);
                            }
                        })?;
                    watcher.watch(&open_root, RecursiveMode::Recursive)?;
                    let mut created = false;
                    let verification = (|| -> std::io::Result<()> {
                        let mut file = std::fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(&canary)?;
                        created = true;
                        file.write_all(b"paseo watcher liveness canary\n")?;
                        receipt
                            .recv_timeout(Duration::from_secs(10))
                            .map_err(|error| {
                                std::io::Error::new(std::io::ErrorKind::TimedOut, error)
                            })?;
                        Ok(())
                    })();
                    let cleanup = if created {
                        std::fs::remove_file(&canary)
                    } else {
                        Ok(())
                    };
                    verification.map_err(notify::Error::io)?;
                    cleanup.map_err(notify::Error::io)?;
                    Ok(watcher)
                });
            // The blocking watcher task returns only its owned watcher. On
            // deadline/cancellation its eventual result is dropped, not leaked.
            let opened = tokio::select! {_=self.cancel.cancelled()=>return,result=tokio::time::timeout(Duration::from_secs(10),open)=>result};
            drop(permit);
            let watcher = match opened {
                Ok(Ok(Ok(watcher))) => Some(watcher),
                _ => None,
            };
            let attempts = if watcher.is_none() {
                attempts.saturating_add(1)
            } else {
                attempts
            };
            let retry = RECOVERY_BASE
                .saturating_mul(1u32 << attempts.saturating_sub(1).min(4))
                .min(RECOVERY_MAX);
            self.watches.insert(
                root,
                Watch {
                    kind,
                    watcher,
                    failed,
                    retry_at: now + retry,
                    attempts,
                    established: now,
                    poll_at: now + POLL_BASE,
                    poll_interval,
                },
            );
        }
    }

    fn mark_failed(&mut self, root: &Path) {
        if let Some(watch) = self.watches.get_mut(root) {
            watch.failed.store(false, Ordering::Release);
            if watch.watcher.take().is_some() {
                if watch.established.elapsed() >= RECOVERY_MAX {
                    watch.attempts = 0;
                }
                watch.attempts = watch.attempts.saturating_add(1);
                watch.retry_at = Instant::now()
                    + RECOVERY_BASE
                        .saturating_mul(1u32 << watch.attempts.saturating_sub(1).min(4))
                        .min(RECOVERY_MAX);
                watch.poll_interval = POLL_BASE;
                watch.poll_at = Instant::now() + POLL_BASE;
            }
        }
    }
    fn queue_all(&mut self, metadata: bool) {
        for cwd in self.demands().keys() {
            self.queue(cwd, metadata, &[]);
        }
    }
    fn queue(&mut self, cwd: &Path, metadata: bool, paths: &[PathBuf]) {
        let clock = self.clocks.entry(cwd.to_owned()).or_default();
        clock.pending.metadata |= metadata;
        if clock.pending.paths.len() + paths.len() > 50_000 {
            clock.pending.metadata = true;
            clock.pending.paths.clear();
        } else {
            clock.pending.paths.extend(paths.iter().cloned());
        }
        let now = Instant::now();
        clock.due =
            Some((now + DEBOUNCE).max(clock.last.map(|last| last + MIN_GAP).unwrap_or(now)));
    }
    fn queue_root(&mut self, root: &Path, poll: bool, paths: &[PathBuf]) {
        let Some(kind) = self.watches.get(root).map(|w| w.kind) else {
            return;
        };
        let demands = self.demands();
        for (cwd, demand) in demands {
            match kind {
                Kind::Worktree if demand.root == root => {
                    let filtered: Vec<_> = paths
                        .iter()
                        .filter(|p| !demand.is_git || !p.starts_with(demand.root.join(".git")))
                        .cloned()
                        .collect();
                    if poll || !filtered.is_empty() {
                        self.queue(&cwd, poll || !demand.is_git, &filtered);
                    }
                }
                Kind::Private if demand.git_dir == root => {
                    if poll
                        || paths.iter().any(|path| {
                            metadata_effect(path.strip_prefix(root).unwrap_or(path), false)
                                != Effect::Ignore
                        })
                    {
                        self.queue(&cwd, true, &[]);
                    }
                }
                Kind::Common => {
                    let relevant = poll
                        || paths.iter().any(|path| {
                            match metadata_effect(path.strip_prefix(root).unwrap_or(path), true) {
                                Effect::Ignore => false,
                                Effect::Owner => demand.git_dir == self.common,
                                Effect::All => true,
                            }
                        });
                    if relevant {
                        self.queue(&cwd, true, &[]);
                    }
                }
                _ => {}
            }
        }
    }

    async fn refresh_ready(&mut self) {
        let now = Instant::now();
        let ready: Vec<_> = self
            .clocks
            .iter()
            .filter_map(|(cwd, clock)| {
                clock
                    .due
                    .is_some_and(|due| due <= now)
                    .then_some(cwd.clone())
            })
            .collect();
        let demands = self.demands();
        let mut poll_changes: HashMap<PathBuf, bool> = HashMap::new();
        for cwd in ready {
            if self.cancel.is_cancelled() {
                return;
            }
            let Some(demand) = demands.get(&cwd) else {
                continue;
            };
            let pending = {
                let clock = self.clocks.get_mut(&cwd).unwrap();
                clock.due = None;
                clock.last = Some(Instant::now());
                std::mem::take(&mut clock.pending)
            };
            let Some(app) = self.app.upgrade() else {
                return;
            };
            let env = match app.git.environment() {
                Ok(env) => env,
                Err(_) => return,
            };
            if !pending.metadata {
                match self
                    .relevant_files(&demand.root, &pending.paths, &env, &demand.lifetime)
                    .await
                {
                    Ok(false) => continue,
                    Ok(true) => {}
                    Err(error) => {
                        tracing::debug!(%error,"Git ignore inventory unavailable; refreshing observed checkout")
                    }
                }
            }
            let _permit = tokio::select! {_=self.cancel.cancelled()=>return,_=demand.lifetime.cancelled()=>continue,permit=refresh_limit().acquire()=>match permit{Ok(p)=>p,Err(_)=>return}};
            let before = match app.git.checkout_entry(&cwd).await {
                Ok((_, entry)) => entry
                    .snapshot
                    .value
                    .lock()
                    .await
                    .as_ref()
                    .map(|(_, value)| value.clone()),
                Err(error) => {
                    tracing::debug!(%error,"Observed checkout unavailable");
                    continue;
                }
            };
            if let Err(error) = app.git.invalidate(&cwd).await {
                tracing::debug!(%error,"Checkout invalidation failed");
                continue;
            }
            let snapshot = app
                .git
                .snapshot(&cwd, false, demand.lifetime.child_token())
                .await;
            if demand.lifetime.is_cancelled() {
                continue;
            }
            match snapshot {
                Ok(snapshot) => {
                    let changed = before.as_ref() != Some(&snapshot);
                    for (root, watch) in &self.watches {
                        if watch.watcher.is_none()
                            && (root == &self.common
                                || root == &demand.root
                                || root == &demand.git_dir)
                        {
                            *poll_changes.entry(root.clone()).or_default() |= changed;
                        }
                    }
                    if changed {
                        if let Err(error) = app.git.publish_checkout_snapshot(&cwd, &snapshot).await
                        {
                            tracing::debug!(%error,"Checkout update publication failed");
                        }
                    }
                }
                Err(error) => tracing::debug!(%error,"Checkout observation refresh failed"),
            }
            if pending.metadata {
                app.git.request_forge_refresh(&cwd).await;
            }
        }
        for (root, changed) in poll_changes {
            if let Some(watch) = self.watches.get_mut(&root) {
                watch.poll_interval = if changed {
                    POLL_BASE
                } else {
                    (watch.poll_interval * 2).min(POLL_MAX)
                };
            }
        }
    }

    async fn relevant_files(
        &mut self,
        root: &Path,
        paths: &HashSet<PathBuf>,
        env: &BTreeMap<String, String>,
        cancel: &CancellationToken,
    ) -> Result<bool> {
        let bytes = command::checked(
            root,
            &[
                "ls-files".into(),
                "-z".into(),
                "--cached".into(),
                "--others".into(),
                "--exclude-standard".into(),
            ],
            env,
            cancel.child_token(),
            command::READ_LIMIT,
            command::READ_TIMEOUT,
        )
        .await?;
        let current: HashSet<PathBuf> = bytes
            .split(|b| *b == 0)
            .filter(|p| !p.is_empty())
            .map(|p| root.join(String::from_utf8_lossy(p).as_ref()))
            .collect();
        let matches = |files: &HashSet<PathBuf>| {
            paths
                .iter()
                .any(|path| files.contains(path) || files.iter().any(|file| file.starts_with(path)))
        };
        let relevant = matches(&current)
            || self.known_files.get(root).is_none_or(matches)
            || paths
                .iter()
                .any(|p| p.file_name().is_some_and(|name| name == ".gitignore"));
        self.known_files.insert(root.to_owned(), current);
        Ok(relevant)
    }

    fn start_fetch(&mut self) {
        let Some((cwd, _)) = self.demands().into_iter().find(|(_, demand)| demand.is_git) else {
            return;
        };
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let Ok(env) = app.git.environment() else {
            return;
        };
        let cancel = self.cancel.child_token();
        self.fetch_task = Some(tokio::spawn(async move {
            let origin = command::optional(
                &cwd,
                &["config", "--get", "remote.origin.url"],
                &env,
                cancel.clone(),
            )
            .await;
            if !matches!(origin, Ok(Some(_))) {
                return false;
            }
            let refs = ["for-each-ref", "--format=%(refname)%00%(objectname)"];
            let before = command::text(&cwd, &refs, &env, cancel.clone()).await;
            let result = command::checked(
                &cwd,
                &["fetch".into(), "origin".into(), "--prune".into()],
                &env,
                cancel.clone(),
                command::READ_LIMIT,
                command::MUTATION_TIMEOUT,
            )
            .await;
            if let Err(error) = result {
                tracing::debug!(%error,"Background Git fetch failed");
            }
            let after = command::text(&cwd, &refs, &env, cancel).await;
            !matches!((&before,&after),(Ok(before),Ok(after)) if before==after)
        }));
    }
}

#[derive(PartialEq, Eq)]
enum Effect {
    Ignore,
    Owner,
    All,
}
fn metadata_effect(path: &Path, common: bool) -> Effect {
    let path = path.to_string_lossy().replace('\\', "/");
    if path.ends_with(".lock") {
        return Effect::Ignore;
    }
    if matches!(
        path.as_str(),
        "index" | "HEAD" | "config.worktree" | "paseo/worktree.json"
    ) {
        return Effect::Owner;
    }
    if common
        && (matches!(
            path.as_str(),
            "packed-refs" | "config" | "refs/heads" | "refs/remotes" | "reftable"
        ) || path.starts_with("refs/heads/")
            || path.starts_with("refs/remotes/")
            || path.starts_with("reftable/"))
    {
        return Effect::All;
    }
    Effect::Ignore
}

struct ForgeDemand {
    count: usize,
    next: Instant,
    failures: u32,
    status: Option<Value>,
}
pub(super) struct ForgeObservation {
    demands: Arc<StdMutex<HashMap<PathBuf, ForgeDemand>>>,
    wake: Arc<Notify>,
    cancel: CancellationToken,
}
impl Drop for ForgeObservation {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl GitService {
    async fn retain_forge_observation(&self, cwd: &Path, consumers: usize) {
        let mut slot = self.forge_observation.lock().await;
        if slot.is_none() {
            let demands = Arc::new(StdMutex::new(HashMap::new()));
            let wake = Arc::new(Notify::new());
            let cancel = self.cancel.child_token();
            *slot = Some(ForgeObservation {
                demands: demands.clone(),
                wake: wake.clone(),
                cancel: cancel.clone(),
            });
            tokio::spawn(forge_loop(self.app.clone(), demands, wake, cancel));
        }
        let observation = slot.as_ref().unwrap();
        let mut demands = observation
            .demands
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let demand = demands.entry(cwd.to_owned()).or_insert(ForgeDemand {
            count: 0,
            next: Instant::now(),
            failures: 0,
            status: None,
        });
        demand.count += consumers;
        observation.wake.notify_one();
    }
    async fn release_forge_observation(&self, cwd: &Path) {
        let mut slot = self.forge_observation.lock().await;
        let empty = if let Some(observation) = slot.as_ref() {
            let mut demands = observation
                .demands
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(demand) = demands.get_mut(cwd) {
                demand.count -= 1;
                if demand.count == 0 {
                    demands.remove(cwd);
                }
            }
            observation.wake.notify_one();
            demands.is_empty()
        } else {
            false
        };
        if empty {
            slot.take();
        }
    }
    pub(super) async fn request_forge_refresh(&self, cwd: &Path) {
        if let Some(observation) = self.forge_observation.lock().await.as_ref() {
            if let Some(demand) = observation
                .demands
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get_mut(cwd)
            {
                demand.next = Instant::now();
            }
            observation.wake.notify_one();
        }
    }
    pub(super) async fn apply_forge_payload(&self, cwd: &Path, payload: &Value) -> Result<()> {
        self.apply_forge_payload_at(cwd, payload, None).await
    }
    async fn apply_forge_payload_at(
        &self,
        cwd: &Path,
        payload: &Value,
        expected: Option<u64>,
    ) -> Result<()> {
        let (_, entry) = self.checkout_entry(cwd).await?;
        if entry.snapshot.value.lock().await.is_none() {
            self.snapshot(cwd, false, self.cancel.child_token()).await?;
        }
        let flight = entry.snapshot.build.lock().await;
        if expected
            .is_some_and(|expected| entry.snapshot.generation.load(Ordering::Acquire) != expected)
        {
            drop(flight);
            self.request_forge_refresh(cwd).await;
            return Ok(());
        }
        let mut forge = json!({"featuresEnabled":payload["githubFeaturesEnabled"],"pullRequest":payload["status"],"authState":payload["authState"],"error":payload["error"]});
        if let Some(brand) = payload.get("forge") {
            forge["forge"] = brand.clone();
        }
        let snapshot = {
            let mut cached = entry.snapshot.value.lock().await;
            let Some((_, snapshot)) = cached.as_mut() else {
                return Ok(());
            };
            if snapshot["forge"] == forge {
                return Ok(());
            }
            snapshot["forge"] = forge;
            snapshot.clone()
        };
        drop(flight);
        self.publish_checkout_snapshot(cwd, &snapshot).await
    }
    pub(super) async fn publish_checkout_snapshot(
        &self,
        cwd: &Path,
        snapshot: &Value,
    ) -> Result<()> {
        let root = snapshot["git"]["repoRoot"]
            .as_str()
            .map(Path::new)
            .unwrap_or(cwd);
        let aliases = {
            let observers = self.observers.lock().await;
            let mut aliases = HashSet::new();
            for observer in observers.values() {
                for (key, demand) in observer
                    .demands
                    .lock()
                    .map_err(|_| anyhow!("Git observation state poisoned"))?
                    .iter()
                {
                    if key == cwd || demand.root == root {
                        aliases.extend(demand.aliases.keys().cloned());
                    }
                }
            }
            if aliases.is_empty() {
                aliases.insert(cwd.to_owned());
            }
            aliases
        };
        let request_id = format!("subscription:{}", cwd.display());
        let mut payload = super::status_projection(snapshot);
        payload["cwd"] = json!(cwd);
        payload["requestId"] = json!(request_id);
        let forge = &snapshot["forge"];
        let mut pr = json!({"cwd":cwd,"requestId":request_id,"status":forge["pullRequest"],"githubFeaturesEnabled":forge["featuresEnabled"],"authState":forge["authState"],"error":null});
        if let Some(brand) = forge.get("forge") {
            pr["forge"] = brand.clone();
        }
        if !forge["error"].is_null() {
            pr["error"] = json!({"code":"UNKNOWN","message":forge["error"]["message"]});
        }
        payload["prStatus"] = pr;
        let app = self
            .app
            .upgrade()
            .ok_or_else(|| anyhow!("Daemon shutting down"))?;
        for alias in aliases {
            let mut payload = payload.clone();
            let request_id = format!("subscription:{}", alias.display());
            payload["cwd"] = json!(alias);
            payload["requestId"] = json!(request_id);
            payload["prStatus"]["cwd"] = json!(alias);
            payload["prStatus"]["requestId"] = json!(request_id);
            app.transport
                .publish_event(json!({"type":"checkout_status_update","payload":payload}))
                .await?;
            let mut owned_snapshot = snapshot.clone();
            owned_snapshot["cwd"] = json!(alias);
            app.projects.observe_git_snapshot(&owned_snapshot).await?;
        }
        app.projects.reconcile_checkout(cwd).await?;
        Ok(())
    }
}

fn forge_interval(status: Option<&Value>, failures: u32) -> Duration {
    let pending = status.is_some_and(|status| {
        status["checksStatus"] == "pending"
            || status["checks"]
                .as_array()
                .is_some_and(|checks| checks.iter().any(|check| check["status"] == "pending"))
    });
    let base = Duration::from_secs(if pending { 20 } else { 120 });
    if failures <= 1 {
        base
    } else {
        base.saturating_mul(1u32 << failures.saturating_sub(1).min(4))
            .min(Duration::from_secs(300))
    }
}

async fn forge_loop(
    app: Weak<AppState>,
    demands: Arc<StdMutex<HashMap<PathBuf, ForgeDemand>>>,
    wake: Arc<Notify>,
    cancel: CancellationToken,
) {
    let epoch = Instant::now();
    loop {
        let next = demands
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .map(|d| d.next)
            .min();
        let Some(next) = next else {
            tokio::select! {_=cancel.cancelled()=>return,_=wake.notified()=>continue}
        };
        // All repositories use one five-second alignment and one engine batch.
        let ticks = next
            .saturating_duration_since(epoch)
            .as_millis()
            .div_ceil(5000) as u64;
        let deadline = epoch + Duration::from_secs(ticks.saturating_mul(5));
        tokio::select! {_=cancel.cancelled()=>return,_=wake.notified()=>continue,_=tokio::time::sleep_until(deadline)=>{}}
        let due: Vec<_> = demands
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter_map(|(cwd, d)| (d.next <= Instant::now()).then(|| cwd.clone()))
            .collect();
        let Some(app) = app.upgrade() else { return };
        let Ok(env) = app.git.environment() else {
            return;
        };
        let mut generations = HashMap::new();
        for cwd in &due {
            if let Ok((_, entry)) = app.git.checkout_entry(cwd).await {
                generations.insert(
                    cwd.clone(),
                    entry.snapshot.generation.load(Ordering::Acquire),
                );
            }
        }
        let results = app
            .git
            .forge
            .poll_statuses(&due, &env, cancel.child_token())
            .await;
        if cancel.is_cancelled() {
            return;
        }
        for (cwd, payload) in results {
            let apply = {
                let mut demands = demands.lock().unwrap_or_else(|e| e.into_inner());
                let Some(demand) = demands.get_mut(&cwd) else {
                    continue;
                };
                let success = payload["error"].is_null();
                let apply = success || demand.status.is_none();
                if success {
                    demand.failures = 0;
                    demand.status = Some(payload["status"].clone());
                } else {
                    demand.failures = demand.failures.saturating_add(1);
                }
                demand.next =
                    Instant::now() + forge_interval(demand.status.as_ref(), demand.failures);
                apply
            };
            if apply {
                if let Err(error) = app
                    .git
                    .apply_forge_payload_at(&cwd, &payload, generations.get(&cwd).copied())
                    .await
                {
                    tracing::debug!(%error,"Forge observation publication failed");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metadata_filters_ignore_object_noise_and_route_worktree_owners() {
        assert!(metadata_effect(Path::new("objects/ab/object"), true) == Effect::Ignore);
        assert!(metadata_effect(Path::new("refs/heads/main.lock"), true) == Effect::Ignore);
        assert!(metadata_effect(Path::new("worktrees/feature/HEAD"), true) == Effect::Ignore);
        assert!(metadata_effect(Path::new("HEAD"), true) == Effect::Owner);
        assert!(metadata_effect(Path::new("paseo/worktree.json"), false) == Effect::Owner);
        assert!(metadata_effect(Path::new("refs/remotes/origin/main"), true) == Effect::All);
        assert!(metadata_effect(Path::new("packed-refs"), true) == Effect::All);
    }
    #[test]
    fn forge_interval_tracks_pending_checks_and_caps_error_backoff() {
        let pending = json!({"checksStatus":"pending"});
        let individual = json!({"checksStatus":"success","checks":[{"status":"pending"}]});
        assert_eq!(forge_interval(Some(&pending), 0), Duration::from_secs(20));
        assert_eq!(
            forge_interval(Some(&individual), 1),
            Duration::from_secs(20)
        );
        assert_eq!(forge_interval(Some(&pending), 2), Duration::from_secs(40));
        assert_eq!(forge_interval(None, 0), Duration::from_secs(120));
        assert_eq!(forge_interval(None, 8), Duration::from_secs(300));
    }
    #[test]
    fn last_repository_owner_cancels_descendant_work() {
        let cancel = CancellationToken::new();
        let active = cancel.child_token();
        let (events, _) = mpsc::channel(1);
        let observer = Observation {
            demands: Arc::new(StdMutex::new(HashMap::new())),
            events,
            cancel,
        };
        drop(observer);
        assert!(active.is_cancelled());
    }
}

#[cfg(test)]
mod directory_tests {
    use super::*;
    #[tokio::test]
    async fn existing_directory_can_transition_to_git_without_workspace_recreation() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mut env = BTreeMap::new();
        env.insert("GIT_CONFIG_NOSYSTEM".into(), "1".into());
        env.insert(
            "GIT_CONFIG_GLOBAL".into(),
            if cfg!(windows) {
                "NUL".into()
            } else {
                "/dev/null".into()
            },
        );
        let cancel = CancellationToken::new();
        let before = observation_roots(directory.path(), &env, cancel.clone()).await?;
        assert!(!before.3);
        assert_eq!(before.0, before.1);
        command::checked(
            directory.path(),
            &["init".into(), "-b".into(), "main".into()],
            &env,
            cancel.clone(),
            command::READ_LIMIT,
            command::READ_TIMEOUT,
        )
        .await?;
        let after = observation_roots(directory.path(), &env, cancel).await?;
        assert!(after.3);
        assert_eq!(after.1, before.1);
        assert_eq!(after.0, after.2);
        assert_ne!(after.0, after.1);
        Ok(())
    }
}
