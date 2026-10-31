use crate::state::AppState;
use anyhow::{Context, Result};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    path::Path,
    time::{Duration, Instant},
};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

fn section(title: &str, entries: impl IntoIterator<Item = (String, String)>) -> String {
    let mut output = title.to_owned();
    for (label, value) in entries {
        output.push_str(&format!("\n  {label}: {value}"));
    }
    output
}
fn count<'a>(items: impl Iterator<Item = &'a Value>, key: &str) -> String {
    let mut counts = BTreeMap::new();
    for item in items {
        *counts
            .entry(item[key].as_str().unwrap_or("unknown"))
            .or_insert(0usize) += 1;
    }
    if counts.is_empty() {
        "none".into()
    } else {
        counts
            .into_iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}
fn bytes(value: u64) -> String {
    let units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = value as f64;
    let mut index = 0;
    while size >= 1024.0 && index < units.len() - 1 {
        size /= 1024.0;
        index += 1;
    }
    format!("{size:.1} {}", units[index])
}
fn entries<const N: usize>(items: [(&str, String); N]) -> Vec<(String, String)> {
    items.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()
}

pub async fn collect(app: &AppState, started: Instant) -> Result<String> {
    let mut sections = vec![section(
        "Paseo diagnostics",
        entries([
            ("Collected at", chrono::Utc::now().to_rfc3339()),
            (
                "Server ID",
                app.transport.identity()["serverId"]
                    .as_str()
                    .unwrap_or("unknown")
                    .into(),
            ),
            ("Daemon version", env!("CARGO_PKG_VERSION").into()),
        ]),
    )];
    let home = app.home.clone();
    let executable = app.executable.clone();
    let native = tokio::task::spawn_blocking(move || {
        let system = sysinfo::System::new_all();
        let process = system
            .process(sysinfo::Pid::from_u32(std::process::id()))
            .context("Current process metrics unavailable")?;
        let process_section = section(
            "Daemon process",
            entries([
                ("PID", std::process::id().to_string()),
                ("Runtime", "native Rust".into()),
                ("Executable", executable.display().to_string()),
                (
                    "PATH",
                    std::env::var_os("PATH")
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "unset".into()),
                ),
                (
                    "Shell",
                    std::env::var("SHELL")
                        .or_else(|_| std::env::var("ComSpec"))
                        .unwrap_or_else(|_| "unset".into()),
                ),
                ("Uptime", format!("{}s", started.elapsed().as_secs())),
                ("Paseo home", home.display().to_string()),
                ("RSS", bytes(process.memory())),
                ("Virtual memory", bytes(process.virtual_memory())),
            ]),
        );
        let load = sysinfo::System::load_average();
        let system_section = section(
            "System",
            entries([
                (
                    "OS",
                    sysinfo::System::long_os_version().unwrap_or_else(|| "unknown".into()),
                ),
                (
                    "Platform",
                    format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
                ),
                ("CPU cores", system.cpus().len().to_string()),
                (
                    "Load avg",
                    format!("{:.2}, {:.2}, {:.2}", load.one, load.five, load.fifteen),
                ),
                (
                    "Memory free",
                    format!(
                        "{} / {}",
                        bytes(system.available_memory()),
                        bytes(system.total_memory())
                    ),
                ),
            ]),
        );
        let disks = sysinfo::Disks::new_with_refreshed_list();
        let canonical = home.canonicalize()?;
        let disk = disks
            .iter()
            .filter(|disk| canonical.starts_with(disk.mount_point()))
            .max_by_key(|disk| disk.mount_point().components().count());
        let disk_section = match disk {
            Some(disk) => section(
                "Disk",
                entries([
                    ("Path", home.display().to_string()),
                    (
                        "Free",
                        format!(
                            "{} / {}",
                            bytes(disk.available_space()),
                            bytes(disk.total_space())
                        ),
                    ),
                ]),
            ),
            None => section(
                "Disk",
                entries([(
                    "Error",
                    "No matching filesystem metrics were returned".into(),
                )]),
            ),
        };
        Ok::<_, anyhow::Error>(vec![process_section, system_section, disk_section])
    })
    .await?;
    match native {
        Ok(values) => sections.extend(values),
        Err(e) => sections.push(section(
            "Native system metrics",
            entries([("Error", e.to_string())]),
        )),
    }
    let config = app.transport.config_snapshot();
    let listen = app.transport.listen_string();
    let kind = match listen.as_deref() {
        None => "not configured",
        Some(s) if s.starts_with('/') || s.starts_with("unix://") => "local socket",
        Some(s) if s.starts_with("\\\\.\\pipe\\") || s.starts_with("pipe://") => "local pipe",
        _ => "direct TCP",
    };
    sections.push(section(
        "Runtime config",
        entries([
            ("Listen", kind.into()),
            ("Relay enabled", config["relayEnabled"].to_string()),
            (
                "Relay endpoint configured",
                config["relayEndpoint"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty())
                    .to_string(),
            ),
            (
                "Relay public endpoint configured",
                config["relayPublicEndpoint"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty())
                    .to_string(),
            ),
            ("Relay TLS", config["relayUseTls"].to_string()),
            ("Relay public TLS", config["relayPublicUseTls"].to_string()),
        ]),
    ));
    match app.agents.list_snapshots().await {
        Ok(agents) => sections.push(section(
            "Agents",
            entries([
                ("Total", agents.len().to_string()),
                ("By provider", count(agents.iter(), "provider")),
                ("By lifecycle", count(agents.iter(), "lifecycle")),
                (
                    "Pending permissions",
                    agents
                        .iter()
                        .map(|v| {
                            v["pendingPermissions"]
                                .as_array()
                                .map(Vec::len)
                                .or_else(|| v["pendingPermissions"].as_object().map(|m| m.len()))
                                .unwrap_or(0)
                        })
                        .sum::<usize>()
                        .to_string(),
                ),
            ]),
        )),
        Err(e) => sections.push(section("Agents", entries([("Error", e.to_string())]))),
    }
    match tokio::try_join!(app.projects.list_projects(), app.projects.list_workspaces()) {
        Ok((projects, workspaces)) => sections.push(section(
            "Workspaces",
            entries([
                (
                    "Projects",
                    format!(
                        "{} active / {} total",
                        projects
                            .iter()
                            .filter(|v| v["archivedAt"].is_null())
                            .count(),
                        projects.len()
                    ),
                ),
                (
                    "Workspaces",
                    format!(
                        "{} active / {} total",
                        workspaces
                            .iter()
                            .filter(|v| v["archivedAt"].is_null())
                            .count(),
                        workspaces.len()
                    ),
                ),
                (
                    "Workspaces by kind",
                    count(
                        workspaces.iter().filter(|v| v["archivedAt"].is_null()),
                        "kind",
                    ),
                ),
            ]),
        )),
        Err(e) => sections.push(section("Workspaces", entries([("Error", e.to_string())]))),
    }
    match app.providers.list(None, false).await {
        Ok(providers) => sections.push(section(
            "Providers",
            entries([
                ("Total", providers.len().to_string()),
                (
                    "Available",
                    providers
                        .iter()
                        .filter(|v| v["available"] == true)
                        .count()
                        .to_string(),
                ),
                (
                    "Unavailable",
                    providers
                        .iter()
                        .filter(|v| v["available"] != true)
                        .map(|v| {
                            format!(
                                "{} ({})",
                                v["provider"].as_str().unwrap_or("unknown"),
                                v["error"].as_str().unwrap_or("unavailable")
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", "),
                ),
            ]),
        )),
        Err(e) => sections.push(section("Providers", entries([("Error", e.to_string())]))),
    }
    let metrics = app.transport.runtime_metrics();
    sections.push(section(
        "WebSocket runtime metrics",
        metrics
            .as_object()
            .into_iter()
            .flat_map(|object| object.iter().map(|(k, v)| (k.clone(), v.to_string()))),
    ));
    let (git, gh) = tokio::join!(
        tool("git", &["-c", "core.fsmonitor=false", "--version"]),
        tool("gh", &["--version"])
    );
    sections.push(section("Tools", entries([("git", git), ("gh", gh)])));
    let path = crate::daemon_control::daemon_log_path(&app.home);
    sections.push(format!(
        "Daemon log tail\n  Path: {}\n{}",
        path.display(),
        match tail(&path).await {
            Ok(value) if value.is_empty() => "  No log lines found".into(),
            Ok(value) => value,
            Err(e) => format!("  Error: {e}"),
        }
    ));
    let mut sensitive = vec![];
    if let Some(listen) = listen.as_deref() {
        sensitive.push(listen)
    }
    for key in ["relayEndpoint", "relayPublicEndpoint"] {
        if let Some(value) = config[key].as_str() {
            sensitive.push(value)
        }
    }
    Ok(redact(&sections.join("\n\n"), &sensitive))
}
async fn tool(name: &str, args: &[&str]) -> String {
    let result = async {
        let mut command = tokio::process::Command::new(name);
        command
            .args(args)
            .kill_on_drop(true)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = command.spawn()?;
        let out = child.stdout.take().context("Missing tool output")?;
        let err = child.stderr.take().context("Missing tool error output")?;
        let (stdout, stderr, status) = tokio::join!(drain_tool(out), drain_tool(err), child.wait());
        let (stdout, stderr, status) = (stdout?, stderr?, status?);
        let data = if stdout.is_empty() { &stderr } else { &stdout };
        let mut text = String::from_utf8_lossy(data).trim().to_owned();
        if !status.success() {
            text = format!("error: {status}: {text}");
        }
        Ok::<_, anyhow::Error>(text)
    };
    match tokio::time::timeout(Duration::from_secs(3), result).await {
        Ok(Ok(value)) => value,
        Ok(Err(e)) => format!("error: {e}"),
        Err(_) => "error: command timed out".into(),
    }
}
async fn drain_tool(mut input: impl tokio::io::AsyncRead + Unpin) -> Result<Vec<u8>> {
    let mut buffer = [0u8; 4096];
    let mut retained = Vec::with_capacity(512);
    loop {
        let count = input.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        let keep = count.min(512 - retained.len());
        retained.extend_from_slice(&buffer[..keep]);
    }
    Ok(retained)
}
pub(super) async fn tail(path: &Path) -> Result<String> {
    let mut file = tokio::fs::File::open(path).await?;
    let length = file.metadata().await?.len();
    let size = length.min(64 * 1024);
    file.seek(std::io::SeekFrom::Start(length - size)).await?;
    let mut data = Vec::with_capacity(size as usize);
    file.take(size).read_to_end(&mut data).await?;
    let text = String::from_utf8_lossy(&data);
    let lines = text.lines().filter(|l| !l.is_empty()).collect::<Vec<_>>();
    Ok(lines[lines.len().saturating_sub(80)..]
        .iter()
        .map(|l| format!("  {l}"))
        .collect::<Vec<_>>()
        .join("\n"))
}
pub(super) fn redact(value: &str, sensitive: &[&str]) -> String {
    use std::sync::LazyLock;
    static PATTERNS: LazyLock<[regex::Regex; 3]> = LazyLock::new(|| {
        [
        regex::Regex::new(r"(?i)paseo://\S+").unwrap(),
        regex::Regex::new(r#"(?i)([?&](?:password|token|secret|key|publicKey|daemonPublicKeyB64)=)[^&\s\"']+"#).unwrap(),
        regex::Regex::new(r#"(?i)((?:password|token|secret|authorization|api[_-]?key|daemonPublicKeyB64|relayKey)[\"']?\s*[:=]\s*)(\"[^\"]+\"|'[^']+'|[^\s,}]+)"#).unwrap(),
    ]
    });
    let mut value = value.to_owned();
    for secret in sensitive.iter().filter(|s| !s.is_empty()) {
        value = value.replace(secret, "[redacted]");
    }
    value = PATTERNS[0]
        .replace_all(&value, "paseo://[redacted]")
        .into_owned();
    for pattern in &PATTERNS[1..] {
        value = pattern.replace_all(&value, "${1}[redacted]").into_owned();
    }
    value
}
