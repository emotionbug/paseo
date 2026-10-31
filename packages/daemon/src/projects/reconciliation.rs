use super::*;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{
        Mutex as StdMutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

pub(super) struct Reconciliation {
    started: AtomicBool,
    cancel: CancellationToken,
    changes: watch::Sender<u64>,
    watchers: StdMutex<HashMap<PathBuf, RecommendedWatcher>>,
    merges: StdMutex<MergeState>,
}
#[derive(Default)]
struct MergeState {
    open: VecDeque<(String, String)>,
    in_flight: HashSet<String>,
}
impl Default for Reconciliation {
    fn default() -> Self {
        Self {
            started: AtomicBool::new(false),
            cancel: CancellationToken::new(),
            changes: watch::channel(0).0,
            watchers: StdMutex::new(HashMap::new()),
            merges: StdMutex::new(MergeState::default()),
        }
    }
}
#[derive(PartialEq, Eq)]
enum DirectoryObservation {
    Present,
    Missing,
    Unreadable,
}
async fn inspect(path: &Path) -> DirectoryObservation {
    match tokio::fs::metadata(path).await {
        Ok(m) if m.is_dir() => DirectoryObservation::Present,
        Ok(_) => DirectoryObservation::Missing,
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            DirectoryObservation::Missing
        }
        Err(_) => DirectoryObservation::Unreadable,
    }
}

