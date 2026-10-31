use super::{CreateTerminal, Terminal, TerminalEvent, setup::Setup};
use crate::{
    projects::runtime::{
        ServicePeer, ServicePortDeclaration, assert_no_service_env_name_collisions,
        build_workspace_service_env, project_service_proxy_urls, service_proxy_label,
    },
    state::AppState,
    transport::RequestContext,
};
use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Weak},
    time::{Duration, Instant},
};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
pub(crate) mod config;

#[derive(Clone)]
struct Runtime {
    terminal_id: String,
    generation: uuid::Uuid,
    service: bool,
    running: bool,
    exit_code: Option<i32>,
    route: Option<Value>,
    health: Option<&'static str>,
    failures: u32,
    registered: Instant,
    cancel: CancellationToken,
}
impl Runtime {
    fn record_probe(&mut self, healthy: bool) -> bool {
        let previous = self.health;
        if healthy {
            self.failures = 0;
            self.health = Some("healthy");
        } else {
            self.failures = self.failures.saturating_add(1);
            if self.failures >= 2 {
                self.health = Some("unhealthy");
            }
        }
        previous != self.health
    }
}
struct Inner {
    app: Weak<AppState>,
    entries: Mutex<HashMap<(String, String), Runtime>>,
    admission: tokio::sync::Mutex<()>,
    cancel: CancellationToken,
}
#[derive(Clone)]
pub(crate) struct Scripts {
    inner: Arc<Inner>,
    setup: Setup,
}
impl Scripts {
    pub fn new(app: Weak<AppState>) -> Self {
        Self {
            setup: Setup::new(app.clone()),
            inner: Arc::new(Inner {
                app,
                entries: Mutex::new(HashMap::new()),
                admission: tokio::sync::Mutex::new(()),
                cancel: CancellationToken::new(),
            }),
        }
    }
    fn app(&self) -> Result<Arc<AppState>> {
        self.inner
            .app
            .upgrade()
            .ok_or_else(|| anyhow!("Daemon is shutting down"))
    }
    async fn workspace(&self, id: &str) -> Result<Value> {
        self.app()?
            .projects
            .workspace(id)
            .await?
            .ok_or_else(|| anyhow!("Workspace not found: {id}"))
    }
    pub async fn setup_status(&self, id: &str) -> Result<Option<Value>> {
        self.setup.status(id).await
    }
    pub async fn start_setup(&self, id: &str) -> Result<Value> {
        let _admission = self.inner.admission.lock().await;
        if self.inner.cancel.is_cancelled() {
            bail!("Daemon is shutting down");
        }
        self.setup.start(id).await
    }
    pub async fn stop_setup(&self, id: &str) -> Result<()> {
        self.setup.stop(id).await
    }
    pub async fn teardown(&self, id: &str) -> Result<()> {
        self.setup.teardown(id).await
    }
    pub async fn teardown_path(&self, cwd: &Path, repo_root: Option<&Path>) -> Result<()> {
        self.setup.teardown_path(cwd, repo_root).await
    }

