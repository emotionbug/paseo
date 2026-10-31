use super::{duration, error, pretty, set_path};
use crate::cli::{Context, flag, option, output::Output, positional, string, switch};
use crate::{
    client::{ClientError, ConnectOptions, DaemonClient, DaemonTarget},
    daemon_control::{self as control, DaemonInstance, StartOptions, StopOptions},
};
use anyhow::{Context as _, Result, bail, ensure};
use clap::{ArgMatches, Command};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};
use tokio_util::sync::CancellationToken;

const REMOVED_FLAGS: &[(&str, &str, bool)] = &[
    ("port", "daemon.listen", true),
    ("listen", "daemon.listen", true),
    ("relay", "daemon.relay.enabled", false),
    ("no-relay", "daemon.relay.enabled", false),
    ("relay-use-tls", "daemon.relay.useTls", false),
    ("no-mcp", "daemon.mcp.enabled", false),
    ("no-inject-mcp", "daemon.mcp.injectIntoAgents", false),
    ("hostnames", "daemon.hostnames", true),
    ("allowed-hosts", "daemon.hostnames", true),
    ("foreground", "", false),
];
fn launch_options(mut command: Command) -> Command {
    for &(key, _, value) in REMOVED_FLAGS {
        command = command.arg(if value {
            option(key, key).hide(true)
        } else {
            switch(key, key).hide(true)
        });
    }
    command
}
pub fn command() -> Command {
    Command::new("daemon")
        .about("Manage the Paseo daemon")
        .subcommand_required(true)
        .subcommand(launch_options(
            Command::new("start").arg(option("timeout", "timeout")),
        ))
        .subcommand(launch_options(Command::new("run")))
        .subcommand(Command::new("status"))
        .subcommand(
            Command::new("stop")
                .arg(option("timeout", "timeout"))
                .arg(option("kill-timeout", "kill-timeout"))
                .arg(switch("force", "force")),
        )
        .subcommand(launch_options(
            Command::new("restart").arg(option("timeout", "timeout")),
        ))
        .subcommand(Command::new("reload"))
        .subcommand(Command::new("pair").arg(switch("relay", "relay")))
        .subcommand(Command::new("set-password"))
        .subcommand(
            Command::new("config")
                .subcommand_required(true)
                .subcommand(Command::new("get").arg(positional("path", false)))
                .subcommand(
                    Command::new("set")
                        .arg(positional("path", true))
                        .arg(positional("value", true))
                        .arg(switch("string", "string")),
                )
                .subcommand(Command::new("unset").arg(positional("path", true))),
        )
}
pub async fn execute(path: &[&str], m: &ArgMatches, ctx: &Context) -> Result<Output> {
    if matches!(path, ["start"] | ["run"] | ["restart"]) {
        for &(key, config, value) in REMOVED_FLAGS {
            if (value && string(m, key).is_some()) || (!value && flag(m, key)) {
                let guidance = if config.is_empty() {
                    "Use paseo daemon run --home <path> for foreground deployment.".to_owned()
                } else {
                    format!(
                        "Use paseo daemon config set {config} <value> --home <path>, then start or restart."
                    )
                };
                return Err(error(
                    "REMOVED_LAUNCH_OPTION",
                    format!(
                        "--{key} was removed. {guidance} Deployment environment overrides belong to paseo daemon run."
                    ),
                ));
            }
        }
    }
    match path {
        ["start"] | ["run"] => {
            let home = ctx.local_home()?;
            let foreground = path[0] == "run";
            let started = control::start_daemon_instance(
                StartOptions {
                    home: home.clone(),
                    foreground,
                    deployment: foreground,
                    timeout: duration(m, "timeout", 600.0)?,
                    ..Default::default()
                },
                &ctx.cancel,
            )
            .await?;
            let action = if !started.spawned {
                "already_running"
            } else if foreground {
                "exited"
            } else {
                "started"
            };
            let data = if foreground {
                json!({"action":action,"pid":started.instance.pid})
            } else {
                json!({"action":action,"home":home,"pid":started.instance.pid,"listen":started.instance.listen,"logPath":control::daemon_log_path(&home)})
            };
            let mut output = Output::new(data)
                .id("pid")
                .exit(started.exit_code.unwrap_or(0));
            if !foreground {
                output = output.human(format!(
                    "{}: PID {}{}\nLogs: {}",
                    if started.spawned {
                        "Started"
                    } else {
                        "Already running"
                    },
                    started.instance.pid,
                    started
                        .instance
                        .listen
                        .as_ref()
                        .map(|listen| format!(", listening on {listen}"))
                        .unwrap_or_else(|| ", not ready".into()),
                    control::daemon_log_path(&home).display()
                ));
            }
            Ok(output)
        }
        ["status"] => {
            let data = observe_status(&ctx.target, &ctx.cancel).await?;
            let human = status_text(&data)?;
            Ok(Output::new(data).quiet_text("daemon").human(human))
        }
        ["stop"] => {
            let timeout = duration(m, "timeout", 15.0)?;
            match &ctx.target {
                DaemonTarget::Instance { home } => {
                    let stopped = control::stop_daemon_instance(
                        home,
                        StopOptions {
                            force: flag(m, "force"),
                            timeout,
                            kill_timeout: duration(m, "kill-timeout", 3.0)?,
                            ..Default::default()
                        },
                        &ctx.cancel,
                    )
                    .await?;
                    let human = format!("{}: {}", stopped.action.replace('_', " "), home.display());
                    Ok(Output::new(json!({"action":stopped.action,"pid":stopped.pid,"forced":stopped.forced,"usedLifecycleRpc":stopped.used_lifecycle_rpc,"home":home})).id("action").human(human))
                }
                DaemonTarget::Endpoint { .. } => {
                    ensure!(
                        !flag(m, "force"),
                        "--force requires a local --home; an endpoint gives no remote process authority."
                    );
                    let client = ctx.connect().await?;
                    let result = client.shutdown(timeout).await;
                    client.close().await;
                    result?;
                    Ok(Output::new(
                        json!({"action":"shutdown_requested","host":ctx.target.description()}),
                    )
                    .id("action")
                    .human("Shutdown requested; remote process exit was not verified."))
                }
            }
        }
        ["restart"] => {
            let data = restart_confirmed(
                &ctx.target,
                None,
                "cli_restart",
                duration(m, "timeout", 600.0)?,
                &ctx.cancel,
            )
            .await?;
            let human = format!(
                "Restarted worker {} → {} at {}. Supervisor launch retained.",
                data["previousWorkerPid"],
                data["workerPid"],
                ctx.target.description()
            );
            Ok(Output::new(data).id("action").human(human))
        }
        ["reload"] => {
            let client = ctx.connect().await?;
            let result = reload(&client).await;
            client.close().await;
            let payload = result?;
            let mut data = json!({"appliedPaths":payload["appliedPaths"],"restartRequiredPaths":payload["restartRequiredPaths"],"overrideControlledPaths":payload["overrideControlledPaths"]});
            let selector = match &ctx.target {
                DaemonTarget::Instance { home } => {
                    format!("--home {}", serde_json::to_string(home)?)
                }
                DaemonTarget::Endpoint { .. } => format!(
                    "--host {}",
                    serde_json::to_string(&ctx.target.description())?
                ),
            };
            let restart = format!("paseo daemon restart {selector}");
            data["restartCommand"] = json!(restart);
            let mut human = "Configuration reloaded.".to_owned();
            for (field, heading) in [
                (
                    "restartRequiredPaths",
                    "These changes require a daemon restart:",
                ),
                (
                    "overrideControlledPaths",
                    "These settings are controlled by daemon launch overrides:",
                ),
            ] {
                if let Some(paths) = data[field].as_array().filter(|a| !a.is_empty()) {
                    human.push_str(&format!("\n\nWarning: {heading}\n"));
                    for path in paths {
                        human.push_str(&format!("  {}\n", path.as_str().unwrap_or("")));
                    }
                    if field == "restartRequiredPaths" {
                        human.push_str(&format!("\nRun: {restart}"));
                    }
                }
            }
            Ok(Output::new(data).quiet_text("daemon-config").human(human))
        }
        ["pair"] => {
            let offer = pairing(&ctx.target, flag(m, "relay"), &ctx.cancel).await?;
            if offer["relayEnabled"] != true || offer["url"].as_str().is_none_or(str::is_empty) {
                return Err(error(
                    "RELAY_DISABLED",
                    "Relay pairing is disabled for this daemon. Run paseo daemon pair --relay to enable it.",
                ));
            }
            let text = format!(
                "{}\n{}\n{}",
                offer["qr"].as_str().unwrap_or(""),
                offer["url"].as_str().unwrap_or(""),
                offer["connectionUri"].as_str().unwrap_or("")
            );
            Ok(Output::new(offer).human(text))
        }
        ["set-password"] => set_password(ctx).await,
        ["config", action] => configuration(action, m, ctx).await,
        _ => bail!("Unknown daemon command"),
    }
}
async fn set_password(ctx: &Context) -> Result<Output> {
    use std::io::IsTerminal;
    ensure!(
        std::io::stdin().is_terminal(),
        error(
            "PASSWORD_TTY_REQUIRED",
            "paseo daemon set-password needs a terminal to read the password. Run it in an interactive terminal, or set PASEO_PASSWORD in the daemon's environment instead."
        )
    );
    let home = ctx.local_home()?;
    let password = super::prompts::password(&ctx.cancel).await?;
    ensure!(
        !ctx.cancel.is_cancelled(),
        error("PASSWORD_CANCELLED", "Password update cancelled")
    );
    let mut config = crate::config::read_persisted(&home, false).await?;
    let hash = crate::transport::hash_daemon_password(&password).await?;
    set_path(&mut config, "daemon.auth.password", Some(json!(hash)))?;
    crate::config::save_persisted(&home, &config).await?;
    let restart = format!(
        "paseo daemon restart --home {}",
        serde_json::to_string(&home)?
    );
    let message = format!(
        "Password written to {}\nRestart the daemon for the change to take effect.\nRun: {restart}",
        home.join("config.json").display()
    );
    Ok(Output::new(json!({"action":"password_set","configPath":home.join("config.json"),"restartCommand":restart,"message":message})).id("action").human(message).color_column("action",|_,_|Some(32)))
}
async fn reload(client: &DaemonClient) -> Result<Value> {
    client.require_feature(
        "daemonConfigReload",
        "Update the Paseo daemon before reloading configuration.",
    )?;
    client
        .request(
            json!({"type":"daemon.config.reload.request"}),
            "daemon.config.reload.response",
            Duration::from_secs(30),
        )
        .await
}
pub(crate) async fn assert_instance(home: &Path, expected: &DaemonInstance) -> Result<()> {
    let current = control::read_daemon_instance(home).await?;
    if !current
        .as_ref()
        .is_some_and(|current| control::same_daemon_instance(expected, current))
    {
        return Err(error(
            "DAEMON_REPLACED",
            "Daemon changed since confirmation; inspect its current home, PID, and start time.",
        ));
    }
    Ok(())
}
pub(crate) async fn observe_status(
    target: &DaemonTarget,
    cancel: &CancellationToken,
) -> Result<Value> {
    let instance = match target {
        DaemonTarget::Instance { home } => control::read_daemon_instance(home).await?,
        _ => None,
    };
    let mut local = match target {
        DaemonTarget::Instance { home } => {
            let config = crate::config::resolve_persisted_for_pairing(home).await?;
            json!({"home":home,"pid":instance.as_ref().map(|i|i.pid),"startedAt":instance.as_ref().map(|i|&i.started_at),"listen":instance.as_ref().and_then(|i|i.listen.as_ref()),"serverId":instance.as_ref().and_then(|i|i.server_id.as_ref()),"hostname":instance.as_ref().map(|i|&i.hostname),"configuredListen":config["listen"],"localDaemon":if instance.is_none(){"stopped"}else if instance.as_ref().unwrap().listen.is_some(){"running"}else{"not_ready"},"desktopManaged":instance.as_ref().is_some_and(|i|i.desktop_managed),"logPath":control::daemon_log_path(home)})
        }
        _ => json!({"host":target.description()}),
    };
    if matches!(target, DaemonTarget::Instance { .. })
        && instance.as_ref().is_none_or(|i| i.listen.is_none())
    {
        local["connectedDaemon"] = json!("not_probed");
        return Ok(local);
    }
    let mut options = ConnectOptions::new(target.clone());
    options.instance = instance.clone();
    options.timeout = Duration::from_millis(1500);
    let client = match DaemonClient::connect(options, cancel.child_token()).await {
        Ok(client) => client,
        Err(failure) => {
            if matches!(target, DaemonTarget::Endpoint { .. }) {
                return Err(failure);
            }
            let code = failure
                .downcast_ref::<ClientError>()
                .map(|e| e.code.as_str());
            local["connectedDaemon"] = json!(match code {
                Some("AUTH_REQUIRED") => "auth_required",
                Some("AUTH_FAILED") => "auth_failed",
                _ => "unreachable",
            });
            local["note"] = json!(failure.to_string());
            return Ok(local);
        }
    };
    let result = async {
        let status = client.daemon_status(Duration::from_millis(1500)).await;
        if let (DaemonTarget::Instance { home }, Some(instance)) = (target, instance.as_ref()) {
            assert_instance(home, instance).await?;
        }
        let info = client.server_info();
        local["serverId"] = info["serverId"].clone();
        local["daemonVersion"] = info["version"].clone();
        local["connectedDaemon"] = json!(if client.cancellation().is_cancelled() {
            "unreachable"
        } else {
            "reachable"
        });
        match status {
            Ok(status) => {
                for (key, source) in [
                    ("daemonVersion", "version"),
                    ("workerPid", "pid"),
                    ("daemonExecutable", "nodePath"),
                    ("providers", "providers"),
                    ("relay", "relay"),
                ] {
                    if let Some(value) = status.get(source) {
                        local[key] = value.clone();
                    }
                }
            }
            Err(failure) => {
                if matches!(target, DaemonTarget::Endpoint { .. })
                    || client.cancellation().is_cancelled()
                    || failure
                        .downcast_ref::<ClientError>()
                        .is_none_or(|e| e.code != "DAEMON_REQUEST_TIMEOUT")
                {
                    return Err(failure);
                }
                local["note"] = json!(format!("Status details unavailable: {failure}"));
            }
        }
        Ok(local)
    }
    .await;
    client.close().await;
    result
}
fn reconnect_failure(error: &anyhow::Error) -> bool {
    let code = error
        .downcast_ref::<ClientError>()
        .map(|e| e.code.as_str())
        .or_else(|| {
            error
                .downcast_ref::<control::DaemonInstanceError>()
                .map(|e| e.code.as_str())
        });
    matches!(
        code,
        Some(
            "DAEMON_CONNECTION_LOST"
                | "DAEMON_REQUEST_TIMEOUT"
                | "DAEMON_UNREACHABLE"
                | "DAEMON_NOT_READY"
        )
    ) || error.chain().any(|cause| {
        cause.downcast_ref::<std::io::Error>().is_some_and(|error| {
            matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionRefused
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::NotConnected
                    | std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::TimedOut
                    | std::io::ErrorKind::UnexpectedEof
            )
        })
    })
}
pub(crate) async fn restart_confirmed(
    target: &DaemonTarget,
    pinned: Option<DaemonInstance>,
    reason: &str,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<Value> {
    let instance = match target {
        DaemonTarget::Instance { home } => Some(match pinned {
            Some(i) => i,
            None => control::read_daemon_instance(home)
                .await?
                .ok_or_else(|| error("DAEMON_NOT_RUNNING", "Daemon is not running."))?,
        }),
        _ => None,
    };
    let deadline = tokio::time::Instant::now() + timeout;
    let remaining = || {
        deadline
            .saturating_duration_since(tokio::time::Instant::now())
            .max(Duration::from_millis(1))
    };
    let connect = |timeout| {
        let mut options = ConnectOptions::new(target.clone());
        options.instance = instance.clone();
        options.timeout = timeout;
        DaemonClient::connect(options, cancel.child_token())
    };
    let client = connect(remaining()).await?;
    let previous = async {
        let worker = client.daemon_status(remaining()).await?;
        ensure!(
            worker["pid"].as_u64().is_some_and(|pid| pid > 0),
            "Daemon status did not identify the current worker PID"
        );
        let server_id = client.server_info()["serverId"]
            .as_str()
            .filter(|id| !id.is_empty())
            .context("Daemon greeting did not identify the server")?
            .to_owned();
        if let (DaemonTarget::Instance { home }, Some(instance)) = (target, instance.as_ref()) {
            assert_instance(home, instance).await?;
        }
        let ack = match client.restart(reason, remaining()).await {
            Ok(_) => true,
            Err(error) if reconnect_failure(&error) => false,
            Err(error) => return Err(error),
        };
        Ok((worker["pid"].clone(), json!(server_id), ack))
    }
    .await;
    client.close().await;
    let (worker, server_id, acknowledged) = previous?;
    let mut last = "No replacement worker observed".to_owned();
    while tokio::time::Instant::now() < deadline {
        if let (DaemonTarget::Instance { home }, Some(instance)) = (target, instance.as_ref()) {
            assert_instance(home, instance).await?;
        }
        let observed = match connect(remaining().min(Duration::from_secs(1))).await {
            Ok(client) => {
                let result = async {
                    ensure!(
                        client.server_info()["serverId"] == server_id,
                        "Connected peer identity changed."
                    );
                    let status = client.daemon_status(remaining()).await?;
                    ensure!(
                        status["pid"].as_u64().is_some_and(|pid| pid > 0),
                        "Daemon status did not identify the replacement worker PID"
                    );
                    Ok(status)
                }
                .await;
                client.close().await;
                result
            }
            Err(error) => Err(error),
        };
        match observed {
            Ok(status) if status["pid"] != worker => {
                if let (DaemonTarget::Instance { home }, Some(instance)) =
                    (target, instance.as_ref())
                {
                    assert_instance(home, instance).await?;
                }
                return Ok(
                    json!({"action":"restarted","target":target.description(),"supervisorPid":instance.as_ref().map(|i|i.pid),"previousWorkerPid":worker,"workerPid":status["pid"],"acknowledged":acknowledged}),
                );
            }
            Ok(_) => {}
            Err(error) if reconnect_failure(&error) => last = error.to_string(),
            Err(error) => return Err(error),
        }
        tokio::select! {_=cancel.cancelled()=>bail!("Restart cancelled"),_=tokio::time::sleep(remaining().min(Duration::from_millis(100)))=>{}}
    }
    Err(error(
        "RESTART_NOT_CONFIRMED",
        format!(
            "Replacement worker was not confirmed. Restart acknowledged: {acknowledged}. {last}"
        ),
    ))
}
async fn configuration(action: &str, m: &ArgMatches, ctx: &Context) -> Result<Output> {
    let home = ctx.local_home()?;
    let path = string(m, "path");
    if action == "get" {
        if let Some(path) = path {
            crate::config::validate_persisted_path(path)?;
        }
        let config = crate::config::read_persisted(&home, false).await?;
        let value = path
            .map(|p| p.split('.').try_fold(&config, |v, key| v.get(key)))
            .unwrap_or(Some(&config));
        let redacted = value
            .map(|v| redact(v, path.is_some_and(secret)))
            .unwrap_or(Value::Null);
        return Ok(pretty(
            json!({"source":"configured","path":path,"set":value.is_some(),"value":redacted}),
        )?
        .quiet_text("configuration"));
    }
    let path = path.context("Configuration path required")?;
    let value = if action == "unset" {
        None
    } else {
        let raw = string(m, "value").context("Configuration value required")?;
        Some(if flag(m, "string") {
            json!(raw)
        } else {
            serde_json::from_str(raw).unwrap_or(json!(raw))
        })
    };
    crate::config::edit_persisted(&home, path, value).await?;
    let running = control::read_daemon_instance(&home)
        .await?
        .is_some_and(|i| i.listen.is_some());
    if running {
        if let Ok(client) = ctx.connect().await {
            let result = reload(&client).await;
            client.close().await;
            let mut result = result.map_err(|e| {
                error(
                    "CONFIG_SAVED_RELOAD_FAILED",
                    format!("Saved; reload failed: {e}"),
                )
            })?;
            result["action"] = json!("saved");
            return Ok(pretty(result)?.quiet_text("configuration"));
        }
    }
    Ok(pretty(json!({"action":"saved","applied":false,"message":"Saved; not applied to a running daemon","nextCommand":format!("paseo daemon {} --home {}",if running{"reload"}else{"start"},serde_json::to_string(&home)?)}))?.quiet_text("configuration"))
}
fn secret(key: &str) -> bool {
    let key = key.to_lowercase().replace(['_', '-'], "");
    [
        "password",
        "apikey",
        "privatekey",
        "accesskey",
        "token",
        "secret",
        "authorization",
    ]
    .iter()
    .any(|part| key.contains(part))
}
fn redact(value: &Value, hidden: bool) -> Value {
    if hidden {
        return json!("[redacted]");
    }
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), redact(v, secret(k))))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(|v| redact(v, false)).collect()),
        _ => value.clone(),
    }
}
pub async fn pairing(
    target: &DaemonTarget,
    enable: bool,
    cancel: &CancellationToken,
) -> Result<Value> {
    if let DaemonTarget::Instance { home } = target {
        if control::read_daemon_instance(home).await?.is_none() {
            if enable {
                let mut config = crate::config::read_persisted(home, true).await?;
                set_path(&mut config, "daemon.relay.enabled", Some(json!(true)))?;
                crate::config::save_persisted(home, &config).await?;
            }
            return pairing_result(crate::transport::offline_pairing_offer(home).await?);
        }
    }
    let mut options = ConnectOptions::new(target.clone());
    options.timeout = Duration::from_millis(1500);
    let client = DaemonClient::connect(options, cancel.child_token()).await?;
    let result = async {
        client.require_feature(
            "daemonStatusRpc",
            "Update the Paseo daemon before pairing from this command.",
        )?;
        let request = || {
            client.request(
                json!({"type":"daemon.get_pairing_offer.request"}),
                "daemon.get_pairing_offer.response",
                Duration::from_millis(1500),
            )
        };
        let mut offer = request().await?;
        if offer["relayEnabled"] != true && enable {
            client.require_feature(
                "relayConfig",
                "Update the Paseo daemon before enabling relay from this command.",
            )?;
            client
                .request(
                    json!({"type":"set_daemon_config_request","config":{"relay":{"enabled":true}}}),
                    "set_daemon_config_response",
                    Duration::from_secs(30),
                )
                .await?;
            offer = request()
                .await
                .context("Relay configuration was saved, but fetching the pairing offer failed")?;
        }
        pairing_result(offer)
    }
    .await;
    client.close().await;
    result
}
fn pairing_result(offer: Value) -> Result<Value> {
    use base64::Engine;
    let url = offer["url"].as_str().filter(|s| !s.is_empty());
    let connection = if let Some((_, encoded)) = url.and_then(|url| url.split_once("#offer=")) {
        let payload: Value = serde_json::from_slice(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(encoded.trim().trim_end_matches('='))?,
        )?;
        ensure!(payload["v"] == 2, "Invalid pairing offer version");
        let required = |key: &str| {
            payload[key]
                .as_str()
                .filter(|s| !s.is_empty())
                .context("Malformed pairing offer")
        };
        let endpoint = payload["relay"]["endpoint"]
            .as_str()
            .filter(|s| !s.is_empty())
            .context("Missing relay endpoint")?;
        const COMPONENT: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
            .remove(b'-')
            .remove(b'_')
            .remove(b'.')
            .remove(b'!')
            .remove(b'~')
            .remove(b'*')
            .remove(b'\'')
            .remove(b'(')
            .remove(b')');
        let encoded =
            percent_encoding::utf8_percent_encode(required("serverId")?, COMPONENT).to_string();
        let mut uri = url::Url::parse(&format!("relay://{endpoint}/{encoded}"))?;
        uri.query_pairs_mut()
            .append_pair("key", required("daemonPublicKeyB64")?);
        if payload["relay"]["useTls"] == true {
            uri.query_pairs_mut().append_pair("ssl", "true");
        }
        if let Some(security) = payload.get("minimumSecurityMode") {
            ensure!(security == "protected", "Invalid pairing security mode");
            uri.query_pairs_mut().append_pair("security", "protected");
        }
        Some(uri.to_string())
    } else {
        None
    };
    Ok(
        json!({"relayEnabled":offer["relayEnabled"],"url":url,"qr":offer["qr"],"connectionUri":connection}),
    )
}
pub async fn onboard(m: &ArgMatches, ctx: &Context) -> Result<Output> {
    let timeout = duration(m, "timeout", 600.0)?;
    let home = ctx.local_home()?;
    let existing = control::read_daemon_instance(&home).await?;
    let mut config = crate::config::read_persisted(&home, true).await?;
    if let Some(listen) = string(m, "listen")
        .map(str::to_owned)
        .or_else(|| string(m, "port").map(|p| format!("127.0.0.1:{p}")))
    {
        set_path(&mut config, "daemon.listen", Some(json!(listen)))?;
    }
    if flag(m, "relay") || flag(m, "no-relay") {
        set_path(
            &mut config,
            "daemon.relay.enabled",
            Some(json!(flag(m, "relay"))),
        )?;
    }
    if flag(m, "no-mcp") {
        set_path(&mut config, "daemon.mcp.enabled", Some(json!(false)))?;
    }
    if let Some(hosts) = string(m, "hostnames") {
        set_path(
            &mut config,
            "daemon.hostnames",
            Some(if hosts == "true" {
                json!(true)
            } else {
                json!(hosts.split(',').collect::<Vec<_>>())
            }),
        )?;
    }
    crate::config::save_persisted(&home, &config).await?;
    let saved_voice = config
        .pointer("/features/voiceMode/enabled")
        .and_then(Value::as_bool)
        .or_else(|| {
            config
                .pointer("/features/dictation/enabled")
                .and_then(Value::as_bool)
        });
    let voice = match string(m, "voice").unwrap_or("ask") {
        "enable" => true,
        "disable" => false,
        _ => match saved_voice {
            Some(voice) => voice,
            None if super::prompts::interactive() => {
                super::prompts::confirm(
                    "Enable voice features? (downloads local STT/TTS models in background)",
                    false,
                    &ctx.cancel,
                )
                .await?
            }
            None => false,
        },
    };
    for path in ["features.voiceMode.enabled", "features.dictation.enabled"] {
        set_path(&mut config, path, Some(json!(voice)))?;
    }
    crate::config::save_persisted(&home, &config).await?;
    if existing.is_some() {
        let client = ctx.connect().await?;
        let result = reload(&client).await;
        client.close().await;
        result?;
    } else {
        control::start_daemon_instance(
            StartOptions {
                home: home.clone(),
                timeout,
                ..Default::default()
            },
            &ctx.cancel,
        )
        .await?;
    }
    let ready = control::wait_for_daemon_ready(&home, timeout, None, &ctx.cancel).await?;
    let mut offer = if flag(m, "no-relay") {
        Value::Null
    } else {
        pairing(&ctx.target, flag(m, "relay"), &ctx.cancel).await?
    };
    if !flag(m, "no-relay") && offer["relayEnabled"] != true && super::prompts::interactive() {
        eprintln!(
            "Your connection is end-to-end encrypted. Paseo cannot read your code or messages.\nLearn how it works: https://paseo.sh/docs/security"
        );
        if super::prompts::confirm("Enable relay to pair a device?", false, &ctx.cancel).await? {
            offer = pairing(&ctx.target, true, &ctx.cancel).await?;
        }
    }
    pretty(
        json!({"home":home,"pid":ready.pid,"listen":ready.listen,"voiceEnabled":voice,"pairing":offer,"logPath":control::daemon_log_path(&home)}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn worker_restart_retries_socket_handoff_but_not_authority_errors() {
        for kind in [
            std::io::ErrorKind::ConnectionRefused,
            std::io::ErrorKind::ConnectionReset,
            std::io::ErrorKind::UnexpectedEof,
        ] {
            let error = anyhow::Error::new(tokio_tungstenite::tungstenite::Error::Io(kind.into()))
                .context("WebSocket handshake failed");
            assert!(reconnect_failure(&error), "{error:#}");
        }
        for code in [
            "AUTH_REQUIRED",
            "AUTH_FAILED",
            "DAEMON_REPLACED",
            "INCOMPATIBLE_PROTOCOL",
        ] {
            assert!(!reconnect_failure(&error(
                code,
                "Not a temporary worker handoff"
            )));
        }
        assert!(!reconnect_failure(
            &std::io::Error::from(std::io::ErrorKind::PermissionDenied).into()
        ));
    }
    #[test]
    fn pairing_uri_preserves_protected_security_and_tls() {
        use base64::Engine;
        let encoded=base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(&json!({"v":2,"serverId":"srv_example","daemonPublicKeyB64":"key+value=","relay":{"endpoint":"relay.example:443","useTls":true},"minimumSecurityMode":"protected"})).unwrap());
        let result=pairing_result(json!({"relayEnabled":true,"url":format!("https://app.example/#offer={encoded}"),"qr":null})).unwrap();
        assert_eq!(
            result["connectionUri"],
            "relay://relay.example:443/srv_example?key=key%2Bvalue%3D&ssl=true&security=protected"
        );
    }
    #[test]
    fn config_redaction_covers_nested_arrays_without_mutating_values() {
        let config = json!({"providers":[{"api_key":"secret","model":"model"}],"daemon":{"auth":{"password":"hash"}}});
        let redacted = redact(&config, false);
        assert_eq!(redacted["providers"][0]["api_key"], "[redacted]");
        assert_eq!(redacted["providers"][0]["model"], "model");
        assert_eq!(config["providers"][0]["api_key"], "secret");
    }
}

pub(crate) fn status_text(status: &Value) -> Result<String> {
    Ok(status
        .as_object()
        .context("Invalid daemon status")?
        .iter()
        .map(|(key, value)| {
            format!(
                "{key}: {}",
                value
                    .as_str()
                    .map(std::borrow::Cow::Borrowed)
                    .unwrap_or_else(|| std::borrow::Cow::Owned(value.to_string()))
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}
