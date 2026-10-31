use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use rand::Rng;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use tokio::{io::AsyncReadExt, net::TcpListener, sync::watch};
use tokio_util::sync::CancellationToken;
use unicode_normalization::UnicodeNormalization;

use crate::process::{self, CommandSpec, ManagedProcess};

pub type PortPlan = BTreeMap<String, f64>;

#[derive(Clone, Debug)]
pub struct ServicePortDeclaration {
    pub script_name: String,
    pub port: Option<f64>,
}

#[derive(Clone, Debug)]
pub struct ServicePeer {
    pub script_name: String,
    pub port: f64,
}

#[derive(Default)]
struct PortState {
    plans: BTreeMap<String, PortPlan>,
    pending: BTreeMap<String, watch::Sender<Option<bool>>>,
    released: BTreeSet<String>,
    owners: BTreeMap<u16, String>,
    dynamic: BTreeMap<String, BTreeMap<String, u16>>,
}

/// One instance is shared by all workspaces. A pending transaction owns its
/// reservations until commit or rollback, including when its future is dropped.
#[derive(Clone, Default)]
pub struct RuntimePorts {
    state: Arc<Mutex<PortState>>,
}

struct PlanUpdate {
    registry: RuntimePorts,
    workspace: String,
    previous: BTreeMap<String, u16>,
    committed: bool,
}

impl Drop for PlanUpdate {
    fn drop(&mut self) {
        let mut state = self.registry.state.lock().unwrap();
        let released = state.released.remove(&self.workspace);
        if !self.committed {
            let current = state
                .dynamic
                .get(&self.workspace)
                .cloned()
                .unwrap_or_default();
            for (name, port) in current {
                if released || self.previous.get(&name) != Some(&port) {
                    release_dynamic(&mut state, &self.workspace, &name, port);
                }
            }
            if !released {
                for (name, port) in &self.previous {
                    reserve_dynamic(&mut state, &self.workspace, name, *port);
                }
            }
        }
        if let Some(pending) = state.pending.remove(&self.workspace) {
            pending.send_replace(Some(released));
        }
    }
}

fn owner(workspace: &str, script: &str) -> String {
    format!("{workspace}\0{script}")
}

fn reserve_dynamic(state: &mut PortState, workspace: &str, script: &str, port: u16) {
    state.owners.insert(port, owner(workspace, script));
    state
        .dynamic
        .entry(workspace.to_owned())
        .or_default()
        .insert(script.to_owned(), port);
}

fn release_dynamic(state: &mut PortState, workspace: &str, script: &str, port: u16) {
    if state
        .owners
        .get(&port)
        .is_some_and(|value| *value == owner(workspace, script))
    {
        state.owners.remove(&port);
    }
    if let Some(ports) = state.dynamic.get_mut(workspace) {
        if ports.get(script) == Some(&port) {
            ports.remove(script);
        }
        if ports.is_empty() {
            state.dynamic.remove(workspace);
        }
    }
}

fn released_error(workspace: &str) -> anyhow::Error {
    anyhow!("Workspace service port plan was released while being created for '{workspace}'")
}

impl RuntimePorts {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn ensure_plan(
        &self,
        workspace_id: &str,
        cwd: &Path,
        branch_name: Option<&str>,
        services: &[ServicePortDeclaration],
        allocation: Option<&Value>,
    ) -> Result<PortPlan> {
        self.update(workspace_id, cwd, branch_name, services, allocation, None)
            .await
    }

    pub async fn refresh_port(
        &self,
        workspace_id: &str,
        cwd: &Path,
        branch_name: Option<&str>,
        service: &ServicePortDeclaration,
        allocation: Option<&Value>,
    ) -> Result<f64> {
        let plan = self
            .update(
                workspace_id,
                cwd,
                branch_name,
                std::slice::from_ref(service),
                allocation,
                Some(&service.script_name),
            )
            .await?;
        require_planned_port(&plan, &service.script_name)
    }

    pub fn release_plan(&self, workspace_id: &str) {
        let mut state = self.state.lock().unwrap();
        if state.pending.contains_key(workspace_id) {
            state.released.insert(workspace_id.to_owned());
        }
        state.plans.remove(workspace_id);
        if let Some(ports) = state.dynamic.remove(workspace_id) {
            for (name, port) in ports {
                release_dynamic(&mut state, workspace_id, &name, port);
            }
        }
    }

    async fn update(
        &self,
        workspace: &str,
        cwd: &Path,
        branch: Option<&str>,
        services: &[ServicePortDeclaration],
        allocation: Option<&Value>,
        refresh: Option<&str>,
    ) -> Result<PortPlan> {
        let (mut transaction, mut plan) = loop {
            let pending = {
                let mut state = self.state.lock().unwrap();
                if let Some(pending) = state.pending.get(workspace) {
                    Some(pending.subscribe())
                } else {
                    let mut plan = state.plans.get(workspace).cloned().unwrap_or_default();
                    if refresh.is_none()
                        && services
                            .iter()
                            .all(|service| plan.contains_key(&service.script_name))
                    {
                        return Ok(plan);
                    }
                    if let Some(script) = refresh {
                        plan.remove(script);
                    }
                    let previous = state.dynamic.get(workspace).cloned().unwrap_or_default();
                    let (sender, _) = watch::channel(None);
                    state.pending.insert(workspace.to_owned(), sender);
                    break (
                        PlanUpdate {
                            registry: self.clone(),
                            workspace: workspace.to_owned(),
                            previous,
                            committed: false,
                        },
                        plan,
                    );
                }
            };
            if let Some(mut pending) = pending {
                let released = *pending
                    .wait_for(|result| result.is_some())
                    .await
                    .context("Workspace service port plan update was interrupted")?;
                if released == Some(true) {
                    return Err(released_error(workspace));
                }
            }
        };

        let result = async {
            let mut explicit: Vec<f64> = plan.values().copied().collect();
            for service in services {
                if !plan.contains_key(&service.script_name) {
                    if let Some(port) = service.port {
                        if explicit.contains(&port) {
                            bail!(
                                "Service '{}' has a duplicate port {}",
                                service.script_name,
                                js_number_string(port)
                            );
                        }
                        explicit.push(port);
                    }
                }
            }
            for service in services {
                if plan.contains_key(&service.script_name) {
                    continue;
                }
                let port = if let Some(port) = service.port {
                    port
                } else {
                    let reserved: BTreeSet<u16> = explicit
                        .iter()
                        .chain(plan.values())
                        .filter_map(|port| tcp_port(*port))
                        .collect();
                    let service_owner = owner(workspace, &service.script_name);
                    let mut selected = None;
                    for _ in 0..10 {
                        let unavailable = {
                            let state = self.state.lock().unwrap();
                            let mut ports = reserved.clone();
                            ports.extend(state.owners.iter().filter_map(|(port, owner)| {
                                (owner != &service_owner).then_some(*port)
                            }));
                            ports
                        };
                        let port = allocate_workspace_service_port(
                            allocation,
                            cwd,
                            &service.script_name,
                            workspace,
                            branch,
                            &unavailable,
                        )
                        .await?;
                        let mut state = self.state.lock().unwrap();
                        if reserved.contains(&port)
                            || state
                                .owners
                                .get(&port)
                                .is_some_and(|owner| owner != &service_owner)
                        {
                            continue;
                        }
                        reserve_dynamic(&mut state, workspace, &service.script_name, port);
                        selected = Some(port);
                        break;
                    }
                    f64::from(selected.ok_or_else(|| {
                        anyhow!(
                            "Could not allocate a unique port for service '{}' after 10 attempts",
                            service.script_name
                        )
                    })?)
                };
                plan.insert(service.script_name.clone(), port);
            }
            Ok::<_, anyhow::Error>(())
        }
        .await;

        let mut state = self.state.lock().unwrap();
        if state.released.contains(workspace) {
            return Err(released_error(workspace));
        }
        result?;
        for (name, port) in &transaction.previous {
            if plan.get(name).copied() != Some(f64::from(*port)) {
                release_dynamic(&mut state, workspace, name, *port);
            }
        }
        state.plans.insert(workspace.to_owned(), plan.clone());
        transaction.committed = true;
        drop(state);
        Ok(plan)
    }
}

