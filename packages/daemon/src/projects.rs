use crate::{state::AppState, transport::RequestContext};
use anyhow::{Context, Result, anyhow, bail};
use chrono::{SecondsFormat, Utc};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    path::{Component, Path, PathBuf},
    sync::{Arc, RwLock, Weak},
};
use tokio::sync::{Mutex, OnceCell};

mod acquisition;
mod bootstrap;
pub mod directory;
mod naming;
mod project_config;
mod reconciliation;
pub mod runtime;
pub mod store;
mod wire;
pub mod worktrees;

pub struct ProjectService {
    app: Weak<AppState>,
    store: OnceCell<store::Store>,
    lifecycle: Mutex<()>,
    reconciliation: reconciliation::Reconciliation,
    icon_snapshots: RwLock<HashMap<String, Value>>,
    pub runtime_ports: runtime::RuntimePorts,
    pub(super) directory_state: directory::DirectoryState,
}
impl ProjectService {
    pub fn new(app: Weak<AppState>) -> Self {
        Self {
            app,
            store: OnceCell::new(),
            lifecycle: Mutex::new(()),
            reconciliation: Default::default(),
            icon_snapshots: Default::default(),
            runtime_ports: runtime::RuntimePorts::new(),
            directory_state: Default::default(),
        }
    }
    fn app(&self) -> Result<Arc<AppState>> {
        self.app
            .upgrade()
            .ok_or_else(|| anyhow!("Daemon is shutting down"))
    }
    async fn storage(&self) -> Result<&store::Store> {
        self.store
            .get_or_try_init(|| async { store::Store::load(&self.app()?.home).await })
            .await
    }
    pub async fn initialize(&self) -> Result<()> {
        self.storage().await?;
        Ok(())
    }
    pub async fn start_observations(&self) -> Result<()> {
        let app = self.app()?;
        bootstrap::bootstrap(&app).await?;
        for workspace in self.list_workspaces().await? {
            if workspace["archivedAt"].is_null() {
                let cwd = Path::new(text(&workspace, "cwd")?);
                if let Err(error) = app.git.retain_observation(cwd).await {
                    tracing::warn!(%error,cwd=%cwd.display(),"Workspace observation unavailable");
                }
            }
        }
        self.start_reconciliation().await
    }
    pub async fn teardown_path_environment(
        &self,
        cwd: &Path,
        repo_root: Option<&Path>,
    ) -> Result<BTreeMap<String, String>> {
        let app = self.app()?;
        let config = app.config.snapshot();
        let ownership =
            worktrees::ownership(&app.home, &config, cwd, app.cancel.child_token()).await?;
        let root = Path::new(
            ownership["worktreePath"]
                .as_str()
                .unwrap_or(cwd.to_str().context("Invalid worktree path")?),
        );
        let repo = match repo_root {
            Some(path) => path.to_path_buf(),
            None => worktrees::repo_root(root, app.cancel.child_token()).await?,
        };
        let branch = crate::git::command::optional(
            root,
            &["branch", "--show-current"],
            &app.config.environment(),
            app.cancel.child_token(),
        )
        .await
        .ok()
        .flatten()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| {
            root.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        });
        let mut env = BTreeMap::from([
            (
                "PASEO_SOURCE_CHECKOUT_PATH".into(),
                repo.to_string_lossy().into_owned(),
            ),
            (
                "PASEO_ROOT_PATH".into(),
                repo.to_string_lossy().into_owned(),
            ),
            (
                "PASEO_WORKTREE_PATH".into(),
                root.to_string_lossy().into_owned(),
            ),
            ("PASEO_BRANCH_NAME".into(), branch),
        ]);
        if let Some(port) = runtime::read_worktree_runtime_port(root).await? {
            env.insert("PASEO_WORKTREE_PORT".into(), port.to_string());
        }
        Ok(env)
    }
    pub async fn run_in_import_workspace<T, F, Fut>(
        &self,
        cwd: &Path,
        requested: Option<&str>,
        operation: F,
    ) -> Result<T>
    where
        F: FnOnce(Value) -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        let projects = self.list_projects().await?;
        let before = self.list_workspaces().await?;
        let workspace = if let Some(id) = requested {
            let workspace = self
                .workspace(id)
                .await?
                .filter(|w| w["archivedAt"].is_null())
                .ok_or_else(|| anyhow!("Workspace not found: {id}"))?;
            self.active_project(text(&workspace, "projectId")?).await?;
            if !same_path(Path::new(text(&workspace, "cwd")?), &self.absolute(cwd)?).await {
                bail!("Import cwd does not match workspace: {id}")
            }
            workspace
        } else {
            self.resolve_workspace(cwd).await?
        };
        let created = requested.is_none()
            && !before
                .iter()
                .any(|w| w["workspaceId"] == workspace["workspaceId"]);
        let result = operation(workspace.clone()).await;
        if result.is_err() && created {
            let rollback = async {
                let id = text(&workspace, "workspaceId")?;
                let project_id = text(&workspace, "projectId")?;
                self.storage().await?.remove_workspace(id).await?;
                self.app()?
                    .git
                    .release_observation(Path::new(text(&workspace, "cwd")?))
                    .await?;
                if !self
                    .list_workspaces()
                    .await?
                    .iter()
                    .any(|w| w["projectId"] == project_id && w["archivedAt"].is_null())
                {
                    match projects.iter().find(|p| p["projectId"] == project_id) {
                        Some(previous) if !previous["archivedAt"].is_null() => {
                            self.storage()
                                .await?
                                .upsert_project(previous.clone())
                                .await?
                        }
                        None => {
                            self.storage().await?.remove_project(project_id).await?;
                        }
                        _ => {}
                    }
                }
                self.publish_workspace(id).await
            }
            .await;
            if let Err(error) = rollback {
                tracing::error!(%error,"Failed to restore workspace state after provider import failure");
            }
        }
        result
    }
    pub async fn workspace(&self, id: &str) -> Result<Option<Value>> {
        Ok(self.storage().await?.get_workspace(id).await)
    }
    pub async fn project(&self, id: &str) -> Result<Option<Value>> {
        Ok(self.storage().await?.get_project(id).await)
    }
    pub async fn list_workspaces(&self) -> Result<Vec<Value>> {
        Ok(self.storage().await?.list_workspaces().await)
    }
    pub async fn list_projects(&self) -> Result<Vec<Value>> {
        Ok(self.storage().await?.list_projects().await)
    }
    pub async fn descriptor(&self, id: &str) -> Result<Value> {
        let workspace = self
            .workspace(id)
            .await?
            .ok_or_else(|| anyhow!("Workspace not found: {id}"))?;
        let project = self
            .project(text(&workspace, "projectId")?)
            .await?
            .ok_or_else(|| anyhow!("Project not found"))?;
        directory::descriptor(self.app()?.as_ref(), &workspace, &project).await
    }
    pub async fn project_placement_for_workspace(&self, id: &str) -> Result<Option<Value>> {
        let Some(workspace) = self.workspace(id).await? else {
            return Ok(None);
        };
        let Some(project) = self.project(text(&workspace, "projectId")?).await? else {
            return Ok(None);
        };
        Ok(Some(
            directory::placement(self.app()?.as_ref(), &workspace, &project).await?,
        ))
    }
    pub async fn assert_automation_allowed(&self, id: &str) -> Result<()> {
        let workspace = self
            .workspace(id)
            .await?
            .ok_or_else(|| anyhow!("Workspace not found: {id}"))?;
        if let Some(source) = workspace.get("untrustedSource").filter(|s| !s.is_null()) {
            bail!("{}", automation_blocked_message(source));
        }
        Ok(())
    }
    pub async fn clear_automation_block(&self, id: &str) -> Result<bool> {
        let mut changed = false;
        let updated = self
            .storage()
            .await?
            .update_workspace(id, |record| {
                let mut record = record.clone();
                changed = record
                    .as_object_mut()
                    .unwrap()
                    .remove("untrustedSource")
                    .is_some();
                if changed {
                    record["updatedAt"] = json!(now());
                }
                Ok(record)
            })
            .await?;
        if updated.is_none() {
            bail!("Workspace not found: {id}");
        }
        if changed {
            self.publish_workspace(id).await?;
        }
        Ok(changed)
    }
    pub async fn environment(&self, id: &str) -> Result<BTreeMap<String, String>> {
        let mut env = self.teardown_environment(id).await?;
        let workspace = self
            .workspace(id)
            .await?
            .ok_or_else(|| anyhow!("Workspace not found: {id}"))?;
        if workspace["kind"] == "worktree" {
            let root = workspace["worktreeRoot"]
                .as_str()
                .unwrap_or(text(&workspace, "cwd")?);
            env.insert(
                "PASEO_WORKTREE_PORT".into(),
                runtime::ensure_worktree_runtime_port(Path::new(root))
                    .await?
                    .to_string(),
            );
        }
        Ok(env)
    }
    pub async fn teardown_environment(&self, id: &str) -> Result<BTreeMap<String, String>> {
        let workspace = self
            .workspace(id)
            .await?
            .ok_or_else(|| anyhow!("Workspace not found: {id}"))?;
        let cwd = text(&workspace, "cwd")?;
        let root = workspace["worktreeRoot"].as_str().unwrap_or(cwd);
        let source = workspace["mainRepoRoot"].as_str().unwrap_or(cwd);
        let mut env = BTreeMap::new();
        env.insert("PASEO_SOURCE_CHECKOUT_PATH".into(), source.into());
        env.insert("PASEO_ROOT_PATH".into(), source.into());
        env.insert("PASEO_WORKTREE_PATH".into(), root.into());
        if let Some(branch) = workspace["branch"].as_str() {
            env.insert("PASEO_BRANCH_NAME".into(), branch.into());
        }
        if workspace["kind"] == "worktree"
            && tokio::fs::try_exists(Path::new(root).join(".git")).await?
        {
            if let Some(port) = runtime::read_worktree_runtime_port(Path::new(root)).await? {
                env.insert("PASEO_WORKTREE_PORT".into(), port.to_string());
            }
        }
        Ok(env)
    }
    pub async fn register_project(&self, cwd: &Path) -> Result<Value> {
        let cwd = self.absolute(cwd)?;
        require_directory(&cwd).await?;
        let checkout = self.checkout(&cwd).await?;
        let identity = self.app()?.transport.identity();
        let key = store::derive_project_key(
            &cwd.to_string_lossy(),
            checkout["remoteUrl"].as_str(),
            checkout["worktreeRoot"].as_str(),
            checkout["mainRepoRoot"].as_str(),
            identity["serverId"].as_str(),
        )
        .await?;
        let project = self
            .storage()
            .await?
            .get_or_create_active_project(
                &cwd.to_string_lossy(),
                if checkout["isGit"] == true {
                    "git"
                } else {
                    "non_git"
                },
                cwd.file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or(&cwd.to_string_lossy()),
                Some(&key),
                &now(),
            )
            .await?;
        self.publish_project(&project).await?;
        Ok(project)
    }
    async fn checkout(&self, cwd: &Path) -> Result<Value> {
        let app = self.app()?;
        app.git.checkout(cwd, app.cancel.child_token()).await
    }
    fn absolute(&self, path: &Path) -> Result<PathBuf> {
        let app = self.app()?;
        let expanded = if path == Path::new("~") {
            app.user_home.clone()
        } else if let Ok(rest) = path.strip_prefix("~/") {
            app.user_home.join(rest)
        } else {
            path.to_path_buf()
        };
        let path = if expanded.is_absolute() {
            expanded
        } else {
            std::env::current_dir()?.join(expanded)
        };
        let mut out = PathBuf::new();
        for c in path.components() {
            match c {
                Component::CurDir => {}
                Component::ParentDir => {
                    out.pop();
                }
                other => out.push(other.as_os_str()),
            }
        }
        Ok(out)
    }
    pub async fn resolve_workspace(&self, cwd: &Path) -> Result<Value> {
        let _guard = self.lifecycle.lock().await;
        let cwd = self.absolute(cwd)?;
        require_directory(&cwd).await?;
        let mut candidates: Vec<_> = self
            .list_workspaces()
            .await?
            .into_iter()
            .filter(|w| {
                w["background"] != true
                    && store::equivalent_paths(
                        w["cwd"].as_str().unwrap_or(""),
                        &cwd.to_string_lossy(),
                    )
            })
            .collect();
        candidates.sort_by(|a, b| {
            a["createdAt"]
                .as_str()
                .cmp(&b["createdAt"].as_str())
                .then(a["workspaceId"].as_str().cmp(&b["workspaceId"].as_str()))
        });
        if let Some(workspace) = candidates.iter().find(|w| w["archivedAt"].is_null()) {
            return self.refresh_workspace(workspace.clone()).await;
        }
        for workspace in candidates {
            if let Some(project) = self.project(text(&workspace, "projectId")?).await? {
                if project["archivedAt"].is_null() {
                    return self.unarchive(workspace).await;
                }
            }
        }
        self.create_directory(&cwd, &json!({})).await
    }
    pub async fn ensure_workspace_for_create(
        &self,
        cwd: &Path,
        first_agent_context: &Value,
        caller_workspace_id: Option<&str>,
    ) -> Result<Value> {
        let _guard = self.lifecycle.lock().await;
        self.create_directory(cwd,&json!({"firstAgentContext":first_agent_context,"callerWorkspaceId":caller_workspace_id,"deferName":true})).await
    }
    async fn background(&self, input: &Value) -> Result<bool> {
        if let Some(value) = input["background"].as_bool() {
            return Ok(value);
        }
        let mut caller = input["callerWorkspaceId"].as_str().map(str::to_owned);
        if caller.is_none() {
            if let Some(id) = input["callerAgentId"].as_str() {
                let a = self
                    .app()?
                    .agents
                    .agent_snapshot(id)
                    .await?
                    .ok_or_else(|| anyhow!("Caller agent {id} not found"))?;
                caller = a["workspaceId"].as_str().map(str::to_owned);
            }
        }
        match caller {
            None => Ok(false),
            Some(id) => Ok(self
                .workspace(&id)
                .await?
                .ok_or_else(|| anyhow!("Caller workspace {id} not found"))?["background"]
                == true),
        }
    }
    async fn active_project(&self, id: &str) -> Result<Value> {
        let p = self.project(id).await?.ok_or_else(|| {
            RequestError::new("unknown_project", format!("Unknown project: {id}"))
        })?;
        if !p["archivedAt"].is_null() {
            return Err(
                RequestError::new("archived_project", format!("Archived project: {id}")).into(),
            );
        }
        Ok(p)
    }
    async fn create_directory(&self, cwd: &Path, input: &Value) -> Result<Value> {
        let cwd = self.absolute(cwd)?;
        require_directory(&cwd).await?;
        let checkout = self.checkout(&cwd).await?;
        let project = if let Some(id) = input["projectId"].as_str() {
            self.active_project(id).await?
        } else {
            self.register_project(&cwd).await?
        };
        let mut record = store::initial_workspace_placement(
            &json!({"source":"checkout","cwd":cwd,"checkout":checkout}),
        );
        let timestamp = now();
        let id = input["workspaceId"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(store::generate_workspace_id);
        if self.workspace(&id).await?.is_some() {
            bail!("Workspace already exists: {id}")
        }
        record["workspaceId"] = json!(id);
        record["projectId"] = project["projectId"].clone();
        record["createdAt"] = json!(timestamp);
        record["updatedAt"] = json!(timestamp);
        record["title"] = json!(initial_title(input));
        record["background"] = json!(self.background(input).await?);
        let record = store::create_workspace(record)?;
        self.storage()
            .await?
            .upsert_workspace(record.clone())
            .await?;
        self.emit_workspace_hook("workspace.created", &record).await;
        if let Err(error) = self.app()?.git.retain_observation(&cwd).await {
            tracing::warn!(%error,"Created workspace observation unavailable");
        }
        self.publish_workspace(&id).await?;
        if input["deferName"] != true {
            if let Some(context) = input.get("firstAgentContext") {
                self.schedule_name(&record, context);
            }
        }
        Ok(record)
    }
    pub async fn create_workspace(&self, input: &Value) -> Result<Value> {
        if input["internal"] == true {
            bail!("Public internal workspaces were removed; use background workspace creation.")
        }
        if input["agent"].get("background").is_some() {
            bail!("Configure background on the workspace, not its initial agent.")
        }
        let mut transformed;
        let input = if input["type"] == "workspace.create.request" {
            // The source's wire parse strips keys outside the request schema, so the
            // hook (and the creation it feeds) only ever sees schema fields.
            let mut hook_input = input.clone();
            hook_input
                .as_object_mut()
                .context("Invalid workspace request")?
                .retain(|key, _| crate::plugins::WORKSPACE_CREATE_FIELDS.contains(&key.as_str()));
            transformed = self
                .app()?
                .plugins
                .before("workspace.create", hook_input)
                .await?;
            if let Some(workspace_id) = input.get("workspaceId") {
                transformed["workspaceId"] = workspace_id.clone();
            }
            &transformed
        } else {
            input
        };
        let _guard = self.lifecycle.lock().await;
        let source = &input["source"];
        let mut options = input.clone();
        if let Some(id) = source.get("projectId") {
            options["projectId"] = id.clone();
        }
        let mut created = match text(source, "kind")? {
            "directory" => {
                self.create_directory(Path::new(text(source, "path")?), &options)
                    .await?
            }
            "worktree" => self.create_worktree(source, &options).await?,
            _ => bail!("Unexpected workspace source"),
        };
        created["created"] = json!(true);
        created["creationKind"] = json!("created");
        Ok(created)
    }
    async fn create_worktree(&self, source: &Value, options: &Value) -> Result<Value> {
        let app = self.app()?;
        let id = options["workspaceId"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(store::generate_workspace_id);
        if self.workspace(&id).await?.is_some() {
            bail!("Workspace already exists: {id}")
        }
        let background = self.background(options).await?;
        let requested_project = if let Some(id) = source["projectId"].as_str() {
            Some(self.active_project(id).await?)
        } else {
            None
        };
        let cwd = source["cwd"]
            .as_str()
            .or_else(|| {
                requested_project
                    .as_ref()
                    .and_then(|p| p["rootPath"].as_str())
            })
            .ok_or_else(|| {
                RequestError::new(
                    "source_required",
                    "cwd or projectId is required for a worktree-backed workspace",
                )
            })?;
        let cwd = self.absolute(Path::new(cwd))?;
        let checkout = self.checkout(&cwd).await?;
        let source_root = PathBuf::from(
            checkout["worktreeRoot"]
                .as_str()
                .unwrap_or(&cwd.to_string_lossy()),
        );
        let mut request = source.clone();
        if let Some(context) = options.get("firstAgentContext") {
            request["firstAgentContext"] = context.clone();
        }
        if request.get("refName").is_none() {
            if let Some(base) = source.get("baseBranch") {
                request["refName"] = base.clone();
            }
        }
        let created = worktrees::create(
            &app.home,
            &app.config.snapshot(),
            &cwd,
            &request,
            &app.git.forge,
            &app.config.environment(),
            app.cancel.child_token(),
        )
        .await?;
        let mut persisted = false;
        let result=async {
            let worktree=&created["worktree"];let repo=Path::new(text(&created,"repoRoot")?);
            let project=if let Some(project)=requested_project{project}else{
                let candidates=self.list_workspaces().await?;
                let existing=candidates.iter().find(|w|w["archivedAt"].is_null()&&store::equivalent_paths(w["cwd"].as_str().unwrap_or(""),&cwd.to_string_lossy())).or_else(||candidates.iter().find(|w|w["archivedAt"].is_null()&&store::equivalent_paths(w["cwd"].as_str().unwrap_or(""),&repo.to_string_lossy())));
                match existing {Some(w)=>match self.project(text(w,"projectId")?).await? {Some(p)=>p,None=>self.register_project(repo).await?},None=>self.register_project(repo).await?}
            };
            let root=Path::new(text(worktree,"worktreePath")?);
            let target=worktrees::map_workspace_cwd(&source_root,&cwd,root).await?;
            if !tokio::fs::metadata(&target).await.map(|m|m.is_dir()).unwrap_or(false){bail!("Selected project directory is missing from the worktree: {}",target.display())}
            worktrees::seed_paseo_config_file(&cwd,&target).await?;
            if created["intent"]["kind"]=="branch-off" {runtime::write_worktree_first_agent_branch_auto_name_metadata(root,text(worktree,"branchName")?).await?;}
            let mut record=store::initial_workspace_placement(&json!({"source":"created_worktree","cwd":target,"worktreeRoot":root,"branch":worktree["branchName"],"baseBranch":worktree["comparisonBaseRef"],"mainRepoRoot":repo}));
            record["workspaceId"]=json!(id);record["projectId"]=project["projectId"].clone();record["title"]=json!(initial_title(options));record["createdAt"]=json!(now());record["updatedAt"]=record["createdAt"].clone();record["background"]=json!(background);
            let intent=&created["intent"];
            if intent["kind"]=="checkout-change-request"&&intent["headRepository"].as_str().is_some_and(|s|!s.is_empty()){
                record["untrustedSource"]=json!({"kind":"change_request","forge":intent["forge"],"number":intent["changeRequestNumber"],"headRepository":intent["headRepository"]});
            }
            let mut record=store::create_workspace(record)?;
            self.storage().await?.upsert_workspace(record.clone()).await?;persisted=true;
            self.emit_workspace_hook("workspace.created",&record).await;
            if let Err(error)=app.git.retain_observation(Path::new(text(&record,"cwd")?)).await{tracing::warn!(%error,"Created workspace observation unavailable");}
            self.publish_workspace(&id).await?;
            app.terminals.start_setup(&id).await?;
            if let Some(context)=options.get("firstAgentContext"){self.schedule_name(&record,context);}
            record["createdWorktree"]=json!({"worktree":created["worktree"],"intent":created["intent"],"repoRoot":created["repoRoot"],"workspace":record.clone(),"created":true});
            Ok(record)
        }.await;
        if result.is_err() && !persisted {
            if let Err(error) = worktrees::rollback_created(
                &app.home,
                &app.config.snapshot(),
                &created,
                app.cancel.child_token(),
            )
            .await
            {
                tracing::warn!(%error,"Failed to roll back created worktree; retaining owned resources");
            }
        }
        result
    }
    pub async fn refresh_workspace(&self, workspace: Value) -> Result<Value> {
        let cwd = Path::new(text(&workspace, "cwd")?);
        if !tokio::fs::metadata(cwd)
            .await
            .map(|m| m.is_dir())
            .unwrap_or(false)
        {
            return Ok(workspace);
        }
        let checkout = self.checkout(cwd).await?;
        if checkout["isGit"] != true
            && !tokio::fs::metadata(cwd)
                .await
                .map(|m| m.is_dir())
                .unwrap_or(false)
        {
            return Ok(workspace);
        }
        let timestamp = now();
        if store::reconcile_workspace_placement(&workspace, Some(&checkout), &timestamp).is_some() {
            let id = text(&workspace, "workspaceId")?;
            Ok(self
                .storage()
                .await?
                .update_workspace(id, |current| {
                    Ok(
                        store::reconcile_workspace_placement(current, Some(&checkout), &timestamp)
                            .map(|update| update["workspace"].clone())
                            .unwrap_or_else(|| current.clone()),
                    )
                })
                .await?
                .unwrap_or(workspace))
        } else {
            Ok(workspace)
        }
    }
    pub async fn reconcile_checkout(&self, cwd: &Path) -> Result<()> {
        for workspace in self.list_workspaces().await? {
            if !workspace["archivedAt"].is_null() {
                continue;
            }
            let directory = Path::new(text(&workspace, "cwd")?);
            let root = Path::new(
                workspace["worktreeRoot"]
                    .as_str()
                    .unwrap_or(text(&workspace, "cwd")?),
            );
            if same_path(directory, cwd).await || same_path(root, cwd).await {
                let before = workspace["branch"].clone();
                let next = self.refresh_workspace(workspace).await?;
                let id = text(&next, "workspaceId")?;
                if next["branch"] != before {
                    self.app()?.terminals.refresh_routes(id).await?;
                }
                self.publish_workspace(id).await?;
            }
        }
        Ok(())
    }
    async fn unarchive(&self, workspace: Value) -> Result<Value> {
        let mut workspace = self.refresh_workspace(workspace).await?;
        let id = text(&workspace, "workspaceId")?.to_owned();
        let mut project = self
            .project(text(&workspace, "projectId")?)
            .await?
            .ok_or_else(|| anyhow!("Unknown project"))?;
        let app = self.app()?;
        let cwd = Path::new(text(&workspace, "cwd")?);
        let snapshot = if tokio::fs::metadata(cwd)
            .await
            .map(|m| m.is_dir())
            .unwrap_or(false)
        {
            Some(
                app.git
                    .snapshot(cwd, true, app.cancel.child_token())
                    .await?,
            )
        } else {
            app.git.cached_snapshot(cwd).await
        };
        if let Some(snapshot) = snapshot {
            if snapshot["forge"]["pullRequest"]["isMerged"] == true {
                workspace["autoArchivedChangeRequestUrl"] =
                    snapshot["forge"]["pullRequest"]["url"].clone();
            }
        }
        project["archivedAt"] = Value::Null;
        project["updatedAt"] = json!(now());
        self.storage()
            .await?
            .upsert_project(project.clone())
            .await?;
        workspace["archivedAt"] = Value::Null;
        workspace["updatedAt"] = json!(now());
        self.storage()
            .await?
            .upsert_workspace(workspace.clone())
            .await?;
        if let Err(error) = app
            .git
            .retain_observation(Path::new(text(&workspace, "cwd")?))
            .await
        {
            tracing::warn!(%error,"Restored workspace observation unavailable");
        }
        self.publish_project(&project).await?;
        self.publish_workspace(&id).await?;
        Ok(workspace)
    }
    pub async fn rename_workspace(&self, id: &str, title: Option<&str>) -> Result<bool> {
        let title = title.map(str::trim).filter(|s| !s.is_empty());
        let changed = self
            .storage()
            .await?
            .update_workspace(id, |old| {
                let mut next = old.clone();
                next["title"] = json!(title);
                next["updatedAt"] = json!(now());
                Ok(next)
            })
            .await?
            .is_some();
        if changed {
            self.publish_workspace(id).await?;
        }
        Ok(changed)
    }
    pub async fn archive_workspace(&self, actor: &Value, id: &str) -> Result<Value> {
        let _guard = self.lifecycle.lock().await;
        self.archive_locked(actor, id, true, None).await
    }
    async fn archive_locked(
        &self,
        actor: &Value,
        id: &str,
        remove_disk: bool,
        auto_url: Option<&str>,
    ) -> Result<Value> {
        let app = self.app()?;
        let workspace = self
            .workspace(id)
            .await?
            .ok_or_else(|| anyhow!("Workspace not found: {id}"))?;
        let ids = vec![id.to_owned()];
        self.directory_state.mark_archiving(&ids, &now());
        let result=async {
            if let Err(error)=app.terminals.stop_setup(id).await{tracing::warn!(%error,"Failed to stop workspace setup during archive");}
            self.publish_workspace(id).await?;
            let mut agents=Vec::new();
            if workspace["archivedAt"].is_null(){
                for agent in app.agents.list_snapshots().await? {if agent["workspaceId"]==id{let agent_id=text(&agent,"id")?.to_owned();agents.push(agent_id.clone());if agent["archivedAt"].is_null(){if let Err(error)=app.agents.control(actor,&agent_id,"archive",json!({})).await{tracing::warn!(%error,workspace_id=id,"Workspace agent archive failed");}}}}
                if let Err(error)=app.terminals.kill_workspace(id).await{tracing::warn!(%error,workspace_id=id,"Workspace terminal archive failed");}
                self.storage().await?.archive_workspace(id,&now(),auto_url).await?;
                if let Some(archived)=self.workspace(id).await?{self.emit_workspace_hook("workspace.archived",&archived).await;}
                if let Err(error)=app.git.release_observation(Path::new(text(&workspace,"cwd")?)).await{tracing::warn!(%error,"Failed to release workspace observation");}
            }
            let mut removed=false;
            if remove_disk&&workspace["isPaseoOwnedWorktree"]==true {
                let root=workspace["worktreeRoot"].as_str().unwrap_or(text(&workspace,"cwd")?);
                let blocked=workspace.get("untrustedSource").is_some_and(|source|!source.is_null());
                let teardown=if blocked||!workspace["archivedAt"].is_null(){Ok(())}else{app.terminals.teardown(id).await};
                if let Err(error)=teardown{tracing::warn!(%error,workspace_id=id,"Worktree teardown failed; preserving directory");}else{
                    let mut referenced=false;
                    for other in self.list_workspaces().await?{if other["archivedAt"].is_null(){let backing=other["worktreeRoot"].as_str().unwrap_or(text(&other,"cwd")?);if same_path(Path::new(backing),Path::new(root)).await{referenced=true;break}}}
                    if !referenced {if let Some(repo)=workspace["mainRepoRoot"].as_str(){match worktrees::remove(&app.home,&app.config.snapshot(),Path::new(repo),Path::new(root),app.cancel.child_token()).await{Ok(())=>removed=true,Err(error)=>tracing::warn!(%error,"Worktree removal failed; workspace remains archived")}}}
                }
            }
            self.runtime_ports.release_plan(id);
            app.terminals.remove_workspace_runtime(id);
            self.publish_workspace(id).await?;
            let archived=self.workspace(id).await?.unwrap();
            Ok(json!({"workspaceId":id,"archivedAgentIds":agents,"removedDirectory":removed,"archivedAt":archived["archivedAt"],"error":null}))
        }.await;
        self.directory_state.clear_archiving(&ids);
        self.publish_workspace(id).await?;
        result
    }
    async fn emit_workspace_hook(&self, name: &str, workspace: &Value) {
        match self.app() {
            Ok(app) => {
                if let Err(error) = app
                    .plugins
                    .emit(name, json!({"workspace":hook_workspace(workspace)}))
                    .await
                {
                    tracing::warn!(%error,hook=name,"Workspace lifecycle hook was not admitted");
                }
            }
            Err(error) => tracing::warn!(%error,"Workspace lifecycle hook unavailable"),
        }
    }
    async fn publish_project(&self, project: &Value) -> Result<()> {
        let app = self.app()?;
        self.sync_project_watches().await?;
        let mut project = project.clone();
        project["projectIconRevision"] = self.icon_snapshot(&project).await?["revision"].clone();
        let payload = app.agents.directory_sync.sequence_project_update(
            json!({"kind":"upsert","project":directory::project_descriptor(&project)}),
            true,
        )?;
        app.transport
            .publish_event(json!({"type":"project.update","payload":payload}))
            .await
    }
    pub async fn publish_workspace(&self, id: &str) -> Result<()> {
        // Runtime directory subscriptions obtain their status from the same composed snapshot.
        let app = self.app()?;
        let workspace = self.workspace(id).await?;
        let entry = match workspace.as_ref() {
            Some(w) if w["archivedAt"].is_null() => {
                let project = self.project(text(w, "projectId")?).await?;
                match project {
                    Some(p) if p["archivedAt"].is_null() => {
                        Some(directory::descriptor(&app, w, &p).await?)
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        let mut payload = match entry.as_ref() {
            Some(entry) => json!({"kind":"upsert","workspace":entry}),
            None => json!({"kind":"remove","id":id}),
        };
        if entry.is_none() {
            if let Some(w) = &workspace {
                let project_id = text(w, "projectId")?;
                if !self.list_workspaces().await?.iter().any(|candidate| {
                    candidate["projectId"] == project_id && candidate["archivedAt"].is_null()
                }) {
                    if let Some(project) = self
                        .project(project_id)
                        .await?
                        .filter(|p| p["archivedAt"].is_null())
                    {
                        payload["emptyProject"] = directory::project_descriptor(&project);
                    }
                }
            }
        }
        let background = workspace.as_ref().is_some_and(|w| w["background"] == true);
        let payload = app
            .agents
            .directory_sync
            .sequence_workspace_update(payload, entry, id, true, background)?;
        if !background {
            app.transport
                .publish_event(json!({"type":"workspace_update","payload":payload}))
                .await?;
        }
        Ok(())
    }
    pub async fn handle(&self, context: &RequestContext, message: &Value) -> Result<bool> {
        // COMPAT(desktopEditorBridge): old clients still receive the source's refusal.
        const EDITOR_MOVED: &str =
            "Editor opening moved to the desktop app and is no longer supported by the daemon";
        match message["type"].as_str() {
            Some("list_available_editors_request") => {
                context.send(json!({"type":"list_available_editors_response","payload":{"requestId":message["requestId"],"editors":[],"error":EDITOR_MOVED}})).await?;
                return Ok(true);
            }
            Some("open_in_editor_request") => {
                context.send(json!({"type":"open_in_editor_response","payload":{"requestId":message["requestId"],"error":EDITOR_MOVED}})).await?;
                return Ok(true);
            }
            _ => {}
        }
        Ok(self.handle_project_config(context, message).await?
            || self.handle_wire(context, message).await?)
    }
}
fn hook_workspace(workspace: &Value) -> Value {
    json!({"id":workspace["workspaceId"],"projectId":workspace["projectId"],"cwd":workspace["cwd"],"name":workspace["title"],"archivedAt":workspace["archivedAt"]})
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct RequestError {
    pub code: &'static str,
    pub message: String,
}
impl RequestError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}
pub fn automation_blocked_message(source: &Value) -> String {
    format!(
        "Scripts are blocked for {} #{} from {}. Run setup to allow them.",
        if source["forge"] == "gitlab" {
            "MR"
        } else {
            "PR"
        },
        source["number"],
        source["headRepository"].as_str().unwrap_or("")
    )
}
fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}
fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key].as_str().ok_or_else(|| anyhow!("Missing {key}"))
}
fn initial_title(input: &Value) -> Option<String> {
    if let Some(title) = input["title"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Some(title.into());
    }
    let line = input["firstAgentContext"]["prompt"]
        .as_str()?
        .lines()
        .map(str::trim)
        .find(|s| !s.is_empty())?;
    let normalized = line.split_whitespace().collect::<Vec<_>>().join(" ");
    let title = String::from_utf16_lossy(&normalized.encode_utf16().take(60).collect::<Vec<_>>())
        .trim()
        .to_owned();
    (!title.is_empty()).then_some(title)
}
async fn require_directory(path: &Path) -> Result<()> {
    if !tokio::fs::metadata(path)
        .await
        .map(|m| m.is_dir())
        .unwrap_or(false)
    {
        return Err(RequestError::new(
            "directory_not_found",
            format!("Directory not found: {}", path.display()),
        )
        .into());
    }
    Ok(())
}
async fn same_path(a: &Path, b: &Path) -> bool {
    let left = tokio::fs::canonicalize(a)
        .await
        .unwrap_or_else(|_| a.to_path_buf());
    let right = tokio::fs::canonicalize(b)
        .await
        .unwrap_or_else(|_| b.to_path_buf());
    store::equivalent_paths(&left.to_string_lossy(), &right.to_string_lossy())
}
