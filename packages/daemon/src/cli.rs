mod agents;
mod catalog;
mod hooks;
pub mod output;
mod services;
pub(crate) mod system;
use crate::client::{ConnectOptions, DaemonClient, DaemonTarget};
use anyhow::{Result, anyhow, bail, ensure};
use clap::{Arg, ArgAction, ArgMatches, Command};
use output::{Format, Output, OutputOptions};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio_util::sync::CancellationToken;

pub struct Context {
    pub target: DaemonTarget,
    pub output: OutputOptions,
    pub cancel: CancellationToken,
}
impl Context {
    pub async fn connect(&self) -> Result<DaemonClient> {
        DaemonClient::connect(ConnectOptions::new(self.target.clone()),self.cancel.clone()).await.map_err(|error|{
            let code=error.downcast_ref::<crate::client::ClientError>().map(|error|error.code.as_str()).or_else(||error.downcast_ref::<crate::daemon_control::DaemonInstanceError>().map(|error|error.code.as_str())).unwrap_or("DAEMON_UNREACHABLE");
            let description=self.target.description();let mut message=error.to_string();if let DaemonTarget::Endpoint{host}=&self.target{message=message.replace(host,&description);}
            if !message.starts_with("Cannot connect to daemon at "){message=format!("Cannot connect to daemon at {description}: {message}");}
            let details=match code{"AUTH_REQUIRED"=>"The daemon requires a password. Set PASEO_PASSWORD and retry.".into(),"AUTH_FAILED"=>"The daemon rejected the password. Check PASEO_PASSWORD and retry.".into(),_=>match &self.target{DaemonTarget::Instance{home}=>format!("Start with: paseo daemon start --home {}",serde_json::to_string(&home.to_string_lossy()).expect("path string serializes")),DaemonTarget::Endpoint{..}=>"Check the selected endpoint and credentials. SSH transport does not install or start the daemon.".into()}};
            crate::client::ClientError::new(code,message).details(details).into()
        })
    }
    pub fn local_home(&self) -> Result<PathBuf> {
        match &self.target {
            DaemonTarget::Instance { home } => Ok(home.clone()),
            _ => bail!("This is a local operation; use --home. --host is not supported."),
        }
    }
}
pub fn option(id: &'static str, long: &'static str) -> Arg {
    Arg::new(id).long(long).num_args(1)
}
pub fn switch(id: &'static str, long: &'static str) -> Arg {
    Arg::new(id).long(long).action(ArgAction::SetTrue)
}
pub fn positional(id: &'static str, required: bool) -> Arg {
    Arg::new(id).required(required).num_args(1)
}
pub fn multiple(id: &'static str, long: &'static str) -> Arg {
    option(id, long).action(ArgAction::Append)
}
pub fn string<'a>(matches: &'a ArgMatches, key: &str) -> Option<&'a str> {
    matches
        .try_get_one::<String>(key)
        .ok()
        .flatten()
        .map(String::as_str)
}
pub fn strings(matches: &ArgMatches, key: &str) -> Vec<String> {
    matches
        .try_get_many::<String>(key)
        .ok()
        .flatten()
        .map(|v| v.cloned().collect())
        .unwrap_or_default()
}
pub fn flag(matches: &ArgMatches, key: &str) -> bool {
    matches
        .try_get_one::<bool>(key)
        .ok()
        .flatten()
        .copied()
        .unwrap_or(false)
}
pub fn number(matches: &ArgMatches, key: &str) -> Result<Option<u64>> {
    string(matches, key)
        .map(|s| {
            s.parse()
                .map_err(|_| anyhow!("--{key} must be a non-negative integer"))
        })
        .transpose()
}
pub fn required<'a>(matches: &'a ArgMatches, key: &str) -> Result<&'a str> {
    string(matches, key).ok_or_else(|| anyhow!("Missing required {key}"))
}
pub fn parse_duration(input: &str) -> Result<Duration> {
    let text = input.trim();
    ensure!(
        !text.is_empty(),
        "Invalid duration format: {input}. Use formats like: 5m, 30s, 1h, 2h30m, 1d"
    );
    if text.bytes().all(|b| b.is_ascii_digit()) {
        return Ok(Duration::from_secs(text.parse()?));
    }
    let mut start = 0;
    let mut total = 0u64;
    for (index, byte) in text.bytes().enumerate() {
        if byte.is_ascii_digit() {
            continue;
        }
        ensure!(index > start, "Invalid duration format: {input}");
        let value: u64 = text[start..index].parse()?;
        let unit = match byte {
            b's' => 1,
            b'm' => 60,
            b'h' => 3600,
            b'd' => 86400,
            _ => bail!("Invalid duration format: {input}"),
        };
        total = total
            .checked_add(
                value
                    .checked_mul(unit)
                    .ok_or_else(|| anyhow!("Duration is too large"))?,
            )
            .ok_or_else(|| anyhow!("Duration is too large"))?;
        start = index + 1;
    }
    ensure!(start == text.len(), "Invalid duration format: {input}");
    Ok(Duration::from_secs(total))
}
pub fn provider_model(
    provider: Option<&str>,
    model: Option<&str>,
    default_provider: Option<&str>,
) -> Result<(String, Option<String>)> {
    let provider=provider.map(str::trim).filter(|v|!v.is_empty()).or(default_provider).ok_or_else(||crate::client::ClientError::new("MISSING_PROVIDER","Provider is required. Set a CLI default profile in Paseo host settings, or pass --provider <provider>."))?;
    let model = match model {
        Some(value) => {
            if value.trim().is_empty() {
                return Err(crate::client::ClientError::new(
                    "INVALID_MODEL",
                    "--model cannot be empty",
                )
                .into());
            }
            Some(value.trim())
        }
        None => None,
    };
    if let Some((provider, embedded)) = provider.split_once('/') {
        let (provider, embedded) = (provider.trim(), embedded.trim());
        if provider.is_empty() || embedded.is_empty() {
            return Err(crate::client::ClientError::new(
                "INVALID_PROVIDER",
                "Invalid --provider value",
            )
            .into());
        }
        if model.is_some_and(|model| model != embedded) {
            return Err(crate::client::ClientError::new(
                "CONFLICTING_MODEL_OPTIONS",
                "Conflicting model values provided",
            )
            .into());
        }
        Ok((provider.into(), Some(model.unwrap_or(embedded).into())))
    } else {
        Ok((provider.into(), model.map(str::to_owned)))
    }
}
pub fn build_command() -> Command {
    let mut command = Command::new("paseo")
        .about("Paseo CLI - control your AI coding agents from the command line")
        .version(env!("CARGO_PKG_VERSION"))
        .disable_version_flag(true)
        .arg(
            Arg::new("version")
                .short('v')
                .long("version")
                .action(ArgAction::Version),
        )
        .arg(
            option("format", "format")
                .short('o')
                .default_value("table")
                .global(true),
        )
        .arg(switch("json", "json").global(true))
        .arg(switch("quiet", "quiet").short('q').global(true))
        .arg(switch("no-headers", "no-headers").global(true))
        .arg(switch("no-color", "no-color").global(true))
        .arg(multiple("home", "home").global(true))
        .arg(multiple("host", "host").global(true));
    for mut child in agents::commands()
        .into_iter()
        .chain(catalog::commands())
        .chain(services::commands())
        .chain(system::commands())
    {
        if child.get_name() != "plugin" {
            child = child
                .version(env!("CARGO_PKG_VERSION"))
                .disable_version_flag(true)
                .arg(
                    Arg::new("version")
                        .short('v')
                        .long("version")
                        .action(ArgAction::Version),
                );
        }
        command = command.subcommand(require_subcommand(child));
    }
    command
        .subcommand(
            Command::new("hooks")
                .about("Record agent hook activity")
                .arg(positional("agent", true))
                .arg(positional("event", true)),
        )
        .subcommand(require_subcommand(
            Command::new("mcp")
                .about("Native MCP transport")
                .subcommand(
                    Command::new("stdio-socket-bridge")
                        .arg(option("socket", "socket").required(true)),
                ),
        ))
}
fn require_subcommand(command: Command) -> Command {
    let command = command.mut_subcommands(require_subcommand);
    if command.get_subcommands().next().is_some() {
        command
            .subcommand_required(true)
            .arg_required_else_help(true)
    } else {
        command
    }
}
pub async fn run() -> Result<i32> {
    run_args(std::env::args().skip(1).collect()).await
}
pub async fn run_args(mut args: Vec<String>) -> Result<i32> {
    if matches!(
        args.first().map(String::as_str),
        Some("__supervisor" | "__worker" | "__update")
    ) {
        return crate::daemon_control::run_internal(&args[0], &args[1..]).await;
    }
    if args.first().is_some_and(|s| s == "desktop-host") {
        ensure!(
            args.len() == 1,
            "desktop-host accepts requests on stdin, not arguments"
        );
        return crate::desktop_host::run().await;
    }
    #[cfg(windows)]
    if args.first().is_some_and(|s| s == "__paseo_pty_child") {
        return crate::terminals::windows::run_child();
    }
    let command = build_command();
    let known: Vec<_> = command.get_subcommands().map(|c| c.get_name()).collect();
    if let Some(first) = args.first() {
        if !first.starts_with('-') && !known.contains(&first.as_str()) {
            let path = crate::client::resolve_home(Some(first))?;
            if tokio::fs::metadata(&path).await.is_ok_and(|m| m.is_dir()) {
                open_desktop(&[path.to_string_lossy().into_owned()]).await?;
                return Ok(0);
            }
        }
    }
    if args
        .first()
        .is_some_and(|s| s == "--relay" || s == "--no-relay")
    {
        args.insert(0, "onboard".into());
    } else if args.is_empty() {
        args.push("chat".into());
    } else if args.first().is_some_and(|s| s.starts_with('-'))
        && !args.iter().any(|s| {
            known.contains(&s.as_str())
                || matches!(s.as_str(), "--help" | "-h" | "--version" | "-v")
        })
    {
        args.insert(0, "chat".into());
    }
    let parsed = match command
        .try_get_matches_from(std::iter::once("paseo".to_owned()).chain(args.iter().cloned()))
    {
        Ok(m) => m,
        Err(e) => {
            e.print()?;
            return Ok(if e.use_stderr() { 1 } else { 0 });
        }
    };
    let mut path = Vec::new();
    let mut leaf = &parsed;
    while let Some((name, matches)) = leaf.subcommand() {
        path.push(name);
        leaf = matches;
    }
    if path == ["speech"] {
        let mut help = build_command();
        help.build();
        help.find_subcommand_mut("speech")
            .expect("registered speech command")
            .print_help()?;
        output::stdout_chunk("\n")?;
        return Ok(0);
    }
    if path == ["hooks"] {
        hooks::run(required(leaf, "agent")?, required(leaf, "event")?).await?;
        return Ok(0);
    }
    if path == ["mcp", "stdio-socket-bridge"] {
        crate::mcp::stdio_socket_bridge(Path::new(required(leaf, "socket")?)).await?;
        return Ok(0);
    }
    let format = if flag(leaf, "json") {
        Format::Json
    } else {
        match string(leaf, "format")
            .unwrap_or("table")
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "table" | "cli" => Format::Table,
            "json" => Format::Json,
            "yaml" => Format::Yaml,
            other => {
                bail!("Unsupported output format: {other}. Supported formats: table, json, yaml")
            }
        }
    };
    let options = OutputOptions {
        format,
        quiet: flag(leaf, "quiet"),
        headers: !flag(leaf, "no-headers"),
        color: !flag(leaf, "no-color"),
    };
    let result = async {
        let homes = strings(leaf, "home");
        let hosts = strings(leaf, "host");
        for (name, values) in [("home", &homes), ("host", &hosts)] {
            if values.iter().any(|v| Some(v) != values.first()) {
                bail!("Conflicting duplicate --{name} selectors.");
            }
        }
        let local_only = matches!(
            path.as_slice(),
            ["start"]
                | ["onboard"]
                | ["daemon", "start" | "run" | "set-password"]
                | ["daemon", "config", ..]
        );
        let target = DaemonTarget::select(
            homes.first().map(String::as_str),
            hosts.first().map(String::as_str),
            local_only,
        )?;
        let cancel = CancellationToken::new();
        let signal_cancel = cancel.clone();
        #[cfg(unix)]
        let signal = {
            use tokio::signal::unix::{SignalKind, signal};
            let mut interrupt = signal(SignalKind::interrupt())?;
            let mut terminate = signal(SignalKind::terminate())?;
            tokio::spawn(async move {
                tokio::select! {_=interrupt.recv()=>{},_=terminate.recv()=>{}}
                signal_cancel.cancel();
            })
        };
        #[cfg(not(unix))]
        let signal = tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                signal_cancel.cancel();
            }
        });
        let context = Context {
            target,
            output: options.clone(),
            cancel: cancel.clone(),
        };
        let result = dispatch(&path, leaf, &context).await;
        signal.abort();
        cancel.cancel();
        result
    }
    .await;
    match result {
        Ok(result) => {
            output::stdout(&result.render(&options)?)?;
            Ok(result.exit_code)
        }
        Err(error) => {
            output::error(&error, &options)?;
            Ok(1)
        }
    }
}
async fn dispatch(path: &[&str], matches: &ArgMatches, context: &Context) -> Result<Output> {
    if let Some(output) = agents::execute(path, matches, context).await? {
        return Ok(output);
    }
    if let Some(output) = catalog::execute(path, matches, context).await? {
        return Ok(output);
    }
    if let Some(output) = services::execute(path, matches, context).await? {
        return Ok(output);
    }
    if let Some(output) = system::execute(path, matches, context).await? {
        return Ok(output);
    }
    bail!("Choose a subcommand; run paseo {} --help", path.join(" "))
}
fn desktop_application() -> Result<PathBuf> {
    let executable = std::env::current_exe()?;
    let runtime = executable
        .parent()
        .ok_or_else(|| anyhow!("Executable directory unavailable"))?;
    #[cfg(target_os = "macos")]
    {
        if let Some(bundle) = runtime
            .ancestors()
            .nth(3)
            .filter(|path| path.extension().is_some_and(|extension| extension == "app"))
        {
            if bundle.is_dir() {
                return Ok(bundle.to_owned());
            }
        }
        let installed = PathBuf::from("/Applications/Paseo Sessions.app");
        if installed.is_dir() {
            return Ok(installed);
        }
        if let Some(home) = dirs::home_dir() {
            let installed = home.join("Applications/Paseo Sessions.app");
            if installed.is_dir() {
                return Ok(installed);
            }
        }
    }
    #[cfg(any(target_os = "linux", windows))]
    {
        let names: &[&str] = if cfg!(windows) {
            &["paseo_sessions.exe"]
        } else {
            &["paseo-sessions", "paseo_sessions"]
        };
        if runtime.file_name().is_some_and(|name| name == "runtime") {
            if let Some(parent) = runtime.parent() {
                let bundled = parent.join(if cfg!(windows) {
                    "paseo_sessions.exe"
                } else {
                    "paseo_sessions"
                });
                if bundled.is_file() {
                    return Ok(bundled);
                }
            }
        }
        if let Some(search) = std::env::var_os("PATH") {
            for directory in std::env::split_paths(&search) {
                for name in names {
                    let candidate = directory.join(name);
                    if candidate.is_file() {
                        return Ok(candidate);
                    }
                }
            }
        }
        #[cfg(target_os = "linux")]
        {
            for installed in [
                "/usr/bin/paseo-sessions",
                "/opt/paseo-sessions/paseo_sessions",
            ] {
                if Path::new(installed).is_file() {
                    return Ok(PathBuf::from(installed));
                }
            }
            if let Some(home) = dirs::home_dir() {
                let installed = home.join(".local/bin/paseo-sessions");
                if installed.is_file() {
                    return Ok(installed);
                }
            }
        }
        #[cfg(windows)]
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            let installed = PathBuf::from(local).join("Programs/Paseo Sessions/paseo_sessions.exe");
            if installed.is_file() {
                return Ok(installed);
            }
        }
    }
    bail!("Paseo desktop app not found. Install the native Paseo Sessions desktop application.")
}

pub(super) async fn open_agent_desktop(server: &str, agent: &str) -> Result<()> {
    let mut link = url::Url::parse("paseo://h").expect("constant hierarchical app URL");
    link.path_segments_mut()
        .expect("hierarchical app URL")
        .extend([server, "agent", agent]);
    open_desktop(&[link.into()]).await
}

pub async fn open_desktop(args: &[String]) -> Result<()> {
    let path = desktop_application()?;
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = tokio::process::Command::new("open");
        command.arg("-a").arg(path);
        command
    };
    #[cfg(not(target_os = "macos"))]
    let mut command = tokio::process::Command::new(path);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    #[cfg(windows)]
    command.creation_flags(0x00000008 | 0x00000200);
    let mut child = command.spawn()?;
    tokio::spawn(async move {
        let _ = child.wait().await;
    });
    Ok(())
}
