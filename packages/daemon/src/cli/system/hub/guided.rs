use super::super::{error, prompts};
use super::{
    credentials::{self, Credential, Store},
    http::HubHttp,
    rpc,
};
use crate::cli::{Context, output::Output};
use anyhow::{Context as _, Result, ensure};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};
use tokio::io::AsyncWriteExt;

#[derive(PartialEq)]
enum Connection {
    Connected(String),
    Pending,
    Conflict(String),
    Connect,
}
fn connection(status: &Value, origin: &str) -> Connection {
    if status["state"] == "connected" && status["hubOrigin"] == origin {
        if let Some(id) = status["daemonId"].as_str() {
            return Connection::Connected(id.into());
        }
    }
    if matches!(
        status["state"].as_str(),
        Some("connecting" | "reconnecting")
    ) && status["hubOrigin"] == origin
    {
        return Connection::Pending;
    }
    if let Some(other) = status["hubOrigin"].as_str() {
        if !matches!(status["state"].as_str(), Some("not_connected" | "revoked")) {
            return Connection::Conflict(other.into());
        }
    }
    Connection::Connect
}
async fn wait_connected(origin: &str, ctx: &Context) -> Result<String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let status = rpc(ctx, "get_status", None).await?;
        match connection(&status["status"], origin) {
            Connection::Connected(id) => return Ok(id),
            Connection::Conflict(other) => {
                return Err(error(
                    "HUB_DAEMON_ALREADY_CONNECTED",
                    format!(
                        "This daemon connected to {other} while Hub init was waiting for {origin}."
                    ),
                ));
            }
            Connection::Connect => {
                return Err(error(
                    "HUB_DAEMON_CONNECTION_LOST",
                    "The daemon lost its Hub relationship while Hub init was waiting for it.",
                ));
            }
            Connection::Pending => {}
        }
        ensure!(
            tokio::time::Instant::now() < deadline,
            error(
                "HUB_DAEMON_CONNECTION_TIMEOUT",
                "The daemon did not connect within 60 seconds. Check paseo hub status, then run Hub init again."
            )
        );
        pause(ctx).await?;
    }
}
async fn pause(ctx: &Context) -> Result<()> {
    tokio::select! {_=ctx.cancel.cancelled()=>Err(error("PROMPT_CANCELLED","Hub init cancelled.")),_=tokio::time::sleep(Duration::from_millis(250))=>Ok(())}
}
async fn connect(
    origin: &str,
    permissions: &[&str],
    http: &HubHttp,
    store: &Store,
    ctx: &Context,
) -> Result<String> {
    let key = store.resolve_key(origin, None)?;
    let token = http
        .post(
            origin,
            "/api/v1/daemons/enrollment-tokens",
            Some(&key),
            &json!({}),
            201,
        )
        .await?;
    let response = rpc(
        ctx,
        "connect",
        Some(json!({"hubUrl":origin,"token":token["token"],"permissions":permissions})),
    )
    .await?;
    if !response["status"]["hubOrigin"].is_null() {
        let actual = response["status"]["permissions"]
            .as_array()
            .context("Invalid Hub permissions response")?;
        if actual.len() != permissions.len()
            || !permissions
                .iter()
                .all(|p| actual.iter().any(|a| a.as_str() == Some(*p)))
        {
            let _ = rpc(ctx, "disconnect", Some(json!({"force":false}))).await;
            return Err(error(
                "HUB_PERMISSIONS_MISMATCH",
                "The daemon did not honor the requested Hub access. Update Paseo before connecting it.",
            ));
        }
    }
    wait_connected(origin, ctx).await
}
async fn ensure_connection(
    origin: &str,
    http: &HubHttp,
    store: &Store,
    ctx: &Context,
) -> Result<String> {
    let current = rpc(ctx, "get_status", None).await?;
    let status = &current["status"];
    match connection(status, origin) {
        Connection::Connected(id) => {
            ensure!(
                status["permissions"]
                    .as_array()
                    .is_some_and(|permissions| permissions.iter().any(|p| p == "hub.execute")),
                error(
                    "HUB_DAEMON_EXECUTION_NOT_ALLOWED",
                    "This daemon is connected to Hub but cannot run Hub automations. Run paseo hub permissions grant hub.execute, then run Hub init again."
                )
            );
            Ok(id)
        }
        Connection::Pending => wait_connected(origin, ctx).await,
        Connection::Conflict(other) => Err(error(
            "HUB_DAEMON_ALREADY_CONNECTED",
            format!(
                "This daemon is connected to {other}. Disconnect it before running Hub init for {origin}."
            ),
        )),
        Connection::Connect => {
            ensure!(prompts::confirm(&format!("Connect this daemon to {origin} and allow Hub workflows to create and control workspaces and agents?"),true,&ctx.cancel).await?,error("PROMPT_CANCELLED","A connected daemon is required to create the trigger."));
            connect(origin, &["hub.execute"], http, store, ctx).await
        }
    }
}
pub(super) async fn continue_login(
    origin: &str,
    http: &HubHttp,
    store: &Store,
    ctx: &Context,
) -> Result<()> {
    let current = rpc(ctx, "get_status", None).await?;
    match connection(&current["status"],origin) {
        Connection::Connected(_)=>eprintln!("This daemon is already connected to {origin}. Permissions: {}",current["status"]["permissions"]),
        Connection::Pending=>{wait_connected(origin,ctx).await?;}
        Connection::Conflict(other)=>eprintln!("This daemon is connected to {other}. Disconnect it before connecting to {origin}."),
        Connection::Connect=>{
            if prompts::confirm("Connect this daemon to Paseo Hub?\nConnecting lets Hub identify this daemon and show whether it is online. It does not allow Hub to create workspaces or run agents.",true,&ctx.cancel).await? {
                let execution=prompts::confirm("Allow Hub automations to run agents on this daemon?\nThis lets workflows triggered from integrations create workspaces and run agents here. Agents can access files and run commands allowed by their workspace runtime.",false,&ctx.cancel).await?;
                connect(origin,if execution{&["hub.execute"]}else{&[]},http,store,ctx).await?;
                if !execution{eprintln!("Daemon connected with no permissions. Enable Hub automations later: paseo hub permissions grant hub.execute");}
            }else{eprintln!("Skipped daemon connection. Connect later with: paseo hub connect {origin}");}
        }
    }
    eprintln!(
        "Configure triggers in Hub: {origin}/triggers\nOr scaffold triggers as code: paseo hub init"
    );
    Ok(())
}
pub(super) async fn run(ctx: &Context) -> Result<Output> {
    ensure!(
        prompts::interactive(),
        error(
            "HUB_INIT_INTERACTIVE_REQUIRED",
            "paseo hub init requires a TTY."
        )
    );
    let result = run_inner(ctx).await;
    match result {
        Err(error)
            if error
                .downcast_ref::<crate::client::ClientError>()
                .is_some_and(|e| e.code == "PROMPT_CANCELLED") =>
        {
            eprintln!("{error}");
            Ok(Output::new(Value::Null).silent())
        }
        other => other,
    }
}
async fn run_inner(ctx: &Context) -> Result<Output> {
    eprintln!("Set up Paseo Hub");
    let home = credentials::home()?;
    let mut store = Store::read(&home).await?;
    let http = HubHttp::new(ctx.cancel.child_token())?;
    let active = store.active().map(|c| c.origin.clone());
    let endpoint = prompts::select(
        "Hub endpoint",
        &[
            ("hosted".into(), "hub.paseo.sh".into()),
            ("custom".into(), "Custom endpoint".into()),
        ],
        Some(
            if active
                .as_deref()
                .is_none_or(|o| o == "https://hub.paseo.sh")
            {
                "hosted"
            } else {
                "custom"
            },
        ),
        &ctx.cancel,
    )
    .await?;
    let origin = if endpoint == "hosted" {
        "https://hub.paseo.sh".to_owned()
    } else {
        let initial = active
            .filter(|o| o != "https://hub.paseo.sh")
            .or_else(|| std::env::var("PASEO_HUB_URL").ok());
        loop {
            let value = prompts::text("Custom Hub URL", initial.as_deref(), &ctx.cancel).await?;
            match credentials::normalize(&value) {
                Ok(origin) => break origin,
                Err(_) => eprintln!("Enter a valid Hub URL"),
            }
        }
    };
    if !store.contains(&origin) {
        let credential = http.authorize(&origin).await?;
        store
            .save(
                &home,
                Credential {
                    origin: origin.clone(),
                    credential,
                },
            )
            .await?;
    }
    eprintln!("Logged in to {origin}");
    let daemon_id = ensure_connection(&origin, &http, &store, ctx).await?;
    let key = store.resolve_key(&origin, None)?;
    let resources=http.get(&origin,"/api/v1/configuration-resources",&key).await.map_err(|e|if e.downcast_ref::<crate::client::ClientError>().is_some_and(|e|matches!(e.code.as_str(),"HUB_NOT_FOUND"|"HUB_INVALID_RESPONSE")){error("HUB_UPDATE_REQUIRED","This Hub needs an update before guided setup can use organization triggers. Update Hub and try again.")}else{e})?;
    let cwd = std::env::current_dir()?;
    let remote = command_value("git", &["remote", "get-url", "origin"], Some(&cwd)).await;
    let repository = remote.as_deref().and_then(github_repository);
    let connections = connections(&resources, repository.as_deref());
    ensure!(
        !connections.is_empty(),
        error(
            "PROMPT_CANCELLED",
            "No Hub app connection is ready for this trigger. Connect GitHub, Slack, or Discord in Hub Apps, then run paseo hub init again."
        )
    );
    eprintln!("Hub app connections ready for this trigger:");
    for item in &connections {
        eprintln!("{}", item.label);
    }
    eprintln!(
        "Only configured connections are shown. To add another, open Hub Apps, then run paseo hub init again."
    );
    let selected = if connections.len() == 1 {
        0
    } else {
        let choices = connections
            .iter()
            .enumerate()
            .map(|(i, c)| (i.to_string(), c.label.clone()))
            .collect::<Vec<_>>();
        prompts::select("Trigger connection", &choices, None, &ctx.cancel)
            .await?
            .parse::<usize>()?
    };
    let connection = &connections[selected];
    let daemon=resources["daemons"].as_array().and_then(|a|a.iter().find(|d|d["id"]==daemon_id)).context("The connected daemon is not available in this Hub organization. Reconnect it and try again.")?;
    eprintln!("Connected as {}", daemon["slug"].as_str().unwrap());
    let agent = choose_runtime(ctx, &cwd).await?;
    let initial = if connection.provider == "github" {
        command_value("gh", &["api", "user", "--jq", ".login"], None).await
    } else {
        None
    };
    let label = match connection.provider {
        "github" => "Your GitHub username",
        "slack" => "Your Slack member ID",
        _ => "Your Discord user ID",
    };
    let user = prompts::text(
        &format!("{label} (only this user can trigger the bot)"),
        initial.as_deref(),
        &ctx.cancel,
    )
    .await?;
    let (relative, trigger, test_action) = scaffold(
        connection,
        &user,
        daemon["slug"].as_str().unwrap(),
        &cwd,
        agent,
    )?;
    http.post(
        &origin,
        "/api/v1/triggers/validate",
        Some(&key),
        &json!({"yaml":trigger}),
        200,
    )
    .await?;
    eprintln!("Dry run passed");
    let replace = prepare_destination(&cwd, &relative).await?;
    if replace {
        ensure!(
            prompts::confirm(
                &format!("Replace the existing {relative}?"),
                false,
                &ctx.cancel
            )
            .await?,
            error("PROMPT_CANCELLED", format!("{relative} left unchanged."))
        );
    }
    write_scaffold(&cwd, &relative, &trigger, replace).await?;
    eprintln!("Created {relative}");
    let deploy = prompts::confirm("Deploy now?", true, &ctx.cancel).await?;
    if deploy {
        http.post(
            &origin,
            "/api/v1/triggers/install",
            Some(&key),
            &json!({"yaml":trigger}),
            201,
        )
        .await?;
        eprintln!("Deployed");
    } else {
        eprintln!("Skipped deployment. Run paseo hub deploy when ready.");
    }
    eprintln!("{test_action}\nManage it at {origin}/triggers");
    Ok(Output::new(Value::Null).silent())
}
struct TriggerConnection {
    label: String,
    provider: &'static str,
    slug: String,
    repository: Option<String>,
}
fn connections(resources: &Value, repository: Option<&str>) -> Vec<TriggerConnection> {
    let mut results = Vec::new();
    if let Some(repository) = repository {
        if let Some(items) = resources["github"].as_array() {
            for item in items {
                if item["repositories"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|r| r == repository))
                {
                    results.push(TriggerConnection {
                        label: format!("GitHub — {repository}"),
                        provider: "github",
                        slug: item["slug"].as_str().unwrap().into(),
                        repository: Some(repository.into()),
                    });
                }
            }
        }
    }
    for (provider, key) in [("slack", "teamName"), ("discord", "guildName")] {
        if let Some(items) = resources[provider].as_array() {
            for item in items {
                results.push(TriggerConnection {
                    label: format!(
                        "{} — {}",
                        if provider == "slack" {
                            "Slack"
                        } else {
                            "Discord"
                        },
                        item[key].as_str().unwrap()
                    ),
                    provider,
                    slug: item["slug"].as_str().unwrap().into(),
                    repository: None,
                });
            }
        }
    }
    results
}
async fn command_value(command: &str, args: &[&str], cwd: Option<&Path>) -> Option<String> {
    let mut command = tokio::process::Command::new(command);
    command.args(args).kill_on_drop(true);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let result = command.output().await.ok()?;
    if !result.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&result.stdout).trim().to_owned();
    (!text.is_empty()).then_some(text)
}
fn github_repository(remote: &str) -> Option<String> {
    let remote = remote.trim().trim_end_matches(".git");
    let repository = if let Some(rest) = remote
        .strip_prefix("git@github.com:")
        .or_else(|| remote.strip_prefix("git@github.com/"))
        .or_else(|| remote.strip_prefix("ssh://git@github.com/"))
        .or_else(|| remote.strip_prefix("ssh://git@github.com:"))
    {
        rest.to_owned()
    } else {
        let url = url::Url::parse(remote).ok()?;
        if url.host_str() != Some("github.com") {
            return None;
        }
        url.path().trim_start_matches('/').to_owned()
    };
    let parts: Vec<_> = repository.split('/').collect();
    (parts.len() == 2 && parts.iter().all(|p| !p.is_empty())).then_some(repository)
}
async fn choose_runtime(ctx: &Context, cwd: &Path) -> Result<Value> {
    let client = ctx.connect().await?;
    let result=async {
        let deadline=tokio::time::Instant::now()+Duration::from_secs(60);
        let providers=loop {
            let response=client.request(json!({"type":"get_providers_snapshot_request","cwd":cwd}),"get_providers_snapshot_response",Duration::from_secs(30)).await?;
            let entries=response["entries"].as_array().context("Invalid provider snapshot")?;
            let providers:Vec<_>=entries.iter().filter(|entry|entry["status"]=="ready"&&entry["enabled"]==true&&entry["models"].as_array().is_some_and(|models|models.iter().any(|model|model["isSelectable"]!=false))).cloned().collect();
            if !providers.is_empty(){let providers:Vec<_>=providers.into_iter().filter(|p|p["modes"].as_array().is_some_and(|m|!m.is_empty())).collect();ensure!(!providers.is_empty(),error("HUB_AGENT_RUNTIME_REQUIRED","No agent runtime with an execution mode is available from this daemon. Configure one, then run paseo hub init again."));break providers;}
            ensure!(entries.is_empty()||entries.iter().any(|e|e["enabled"]==true&&e["status"]=="loading"),error("HUB_AGENT_RUNTIME_REQUIRED","No usable agent runtime is available from this daemon. Configure an enabled provider with a selectable model, then run paseo hub init again."));
            ensure!(tokio::time::Instant::now()<deadline,error("HUB_AGENT_RUNTIME_TIMEOUT","Agent runtime discovery did not finish within 60 seconds. Check the daemon provider configuration, then run paseo hub init again."));pause(ctx).await?;
        };
        let choices=providers.iter().map(|p|(p["provider"].as_str().unwrap().to_owned(),p["label"].as_str().or_else(||p["provider"].as_str()).unwrap().to_owned())).collect::<Vec<_>>();
        let selected=prompts::select("Starter agent provider",&choices,None,&ctx.cancel).await?;
        let provider=providers.iter().find(|p|p["provider"]==selected).context("Selected provider is no longer available")?;
        let models:Vec<_>=provider["models"].as_array().unwrap().iter().filter(|m|m["isSelectable"]!=false).collect();
        let choices=models.iter().map(|m|(m["id"].as_str().unwrap().to_owned(),m["label"].as_str().unwrap().to_owned())).collect::<Vec<_>>();let suggested=models.iter().find(|m|m["isDefault"]==true).and_then(|m|m["id"].as_str());
        let model=prompts::select("Starter agent model",&choices,suggested,&ctx.cancel).await?;
        let choices=provider["modes"].as_array().unwrap().iter().map(|m|(m["id"].as_str().unwrap().to_owned(),m["label"].as_str().unwrap().to_owned())).collect::<Vec<_>>();
        let mode=prompts::select("Starter agent mode",&choices,provider["defaultModeId"].as_str(),&ctx.cancel).await?;
        Ok(json!({"provider":selected,"model":model,"mode":mode}))
    }.await;
    client.close().await;
    result
}
fn scaffold(
    connection: &TriggerConnection,
    user: &str,
    daemon: &str,
    cwd: &Path,
    agent: Value,
) -> Result<(String, String, String)> {
    let provider = connection.provider;
    let (event, filters, reply, test_action) = match provider {
        "github" => {
            let repo = connection
                .repository
                .as_deref()
                .context("GitHub repository required")?;
            (
                "github.issue_comment",
                json!({"repo":repo,"contains":"@paseo","from_users":[user]}),
                None,
                format!("Comment @paseo have a look on {repo}."),
            )
        }
        "slack" => (
            "slack.mention",
            json!({"from_users":[user]}),
            Some("slack.reply"),
            "Mention @Paseo have a look in Slack.".to_owned(),
        ),
        _ => (
            "discord.mention",
            json!({"from_users":[user]}),
            Some("discord.reply"),
            "Mention @Paseo have a look in Discord.".to_owned(),
        ),
    };
    let mut trigger = json!({"name":format!("{provider}-help"),"enabled":true,"on":{},"max_runtime":"2h","run":{"target":{"daemon":daemon,"cwd":cwd},"agent":agent,"continuation":{"mode":"conversation"},"max_runtime":"90m","idle_timeout":"10m","prompt":format!("{}complete this request and call hub.finish_execution when done.\n\n<user-prompt>\n${{{{ paseo.prompt }}}}\n</user-prompt>\n",if reply.is_some(){"Answer with hub.reply, then "}else{""})}});
    trigger["on"][event] = json!({"connection":connection.slug,"filters":filters});
    if let Some(reply) = reply {
        trigger["run"]["outputs"] = json!({});
        trigger["run"]["outputs"][reply] = json!({"max":1,"required":true});
    }
    Ok((
        format!(".paseo/triggers/{provider}-help.yml"),
        serde_yaml::to_string(&trigger)?,
        test_action,
    ))
}
async fn prepare_destination(root: &Path, relative: &str) -> Result<bool> {
    for relative in [".paseo", ".paseo/triggers"] {
        let path = root.join(relative);
        match tokio::fs::symlink_metadata(&path).await {
            Ok(metadata) => {
                ensure!(
                    !metadata.file_type().is_symlink(),
                    error(
                        "HUB_TRIGGER_UNSAFE_PATH",
                        format!("{relative} must not use a symlink.")
                    )
                );
                ensure!(
                    metadata.is_dir(),
                    error(
                        "HUB_TRIGGER_DIRECTORY_INVALID",
                        format!("{relative} must be a directory.")
                    )
                );
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tokio::fs::create_dir(&path).await?
            }
            Err(e) => return Err(e.into()),
        }
    }
    match tokio::fs::symlink_metadata(root.join(relative)).await {
        Ok(metadata) => {
            ensure!(
                !metadata.file_type().is_symlink(),
                error(
                    "HUB_TRIGGER_UNSAFE_PATH",
                    format!("{relative} must not use a symlink.")
                )
            );
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}
async fn write_scaffold(root: &Path, relative: &str, contents: &str, replace: bool) -> Result<()> {
    let destination = root.join(relative);
    let temporary = destination.with_file_name(format!(
        ".{}.{}.{}.tmp",
        destination.file_name().unwrap().to_string_lossy(),
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let result = async {
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .await?;
        file.write_all(contents.as_bytes()).await?;
        file.flush().await?;
        drop(file);
        if replace {
            tokio::fs::rename(&temporary, &destination).await?;
        } else {
            tokio::fs::hard_link(&temporary, &destination).await?;
            tokio::fs::remove_file(&temporary).await?;
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temporary).await;
    }
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn scaffold_does_not_overwrite_unconfirmed_file() {
        let root = tempfile::tempdir().unwrap();
        assert!(
            !prepare_destination(root.path(), ".paseo/triggers/slack-help.yml")
                .await
                .unwrap()
        );
        write_scaffold(
            root.path(),
            ".paseo/triggers/slack-help.yml",
            "original",
            false,
        )
        .await
        .unwrap();
        assert!(
            write_scaffold(
                root.path(),
                ".paseo/triggers/slack-help.yml",
                "replacement",
                false
            )
            .await
            .is_err()
        );
        assert_eq!(
            tokio::fs::read_to_string(root.path().join(".paseo/triggers/slack-help.yml"))
                .await
                .unwrap(),
            "original"
        );
    }
    #[test]
    fn scaffold_contains_explicit_user_and_execution_boundaries() {
        let connection = TriggerConnection {
            label: "Slack".into(),
            provider: "slack",
            slug: "team".into(),
            repository: None,
        };
        let (_, yaml, _) = scaffold(
            &connection,
            "U123",
            "daemon",
            Path::new("/repo"),
            json!({"provider":"codex","model":"model","mode":"mode"}),
        )
        .unwrap();
        let doc: Value = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(
            doc["on"]["slack.mention"]["filters"]["from_users"],
            json!(["U123"])
        );
        assert_eq!(doc["run"]["outputs"]["slack.reply"]["max"], 1);
        assert!(
            doc["run"]["prompt"]
                .as_str()
                .unwrap()
                .contains("${{ paseo.prompt }}")
        );
    }
}