    /// The source's `resolveGitMetadata`: a registered project derives the
    /// service slug from its root and id; a project-less workspace uses the
    /// GitHub repository name of the live snapshot's remote, else its cwd name.
    /// The live snapshot's current branch wins over the persisted workspace branch.
    async fn git_metadata(&self, workspace: &Value) -> Result<(String, Option<String>)> {
        let app = self.app()?;
        let snapshot = match directory(workspace) {
            Ok(cwd) => app.git.cached_snapshot(&cwd).await,
            Err(_) => None,
        };
        let git = snapshot.as_ref().map(|snapshot| &snapshot["git"]);
        let branch = git
            .and_then(|git| git["currentBranch"].as_str())
            .or_else(|| workspace["branch"].as_str())
            .map(str::to_owned);
        if let Some(id) = workspace["projectId"].as_str() {
            if let Some(project) = app.projects.project(id).await? {
                let root = project["rootPath"]
                    .as_str()
                    .ok_or_else(|| anyhow!("Project rootPath missing"))?;
                let slug = format!(
                    "{}-{}",
                    project_slug(root, None),
                    &hex::encode(Sha256::digest(id.as_bytes()))[..8]
                );
                return Ok((slug, branch));
            }
        }
        let remote = git
            .filter(|git| git["isGit"].as_bool() == Some(true))
            .and_then(|git| git["remoteUrl"].as_str());
        let cwd = workspace["cwd"]
            .as_str()
            .ok_or_else(|| anyhow!("Workspace cwd missing"))?;
        Ok((project_slug(cwd, remote), branch))
    }
    fn proxy_config(&self) -> Value {
        self.inner
            .app
            .upgrade()
            .map(|app| app.transport.service_proxy_config())
            .unwrap_or(Value::Null)
    }
    fn projection(&self, slug: &str, branch: Option<&str>, name: &str) -> Result<Value> {
        let settings = self.proxy_config();
        let mut urls = project_service_proxy_urls(
            slug,
            branch,
            name,
            port(&settings["daemonPort"]),
            settings["publicBaseUrl"].as_str(),
        )?;
        urls["hostname"] = format!("{}.localhost", service_proxy_label(slug, branch, name)).into();
        Ok(urls)
    }
    pub async fn list(&self, id: &str) -> Result<Vec<Value>> {
        let workspace = self.workspace(id).await?;
        let (slug, branch) = self.git_metadata(&workspace).await?;
        let cwd = directory(&workspace)?;
        let config = match config::read(&cwd).await {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(%error,workspace_id=id,"Failed to parse paseo.json; treating workspace as having no scripts");
                Value::Null
            }
        };
        let mut scripts = config::scripts(&config);
        let runtimes: HashMap<_, _> = self
            .inner
            .entries
            .lock()
            .unwrap()
            .iter()
            .filter(|((workspace, _), _)| workspace == id)
            .map(|((_, name), runtime)| (name.clone(), runtime.clone()))
            .collect();
        for (name, runtime) in &runtimes {
            if runtime.running && !scripts.iter().any(|(configured, _)| configured == name) {
                scripts.push((
                    name.clone(),
                    config::ScriptConfig {
                        command: String::new(),
                        service: runtime.service,
                        port: None,
                    },
                ));
            }
        }
        let mut result = Vec::with_capacity(scripts.len());
        for (name, script) in scripts {
            let runtime = runtimes.get(&name);
            let mut value = if script.service {
                if let Some(route) = runtime.and_then(|runtime| runtime.route.as_ref()) {
                    registered_projection(route, port(&self.proxy_config()["daemonPort"]))?
                } else {
                    self.projection(&slug, branch.as_deref(), &name)?
                }
            } else {
                json!({"hostname":name,"proxyUrl":null})
            };
            value["scriptName"] = name.into();
            value["type"] = if script.service { "service" } else { "script" }.into();
            value["port"] = if script.service {
                runtime
                    .and_then(|r| r.route.as_ref())
                    .and_then(|r| r["port"].as_f64())
                    .or(script.port)
                    .map(Value::from)
                    .unwrap_or(Value::Null)
            } else {
                Value::Null
            };
            value["lifecycle"] = if runtime.is_some_and(|r| r.running) {
                "running"
            } else {
                "stopped"
            }
            .into();
            value["health"] = if script.service {
                runtime
                    .and_then(|r| r.health)
                    .map(Value::from)
                    .unwrap_or(Value::Null)
            } else {
                Value::Null
            };
            value["exitCode"] = runtime
                .and_then(|r| r.exit_code)
                .map(Value::from)
                .unwrap_or(Value::Null);
            value["terminalId"] = runtime
                .map(|r| Value::from(r.terminal_id.clone()))
                .unwrap_or(Value::Null);
            result.push(value);
        }
        result.sort_by(|a, b| {
            crate::json::numeric_base_locale_compare(
                a["scriptName"].as_str().unwrap(),
                b["scriptName"].as_str().unwrap(),
            )
        });
        Ok(result)
    }
    async fn publish(&self, id: &str) {
        if let (Ok(app), Ok(scripts)) = (self.app(), self.list(id).await) {
            let _=app.transport.publish_event(json!({"type":"script_status_update","payload":{"workspaceId":id,"scripts":scripts}})).await;
        }
    }
    pub async fn launch(&self, id: &str, name: &str) -> Result<Value> {
        let _admission = self.inner.admission.lock().await;
        if self.inner.cancel.is_cancelled() {
            bail!("Daemon is shutting down");
        }
        let workspace = self.workspace(id).await?;
        let app = self.app()?;
        app.projects.assert_automation_allowed(id).await?;
        let cwd = directory(&workspace)?;
        let config = config::read(&cwd).await?;
        let configs = config::scripts(&config);
        let script = configs
            .iter()
            .find(|(configured, _)| configured == name)
            .map(|(_, script)| script)
            .ok_or_else(|| anyhow!("Script '{name}' is not configured in paseo.json"))?;
        let key = (id.to_owned(), name.to_owned());
        let existing = self.inner.entries.lock().unwrap().get(&key).cloned();
        if existing.as_ref().is_some_and(|r| r.running) {
            bail!("Script '{name}' is already running");
        }
        let mut env = BTreeMap::new();
        let mut route = None;
        if script.service {
            let (slug, branch) = self.git_metadata(&workspace).await?;
            let branch = branch.as_deref();
            let settings = self.proxy_config();
            let declarations: Vec<_> = configs
                .iter()
                .filter(|(_, s)| s.service)
                .map(|(name, s)| ServicePortDeclaration {
                    script_name: name.clone(),
                    port: s.port,
                })
                .collect();
            assert_no_service_env_name_collisions(
                &declarations
                    .iter()
                    .map(|service| service.script_name.as_str())
                    .collect::<Vec<_>>(),
            )?;
            let global = app.config.snapshot();
            let allocation = config["worktree"].get("servicePorts").or_else(|| {
                global.pointer("/configReload/startupPersisted/worktrees/servicePorts")
            });
            let mut plan = app
                .projects
                .runtime_ports
                .ensure_plan(id, &cwd, branch, &declarations, allocation)
                .await?;
            if existing.is_some() {
                let declaration = ServicePortDeclaration {
                    script_name: name.to_owned(),
                    port: script.port,
                };
                let fresh = app
                    .projects
                    .runtime_ports
                    .refresh_port(id, &cwd, branch, &declaration, allocation)
                    .await?;
                plan.insert(name.to_owned(), fresh);
            }
            let service_port = *plan.get(name).ok_or_else(|| {
                anyhow!("Service '{name}' is missing from workspace service port plan")
            })?;
            let peers: Vec<_> = plan
                .into_iter()
                .map(|(script_name, port)| ServicePeer { script_name, port })
                .collect();
            env = build_workspace_service_env(
                name,
                &slug,
                branch,
                port(&settings["daemonPort"]),
                settings["daemonListenHost"].as_str(),
                settings["publicBaseUrl"].as_str(),
                &peers,
            )?;
            let projection = self.projection(&slug, branch, name)?;
            let public_hostname = projection["publicProxyUrl"]
                .as_str()
                .and_then(|url| url::Url::parse(url).ok())
                .and_then(|u| u.host_str().map(str::to_owned));
            route = Some(
                json!({"hostname":projection["hostname"],"localHostname":projection["hostname"],"publicHostname":public_hostname,"publicBaseUrl":settings["publicBaseUrl"],"port":service_port,"workspaceId":id,"projectSlug":slug,"scriptName":name}),
            );
            if let Some(incoming) = &route {
                for hostname in [
                    incoming["hostname"].as_str(),
                    incoming["publicHostname"].as_str(),
                ]
                .into_iter()
                .flatten()
                {
                    if let Some(existing) = self.route(hostname) {
                        if existing["workspaceId"] != id || existing["scriptName"] != name {
                            bail!(
                                "Another workspace is already serving \"{name}\" at {hostname}. Stop that service or run this one on a different branch to free the address."
                            );
                        }
                    }
                }
            }
        }
        let reusable = if !script.service {
            if let Some(existing) = &existing {
                app.terminals.get(&existing.terminal_id).await.ok()
            } else {
                None
            }
        } else {
            None
        };
        let reused = reusable.is_some();
        let terminal = match reusable {
            Some(terminal) => terminal,
            None => {
                app.terminals
                    .create(CreateTerminal {
                        cwd,
                        workspace_id: id.to_owned(),
                        name: Some(name.to_owned()),
                        command: None,
                        args: Vec::new(),
                        env,
                        rows: 24,
                        cols: 80,
                    })
                    .await?
            }
        };
        if terminal.workspace_id != id {
            bail!("Terminal does not belong to workspace: {id}");
        }
        let mut events = terminal.subscribe_lifecycle();
        let generation = uuid::Uuid::new_v4();
        let cancel = self.inner.cancel.child_token();
        if let Some(previous) = existing {
            previous.cancel.cancel();
        }
        self.inner.entries.lock().unwrap().insert(
            key.clone(),
            Runtime {
                terminal_id: terminal.id.clone(),
                generation,
                service: script.service,
                running: true,
                exit_code: None,
                route,
                health: None,
                failures: 0,
                registered: Instant::now(),
                cancel: cancel.clone(),
            },
        );
        let this = self.clone();
        let service = script.service;
        let terminal_for_events = terminal.clone();
        let event_key = key.clone();
        tokio::spawn(async move {
            let mut health_tick = tokio::time::interval(Duration::from_secs(3));
            loop {
                tokio::select! {
                    _=cancel.cancelled()=>break,
                    result=terminal_for_events.wait()=>{this.finish(&event_key,generation,result.ok()).await;break;}
                    event=events.recv()=>match event {
                        Ok(TerminalEvent::Exit(code))=>{this.finish(&event_key,generation,Some(code)).await;break;}
                        Ok(TerminalEvent::CommandFinished(code)) if !service=>{this.finish(&event_key,generation,code).await;break;}
                        Err(broadcast::error::RecvError::Closed)=>{let code=terminal_for_events.wait().await.ok();this.finish(&event_key,generation,code).await;break;}
                        Err(broadcast::error::RecvError::Lagged(count))=>{
                            tracing::error!(workspace_id=%event_key.0,script_name=%event_key.1,missed_events=count,"Script lifecycle observation overflowed; terminating owned terminal");
                            if let Ok(app)=this.app(){let _=app.terminals.kill(&terminal_for_events.id).await;}
                            let code=terminal_for_events.wait().await.ok();this.finish(&event_key,generation,code).await;break;
                        }
                        _=>{}
                    },
                    _=health_tick.tick(),if service=>this.probe(&event_key,generation).await,
                }
            }
        });
        if !reused {
            wait_ready(&terminal).await;
        }
        if let Err(error) = terminal
            .input(format!("{}\r", script.command).as_bytes())
            .await
        {
            let _ = app.terminals.kill(&terminal.id).await;
            self.finish(&key, generation, None).await;
            return Err(error);
        }
        self.publish(id).await;
        self.list(id)
            .await?
            .into_iter()
            .find(|v| v["scriptName"] == name)
            .ok_or_else(|| anyhow!("Script '{name}' did not produce a status record"))
    }
    async fn finish(&self, key: &(String, String), generation: uuid::Uuid, exit: Option<i32>) {
        let changed = {
            let mut entries = self.inner.entries.lock().unwrap();
            if let Some(current) = entries
                .get_mut(key)
                .filter(|r| r.generation == generation && r.running)
            {
                current.running = false;
                current.exit_code = exit;
                current.route = None;
                current.health = None;
                current.cancel.cancel();
                true
            } else {
                false
            }
        };
        if changed {
            self.publish(&key.0).await;
        }
    }
    async fn probe(&self, key: &(String, String), generation: uuid::Uuid) {
        let target = self
            .inner
            .entries
            .lock()
            .unwrap()
            .get(key)
            .filter(|r| {
                r.generation == generation
                    && r.running
                    && r.registered.elapsed() >= Duration::from_secs(5)
            })
            .and_then(|r| r.route.as_ref())
            .map(|r| port(&r["port"]));
        let Some(target) = target else { return };
        let healthy = if let Some(port) = target {
            tokio::time::timeout(Duration::from_millis(500), async {
                if tokio::net::TcpStream::connect(("127.0.0.1", port))
                    .await
                    .is_ok()
                {
                    true
                } else {
                    tokio::net::TcpStream::connect(("::1", port)).await.is_ok()
                }
            })
            .await
            .unwrap_or(false)
        } else {
            false
        };
        let changed = {
            let mut entries = self.inner.entries.lock().unwrap();
            entries
                .get_mut(key)
                .filter(|r| r.generation == generation && r.running)
                .is_some_and(|runtime| runtime.record_probe(healthy))
        };
        if changed {
            self.publish(&key.0).await;
        }
    }
    pub async fn stop(&self, id: &str, name: &str) -> Result<Value> {
        let _admission = self.inner.admission.lock().await;
        self.workspace(id).await?;
        let key = (id.to_owned(), name.to_owned());
        let runtime = self
            .inner
            .entries
            .lock()
            .unwrap()
            .get(&key)
            .filter(|r| r.running)
            .cloned()
            .ok_or_else(|| anyhow!("Script '{name}' is not running"))?;
        let app = self.app()?;
        let terminal = app
            .terminals
            .get(&runtime.terminal_id)
            .await
            .map_err(|_| anyhow!("Terminal for script '{name}' is no longer available"))?;
        if terminal.workspace_id != id {
            bail!("Terminal does not belong to workspace: {id}");
        }
        app.terminals.kill(&runtime.terminal_id).await?;
        let exit = terminal.wait().await.ok();
        self.finish(&key, runtime.generation, exit).await;
        self.list(id)
            .await?
            .into_iter()
            .find(|v| v["scriptName"] == name)
            .ok_or_else(|| anyhow!("Script '{name}' did not produce a status record"))
    }
    pub async fn stop_workspace(&self, id: &str) -> Result<()> {
        self.stop_setup(id).await?;
        let names: Vec<_> = self
            .inner
            .entries
            .lock()
            .unwrap()
            .iter()
            .filter(|((workspace, _), runtime)| workspace == id && runtime.running)
            .map(|((_, name), _)| name.clone())
            .collect();
        for name in names {
            self.stop(id, &name).await?;
        }
        Ok(())
    }
    pub fn remove_workspace_runtime(&self, id: &str) {
        self.inner
            .entries
            .lock()
            .unwrap()
            .retain(|(workspace, _), runtime| {
                if workspace == id {
                    runtime.cancel.cancel();
                    false
                } else {
                    true
                }
            });
    }
    pub async fn shutdown(&self) -> Result<()> {
        self.inner.cancel.cancel();
        let _admission = self.inner.admission.lock().await;
        self.setup.shutdown().await?;
        let app = self.app()?;
        let terminals: Vec<_> = self
            .inner
            .entries
            .lock()
            .unwrap()
            .values()
            .filter(|r| r.running)
            .map(|r| r.terminal_id.clone())
            .collect();
        let mut error = None;
        for terminal in terminals {
            if let Err(e) = app.terminals.kill(&terminal).await {
                error.get_or_insert(e);
            }
        }
        self.inner.entries.lock().unwrap().clear();
        if let Some(error) = error {
            return Err(error);
        }
        Ok(())
    }
    /// The source's `replaceWorkspaceBranchRoutes`: re-derive each live route's
    /// hostnames from its own registered project slug and public base URL for the
    /// new branch. Unchanged routes are a no-op (no health reset, no publish).
    pub async fn refresh_routes(&self, id: &str) -> Result<()> {
        let _admission = self.inner.admission.lock().await;
        let workspace = self.workspace(id).await?;
        let branch = self.git_metadata(&workspace).await?.1;
        let branch = branch.as_deref();
        let settings = self.proxy_config();
        {
            let mut entries = self.inner.entries.lock().unwrap();
            let mut updates = Vec::new();
            let mut changed = false;
            for ((workspace_id, name), runtime) in
                entries.iter().filter(|((workspace_id, _), runtime)| {
                    workspace_id == id && runtime.running && runtime.service
                })
            {
                let Some(mut route) = runtime.route.clone() else {
                    continue;
                };
                let slug = route["projectSlug"].as_str().unwrap_or_default().to_owned();
                let public_base = route["publicBaseUrl"].as_str().map(str::to_owned);
                let hostname = format!("{}.localhost", service_proxy_label(&slug, branch, name));
                let urls = project_service_proxy_urls(
                    &slug,
                    branch,
                    name,
                    port(&settings["daemonPort"]),
                    public_base.as_deref(),
                )?;
                let public_hostname = urls["publicProxyUrl"]
                    .as_str()
                    .and_then(|value| url::Url::parse(value).ok())
                    .and_then(|url| url.host_str().map(str::to_owned));
                let public_hostname = public_hostname.map(Value::from).unwrap_or(Value::Null);
                changed |= route["hostname"].as_str() != Some(hostname.as_str())
                    || route["publicHostname"] != public_hostname;
                route["hostname"] = hostname.clone().into();
                route["localHostname"] = hostname.into();
                route["publicHostname"] = public_hostname;
                updates.push(((workspace_id.clone(), name.clone()), route));
            }
            if !changed {
                return Ok(());
            }
            for (index, ((_, name), route)) in updates.iter().enumerate() {
                for alias in [route["hostname"].as_str(), route["publicHostname"].as_str()]
                    .into_iter()
                    .flatten()
                {
                    let conflict = entries
                        .iter()
                        .filter(|((other, _), runtime)| other != id && runtime.running)
                        .filter_map(|(_, runtime)| runtime.route.as_ref())
                        .any(|other| {
                            other["hostname"].as_str() == Some(alias)
                                || other["publicHostname"].as_str() == Some(alias)
                        });
                    let internal = updates[..index].iter().any(|(_, other)| {
                        other["hostname"].as_str() == Some(alias)
                            || other["publicHostname"].as_str() == Some(alias)
                    });
                    if conflict || internal {
                        bail!(
                            "Another workspace is already serving \"{name}\" at {alias}. Stop that service or run this one on a different branch to free the address."
                        );
                    }
                }
            }
            for (key, route) in updates {
                let runtime = entries.get_mut(&key).unwrap();
                runtime.route = Some(route);
                runtime.health = None;
                runtime.failures = 0;
                runtime.registered = Instant::now();
            }
        }
        self.publish(id).await;
        Ok(())
    }
    pub fn route(&self, host: &str) -> Option<Value> {
        let host = normalize_host(host);
        self.inner
            .entries
            .lock()
            .unwrap()
            .values()
            .filter(|r| r.running)
            .filter_map(|r| r.route.as_ref())
            .find(|r| {
                r["hostname"].as_str() == Some(&host) || r["publicHostname"].as_str() == Some(&host)
            })
            .cloned()
    }
    pub fn classify_service_host(&self, host: &str) -> Value {
        if let Some(route) = self.route(host) {
            return json!({"type":"registered-service","route":route});
        }
        let host = normalize_host(host);
        if host.ends_with(".localhost") && host.split('.').next().is_some_and(|s| s.contains("--"))
        {
            return json!({"type":"known-service-miss"});
        }
        let settings = self.proxy_config();
        let configured = settings["publicBaseUrl"]
            .as_str()
            .is_some_and(|base| is_public_namespace(&host, base));
        let registered = self
            .inner
            .entries
            .lock()
            .unwrap()
            .values()
            .filter_map(|runtime| runtime.route.as_ref())
            .filter_map(|route| route["publicBaseUrl"].as_str())
            .any(|base| is_public_namespace(&host, base));
        if configured || registered {
            json!({"type":"known-service-miss"})
        } else {
            json!({"type":"daemon"})
        }
    }
    pub async fn handle(&self, ctx: &RequestContext, message: &Value) -> Result<bool> {
        let kind = message["type"].as_str().unwrap_or("");
        if !matches!(
            kind,
            "workspace_setup_status_request"
                | "workspace.setup.subscribe.request"
                | "workspace.setup.run.request"
                | "start_workspace_script_request"
                | "workspace.script.list.request"
                | "workspace.script.start.request"
                | "workspace.script.stop.request"
        ) {
            return Ok(false);
        }
        let id = message["workspaceId"]
            .as_str()
            .ok_or_else(|| anyhow!("workspaceId is required"))?;
        let request = message["requestId"].clone();
        match kind {
            "workspace_setup_status_request"=>ctx.send(json!({"type":"workspace_setup_status_response","payload":{"requestId":request,"workspaceId":id,"snapshot":self.setup_status(id).await?}})).await?,
            "workspace.setup.subscribe.request"=>{
                if !ctx.is_modern()||!crate::transport::has_capability(ctx.capabilities(),"workspace_setup_observation"){bail!("Workspace setup observation requires owned subscriptions");}
                let owner=ctx.begin_subscription("workspace-setup",None,None)?;
                let initial=match self.setup_status(id).await{Ok(value)=>value,Err(error)=>{owner.release().await?;return Err(error);}};
                let mut events=self.setup.subscribe();
                let snapshot=self.setup.cached(id).or(initial);
                if let Err(error)=ctx.send(json!({"type":"workspace.setup.subscribe.response","payload":{"requestId":request,"subscriptionId":owner.response_id(),"workspaceId":id,"snapshot":snapshot}})).await{owner.release().await?;return Err(error);}
                owner.activate().await?;let this=self.clone();let id=id.to_owned();
                tokio::spawn(async move {let cancel=owner.cancellation();loop{let value=tokio::select!{_=cancel.cancelled()=>break,event=events.recv()=>match event{Ok((workspace,snapshot))if workspace==id=>Some(snapshot),Ok(_)=>None,Err(broadcast::error::RecvError::Lagged(_))=>this.setup.cached(&id),Err(_)=>break}};if let Some(mut payload)=value{payload["workspaceId"]=id.clone().into();if owner.emit(json!({"type":"workspace_setup_progress","payload":payload})).await.is_err(){break;}}}let _=owner.release().await;});
            }
            "workspace.setup.run.request"=>{
                let result=async{let workspace=self.workspace(id).await?;if !workspace["archivedAt"].is_null(){bail!("Workspace not found: {id}");}let started=self.app()?.projects.clear_automation_block(id).await?;if started{self.start_setup(id).await?;}Ok::<_,anyhow::Error>(started)}.await;
                let (started,error)=match result{Ok(started)=>(started,None),Err(error)=>(false,Some(error.to_string()))};
                ctx.send(json!({"type":"workspace.setup.run.response","payload":{"requestId":request,"workspaceId":id,"started":started,"error":error}})).await?;
            }
            _=>{
                let name=message["scriptName"].as_str().unwrap_or("");let mut payload=json!({"requestId":request,"workspaceId":id,"error":null});
                let response=if kind=="workspace.script.list.request"{"workspace.script.list.response"}else if kind=="workspace.script.stop.request"{"workspace.script.stop.response"}else if kind=="start_workspace_script_request"{"start_workspace_script_response"}else{"workspace.script.start.response"};
                if kind=="workspace.script.list.request"{match self.list(id).await{Ok(scripts)=>payload["scripts"]=scripts.into(),Err(error)=>{payload["scripts"]=json!([]);payload["error"]=error.to_string().into();}}}else{
                    payload["scriptName"]=name.into();let result=if kind=="workspace.script.stop.request"{self.stop(id,name).await}else{self.launch(id,name).await};
                    let legacy=kind=="start_workspace_script_request";let field=if legacy{"terminalId"}else{"script"};
                    match result{Ok(script)=>payload[field]=if legacy{script["terminalId"].clone()}else{script},Err(error)=>{payload[field]=Value::Null;payload["error"]=error.to_string().into();}}
                }
                ctx.send(json!({"type":response,"payload":payload})).await?;
            }
        }
        Ok(true)
    }
}
pub(crate) async fn wait_ready(terminal: &Arc<Terminal>) {
    let mut events = terminal.subscribe();
    if let Ok((lines, _)) = terminal.capture(None, None, true).await {
        if lines.iter().any(|line| !line.trim().is_empty()) {
            return;
        }
    }
    let _ = tokio::time::timeout(Duration::from_millis(1500), async {
        loop {
            match events.recv().await {
                Ok(TerminalEvent::Output { .. }) | Ok(TerminalEvent::Exit(_)) | Err(_) => break,
                _ => {}
            }
        }
    })
    .await;
}
fn directory(workspace: &Value) -> Result<PathBuf> {
    Ok(PathBuf::from(
        workspace["cwd"]
            .as_str()
            .ok_or_else(|| anyhow!("Workspace cwd missing"))?,
    ))
}
fn port(value: &Value) -> Option<u16> {
    value
        .as_f64()
        .filter(|n| n.is_finite() && n.fract() == 0.0 && (1.0..=65535.0).contains(n))
        .map(|n| n as u16)
}
fn normalize_host(host: &str) -> String {
    let host = host.trim().to_lowercase();
    match host.rsplit_once(':') {
        Some((name, p)) if !p.is_empty() && p.bytes().all(|c| c.is_ascii_digit()) => {
            name.to_owned()
        }
        _ => host,
    }
}
fn is_public_namespace(host: &str, base_url: &str) -> bool {
    let Ok(url) = url::Url::parse(base_url) else {
        return false;
    };
    let Some(base) = url.host_str() else {
        return false;
    };
    host == base
        || host
            .strip_suffix(base)
            .is_some_and(|prefix| prefix.ends_with('.'))
}
fn registered_projection(route: &Value, daemon_port: Option<u16>) -> Result<Value> {
    let hostname = route["hostname"]
        .as_str()
        .ok_or_else(|| anyhow!("Service route hostname is missing"))?;
    let local = daemon_port.map(|port| format!("http://{hostname}:{port}"));
    let public = match (
        route["publicHostname"].as_str(),
        route["publicBaseUrl"].as_str(),
    ) {
        (Some(host), Some(base)) => {
            let base = url::Url::parse(base)?;
            let port = base
                .port()
                .map(|port| format!(":{port}"))
                .unwrap_or_default();
            Some(format!("{}://{host}{port}", base.scheme()))
        }
        _ => None,
    };
    let proxy = public.as_ref().or(local.as_ref());
    Ok(json!({"hostname":hostname,"localProxyUrl":local,"publicProxyUrl":public,"proxyUrl":proxy}))
}
/// The source's `deriveProjectSlug(cwd, remoteUrl)`.
fn project_slug(root: &str, remote: Option<&str>) -> String {
    let repo = remote.and_then(crate::projects::store::github_repo_name);
    let name = repo.as_deref().unwrap_or_else(|| {
        Path::new(root)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
    });
    let mut slug = String::new();
    let mut separator = false;
    for c in name.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            if separator && !slug.is_empty() {
                slug.push('-');
            }
            slug.push(c);
            separator = false;
        } else {
            separator = true;
        }
    }
    if slug.len() > 50 {
        slug.truncate(50);
        if let Some(hyphen) = slug.rfind('-').filter(|index| *index > 25) {
            slug.truncate(hyphen);
        }
        while slug.ends_with('-') {
            slug.pop();
        }
    }
    if slug.is_empty() {
        "untitled".into()
    } else {
        slug
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn host_normalization_preserves_identity() {
        assert_eq!(
            normalize_host(" WEB--APP.localhost:4000 "),
            "web--app.localhost"
        );
    }
    #[test]
    fn natural_script_order_is_numeric() {
        assert!(crate::json::numeric_base_locale_compare("build2", "Build10").is_lt());
    }
    #[test]
    fn projectless_slug_prefers_github_repo_name_over_cwd() {
        assert_eq!(
            project_slug("/work/checkout", Some("git@github.com:Acme/My_Repo.git")),
            "my-repo"
        );
        assert_eq!(
            project_slug("/work/checkout", Some("https://gitlab.com/acme/other.git")),
            "checkout"
        );
        assert_eq!(
            project_slug("/work/checkout", Some("https://github.com/a/b/c")),
            "checkout"
        );
        assert_eq!(project_slug("/work/!!!", None), "untitled");
    }
    #[tokio::test]
    async fn delayed_completion_cannot_stop_reused_terminal() {
        let scripts = Scripts::new(Weak::new());
        let old = uuid::Uuid::from_u128(1);
        let current = uuid::Uuid::from_u128(2);
        let key = ("workspace-b".to_owned(), "build".to_owned());
        scripts.inner.entries.lock().unwrap().insert(
            key.clone(),
            Runtime {
                terminal_id: "same-terminal".into(),
                generation: current,
                service: false,
                running: true,
                exit_code: None,
                route: None,
                health: None,
                failures: 0,
                registered: Instant::now(),
                cancel: CancellationToken::new(),
            },
        );
        scripts.finish(&key, old, Some(1)).await;
        assert!(scripts.inner.entries.lock().unwrap()[&key].running);
        scripts.finish(&key, current, None).await;
        let entries = scripts.inner.entries.lock().unwrap();
        assert!(!entries[&key].running);
        assert_eq!(entries[&key].exit_code, None);
    }
    #[test]
    fn unregistered_service_host_never_routes_to_daemon() {
        let scripts = Scripts::new(Weak::new());
        assert_eq!(
            scripts.classify_service_host("web--feature--repo.localhost:1234")["type"],
            "known-service-miss"
        );
        assert_eq!(
            scripts.classify_service_host("localhost:1234")["type"],
            "daemon"
        );
    }
    #[test]
    fn public_service_namespace_includes_base_and_only_true_subdomains() {
        assert!(is_public_namespace(
            "preview.example.test",
            "https://preview.example.test"
        ));
        assert!(is_public_namespace(
            "web--repo.preview.example.test",
            "https://preview.example.test"
        ));
        assert!(!is_public_namespace(
            "notpreview.example.test",
            "https://preview.example.test"
        ));
        assert!(!is_public_namespace(
            "preview.example.test.attacker.test",
            "https://preview.example.test"
        ));
    }
    #[test]
    fn service_health_requires_two_failures_and_recovers_immediately() {
        let mut runtime = Runtime {
            terminal_id: "terminal".into(),
            generation: uuid::Uuid::nil(),
            service: true,
            running: true,
            exit_code: None,
            route: None,
            health: None,
            failures: 0,
            registered: Instant::now(),
            cancel: CancellationToken::new(),
        };
        assert!(!runtime.record_probe(false));
        assert_eq!(runtime.health, None);
        assert!(runtime.record_probe(false));
        assert_eq!(runtime.health, Some("unhealthy"));
        assert!(runtime.record_probe(true));
        assert_eq!(runtime.health, Some("healthy"));
        assert!(!runtime.record_probe(false));
        assert_eq!(runtime.health, Some("healthy"));
        assert!(runtime.record_probe(false));
        assert_eq!(runtime.health, Some("unhealthy"));
    }
}