impl ProjectService {
    pub(super) async fn start_reconciliation(&self) -> Result<()> {
        if self.reconciliation.started.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        self.sync_project_watches().await?;
        self.reconcile_projects(true).await?;
        let weak = self.app.clone();
        let cancel = self.reconciliation.cancel.clone();
        let mut changes = self.reconciliation.changes.subscribe();
        tokio::spawn(async move {
            let mut timer = tokio::time::interval_at(
                tokio::time::Instant::now() + std::time::Duration::from_secs(300),
                std::time::Duration::from_secs(300),
            );
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                let full = tokio::select! {_=cancel.cancelled()=>break,_=timer.tick()=>true,result=changes.changed()=>{if result.is_err(){break}tokio::select!{_=cancel.cancelled()=>break,_=tokio::time::sleep(std::time::Duration::from_millis(100))=>{}}false}};
                let Some(app) = weak.upgrade() else { break };
                let service = app.projects.clone();
                drop(app);
                if let Err(error) = service.sync_project_watches().await {
                    tracing::warn!(%error,"Failed to refresh project watches");
                }
                if let Err(error) = service.reconcile_projects(full).await {
                    tracing::warn!(%error,"Workspace reconciliation failed");
                }
            }
        });
        Ok(())
    }
    pub async fn shutdown(&self) -> Result<()> {
        self.reconciliation.cancel.cancel();
        self.reconciliation
            .watchers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        Ok(())
    }
    pub(super) async fn sync_project_watches(&self) -> Result<()> {
        if !self.reconciliation.started.load(Ordering::Acquire) {
            return Ok(());
        }
        let desired: HashSet<_> = self
            .list_projects()
            .await?
            .into_iter()
            .filter(|p| p["archivedAt"].is_null())
            .filter_map(|p| p["rootPath"].as_str().map(PathBuf::from))
            .collect();
        let mut watches = self
            .reconciliation
            .watchers
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        watches.retain(|path, _| desired.contains(path));
        for root in desired {
            if watches.contains_key(&root) {
                continue;
            }
            let tx = self.reconciliation.changes.clone();
            let watcher = notify::recommended_watcher(
                move |event: notify::Result<notify::Event>| match event {
                    Ok(event)
                        if event.paths.is_empty()
                            || event.paths.iter().any(|path| {
                                path.file_name().is_some_and(|name| name == ".git")
                            }) =>
                    {
                        tx.send_modify(|seq| *seq = seq.wrapping_add(1));
                    }
                    Err(error) => {
                        tracing::warn!(%error,"Project root watch failed");
                        tx.send_modify(|seq| *seq = seq.wrapping_add(1));
                    }
                    _ => {}
                },
            );
            match watcher {
                Ok(mut watcher) => match watcher.watch(&root, RecursiveMode::NonRecursive) {
                    Ok(()) => {
                        watches.insert(root, watcher);
                    }
                    Err(error) => tracing::debug!(%error,"Project root is not watchable yet"),
                },
                Err(error) => tracing::warn!(%error,"Unable to initialize project watcher"),
            }
        }
        Ok(())
    }
    pub async fn reconcile_projects(&self, full: bool) -> Result<()> {
        let app = self.app()?;
        let projects = self.list_projects().await?;
        let workspaces = self.list_workspaces().await?;
        let mut workspace_states = Vec::new();
        for workspace in &workspaces {
            if workspace["archivedAt"].is_null() {
                workspace_states
                    .push((workspace, inspect(Path::new(text(workspace, "cwd")?)).await));
            }
        }
        // Observe project reachability after its workspaces. A volume disappearing mid-pass can only suppress archival.
        let mut reachable = HashSet::new();
        for project in &projects {
            if project["archivedAt"].is_null()
                && inspect(Path::new(text(project, "rootPath")?)).await
                    == DirectoryObservation::Present
            {
                reachable.insert(text(project, "projectId")?.to_owned());
            }
        }
        if full {
            for (workspace, state) in &workspace_states {
                if *state == DirectoryObservation::Missing
                    && reachable.contains(text(workspace, "projectId")?)
                {
                    let id = text(workspace, "workspaceId")?;
                    self.storage()
                        .await?
                        .archive_workspace(id, &now(), None)
                        .await?;
                    if let Some(archived) = self.workspace(id).await? {
                        self.emit_workspace_hook("workspace.archived", &archived)
                            .await;
                    }
                    app.git
                        .release_observation(Path::new(text(workspace, "cwd")?))
                        .await?;
                    app.terminals.remove_workspace_runtime(id);
                    self.runtime_ports.release_plan(id);
                    self.publish_workspace(id).await?;
                }
            }
        }
        let identity = app.transport.identity();
        let mut checkouts: HashMap<String, Value> = HashMap::new();
        for project in &projects {
            let project_id = text(project, "projectId")?;
            if !reachable.contains(project_id) {
                continue;
            }
            let root = text(project, "rootPath")?;
            if !checkouts.contains_key(root) {
                match app
                    .git
                    .snapshot(Path::new(root), true, app.cancel.child_token())
                    .await
                {
                    Ok(snapshot) => {
                        let mut checkout = snapshot["git"].clone();
                        checkout["worktreeRoot"] = checkout["repoRoot"].clone();
                        checkouts.insert(root.into(), checkout);
                    }
                    Err(error) => {
                        tracing::warn!(%error,"Skipped project reconciliation after Git read failed");
                        continue;
                    }
                }
            }
            let checkout = &checkouts[root];
            if checkout["isGit"] != true
                && inspect(Path::new(root)).await != DirectoryObservation::Present
            {
                continue;
            }
            let kind = if checkout["isGit"] == true {
                "git"
            } else {
                "non_git"
            };
            let key = store::derive_project_key(
                root,
                checkout["remoteUrl"].as_str(),
                checkout["worktreeRoot"].as_str(),
                checkout["mainRepoRoot"].as_str(),
                identity["serverId"].as_str(),
            )
            .await?;
            let project_changed = project["kind"] != kind || project["projectKey"] != key;
            if project_changed {
                if let Some(updated) = self
                    .storage()
                    .await?
                    .update_project(project_id, |current| {
                        let mut p = current.clone();
                        p["kind"] = json!(kind);
                        p["projectKey"] = json!(key);
                        p["updatedAt"] = json!(now());
                        Ok(p)
                    })
                    .await?
                {
                    self.publish_project(&updated).await?;
                }
            }
            for (workspace, state) in &workspace_states {
                if workspace["projectId"] != project_id || *state != DirectoryObservation::Present {
                    continue;
                }
                let id = text(workspace, "workspaceId")?;
                let cwd = text(workspace, "cwd")?;
                if !checkouts.contains_key(cwd) {
                    match app
                        .git
                        .checkout(Path::new(cwd), app.cancel.child_token())
                        .await
                    {
                        Ok(checkout) => {
                            checkouts.insert(cwd.into(), checkout);
                        }
                        Err(error) => {
                            tracing::warn!(%error,"Skipped workspace reconciliation after Git read failed");
                            continue;
                        }
                    }
                }
                let observed = &checkouts[cwd];
                if observed["isGit"] != true
                    && inspect(Path::new(cwd)).await != DirectoryObservation::Present
                {
                    continue;
                }
                let update =
                    store::reconcile_workspace_placement(workspace, Some(observed), &now());
                if let Some(update) = &update {
                    self.storage()
                        .await?
                        .update_workspace(id, |current| {
                            let mut next = current.clone();
                            if let Some(fields) = update["fields"].as_object() {
                                for (key, value) in fields {
                                    next[key] = value.clone();
                                }
                            }
                            next["updatedAt"] = json!(now());
                            Ok(next)
                        })
                        .await?;
                    if update["fields"].get("branch").is_some() {
                        app.terminals.refresh_routes(id).await?;
                    }
                }
                if update.is_some() || project_changed {
                    self.publish_workspace(id).await?;
                }
            }
        }
        Ok(())
    }
    pub async fn observe_git_snapshot(&self, snapshot: &Value) -> Result<()> {
        let cwd = text(snapshot, "cwd")?.to_owned();
        let app = self.app()?;
        let enabled = app.config.snapshot()["autoArchiveAfterMerge"] == true;
        let pr = &snapshot["forge"]["pullRequest"];
        let mut state = self
            .reconciliation
            .merges
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if !enabled {
            state.open.retain(|(path, _)| path != &cwd);
            return Ok(());
        }
        if pr["isMerged"] != true {
            state.open.retain(|(path, _)| path != &cwd);
            if pr["state"]
                .as_str()
                .is_some_and(|s| s.eq_ignore_ascii_case("open"))
            {
                if let Some(url) = pr["url"].as_str() {
                    state.open.push_back((cwd, url.into()));
                    while state.open.len() > 1024 {
                        state.open.pop_front();
                    }
                }
            }
            return Ok(());
        }
        let Some(url) = pr["url"].as_str() else {
            return Ok(());
        };
        if !state
            .open
            .iter()
            .any(|(path, open)| path == &cwd && open == url)
        {
            state.open.retain(|(path, _)| path != &cwd);
            return Ok(());
        }
        if !state.in_flight.insert(cwd.clone()) {
            return Ok(());
        }
        drop(state);
        let weak = self.app.clone();
        let url = url.to_owned();
        drop(app);
        tokio::spawn(async move {
            let Some(app) = weak.upgrade() else { return };
            let service = app.projects.clone();
            drop(app);
            if let Err(error) = service.archive_merged_if_safe(&cwd, &url).await {
                tracing::warn!(%error,"Failed to auto-archive attached workspaces");
            }
            service
                .reconciliation
                .merges
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .in_flight
                .remove(&cwd);
        });
        Ok(())
    }
    async fn archive_merged_if_safe(&self, cwd: &str, url: &str) -> Result<()> {
        let app = self.app()?;
        let Some(snapshot) = app.git.cached_snapshot(Path::new(cwd)).await else {
            return Ok(());
        };
        if snapshot["forge"]["pullRequest"]["isMerged"] != true
            || snapshot["forge"]["pullRequest"]["url"] != url
        {
            self.reconciliation
                .merges
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .open
                .retain(|(path, _)| path != cwd);
            return Ok(());
        }
        if snapshot["git"]["isDirty"] == true
            || snapshot["git"]["aheadOfOrigin"]
                .as_f64()
                .is_some_and(|n| n > 0.0)
        {
            return Ok(());
        }
        if worktrees::ownership(
            &app.home,
            &app.config.snapshot(),
            Path::new(cwd),
            app.cancel.child_token(),
        )
        .await?["allowed"]
            != true
        {
            return Ok(());
        }
        let records = self.list_workspaces().await?;
        for workspace in records {
            if workspace["archivedAt"].is_null()
                && workspace["autoArchivedChangeRequestUrl"] != url
                && store::equivalent_paths(text(&workspace, "cwd")?, cwd)
            {
                let _guard = self.lifecycle.lock().await;
                self.archive_locked(
                    &json!({"kind":"system"}),
                    text(&workspace, "workspaceId")?,
                    true,
                    Some(url),
                )
                .await?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn missing_observation_differs_from_present_file() {
        let temp = tempfile::tempdir().unwrap();
        assert!(inspect(temp.path()).await == DirectoryObservation::Present);
        let path = temp.path().join("removed");
        assert!(inspect(&path).await == DirectoryObservation::Missing);
        tokio::fs::write(&path, b"not a directory").await.unwrap();
        assert!(inspect(&path).await == DirectoryObservation::Missing);
    }
}