pub fn require_planned_port(plan: &PortPlan, script_name: &str) -> Result<f64> {
    plan.get(script_name).copied().ok_or_else(|| {
        anyhow!("Service '{script_name}' is missing from workspace service port plan")
    })
}

fn tcp_port(number: f64) -> Option<u16> {
    (number.fract() == 0.0 && (1.0..=65535.0).contains(&number)).then_some(number as u16)
}

fn js_number_string(number: f64) -> String {
    ryu_js::Buffer::new().format(number).to_owned()
}

pub async fn allocate_workspace_service_port(
    allocation: Option<&Value>,
    cwd: &Path,
    script_name: &str,
    workspace_id: &str,
    branch_name: Option<&str>,
    reserved_ports: &BTreeSet<u16>,
) -> Result<u16> {
    if let Some(command) = allocation
        .and_then(|v| v.get("portScript"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        let branch = branch_name.unwrap_or("");
        let cwd_string = cwd.to_string_lossy();
        let mut env = external_environment();
        env.extend([
            ("PASEO_SCRIPTNAME".to_owned(), script_name.to_owned()),
            ("PASEO_WORKSPACE_ID".to_owned(), workspace_id.to_owned()),
            ("PASEO_BRANCH_NAME".to_owned(), branch.to_owned()),
            ("PASEO_WORKTREE_PATH".to_owned(), cwd_string.to_string()),
        ]);
        let spec = CommandSpec {
            program: command.to_owned(),
            args: vec![
                script_name.to_owned(),
                workspace_id.to_owned(),
                branch.to_owned(),
                cwd_string.to_string(),
            ],
            cwd: Some(cwd.to_owned()),
            env,
            clear_env: true,
        };
        let stdout = run_port_script(spec)
            .await
            .map_err(|error| anyhow!("Service port script '{command}' failed: {error}"))?;
        let output = String::from_utf8_lossy(&stdout);
        let output = output.trim_matches(js_whitespace);
        if output.is_empty() || !output.bytes().all(|b| b.is_ascii_digit()) {
            bail!("Service port script '{command}' must print exactly one TCP port");
        }
        let port = output
            .parse::<f64>()
            .ok()
            .filter(|n| *n >= 1.0 && *n <= 65535.0)
            .ok_or_else(|| {
                anyhow!("Service port script '{command}' returned invalid TCP port '{output}'")
            })? as u16;
        if reserved_ports.contains(&port) {
            bail!("Service port script '{command}' returned reserved port {port}");
        }
        return Ok(port);
    }
    if let Some(range) = allocation
        .and_then(|v| v.get("range"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        let (start, end) = parse_port_range(range)?;
        let count = u32::from(end) - u32::from(start) + 1;
        let start_offset = rand::thread_rng().gen_range(0..count);
        for offset in 0..count {
            let port = (u32::from(start) + (start_offset + offset) % count) as u16;
            if !reserved_ports.contains(&port)
                && TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
                    .await
                    .is_ok()
            {
                return Ok(port);
            }
        }
        bail!("No available service port in configured range {start}-{end}");
    }
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
    Ok(listener.local_addr()?.port())
}

fn external_environment() -> BTreeMap<String, String> {
    std::env::vars()
        .filter(|(key, _)| {
            !matches!(
                key.as_str(),
                "PASEO_NODE_ENV"
                    | "PASEO_DESKTOP_MANAGED"
                    | "PASEO_SUPERVISED"
                    | "ELECTRON_RUN_AS_NODE"
                    | "ELECTRON_NO_ATTACH_CONSOLE"
                    | "ESBUILD_BINARY_PATH"
            )
        })
        .collect()
}

/// Node `execFile` maxBuffer: the limit is exceeded once the collected
/// length is strictly greater than the limit.
async fn read_script_stream(
    mut stream: impl tokio::io::AsyncRead + Unpin,
    bytes: &mut Vec<u8>,
    kind: &str,
) -> Result<()> {
    let mut buffer = [0; 1025];
    loop {
        let count = stream.read(&mut buffer).await?;
        if count == 0 {
            return Ok(());
        }
        if bytes.len() + count > 1024 {
            bail!("{kind} maxBuffer length exceeded");
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
}

/// Runs like Node `execFile(command, args, {timeout: 10000, maxBuffer: 1024})`
/// and reports its error messages: `spawn <file> <code>` when the program
/// cannot start, `<stream> maxBuffer length exceeded`, and
/// `Command failed: <file> <args>\n<stderr>` for nonzero exits and timeouts.
async fn run_port_script(spec: CommandSpec) -> Result<Vec<u8>> {
    let program = spec.program.clone();
    let command_line = std::iter::once(spec.program.as_str())
        .chain(spec.args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ");
    let (process, stdout, stderr) = match ManagedProcess::spawn_with_stderr(spec).await {
        Ok(spawned) => spawned,
        Err(error) => {
            let code = error
                .chain()
                .find_map(|cause| cause.downcast_ref::<std::io::Error>())
                .map(|io| match io.kind() {
                    std::io::ErrorKind::NotFound => "ENOENT".to_owned(),
                    std::io::ErrorKind::PermissionDenied => "EACCES".to_owned(),
                    _ => io.to_string(),
                })
                .unwrap_or_else(|| format!("{error:#}"));
            bail!("spawn {program} {code}");
        }
    };
    process.close_stdin().await?;
    let mut out = Vec::new();
    let mut err = Vec::new();
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::try_join!(
            read_script_stream(stdout, &mut out, "stdout"),
            read_script_stream(stderr, &mut err, "stderr"),
            process.wait()
        )
    })
    .await;
    let failed = |err: &[u8]| {
        anyhow!(
            "Command failed: {command_line}\n{}",
            String::from_utf8_lossy(err)
        )
    };
    match result {
        Ok(Ok((_, _, 0))) => Ok(out),
        Ok(Ok(_)) => Err(failed(&err)),
        Ok(Err(error)) => {
            process.terminate(Duration::from_millis(200)).await?;
            Err(error)
        }
        Err(_) => {
            process.terminate(Duration::from_millis(200)).await?;
            Err(failed(&err))
        }
    }
}

fn parse_port_range(value: &str) -> Result<(u16, u16)> {
    let mut parts = value.split('-');
    let parse = |part: Option<&str>| -> Option<u16> {
        let text = part?.trim_matches(js_whitespace);
        let number = if text.starts_with("0x") || text.starts_with("0X") {
            u32::from_str_radix(&text[2..], 16).ok()? as f64
        } else if text.starts_with("0b") || text.starts_with("0B") {
            u32::from_str_radix(&text[2..], 2).ok()? as f64
        } else if text.starts_with("0o") || text.starts_with("0O") {
            u32::from_str_radix(&text[2..], 8).ok()? as f64
        } else {
            text.parse::<f64>().ok()?
        };
        (number.fract() == 0.0 && (1.0..=65535.0).contains(&number)).then_some(number as u16)
    };
    match (parse(parts.next()), parse(parts.next())) {
        (Some(start), Some(end)) if start <= end => Ok((start, end)),
        _ => bail!("Invalid service port range '{value}'"),
    }
}

fn js_whitespace(c: char) -> bool {
    matches!(c, '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}')
}

fn separated_ascii(input: impl Iterator<Item = char>, separator: char, uppercase: bool) -> String {
    let mut output = String::new();
    let mut pending_separator = false;
    for c in input {
        if c.is_ascii_digit()
            || if uppercase {
                c.is_ascii_uppercase()
            } else {
                c.is_ascii_lowercase()
            }
        {
            if pending_separator && !output.is_empty() {
                output.push(separator);
            }
            output.push(c);
            pending_separator = false;
        } else {
            pending_separator = true;
        }
    }
    output
}

pub fn normalize_service_env_name(script_name: &str) -> String {
    separated_ascii(script_name.to_uppercase().chars(), '_', true)
}

pub fn assert_no_service_env_name_collisions(script_names: &[&str]) -> Result<()> {
    let mut names: Vec<(String, Vec<&str>)> = Vec::new();
    for script in script_names {
        let normalized = normalize_service_env_name(script);
        if let Some((_, values)) = names.iter_mut().find(|(name, _)| *name == normalized) {
            values.push(script);
        } else {
            names.push((normalized, vec![script]));
        }
    }
    let collisions: Vec<_> = names
        .iter()
        .filter(|(_, names)| names.len() > 1)
        .map(|(name, names)| {
            format!(
                "Service env name collision for {name}: {}",
                names.join(", ")
            )
        })
        .collect();
    if !collisions.is_empty() {
        bail!("{}", collisions.join("; "));
    }
    Ok(())
}

pub fn resolve_service_bind_host(host: Option<&str>) -> &'static str {
    if host.is_none_or(|host| {
        host.is_empty()
            || matches!(
                host.trim_matches(js_whitespace).to_lowercase().as_str(),
                "localhost" | "127.0.0.1" | "::1" | "[::1]"
            )
    }) {
        "127.0.0.1"
    } else {
        "0.0.0.0"
    }
}

pub fn service_proxy_label(project_slug: &str, branch: Option<&str>, script_name: &str) -> String {
    let label = |s: &str| {
        let result = separated_ascii(
            s.to_lowercase()
                .nfkd()
                .filter(|c| !('\u{0300}'..='\u{036f}').contains(c)),
            '-',
            false,
        );
        if result.is_empty() {
            "untitled".to_owned()
        } else {
            result
        }
    };
    let mut result = label(script_name);
    if let Some(branch) = branch.filter(|branch| !matches!(*branch, "main" | "master")) {
        result.push_str("--");
        result.push_str(&label(branch));
    }
    result.push_str("--");
    result.push_str(&label(project_slug));
    if result.len() > 63 {
        let digest = Sha256::digest(result.as_bytes());
        let suffix = hex::encode(&digest[..4]);
        result = format!("{}--{suffix}", result[..53].trim_end_matches('-'));
    }
    result
}

pub fn service_proxy_url(
    project_slug: &str,
    branch: Option<&str>,
    script_name: &str,
    daemon_port: Option<u16>,
    public_base_url: Option<&str>,
) -> Result<Option<String>> {
    let label = service_proxy_label(project_slug, branch, script_name);
    if let Some(public) = public_base_url.filter(|s| !s.is_empty()) {
        let base = url::Url::parse(public)?;
        let hostname = base.host_str().unwrap_or("");
        let port = base
            .port()
            .map(|port| format!(":{port}"))
            .unwrap_or_default();
        return Ok(Some(format!(
            "{}://{label}.{hostname}{port}",
            base.scheme()
        )));
    }
    Ok(daemon_port.map(|port| format!("http://{label}.localhost:{port}")))
}

pub fn project_service_proxy_urls(
    project_slug: &str,
    branch: Option<&str>,
    script_name: &str,
    daemon_port: Option<u16>,
    public_base_url: Option<&str>,
) -> Result<Value> {
    let local = service_proxy_url(project_slug, branch, script_name, daemon_port, None)?;
    let public = if public_base_url.is_some_and(|s| !s.is_empty()) {
        service_proxy_url(project_slug, branch, script_name, None, public_base_url)?
    } else {
        None
    };
    Ok(
        json!({"localProxyUrl":local, "publicProxyUrl":public, "proxyUrl":public.as_ref().or(local.as_ref())}),
    )
}

pub fn build_workspace_service_env(
    script_name: &str,
    project_slug: &str,
    branch_name: Option<&str>,
    daemon_port: Option<u16>,
    daemon_listen_host: Option<&str>,
    public_base_url: Option<&str>,
    peers: &[ServicePeer],
) -> Result<BTreeMap<String, String>> {
    assert_no_service_env_name_collisions(
        &peers
            .iter()
            .map(|peer| peer.script_name.as_str())
            .collect::<Vec<_>>(),
    )?;
    let own = peers
        .iter()
        .find(|peer| peer.script_name == script_name)
        .ok_or_else(|| {
            anyhow!("Service '{script_name}' is missing from workspace service peers")
        })?;
    let mut env = BTreeMap::from([
        (
            "HOST".to_owned(),
            resolve_service_bind_host(daemon_listen_host).to_owned(),
        ),
        ("PASEO_PORT".to_owned(), js_number_string(own.port)),
    ]);
    if let Some(url) = service_proxy_url(
        project_slug,
        branch_name,
        script_name,
        daemon_port,
        public_base_url,
    )? {
        env.insert("PASEO_URL".to_owned(), url);
    }
    for peer in peers {
        let name = normalize_service_env_name(&peer.script_name);
        env.insert(
            format!("PASEO_SERVICE_{name}_PORT"),
            js_number_string(peer.port),
        );
        if let Some(url) = service_proxy_url(
            project_slug,
            branch_name,
            &peer.script_name,
            daemon_port,
            public_base_url,
        )? {
            env.insert(format!("PASEO_SERVICE_{name}_URL"), url);
        }
    }
    Ok(env)
}

/// Resolve the worktree's own gitdir, never its shared common gitdir.
pub async fn metadata_path(worktree_root: &Path) -> Result<PathBuf> {
    if !tokio::fs::try_exists(worktree_root.join(".git")).await? {
        bail!("Not a git repository: {}", worktree_root.display());
    }
    let result = process::run(
        CommandSpec {
            program: "git".to_owned(),
            args: vec!["rev-parse".to_owned(), "--absolute-git-dir".to_owned()],
            cwd: Some(worktree_root.to_owned()),
            env: external_environment(),
            clear_env: true,
        },
        1024 * 1024,
        Duration::from_secs(10),
        CancellationToken::new(),
    )
    .await?;
    if result.status != 0 {
        bail!("{}", result.stderr.trim_end());
    }
    let gitdir = std::str::from_utf8(&result.stdout)?.trim_end_matches(['\r', '\n']);
    if gitdir.is_empty() {
        bail!(
            "Git returned an empty gitdir for {}",
            worktree_root.display()
        );
    }
    Ok(PathBuf::from(gitdir).join("paseo").join("worktree.json"))
}

fn nonempty_string<'a>(object: &'a Value, name: &str) -> Result<&'a str> {
    object
        .get(name)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("Invalid worktree metadata: {name} must be a nonempty string"))
}

