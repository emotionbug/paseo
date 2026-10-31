//! Native forge adapters. Commands remain injectable; no daemon/transport state is retained.
use crate::process::{CommandOutput, CommandSpec};
use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    hash::{Hash, Hasher},
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, OnceCell};
use tokio_util::sync::CancellationToken;

mod common;
mod gitea;
mod github;
mod gitlab;
mod handlers;
mod lookup;
mod poll;
use common::*;
pub use common::{
    build_pr_local_branch_name, default_checkout_refs, supports_cross_repo_checkout_without_refs,
};
pub use handlers::normalize_clone_repository;

pub struct HttpRequest {
    pub method: reqwest::Method,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}
pub struct HttpResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

/// Offline fixtures implement this boundary without spawning tools or contacting hosts.
#[async_trait]
pub trait ForgeIo: Send + Sync {
    async fn command(
        &self,
        spec: CommandSpec,
        limit: usize,
        timeout: Duration,
        cancel: CancellationToken,
    ) -> Result<CommandOutput>;
    async fn http(
        &self,
        request: HttpRequest,
        limit: usize,
        timeout: Duration,
        cancel: CancellationToken,
    ) -> Result<HttpResponse>;
}

pub struct NativeForgeIo;
#[async_trait]
impl ForgeIo for NativeForgeIo {
    async fn command(
        &self,
        spec: CommandSpec,
        limit: usize,
        timeout: Duration,
        cancel: CancellationToken,
    ) -> Result<CommandOutput> {
        if spec.program == "git" {
            let cwd = spec
                .cwd
                .as_deref()
                .ok_or_else(|| anyhow!("Forge git command requires cwd"))?;
            return crate::git::command::run(cwd, &spec.args, &spec.env, cancel, limit, timeout)
                .await;
        }
        crate::process::run(spec, limit, timeout, cancel).await
    }
    async fn http(
        &self,
        request: HttpRequest,
        limit: usize,
        timeout: Duration,
        cancel: CancellationToken,
    ) -> Result<HttpResponse> {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let mut builder = client
            .request(request.method, &request.url)
            .body(request.body);
        for (name, value) in request.headers {
            builder = builder.header(name, value);
        }
        let work = async {
            let mut response = builder.send().await?;
            let status = response.status().as_u16();
            let headers = response
                .headers()
                .iter()
                .filter_map(|(k, v)| v.to_str().ok().map(|v| (k.to_string(), v.to_owned())))
                .collect();
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await? {
                if chunk.len() > limit.saturating_sub(body.len()) {
                    bail!("Forge HTTP response exceeded output limit");
                }
                body.extend_from_slice(&chunk);
            }
            Ok(HttpResponse {
                status,
                headers,
                body,
            })
        };
        tokio::select! { result = work => result, _ = cancel.cancelled() => Err(anyhow!("Forge request cancelled")) }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct ForgeError {
    pub auth_state: &'static str,
    pub message: String,
    pub stderr: String,
    pub stdout: String,
    pub exit_code: Option<i32>,
}
pub fn auth_state(error: &anyhow::Error) -> &'static str {
    error
        .downcast_ref::<ForgeError>()
        .map_or("error", |e| e.auth_state)
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForgeResolution {
    pub forge: String,
    pub host: String,
    pub project_path: String,
}
struct Cached<T> {
    born: Instant,
    cell: Arc<OnceCell<T>>,
    used: u64,
}
struct Cache<T> {
    entries: HashMap<String, Cached<T>>,
    clock: u64,
    capacity: usize,
}
impl<T> Default for Cache<T> {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            clock: 0,
            capacity: 512,
        }
    }
}
impl<T> Cache<T> {
    fn cell(&mut self, key: String, ttl: impl Fn(&T) -> Option<Duration>) -> Arc<OnceCell<T>> {
        self.clock = self.clock.wrapping_add(1);
        if let Some(entry) = self.entries.get_mut(&key) {
            if entry
                .cell
                .get()
                .is_none_or(|v| ttl(v).is_none_or(|ttl| entry.born.elapsed() < ttl))
            {
                entry.used = self.clock;
                return entry.cell.clone();
            }
        }
        if self.entries.len() >= self.capacity {
            if let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.used)
                .map(|(k, _)| k.clone())
            {
                self.entries.remove(&oldest);
            }
        }
        let cell = Arc::new(OnceCell::new());
        self.entries.insert(
            key,
            Cached {
                born: Instant::now(),
                cell: cell.clone(),
                used: self.clock,
            },
        );
        cell
    }
}

pub struct ForgeEngine {
    io: Arc<dyn ForgeIo>,
    remotes: Mutex<Cache<Option<Remote>>>,
    hosts: Mutex<Cache<Option<String>>>,
    ssh_hosts: Mutex<Cache<Option<String>>>,
    redirects: Mutex<Cache<Value>>,
    reads: Mutex<Cache<Value>>,
    frozen: Mutex<Cache<Value>>,
    logs: Mutex<Cache<Value>>,
    poll_paused: Mutex<HashMap<String, i64>>,
}
impl Default for ForgeEngine {
    fn default() -> Self {
        Self::new()
    }
}
impl ForgeEngine {
    pub fn new() -> Self {
        Self::with_io(Arc::new(NativeForgeIo))
    }
    pub fn with_io(io: Arc<dyn ForgeIo>) -> Self {
        Self {
            io,
            remotes: Mutex::new(Cache::default()),
            hosts: Mutex::new(Cache::default()),
            ssh_hosts: Mutex::new(Cache::default()),
            redirects: Mutex::new(Cache::default()),
            reads: Mutex::new(Cache::default()),
            frozen: Mutex::new(Cache::default()),
            logs: Mutex::new(Cache {
                entries: HashMap::new(),
                clock: 0,
                capacity: 128,
            }),
            poll_paused: Mutex::new(HashMap::new()),
        }
    }
    pub async fn invalidate(&self, cwd: &Path) {
        let prefix = format!("{}\0", cwd.display());
        self.remotes
            .lock()
            .await
            .entries
            .retain(|k, _| !k.starts_with(&prefix));
        self.reads
            .lock()
            .await
            .entries
            .retain(|k, _| !k.starts_with(&prefix));
    }
    pub async fn resolve(
        &self,
        cwd: &Path,
        env: &BTreeMap<String, String>,
        cancel: CancellationToken,
    ) -> Result<Option<ForgeResolution>> {
        let Some(remote) = self.origin(cwd, env, &cancel).await? else {
            return Ok(None);
        };
        self.resolve_remote(cwd, &remote, env, &cancel).await
    }
    /// `defaultResolveRemoteUrl` memoized per cwd for 60 seconds (`remoteUrlByCwd`).
    async fn origin(
        &self,
        cwd: &Path,
        env: &BTreeMap<String, String>,
        cancel: &CancellationToken,
    ) -> Result<Option<Remote>> {
        let cell = self
            .remotes
            .lock()
            .await
            .cell(cache_prefix(cwd, env), |_| Some(Duration::from_secs(60)));
        cell.get_or_try_init(|| async {
            let result = self
                .raw(
                    cwd,
                    env,
                    cancel,
                    "git",
                    strings(&["config", "--get", "remote.origin.url"]),
                    5,
                )
                .await;
            if cancel.is_cancelled() {
                bail!("Forge request cancelled");
            }
            Ok::<_, anyhow::Error>(
                result
                    .ok()
                    .filter(|r| r.status == 0)
                    .and_then(|r| parse_remote(&String::from_utf8_lossy(&r.stdout))),
            )
        })
        .await
        .cloned()
    }
    /// Host of an origin remote that no registered forge claimed
    /// (`buildUnresolvedRemoteForgeSnapshot`).
    pub(super) async fn origin_host(
        &self,
        cwd: &Path,
        env: &BTreeMap<String, String>,
        cancel: &CancellationToken,
    ) -> Result<Option<String>> {
        Ok(self.origin(cwd, env, cancel).await?.map(|remote| remote.host))
    }
    async fn resolve_remote(
        &self,
        cwd: &Path,
        remote: &Remote,
        env: &BTreeMap<String, String>,
        cancel: &CancellationToken,
    ) -> Result<Option<ForgeResolution>> {
        let mut host = remote.host.clone();
        let mut forge = cloud_forge(&host).map(str::to_owned);
        if forge.is_none() && remote.ssh {
            if let Some(resolved) = self.ssh_hostname(cwd, env, cancel, &host).await? {
                host = resolved;
            }
            forge = cloud_forge(&host).map(str::to_owned);
        }
        if forge.is_none() {
            let key = format!("{}:{}", environment_hash(env), host);
            let cell = self.hosts.lock().await.cell(key, |v| {
                if v.is_some() {
                    None
                } else {
                    Some(Duration::from_secs(60))
                }
            });
            forge = cell
                .get_or_try_init(|| async {
                    let gh = self.raw(
                        cwd,
                        env,
                        cancel,
                        "gh",
                        strings(&["auth", "status", "--hostname", &host]),
                        10,
                    );
                    let glab = self.raw(
                        cwd,
                        env,
                        cancel,
                        "glab",
                        strings(&["auth", "status", "--hostname", &host]),
                        10,
                    );
                    let tea = self.raw(
                        cwd,
                        env,
                        cancel,
                        "tea",
                        strings(&["login", "list", "-o", "json"]),
                        10,
                    );
                    let (gh, glab, tea) = tokio::join!(gh, glab, tea);
                    if cancel.is_cancelled() {
                        bail!("Forge request cancelled");
                    }
                    let mut matches = Vec::new();
                    if gh.is_ok_and(|v| v.status == 0) {
                        matches.push("github".to_owned());
                    }
                    if glab.is_ok_and(|v| v.status == 0) {
                        matches.push("gitlab".to_owned());
                    }
                    if let Ok(tea) = tea {
                        if tea.status == 0 {
                            if let Some(login) = serde_json::from_slice::<Value>(&tea.stdout)
                                .ok()
                                .and_then(|v| tea_login(&v, &host).map(str::to_owned))
                            {
                                let probe = self
                                    .raw(
                                        cwd,
                                        env,
                                        cancel,
                                        "tea",
                                        strings(&[
                                            "api",
                                            "--login",
                                            &login,
                                            "-i",
                                            "/api/forgejo/v1/version",
                                        ]),
                                        5,
                                    )
                                    .await;
                                // `tea api` exits 0 regardless of HTTP status; the first status
                                // line on stderr (else stdout) decides, and anything but 2xx
                                // keeps the Gitea default.
                                let is_forgejo = probe
                                    .ok()
                                    .filter(|v| v.status == 0)
                                    .and_then(|v| {
                                        http_status(&v.stderr).or_else(|| {
                                            http_status(&String::from_utf8_lossy(&v.stdout))
                                        })
                                    })
                                    .is_some_and(|s| (200..300).contains(&s));
                                matches
                                    .push(if is_forgejo { "forgejo" } else { "gitea" }.to_owned());
                            }
                        }
                    }
                    Ok::<_, anyhow::Error>(if matches.len() == 1 {
                        matches.pop()
                    } else {
                        None
                    })
                })
                .await?
                .clone();
        }
        if cancel.is_cancelled() {
            bail!("Forge request cancelled");
        }
        Ok(forge.map(|forge| ForgeResolution {
            forge,
            host,
            project_path: remote.path.clone(),
        }))
    }
    async fn raw(
        &self,
        cwd: &Path,
        env: &BTreeMap<String, String>,
        cancel: &CancellationToken,
        binary: &str,
        args: Vec<String>,
        seconds: u64,
    ) -> Result<CommandOutput> {
        let mut env = env.clone();
        env.insert("GIT_TERMINAL_PROMPT".into(), "0".into());
        if binary == "glab" {
            env.insert("GLAB_CHECK_UPDATE".into(), "0".into());
        }
        // Host probes run `gh auth status --hostname` with prompts disabled.
        if binary == "gh" && args.first().is_some_and(|v| v == "auth") {
            env.insert("GH_PROMPT_DISABLED".into(), "1".into());
        }
        self.io
            .command(
                CommandSpec {
                    program: binary.to_owned(),
                    args,
                    cwd: Some(cwd.to_path_buf()),
                    env,
                    clear_env: true,
                },
                10 * 1024 * 1024,
                Duration::from_secs(seconds),
                cancel.clone(),
            )
            .await
    }
    pub async fn execute(
        &self,
        cwd: &Path,
        operation: &str,
        input: &Value,
        env: &BTreeMap<String, String>,
        cancel: CancellationToken,
    ) -> Result<Value> {
        if b(input, "force") && s(input, "reason").is_empty() {
            bail!("ForgeService forced read requires a reason");
        }
        let resolution = if s(input, "forge") == "github" {
            match self.github_resolution(cwd, env, &cancel).await {
                Ok(resolution) => resolution,
                // The legacy search runs on the GitHub adapter, whose list reads both
                // reject on the routing failure: auth/missing-CLI rejections become an
                // unavailable result and any other rejection is dropped from the items.
                Err(error) if operation == "searchIssuesAndPrs" && !cancel.is_cancelled() => {
                    let state = auth_state(&error);
                    let requested = search_kinds(input);
                    return Ok(
                        if !requested.is_empty()
                            && matches!(state, "cli_missing" | "unauthenticated")
                        {
                            unavailable(state)
                        } else {
                            json!({"items":[],"featuresEnabled":true,"authState":"authenticated","githubFeaturesEnabled":true})
                        },
                    );
                }
                Err(error) => return Err(error),
            }
        } else {
            match self.resolve(cwd, env, cancel.clone()).await? {
                Some(r) => r,
                None if operation == "searchIssuesAndPrs" => return Ok(unavailable("no_remote")),
                None if operation == "isAuthenticated" => return Ok(Value::Bool(false)),
                None => {
                    return Err(ForgeError {
                        auth_state: "no_remote",
                        message: format!(
                            "No supported forge remote is configured for {}",
                            cwd.display()
                        ),
                        stderr: String::new(),
                        stdout: String::new(),
                        exit_code: None,
                    }
                    .into());
                }
            }
        };
        let context = Context {
            engine: self,
            cwd,
            env,
            cancel: &cancel,
            resolution: &resolution,
        };
        let mutation = matches!(
            operation,
            "createPullRequest"
                | "mergePullRequest"
                | "enablePullRequestAutoMerge"
                | "disablePullRequestAutoMerge"
        );
        // `searchRepositories` is the one GitHub read the source never caches.
        let result = if resolution.forge == "github" && !mutation && operation != "searchRepositories"
        {
            let key = read_key(cwd, env, operation, input);
            let cell = {
                let mut reads = self.reads.lock().await;
                if b(input, "force") {
                    reads.entries.remove(&key);
                }
                reads.cell(key, |_| Some(Duration::from_secs(30)))
            };
            cell.get_or_try_init(|| context.dispatch(operation, input))
                .await
                .cloned()
        } else {
            context.dispatch(operation, input).await
        };
        if mutation {
            self.invalidate(cwd).await;
        }
        if cancel.is_cancelled() {
            bail!("Forge request cancelled");
        }
        result
    }
    async fn github_resolution(
        &self,
        cwd: &Path,
        env: &BTreeMap<String, String>,
        cancel: &CancellationToken,
    ) -> Result<ForgeResolution> {
        let remote = self.origin(cwd, env, cancel).await?;
        let mut resolution = ForgeResolution {
            forge: "github".into(),
            host: "github.com".into(),
            project_path: String::new(),
        };
        if let Some(remote) = remote {
            resolution.host = remote.host.clone();
            resolution.project_path = remote.path.clone();
            if remote.ssh && cloud_forge(&resolution.host) != Some("github") {
                if let Some(host) = self
                    .ssh_hostname(cwd, env, cancel, &resolution.host)
                    .await?
                {
                    resolution.host = host;
                }
            }
            if cloud_forge(&resolution.host) != Some("github") {
                // `resolveGitHubEnterpriseHost`: a verified Enterprise host is cached for the
                // daemon's lifetime; an unauthenticated one must never fall back to github.com.
                let host = resolution.host.clone();
                let cell = self.hosts.lock().await.cell(
                    format!("{}:gh-routing:{host}", environment_hash(env)),
                    |_| None,
                );
                cell.get_or_try_init(|| async {
                    let context = Context {
                        engine: self,
                        cwd,
                        env,
                        cancel,
                        resolution: &resolution,
                    };
                    match context
                        .run(strings(&["auth", "status", "--hostname", &host]))
                        .await
                    {
                        Ok(_) => Ok::<_, anyhow::Error>(Some(host.clone())),
                        Err(error) if auth_state(&error) == "unauthenticated" => {
                            Err(ForgeError {
                                auth_state: "unauthenticated",
                                message: "GitHub CLI authentication failed".into(),
                                stderr: format!("GitHub CLI is not authenticated for host {host}"),
                                stdout: String::new(),
                                exit_code: None,
                            }
                            .into())
                        }
                        Err(error) if auth_state(&error) == "cli_missing" => Err(error),
                        Err(_) if cancel.is_cancelled() => bail!("Forge request cancelled"),
                        Err(_) => bail!("Unable to verify GitHub Enterprise host {host}"),
                    }
                })
                .await?;
            }
        }
        Ok(resolution)
    }
    async fn ssh_hostname(
        &self,
        cwd: &Path,
        env: &BTreeMap<String, String>,
        cancel: &CancellationToken,
        host: &str,
    ) -> Result<Option<String>> {
        let cell = self
            .ssh_hosts
            .lock()
            .await
            .cell(format!("{}:{host}", environment_hash(env)), |_| None);
        cell.get_or_try_init(|| async {
            let result = self
                .raw(cwd, env, cancel, "ssh", strings(&["-G", host]), 5)
                .await;
            if cancel.is_cancelled() {
                bail!("Forge request cancelled");
            }
            let hostname = result.ok().filter(|v| v.status == 0).and_then(|v| {
                String::from_utf8_lossy(&v.stdout).lines().find_map(|line| {
                    let mut fields = line.split_whitespace();
                    if fields
                        .next()
                        .is_some_and(|v| v.eq_ignore_ascii_case("hostname"))
                    {
                        fields.next().filter(|v| valid_host(v)).map(normalize_host)
                    } else {
                        None
                    }
                })
            });
            Ok::<_, anyhow::Error>(hostname)
        })
        .await
        .cloned()
    }
}
struct Context<'a> {
    engine: &'a ForgeEngine,
    cwd: &'a Path,
    env: &'a BTreeMap<String, String>,
    cancel: &'a CancellationToken,
    resolution: &'a ForgeResolution,
}
impl Context<'_> {
    fn binary(&self) -> &'static str {
        match self.resolution.forge.as_str() {
            "github" => "gh",
            "gitlab" => "glab",
            _ => "tea",
        }
    }
    fn brand(&self) -> &'static str {
        match self.binary() {
            "gh" => "GitHub",
            "glab" => "GitLab",
            _ => "Gitea",
        }
    }
    async fn run(&self, args: Vec<String>) -> Result<String> {
        let binary = self.binary();
        let mut env = self.env.clone();
        if binary == "gh" {
            // GH_HOST only routes Enterprise hosts; github.com (including a resolved
            // SSH alias) is gh's default and is never forced.
            if cloud_forge(&self.resolution.host) != Some("github") {
                env.insert("GH_HOST".into(), self.resolution.host.clone());
            }
            if args.get(1).is_some_and(|v| v == "merge") {
                env.insert("GH_PROMPT_DISABLED".into(), "1".into());
            }
        }
        let result = self
            .engine
            .raw(self.cwd, &env, self.cancel, binary, args.clone(), 30)
            .await;
        let output = match result {
            Ok(v) => v,
            Err(error) => {
                if error.chain().any(|e| {
                    e.downcast_ref::<std::io::Error>()
                        .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
                }) {
                    return Err(ForgeError {
                        auth_state: "cli_missing",
                        message: format!(
                            "{} CLI ({binary}) is not installed or not in PATH",
                            self.brand()
                        ),
                        stderr: String::new(),
                        stdout: String::new(),
                        exit_code: None,
                    }
                    .into());
                }
                if error.downcast_ref::<ForgeError>().is_some() {
                    return Err(error);
                }
                let message = error.to_string();
                if auth_failure(binary, &message) {
                    return Err(self.command_error(&args, message, String::new(), None));
                }
                // A killed process (timeout or output cap) reports the source's fixed text.
                let stderr = if message.contains("timed out") || message.contains("output limit")
                {
                    format!(
                        "{binary} was terminated before completing (timed out after 30000ms or exceeded the output limit)"
                    )
                } else {
                    message
                };
                return Err(self.command_error(&args, stderr, String::new(), None));
            }
        };
        if output.status != 0 {
            return Err(self.command_error(
                &args,
                output.stderr,
                String::from_utf8_lossy(&output.stdout).into_owned(),
                Some(output.status),
            ));
        }
        Ok(String::from_utf8(output.stdout)?.trim().to_owned())
    }
    fn command_error(
        &self,
        args: &[String],
        stderr: String,
        stdout: String,
        exit_code: Option<i32>,
    ) -> anyhow::Error {
        let auth = auth_failure(self.binary(), &stderr);
        let mut args = args.to_vec();
        if self.binary() == "tea" {
            for i in 1..args.len() {
                if matches!(args[i - 1].as_str(), "--title" | "--description" | "--body") {
                    args[i] = "<redacted>".into();
                }
            }
        }
        ForgeError {
            auth_state: if auth { "unauthenticated" } else { "error" },
            message: if auth {
                format!("{} CLI authentication failed", self.brand())
            } else {
                format!(
                    "{} CLI command failed: {} {}",
                    self.brand(),
                    self.binary(),
                    args.join(" ")
                )
            },
            stderr,
            stdout,
            exit_code,
        }
        .into()
    }
    async fn json(&self, args: Vec<String>) -> Result<Value> {
        let text = self.run(args.clone()).await?;
        if text.is_empty() && self.binary() == "tea" && args.iter().any(|a| a == "list") {
            return Ok(json!([]));
        }
        serde_json::from_str(&text).map_err(|_| {
            self.command_error(
                &args,
                format!(
                    "{} did not return valid JSON ({} bytes)",
                    self.binary(),
                    text.len()
                ),
                text,
                None,
            )
        })
    }
    /// Array reads whose source parser substitutes `[]` for empty stdout
    /// (`emptyFallback: "[]"` / `runJsonArray`).
    async fn json_array(&self, args: Vec<String>) -> Result<Value> {
        let text = self.run(args.clone()).await?;
        if text.is_empty() {
            return Ok(json!([]));
        }
        serde_json::from_str(&text).map_err(|_| {
            self.command_error(
                &args,
                format!(
                    "{} did not return valid JSON ({} bytes)",
                    self.binary(),
                    text.len()
                ),
                text,
                None,
            )
        })
    }
    async fn api(&self, path: String) -> Result<Value> {
        self.json(vec!["api".into(), path]).await
    }
    async fn git(&self, args: &[&str]) -> Result<String> {
        let out = self
            .engine
            .raw(self.cwd, self.env, self.cancel, "git", strings(args), 30)
            .await?;
        if out.status != 0 {
            bail!("{}", out.stderr);
        }
        Ok(String::from_utf8(out.stdout)?.trim().to_owned())
    }
    async fn dispatch(&self, op: &str, input: &Value) -> Result<Value> {
        if op == "searchIssuesAndPrs" {
            return self.search(input).await;
        }
        if op == "getPullRequestTimeline" {
            let result = match self.binary() {
                "gh" => self.github_timeline(input).await,
                "glab" => self.gitlab_timeline(input).await,
                _ => self.gitea_timeline(input).await,
            };
            return Ok(match result {
                Ok(value) => value,
                Err(error) => timeline_result(
                    input,
                    Vec::new(),
                    false,
                    Some(timeline_error(&error, self.binary())),
                ),
            });
        }
        match self.binary() {
            "gh" => self.github(op, input).await,
            "glab" => self.gitlab(op, input).await,
            _ => self.gitea(op, input).await,
        }
    }
    async fn search(&self, input: &Value) -> Result<Value> {
        let kinds = search_kinds(input);
        let mut input = input.clone();
        if self.binary() == "gh" {
            input["query"] =
                github::normalize_search(s(&input, "query"), &self.resolution.host).into();
        }
        let issues = async {
            if kinds.contains(&"issue") {
                self.list(false, &input).await.map(Some)
            } else {
                Ok(None)
            }
        };
        let prs = async {
            if kinds.contains(&"change_request") {
                self.list(true, &input).await.map(Some)
            } else {
                Ok(None)
            }
        };
        let (issues, prs) = tokio::join!(issues, prs);
        let results = [("issue", issues), ("change_request", prs)];
        let requested: Vec<_> = results
            .iter()
            .filter(|(_, r)| !matches!(r, Ok(None)))
            .collect();
        if !requested.is_empty()
            && requested.iter().all(|(_, r)| {
                r.as_ref()
                    .err()
                    .is_some_and(|e| matches!(auth_state(e), "cli_missing" | "unauthenticated"))
            })
        {
            return Ok(unavailable(
                if requested.iter().any(|(_, r)| {
                    r.as_ref()
                        .err()
                        .is_some_and(|e| auth_state(e) == "cli_missing")
                }) {
                    "cli_missing"
                } else {
                    "unauthenticated"
                },
            ));
        }
        let mut items = Vec::new();
        for (kind, result) in results {
            match result {
                Ok(Some(values)) => {
                    for mut item in values {
                        item["kind"] = kind.into();
                        item["forge"] = self.resolution.forge.clone().into();
                        if kind == "issue" {
                            item["baseRefName"] = Value::Null;
                            item["headRefName"] = Value::Null;
                        }
                        items.push(item);
                    }
                }
                Err(e)
                    if self.binary() != "gh"
                        && !matches!(auth_state(&e), "cli_missing" | "unauthenticated") =>
                {
                    return Err(e);
                }
                _ => {}
            }
        }
        let number = if self.binary() == "glab" {
            Some(s(&input, "query").trim())
                .filter(|q| !q.is_empty() && q.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|q| q.parse::<u64>().ok())
        } else {
            None
        };
        items.sort_by(|a, b| {
            let exact = number
                .map(|n| {
                    (number_value(&b["number"]) == Some(n))
                        .cmp(&(number_value(&a["number"]) == Some(n)))
                })
                .unwrap_or(std::cmp::Ordering::Equal);
            exact.then_with(|| time(&b["updatedAt"]).cmp(&time(&a["updatedAt"])))
        });
        Ok(
            json!({"items":items,"featuresEnabled":true,"authState":"authenticated","githubFeaturesEnabled":true}),
        )
    }
    async fn list(&self, pr: bool, input: &Value) -> Result<Vec<Value>> {
        match self.binary() {
            "gh" => self.github_list(pr, input).await,
            "glab" => self.gitlab_list(pr, input).await,
            _ => self.gitea_list(pr, input).await,
        }
    }
}
fn environment_hash(env: &BTreeMap<String, String>) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    env.hash(&mut h);
    h.finish()
}
fn cache_prefix(cwd: &Path, env: &BTreeMap<String, String>) -> String {
    format!("{}\0{}:", cwd.display(), environment_hash(env))
}
#[derive(Clone)]
struct Remote {
    host: String,
    path: String,
    ssh: bool,
}
fn normalize_host(host: &str) -> String {
    host.trim().trim_end_matches('.').to_ascii_lowercase()
}
fn valid_host(host: &str) -> bool {
    !host.is_empty()
        && host.as_bytes()[0].is_ascii_alphanumeric()
        && host
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
        && host
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
}
fn parse_remote(value: &str) -> Option<Remote> {
    let value = value.trim();
    let (host, path, ssh) = if !value.contains("://") {
        let (user, rest) = value.split_once('@')?;
        if user.is_empty() {
            return None;
        }
        let (host, path) = rest.split_once(':')?;
        (normalize_host(host), path.to_owned(), true)
    } else {
        let url = url::Url::parse(value).ok()?;
        if !matches!(url.scheme(), "http" | "https" | "ssh") {
            return None;
        }
        (
            normalize_host(url.host_str()?),
            percent_encoding::percent_decode_str(url.path())
                .decode_utf8()
                .ok()?
                .into_owned(),
            url.scheme() == "ssh",
        )
    };
    let path = path
        .trim()
        .trim_matches('/')
        .strip_suffix(".git")
        .unwrap_or(path.trim().trim_matches('/'))
        .to_owned();
    if !valid_host(&host) || path.is_empty() {
        return None;
    }
    Some(Remote { host, path, ssh })
}
fn cloud_forge(host: &str) -> Option<&'static str> {
    match host {
        "github.com" | "ssh.github.com" => Some("github"),
        "gitlab.com" => Some("gitlab"),
        "gitea.com" => Some("gitea"),
        "codeberg.org" => Some("codeberg"),
        _ => None,
    }
}
fn tea_login<'a>(value: &'a Value, host: &str) -> Option<&'a str> {
    array(value)
        .iter()
        .find(|login| {
            [s(login, "ssh_host"), s(login, "name")]
                .iter()
                .any(|v| v.eq_ignore_ascii_case(host))
                || url::Url::parse(s(login, "url"))
                    .ok()
                    .is_some_and(|u| u.host_str().is_some_and(|h| h.eq_ignore_ascii_case(host)))
        })
        .and_then(|v| v["name"].as_str())
}
/// First `HTTP/x[.y] NNN` status line in a `tea api -i` header dump.
fn http_status(dump: &str) -> Option<u16> {
    static STATUS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?m)^HTTP/\d(?:\.\d)?\s+(\d{3})").expect("valid HTTP status regex")
    });
    STATUS.captures(dump)?.get(1)?.as_str().parse().ok()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tea_api_status_is_the_first_http_status_line() {
        assert_eq!(
            http_status("HTTP/1.1 404 Not Found\r\nX: y\r\n\r\nHTTP/2 200\r\n"),
            Some(404)
        );
        assert_eq!(http_status("HTTP/2 401\r\n"), Some(401));
        assert_eq!(http_status("{\"message\":\"HTTP/1.1 200\"}"), None);
    }
    #[test]
    fn remotes_parse_like_git_remote_location() {
        let scp = parse_remote("git@GitHub.com.:Owner/Repo.git").unwrap();
        assert_eq!((scp.host.as_str(), scp.path.as_str(), scp.ssh), ("github.com", "Owner/Repo", true));
        let https = parse_remote("https://gitlab.example.com/group/sub/proj.git/").unwrap();
        assert_eq!(
            (https.host.as_str(), https.path.as_str(), https.ssh),
            ("gitlab.example.com", "group/sub/proj", false)
        );
        assert!(parse_remote("git://host/repo").is_none());
        assert!(parse_remote("@host:repo").is_none());
        assert!(parse_remote("git@host:.git").is_none());
    }
}