fn positive_integer(value: &Value, field: &str) -> Result<()> {
    if !value
        .as_f64()
        .is_some_and(|n| n.is_finite() && n > 0.0 && n.fract() == 0.0)
    {
        bail!("Invalid worktree metadata: {field} must be a positive integer");
    }
    Ok(())
}

pub fn create_worktree_change_request_hint(input: &Value) -> Result<Value> {
    let mut result = Map::new();
    result.insert(
        "headRef".to_owned(),
        Value::String(nonempty_string(input, "headRef")?.to_owned()),
    );
    for key in ["headRepositoryOwner", "localBranchName"] {
        if input.get(key).is_some() {
            result.insert(
                key.to_owned(),
                Value::String(nonempty_string(input, key)?.to_owned()),
            );
        }
    }
    if let Some(number) = input.get("changeRequestNumber") {
        positive_integer(number, "changeRequestNumber")?;
        result.insert("changeRequestNumber".to_owned(), number.clone());
    }
    Ok(Value::Object(result))
}

/// Like the source Zod object schemas, rejects explicit null optional fields and
/// strips unknown fields (including v2-only fields when the version is 1).
pub(crate) fn parse_metadata(input: &Value) -> Result<Value> {
    let version = input
        .get("version")
        .and_then(Value::as_f64)
        .filter(|v| *v == 1.0 || *v == 2.0)
        .ok_or_else(|| anyhow!("Invalid worktree metadata version"))? as u64;
    let mut output =
        json!({"version": version, "baseRefName": nonempty_string(input, "baseRefName")?});
    if input.get("baseRef").is_some() {
        output["baseRef"] = Value::String(nonempty_string(input, "baseRef")?.to_owned());
    }
    if let Some(hint) = input.get("changeRequestLookupTarget") {
        output["changeRequestLookupTarget"] = create_worktree_change_request_hint(hint)?;
    }
    if version == 2 {
        if let Some(auto_name) = input.get("firstAgentBranchAutoName") {
            let status = nonempty_string(auto_name, "status")?;
            if !matches!(status, "pending" | "attempted") {
                bail!("Invalid first-agent branch auto-name status");
            }
            let mut value = json!({"status": status, "placeholderBranchName": nonempty_string(auto_name, "placeholderBranchName")?});
            if status == "attempted" {
                value["attemptedAt"] =
                    Value::String(nonempty_string(auto_name, "attemptedAt")?.to_owned());
            }
            output["firstAgentBranchAutoName"] = value;
        }
        if let Some(runtime) = input.get("runtime") {
            let port = runtime
                .get("worktreePort")
                .ok_or_else(|| anyhow!("Invalid worktree metadata runtime"))?;
            positive_integer(port, "worktreePort")?;
            output["runtime"] = json!({"worktreePort": port});
        }
    }
    Ok(output)
}

pub async fn read_worktree_metadata(root: &Path) -> Result<Option<Value>> {
    let path = metadata_path(root).await?;
    match tokio::fs::read(path).await {
        Ok(bytes) => Ok(Some(parse_metadata(&serde_json::from_slice(&bytes)?)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

async fn write_metadata_file(root: &Path, metadata: &Value) -> Result<()> {
    let path = metadata_path(root).await?;
    let parent = path.parent().context("Missing metadata directory")?;
    tokio::fs::create_dir_all(parent).await?;
    let mut bytes = serde_json::to_vec_pretty(metadata)?;
    bytes.push(b'\n');
    let temp = path.with_file_name(format!(
        "worktree.json.{}.{}.tmp",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    tokio::fs::write(&temp, bytes).await?;
    if let Err(error) = tokio::fs::rename(&temp, &path).await {
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(error.into());
    }
    Ok(())
}

pub fn is_qualified_ref(reference: &str) -> bool {
    reference.trim_matches(js_whitespace).starts_with("refs/")
}

pub fn branch_name_from_ref(reference: &str) -> &str {
    let reference = reference.trim_matches(js_whitespace);
    if let Some(branch) = reference.strip_prefix("refs/heads/") {
        return branch;
    }
    if let Some(branch) = reference.strip_prefix("refs/remotes/") {
        return branch.split_once('/').map_or(branch, |(_, branch)| branch);
    }
    reference.strip_prefix("origin/").unwrap_or(reference)
}

pub fn normalize_base_ref_name(input: &str) -> Result<&str> {
    if input.trim_matches(js_whitespace).is_empty() {
        bail!("Base branch is required");
    }
    Ok(branch_name_from_ref(input))
}

fn assert_valid_base_ref(value: &str) -> Result<()> {
    if value == "HEAD" {
        bail!("Base branch cannot be HEAD");
    }
    if value.contains("..") || value.contains("@{") {
        bail!("Invalid base branch: {value}");
    }
    Ok(())
}

pub async fn write_worktree_metadata(root: &Path, options: &Value) -> Result<()> {
    let base = normalize_base_ref_name(
        options
            .get("baseRefName")
            .and_then(Value::as_str)
            .context("Base branch is required")?,
    )?;
    assert_valid_base_ref(base)?;
    let mut metadata = json!({"version": 1, "baseRefName": base});
    if let Some(exact) = options
        .get("baseRef")
        .and_then(Value::as_str)
        .map(|s| s.trim_matches(js_whitespace))
        .filter(|s| !s.is_empty())
    {
        assert_valid_base_ref(exact)?;
        metadata["baseRef"] = Value::String(exact.to_owned());
    }
    if let Some(hint) = options
        .get("changeRequestLookupTarget")
        .filter(|v| !v.is_null() && **v != Value::Bool(false))
    {
        metadata["changeRequestLookupTarget"] = hint.clone();
    }
    write_metadata_file(root, &metadata).await
}

pub async fn read_worktree_runtime_port(root: &Path) -> Result<Option<f64>> {
    Ok(read_worktree_metadata(root).await?.and_then(|m| {
        m.get("runtime")
            .and_then(|r| r.get("worktreePort"))
            .and_then(Value::as_f64)
    }))
}

// net.Server.listen(port) without a host first tries the unspecified IPv6
// address, then IPv4. This is deliberately not the service allocator's
// loopback-only probe: setup scripts may bind the persisted port publicly.
async fn bind_worktree_port(port: u16) -> std::io::Result<TcpListener> {
    match TcpListener::bind((std::net::Ipv6Addr::UNSPECIFIED, port)).await {
        Ok(listener) => Ok(listener),
        Err(_) => TcpListener::bind((std::net::Ipv4Addr::UNSPECIFIED, port)).await,
    }
}

pub async fn ensure_worktree_runtime_port(root: &Path) -> Result<f64> {
    if let Some(port) = read_worktree_runtime_port(root).await? {
        if !(0.0..65536.0).contains(&port) || port.fract() != 0.0 {
            bail!("options.port should be >= 0 and < 65536. Received type number ({port}).");
        }
        match bind_worktree_port(port as u16).await {
            Ok(listener) => drop(listener),
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
                bail!("Persisted worktree port {port} is already in use");
            }
            Err(error) => return Err(error.into()),
        }
        return Ok(port);
    }
    let listener = bind_worktree_port(0).await?;
    let port = f64::from(listener.local_addr()?.port());
    drop(listener);
    if read_worktree_metadata(root).await?.is_some() {
        write_worktree_runtime_metadata(root, port).await?;
    }
    Ok(port)
}

pub async fn write_worktree_runtime_metadata(root: &Path, worktree_port: f64) -> Result<()> {
    if !worktree_port.is_finite() || worktree_port <= 0.0 || worktree_port.fract() != 0.0 {
        bail!("Invalid worktree runtime port: {worktree_port}");
    }
    let mut current = read_worktree_metadata(root)
        .await?
        .context("Cannot persist worktree runtime metadata: missing base metadata")?;
    current["version"] = json!(2);
    current["runtime"] = json!({"worktreePort": worktree_port});
    write_metadata_file(root, &current).await
}

pub async fn write_worktree_first_agent_branch_auto_name_metadata(
    root: &Path,
    placeholder_branch_name: &str,
) -> Result<()> {
    let name = placeholder_branch_name.trim_matches(js_whitespace);
    if name.is_empty() {
        bail!("Placeholder branch name is required");
    }
    let mut current = read_worktree_metadata(root)
        .await?
        .context("Cannot persist first-agent branch auto-name metadata: missing base metadata")?;
    current["version"] = json!(2);
    current["firstAgentBranchAutoName"] = json!({"status":"pending", "placeholderBranchName":name});
    write_metadata_file(root, &current).await
}

pub async fn mark_worktree_first_agent_branch_auto_name_attempted(
    root: &Path,
    attempted_at: Option<&str>,
) -> Result<Option<Value>> {
    let Some(mut current) = read_worktree_metadata(root).await? else {
        return Ok(None);
    };
    if current["version"] == 2 && current["firstAgentBranchAutoName"]["status"] == "pending" {
        let name = current["firstAgentBranchAutoName"]["placeholderBranchName"].clone();
        let timestamp = attempted_at.map(str::to_owned).unwrap_or_else(|| {
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        });
        current["firstAgentBranchAutoName"] =
            json!({"status":"attempted", "placeholderBranchName":name, "attemptedAt":timestamp});
        write_metadata_file(root, &current).await?;
    }
    Ok(Some(current))
}

pub fn worktree_change_request_hint_for_branch<'a>(
    metadata: Option<&'a Value>,
    current_branch: &str,
) -> Option<&'a Value> {
    let hint = metadata?.get("changeRequestLookupTarget")?;
    if let Some(local) = hint.get("localBranchName").and_then(Value::as_str) {
        return (local == current_branch).then_some(hint);
    }
    let head = hint.get("headRef")?.as_str()?;
    if let Some(owner) = hint.get("headRepositoryOwner").and_then(Value::as_str) {
        if current_branch == format!("{owner}/{head}") {
            return Some(hint);
        }
        let normalized = owner.trim_matches(js_whitespace).to_lowercase();
        if !normalized.is_empty()
            && normalized
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && current_branch == format!("{normalized}/{head}")
        {
            return Some(hint);
        }
        None
    } else {
        (head == current_branch).then_some(hint)
    }
}

pub async fn rebind_worktree_change_request_hint(
    root: &Path,
    previous_branch: &str,
    current_branch: &str,
) -> Result<bool> {
    let Some(mut metadata) = read_worktree_metadata(root).await? else {
        return Ok(false);
    };
    let Some(hint) = worktree_change_request_hint_for_branch(Some(&metadata), previous_branch)
    else {
        return Ok(false);
    };
    let mut hint = hint.clone();
    if hint["headRef"] == previous_branch
        && hint.get("headRepositoryOwner").is_none()
        && hint.get("changeRequestNumber").is_none()
    {
        hint["headRef"] = json!(current_branch);
    }
    hint["localBranchName"] = json!(current_branch);
    metadata["changeRequestLookupTarget"] = hint;
    write_metadata_file(root, &metadata).await?;
    Ok(true)
}

pub async fn pin_worktree_branch_identity_if_missing(root: &Path, branch: &str) -> Result<bool> {
    let Some(mut metadata) = read_worktree_metadata(root).await? else {
        return Ok(false);
    };
    if metadata.get("changeRequestLookupTarget").is_some() {
        return Ok(false);
    }
    metadata["changeRequestLookupTarget"] =
        create_worktree_change_request_hint(&json!({"headRef":branch,"localBranchName":branch}))?;
    write_metadata_file(root, &metadata).await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dynamic(name: &str) -> ServicePortDeclaration {
        ServicePortDeclaration {
            script_name: name.to_owned(),
            port: None,
        }
    }

    async fn git(root: &Path, args: &[&str]) -> Result<()> {
        let output = process::run(
            CommandSpec {
                program: "git".to_owned(),
                args: args.iter().map(|s| (*s).to_owned()).collect(),
                cwd: Some(root.to_owned()),
                env: BTreeMap::from([
                    ("GIT_CONFIG_NOSYSTEM".to_owned(), "1".to_owned()),
                    (
                        "GIT_CONFIG_GLOBAL".to_owned(),
                        root.join("no-global-config").to_string_lossy().into_owned(),
                    ),
                ]),
                clear_env: false,
            },
            1024 * 1024,
            Duration::from_secs(10),
            CancellationToken::new(),
        )
        .await?;
        if output.status != 0 {
            bail!("git failed: {}", output.stderr);
        }
        Ok(())
    }

    #[tokio::test]
    async fn metadata_uses_linked_gitdir_and_preserves_schema_transitions() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("repo");
        let linked = temp.path().join("linked");
        tokio::fs::create_dir(&root).await?;
        git(&root, &["init", "-b", "main"]).await?;
        git(
            &root,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "--allow-empty",
                "-m",
                "initial",
            ],
        )
        .await?;
        git(
            &root,
            &["worktree", "add", "-b", "feature", linked.to_str().unwrap()],
        )
        .await?;
        assert_eq!(read_worktree_metadata(&linked).await?, None);
        assert!(
            metadata_path(&linked)
                .await?
                .starts_with(root.join(".git/worktrees"))
        );
        write_worktree_metadata(&linked, &json!({
            "baseRefName":" refs/remotes/upstream/main ", "baseRef":" refs/remotes/upstream/main ",
            "changeRequestLookupTarget":{"headRef":"feature","unknown":"stripped on read"}
        })).await?;
        assert_eq!(
            read_worktree_metadata(&linked).await?.unwrap()["baseRefName"],
            "main"
        );
        assert_eq!(read_worktree_metadata(&root).await?, None);
        write_worktree_runtime_metadata(&linked, 70000.0).await?;
        write_worktree_first_agent_branch_auto_name_metadata(&linked, " placeholder ").await?;
        let metadata = read_worktree_metadata(&linked).await?.unwrap();
        assert_eq!(metadata["version"], 2);
        assert_eq!(metadata["baseRef"], "refs/remotes/upstream/main");
        assert!(
            metadata["changeRequestLookupTarget"]
                .get("unknown")
                .is_none()
        );
        assert_eq!(read_worktree_runtime_port(&linked).await?, Some(70000.0));
        assert!(rebind_worktree_change_request_hint(&linked, "feature", "renamed").await?);
        let attempted = mark_worktree_first_agent_branch_auto_name_attempted(
            &linked,
            Some("2026-10-29T00:00:00.000Z"),
        )
        .await?
        .unwrap();
        assert_eq!(attempted["firstAgentBranchAutoName"]["status"], "attempted");
        assert_eq!(attempted["changeRequestLookupTarget"]["headRef"], "renamed");
        assert_eq!(
            mark_worktree_first_agent_branch_auto_name_attempted(&linked, Some("later")).await?,
            Some(attempted)
        );
        let path = metadata_path(&linked).await?;
        tokio::fs::write(
            &path,
            br#"{"version":2,"baseRefName":"main","runtime":null}"#,
        )
        .await?;
        assert!(read_worktree_metadata(&linked).await.is_err());
        tokio::fs::write(path, b"{broken").await?;
        assert!(read_worktree_metadata(&linked).await.is_err());
        Ok(())
    }

    #[test]
    fn metadata_null_unknown_and_legacy_hint_semantics() -> Result<()> {
        let v1 =
            parse_metadata(&json!({"version":1,"baseRefName":"main","runtime":null,"unknown":7}))?;
        assert!(v1.get("runtime").is_none());
        assert!(v1.get("unknown").is_none());
        assert!(parse_metadata(&json!({"version":1,"baseRefName":"main","baseRef":null})).is_err());
        assert!(
            parse_metadata(
                &json!({"version":2,"baseRefName":"main","runtime":{"worktreePort":1.5}})
            )
            .is_err()
        );
        let legacy = json!({"changeRequestLookupTarget":{"headRef":"feature","headRepositoryOwner":" Some-Owner "}});
        assert!(
            worktree_change_request_hint_for_branch(Some(&legacy), "some-owner/feature").is_some()
        );
        assert!(worktree_change_request_hint_for_branch(Some(&legacy), "feature").is_none());
        let pinned =
            json!({"changeRequestLookupTarget":{"headRef":"feature","localBranchName":"local"}});
        assert!(worktree_change_request_hint_for_branch(Some(&pinned), "feature").is_none());
        assert!(worktree_change_request_hint_for_branch(Some(&pinned), "local").is_some());
        assert_eq!(
            branch_name_from_ref("refs/remotes/team/upstream/main"),
            "upstream/main"
        );
        Ok(())
    }

    #[tokio::test]
    async fn port_probe_reservations_rollback_refresh_and_release() -> Result<()> {
        let temp = tempfile::tempdir()?;
        // This test repeatedly releases one port. Do not draw it from the
        // ephemeral pool used by concurrent network tests and outgoing sockets.
        let mut listener = None;
        for port in 10240..16384 {
            match TcpListener::bind(("127.0.0.1", port)).await {
                Ok(bound) => {
                    listener = Some(bound);
                    break;
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::AddrInUse | std::io::ErrorKind::PermissionDenied
                    ) => {}
                Err(error) => return Err(error.into()),
            }
        }
        let listener = listener.context("No free test service port outside the ephemeral pool")?;
        let port = listener.local_addr()?.port();
        let allocation = json!({"range":format!("{port}-{port}")});
        let registry = RuntimePorts::new();
        assert!(
            registry
                .ensure_plan(
                    "one",
                    temp.path(),
                    None,
                    &[dynamic("api")],
                    Some(&allocation)
                )
                .await
                .is_err()
        );
        drop(listener);
        assert!(
            registry
                .ensure_plan(
                    "one",
                    temp.path(),
                    None,
                    &[dynamic("api"), dynamic("web")],
                    Some(&allocation)
                )
                .await
                .is_err()
        );
        let initial = registry
            .ensure_plan(
                "two",
                temp.path(),
                None,
                &[dynamic("api")],
                Some(&allocation),
            )
            .await?;
        assert_eq!(initial["api"], f64::from(port));
        assert!(
            registry
                .ensure_plan(
                    "one",
                    temp.path(),
                    None,
                    &[dynamic("api")],
                    Some(&allocation)
                )
                .await
                .is_err()
        );
        let explicit = ServicePortDeclaration {
            script_name: "web".into(),
            port: Some(f64::from(port)),
        };
        assert!(
            registry
                .ensure_plan("two", temp.path(), None, &[explicit], Some(&allocation))
                .await
                .is_err()
        );
        assert_eq!(
            registry
                .ensure_plan("two", temp.path(), None, &[dynamic("api")], None)
                .await?,
            initial
        );
        assert_eq!(
            registry
                .refresh_port("two", temp.path(), None, &dynamic("api"), Some(&allocation))
                .await?,
            f64::from(port)
        );
        // Allocation probes close their sockets; registry ownership is logical.
        let listener = TcpListener::bind(("127.0.0.1", port)).await?;
        drop(listener);
        registry.release_plan("two");
        assert_eq!(
            registry
                .ensure_plan(
                    "one",
                    temp.path(),
                    None,
                    &[dynamic("api")],
                    Some(&allocation)
                )
                .await?["api"],
            f64::from(port)
        );
        Ok(())
    }

    #[test]
    fn environment_uses_source_namespaces_and_null_urls() -> Result<()> {
        let peers = [
            ServicePeer {
                script_name: "web-ui".into(),
                port: 3000.0,
            },
            ServicePeer {
                script_name: "api".into(),
                port: 4000.0,
            },
        ];
        let env = build_workspace_service_env(
            "web-ui",
            "Café",
            Some("feature/x"),
            Some(6767),
            Some("[::1]"),
            None,
            &peers,
        )?;
        assert_eq!(env["HOST"], "127.0.0.1");
        assert_eq!(env["PASEO_PORT"], "3000");
        assert_eq!(env["PASEO_SERVICE_WEB_UI_PORT"], "3000");
        assert_eq!(
            env["PASEO_URL"],
            "http://web-ui--feature-x--cafe.localhost:6767"
        );
        let env = build_workspace_service_env("api", "app", None, None, Some(" "), None, &peers)?;
        assert_eq!(env["HOST"], "0.0.0.0");
        assert!(!env.contains_key("PASEO_URL"));
        let urls = project_service_proxy_urls(
            "app",
            Some("main"),
            "api",
            Some(6767),
            Some("https://example.test:443/path?q=yes"),
        )?;
        assert_eq!(urls["proxyUrl"], "https://api--app.example.test");
        assert_eq!(urls["localProxyUrl"], "http://api--app.localhost:6767");
        assert!(assert_no_service_env_name_collisions(&["web-ui", "web_ui"]).is_err());
        assert_eq!(service_proxy_label(&"x".repeat(100), None, "api").len(), 63);
        assert_eq!(normalize_service_env_name("straße"), "STRASSE");
        Ok(())
    }

    #[cfg(unix)]
    async fn script(root: &Path, body: &str) -> Result<PathBuf> {
        use std::os::unix::fs::PermissionsExt;
        let path = root.join("allocate");
        tokio::fs::write(&path, format!("#!/bin/sh\n{body}\n")).await?;
        tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).await?;
        Ok(path)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn port_script_receives_exact_arguments_and_rejects_invalid_output() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = script(temp.path(), r#"[ "$1" = "$PASEO_SCRIPTNAME" ] && [ "$2" = "$PASEO_WORKSPACE_ID" ] && [ "$3" = "$PASEO_BRANCH_NAME" ] && [ "$4" = "$PASEO_WORKTREE_PATH" ] || exit 2
printf ' 43210\n'"#).await?;
        let allocation = json!({"portScript":path});
        assert_eq!(
            allocate_workspace_service_port(
                Some(&allocation),
                temp.path(),
                "api",
                "workspace",
                Some("branch"),
                &BTreeSet::new()
            )
            .await?,
            43210
        );
        assert!(
            allocate_workspace_service_port(
                Some(&allocation),
                temp.path(),
                "api",
                "workspace",
                None,
                &BTreeSet::from([43210])
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("reserved port 43210")
        );
        script(temp.path(), "printf '43210 extra'").await?;
        assert!(
            allocate_workspace_service_port(
                Some(&allocation),
                temp.path(),
                "api",
                "workspace",
                None,
                &BTreeSet::new()
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("must print exactly one TCP port")
        );
        script(temp.path(), "printf '65536'").await?;
        assert!(
            allocate_workspace_service_port(
                Some(&allocation),
                temp.path(),
                "api",
                "workspace",
                None,
                &BTreeSet::new()
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("invalid TCP port")
        );
        script(
            temp.path(),
            "i=0; while [ \"$i\" -lt 1100 ]; do printf x >&2; i=$((i+1)); done; printf 43210",
        )
        .await?;
        assert!(
            allocate_workspace_service_port(
                Some(&allocation),
                temp.path(),
                "api",
                "workspace",
                None,
                &BTreeSet::new()
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("stderr maxBuffer length exceeded")
        );
        script(temp.path(), "printf boom >&2; exit 3").await?;
        assert_eq!(
            allocate_workspace_service_port(
                Some(&allocation),
                temp.path(),
                "api",
                "workspace",
                None,
                &BTreeSet::new()
            )
            .await
            .unwrap_err()
            .to_string(),
            format!(
                "Service port script '{}' failed: Command failed: {} api workspace  {}\nboom",
                path.display(),
                path.display(),
                temp.path().display()
            )
        );
        let missing = temp.path().join("missing");
        assert_eq!(
            allocate_workspace_service_port(
                Some(&json!({"portScript":missing})),
                temp.path(),
                "api",
                "workspace",
                None,
                &BTreeSet::new()
            )
            .await
            .unwrap_err()
            .to_string(),
            format!(
                "Service port script '{}' failed: spawn {} ENOENT",
                missing.display(),
                missing.display()
            )
        );
        Ok(())
    }

    #[tokio::test]
    async fn explicit_ports_preserve_finite_js_numbers_without_tcp_validation() -> Result<()> {
        let registry = RuntimePorts::new();
        let temp = tempfile::tempdir()?;
        let services = [
            ServicePortDeclaration {
                script_name: "negative".into(),
                port: Some(-1.5),
            },
            ServicePortDeclaration {
                script_name: "huge".into(),
                port: Some(1e25),
            },
            ServicePortDeclaration {
                script_name: "zero".into(),
                port: Some(-0.0),
            },
        ];
        let plan = registry
            .ensure_plan("one", temp.path(), None, &services, None)
            .await?;
        assert_eq!(plan["negative"], -1.5);
        assert_eq!(plan["huge"], 1e25);
        let duplicate = ServicePortDeclaration {
            script_name: "other-zero".into(),
            port: Some(0.0),
        };
        assert!(
            registry
                .ensure_plan("one", temp.path(), None, &[duplicate], None)
                .await
                .unwrap_err()
                .to_string()
                .contains("duplicate port 0")
        );
        let peers: Vec<_> = services
            .iter()
            .map(|service| ServicePeer {
                script_name: service.script_name.clone(),
                port: service.port.unwrap(),
            })
            .collect();
        let env =
            build_workspace_service_env("negative", "project", None, None, None, None, &peers)?;
        assert_eq!(env["PASEO_PORT"], "-1.5");
        assert_eq!(env["PASEO_SERVICE_HUGE_PORT"], "1e+25");
        assert_eq!(env["PASEO_SERVICE_ZERO_PORT"], "0");
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn release_cancels_owner_and_waiters_without_republishing_ports() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = script(
            temp.path(),
            "touch started\nwhile [ ! -e proceed ]; do sleep 0.01; done\nprintf 43210",
        )
        .await?;
        let allocation = json!({"portScript":path});
        let registry = RuntimePorts::new();
        let task = {
            let registry = registry.clone();
            let cwd = temp.path().to_owned();
            let allocation = allocation.clone();
            tokio::spawn(async move {
                registry
                    .ensure_plan("one", &cwd, None, &[dynamic("api")], Some(&allocation))
                    .await
            })
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            while !tokio::fs::try_exists(temp.path().join("started"))
                .await
                .unwrap()
            {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await?;
        let services = [dynamic("api")];
        let waiting = registry.ensure_plan("one", temp.path(), None, &services, None);
        tokio::pin!(waiting);
        tokio::select! {
            result = &mut waiting => panic!("pending plan finished early: {result:?}"),
            _ = tokio::time::sleep(Duration::from_millis(10)) => {}
        }
        registry.release_plan("one");
        tokio::fs::write(temp.path().join("proceed"), b"").await?;
        assert!(
            task.await?
                .unwrap_err()
                .to_string()
                .contains("was released")
        );
        assert!(
            waiting
                .await
                .unwrap_err()
                .to_string()
                .contains("was released")
        );
        let state = registry.state.lock().unwrap();
        assert!(state.owners.is_empty());
        assert!(state.plans.is_empty());
        assert!(state.pending.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn setup_port_persists_only_with_metadata_and_never_replaces_busy_port() -> Result<()> {
        let temp = tempfile::tempdir()?;
        git(temp.path(), &["init", "-b", "main"]).await?;
        let unpersisted = ensure_worktree_runtime_port(temp.path()).await?;
        assert!((1.0..=65535.0).contains(&unpersisted));
        assert_eq!(read_worktree_metadata(temp.path()).await?, None);
        write_worktree_metadata(temp.path(), &json!({"baseRefName":"main"})).await?;
        let port = ensure_worktree_runtime_port(temp.path()).await?;
        assert_eq!(read_worktree_runtime_port(temp.path()).await?, Some(port));
        assert_eq!(ensure_worktree_runtime_port(temp.path()).await?, port);
        let listener = bind_worktree_port(port as u16).await?;
        assert_eq!(
            ensure_worktree_runtime_port(temp.path())
                .await
                .unwrap_err()
                .to_string(),
            format!("Persisted worktree port {port} is already in use"),
        );
        assert_eq!(read_worktree_runtime_port(temp.path()).await?, Some(port));
        drop(listener);
        assert_eq!(ensure_worktree_runtime_port(temp.path()).await?, port);
        // Storage accepts any positive JS integer, but binding must not truncate
        // a persisted value into a different TCP port.
        write_worktree_runtime_metadata(temp.path(), 70000.0).await?;
        assert!(
            ensure_worktree_runtime_port(temp.path())
                .await
                .unwrap_err()
                .to_string()
                .contains("Received type number (70000)")
        );
        assert_eq!(
            read_worktree_runtime_port(temp.path()).await?,
            Some(70000.0)
        );
        Ok(())
    }
}
