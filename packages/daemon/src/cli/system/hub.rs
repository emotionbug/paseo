mod bundle;
mod credentials;
mod guided;
mod http;
#[cfg(test)]
mod tests;

use super::error;
use crate::cli::{
    Context, flag, multiple, option, output::Output, positional, string, strings, switch,
};
use anyhow::{Context as _, Result, bail, ensure};
use clap::{ArgMatches, Command};
use credentials::{Credential, Store};
use http::HubHttp;
use serde_json::{Value, json};
use std::{path::Path, time::Duration};
use tokio::io::AsyncWriteExt;

fn authority(command: Command) -> Command {
    command
        .arg(option("hub", "hub"))
        .arg(option("api-key", "api-key"))
}
pub fn command() -> Command {
    Command::new("hub")
        .about("Manage Paseo Hub")
        .subcommand_required(true)
        .subcommand(Command::new("login").arg(positional("origin", false)))
        .subcommand(Command::new("init"))
        .subcommand(Command::new("status"))
        .subcommand(
            Command::new("connect")
                .arg(positional("origin", false))
                .arg(option("api-key", "api-key"))
                .arg(multiple("permission", "permission").num_args(1..)),
        )
        .subcommand(Command::new("disconnect").arg(switch("force", "force")))
        .subcommand(
            Command::new("logout")
                .arg(switch("disconnect-daemon", "disconnect-daemon"))
                .arg(switch("force", "force")),
        )
        .subcommand(
            Command::new("permissions")
                .subcommand_required(true)
                .subcommand(Command::new("list"))
                .subcommand(Command::new("grant").arg(positional("permission", true)))
                .subcommand(Command::new("revoke").arg(positional("permission", true))),
        )
        .subcommand(authority(Command::new("projects")))
        .subcommand(
            authority(Command::new("deploy"))
                .arg(option("project", "project").short('p'))
                .arg(switch("dry-run", "dry-run")),
        )
        .subcommand(
            authority(Command::new("export"))
                .arg(positional("directory", false))
                .arg(switch("force", "force")),
        )
}
async fn rpc(ctx: &Context, operation: &str, input: Option<Value>) -> Result<Value> {
    let client = ctx.connect().await?;
    let mut request = json!({"type":format!("hub.management.daemon.{operation}.request")});
    if let Some(Value::Object(input)) = input {
        request.as_object_mut().unwrap().extend(input);
    }
    let result = client
        .request(
            request,
            &format!("hub.management.daemon.{operation}.response"),
            Duration::from_secs(30),
        )
        .await;
    client.close().await;
    result
}
fn status_output(response: Value) -> Result<Output> {
    let status = response
        .get("status")
        .context("Invalid Hub status response")?;
    let permissions = status["permissions"]
        .as_array()
        .context("Invalid Hub permissions")?
        .iter()
        .map(|p| p.as_str().context("Invalid Hub permission"))
        .collect::<Result<Vec<_>>>()?
        .join(", ");
    let mut row = json!({"state":status["state"],"daemonId":status["daemonId"],"hub":status["hubOrigin"],"permissions":permissions,"connectedAt":status["connectedAt"],"error":status["lastError"]});
    if let Some(warning) = response.get("warning") {
        row["warning"] = warning.clone();
    }
    Ok(Output::new(json!([row])).id("state").columns(&[
        ("STATE", "state"),
        ("HUB", "hub"),
        ("DAEMON", "daemonId"),
        ("PERMISSIONS", "permissions"),
        ("CONNECTED", "connectedAt"),
        ("ERROR", "error"),
        ("WARNING", "warning"),
    ]))
}
pub async fn execute(path: &[&str], m: &ArgMatches, ctx: &Context) -> Result<Output> {
    match path {
        ["status"] => return status_output(rpc(ctx, "get_status", None).await?),
        ["disconnect"] => {
            return status_output(
                rpc(ctx, "disconnect", Some(json!({"force":flag(m,"force")}))).await?,
            );
        }
        ["permissions", operation] => {
            let current = rpc(ctx, "get_status", None).await?;
            let status = &current["status"];
            ensure!(
                !status["hubOrigin"].is_null()
                    && matches!(status["state"].as_str(), Some("connected" | "reconnecting")),
                "This daemon is not connected to a Hub"
            );
            if *operation == "list" {
                let permissions = status["permissions"]
                    .as_array()
                    .context("Invalid Hub permissions")?;
                let data=permissions.iter().map(|p|json!({"permission":p,"description":if p=="hub.execute" {json!("Run agents for Hub automations")}else{p.clone()}})).collect();
                return Ok(Output::new(Value::Array(data))
                    .id("permission")
                    .columns(&[("PERMISSION", "permission"), ("DESCRIPTION", "description")]));
            }
            let mut input = json!({});
            input[*operation] = json!([string(m, "permission").context("Permission required")?]);
            return status_output(rpc(ctx, "permissions.update", Some(input)).await?);
        }
        ["init"] => return guided::run(ctx).await,
        _ => {}
    }
    let home = credentials::home()?;
    // Explicit URL and API-key authority does not consult an unrelated saved login.
    let external_authority = path != ["login"]
        && path != ["logout"]
        && (string(m, "origin").or_else(|| string(m, "hub")).is_some()
            || std::env::var_os("PASEO_HUB_URL").is_some())
        && (string(m, "api-key").is_some() || std::env::var_os("PASEO_HUB_API_KEY").is_some());
    let mut store = if external_authority {
        Store::default()
    } else {
        Store::read(&home).await?
    };
    if path == ["logout"] {
        let Some(active) = store.active().cloned() else {
            return Ok(logout_output(
                json!({"origin":null,"status":"not_logged_in","daemonDisconnected":false}),
            ));
        };
        let mut disconnected = false;
        let mut warning = None;
        let may_prompt =
            ctx.output.format != crate::cli::output::Format::Json && super::prompts::interactive();
        if flag(m, "disconnect-daemon") || may_prompt {
            let current = rpc(ctx, "get_status", None).await?;
            if current["status"]["hubOrigin"] == active.origin
                && (flag(m, "disconnect-daemon")
                    || super::prompts::confirm(
                        &format!("Disconnect this daemon from {}?", active.origin),
                        false,
                        &ctx.cancel,
                    )
                    .await?)
            {
                let result = rpc(ctx, "disconnect", Some(json!({"force":flag(m,"force")}))).await?;
                disconnected = result["status"]["state"] == "not_connected";
                warning = result.get("warning").cloned();
            }
        }
        store.logout(&home).await?;
        let mut result =
            json!({"origin":active.origin,"status":"logged_out","daemonDisconnected":disconnected});
        if let Some(warning) = warning {
            result["warning"] = warning;
        }
        return Ok(logout_output(result));
    }
    let origin = store.resolve_origin(string(m, "origin").or_else(|| string(m, "hub")))?;
    let http = HubHttp::new(ctx.cancel.child_token())?;
    if path == ["login"] {
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
        if ctx.output.format != crate::cli::output::Format::Json && super::prompts::interactive() {
            guided::continue_login(&origin, &http, &store, ctx).await?;
        }
        return Ok(Output::new(json!({"origin":origin,"status":"logged_in"}))
            .id("origin")
            .columns(&[("HUB", "origin"), ("STATUS", "status")]));
    }
    let key = store.resolve_key(&origin, string(m, "api-key"))?;
    match path {
        ["connect"] => {
            let token = http
                .post(
                    &origin,
                    "/api/v1/daemons/enrollment-tokens",
                    Some(&key),
                    &json!({}),
                    201,
                )
                .await?;
            let permissions = strings(m, "permission");
            let response = rpc(
                ctx,
                "connect",
                Some(json!({"hubUrl":origin,"token":token["token"],"permissions":permissions})),
            )
            .await?;
            if !response["status"]["hubOrigin"].is_null() {
                let actual = response["status"]["permissions"]
                    .as_array()
                    .context("Invalid Hub permission response")?;
                if actual.len() != permissions.len()
                    || !permissions.iter().all(|p| actual.iter().any(|a| a == p))
                {
                    let _ = rpc(ctx, "disconnect", Some(json!({"force":false}))).await;
                    bail!(
                        "The daemon did not honor the requested Hub access. Update Paseo before connecting it."
                    );
                }
            }
            status_output(response)
        }
        ["projects"] => {
            let projects = http.get(&origin, "/api/v1/projects", &key).await?;
            let table = if ctx.output.format == crate::cli::output::Format::Table
                && !ctx.output.quiet
            {
                Some(Value::Array(projects["projects"].as_array().unwrap().iter().map(|project|json!({"id":project["id"],"slug":project["slug"],"name":project["name"],"origin":origin})).collect()))
            } else {
                None
            };
            let mut output = Output::new(json!({"origin":origin,"projects":projects["projects"]}))
                .id("origin")
                .columns(&[
                    ("SLUG", "slug"),
                    ("NAME", "name"),
                    ("ID", "id"),
                    ("HUB", "origin"),
                ]);
            if let Some(table) = table {
                output = output.table_data(table);
            }
            Ok(output)
        }
        ["deploy"] => deploy(m, &http, &origin, &key, &std::env::current_dir()?).await,
        ["export"] => export(m, &http, &origin, &key, &std::env::current_dir()?).await,
        _ => bail!("Unknown Hub command"),
    }
}
fn logout_output(data: Value) -> Output {
    Output::new(data).id("status").columns(&[
        ("HUB", "origin"),
        ("STATUS", "status"),
        ("DAEMON DISCONNECTED", "daemonDisconnected"),
        ("WARNING", "warning"),
    ])
}
async fn deploy(
    m: &ArgMatches,
    http: &HubHttp,
    origin: &str,
    key: &str,
    root: &Path,
) -> Result<Output> {
    let dry = flag(m, "dry-run");
    if let Some(project) = string(m, "project") {
        let (body, workflows) = bundle::project(root, project).await?;
        let mut data = http
            .post(
                origin,
                if dry {
                    "/api/v1/configurations/validate"
                } else {
                    "/api/v1/configurations/install"
                },
                Some(key),
                &body,
                if dry { 200 } else { 201 },
            )
            .await?;
        data["origin"] = json!(origin);
        data["workflows"] = json!(workflows);
        return Ok(Output::new(data)
            .id(if dry { "projectSlug" } else { "versionId" })
            .columns(if dry {
                &[
                    ("PROJECT", "projectSlug"),
                    ("VALID", "valid"),
                    ("WORKFLOWS", "workflows"),
                    ("HUB", "origin"),
                ]
            } else {
                &[
                    ("PROJECT", "projectSlug"),
                    ("VERSION", "version"),
                    ("VERSION ID", "versionId"),
                    ("ACTIVE", "active"),
                    ("WORKFLOWS", "workflows"),
                    ("HUB", "origin"),
                ]
            }));
    }
    let triggers = bundle::triggers(root).await?;
    let mut validated = Vec::with_capacity(triggers.len());
    for trigger in &triggers {
        let mut result = http
            .post(
                origin,
                "/api/v1/triggers/validate",
                Some(key),
                &json!({"yaml":trigger["yaml"]}),
                200,
            )
            .await?;
        result["origin"] = json!(origin);
        result["path"] = trigger["path"].clone();
        validated.push(result);
    }
    let data = if dry {
        validated
    } else {
        let mut installed = Vec::with_capacity(triggers.len());
        for trigger in &triggers {
            match http
                .post(
                    origin,
                    "/api/v1/triggers/install",
                    Some(key),
                    &json!({"yaml":trigger["yaml"]}),
                    201,
                )
                .await
            {
                Ok(mut result) => {
                    result["origin"] = json!(origin);
                    result["path"] = trigger["path"].clone();
                    installed.push(result);
                }
                Err(failure) if !installed.is_empty() => {
                    return Err(error(
                        "HUB_TRIGGER_DEPLOY_PARTIAL",
                        format!(
                            "Could not deploy {} after {} triggers had been installed.\n{failure}\nInstalled before the failure:\n{}",
                            trigger["path"].as_str().unwrap(),
                            installed.len(),
                            installed
                                .iter()
                                .map(|i| format!("- {}", i["path"].as_str().unwrap()))
                                .collect::<Vec<_>>()
                                .join("\n")
                        ),
                    ));
                }
                Err(failure) => return Err(failure),
            }
        }
        installed
    };
    Ok(Output::new(Value::Array(data))
        .id(if dry { "name" } else { "triggerId" })
        .columns(if dry {
            &[
                ("TRIGGER", "name"),
                ("VALID", "valid"),
                ("PATH", "path"),
                ("HUB", "origin"),
            ]
        } else {
            &[
                ("TRIGGER", "name"),
                ("VERSION", "version"),
                ("REVISION ID", "revisionId"),
                ("ACTIVE", "active"),
                ("PATH", "path"),
                ("HUB", "origin"),
            ]
        }))
}
async fn export(
    m: &ArgMatches,
    http: &HubHttp,
    origin: &str,
    key: &str,
    root: &Path,
) -> Result<Output> {
    let directory = root.join(string(m, "directory").unwrap_or(".paseo/triggers"));
    let response = http.get(origin, "/api/v1/triggers", key).await?;
    let triggers = response["triggers"]
        .as_array()
        .context("Invalid trigger response")?;
    tokio::fs::create_dir_all(&directory).await?;
    let mut files = Vec::with_capacity(triggers.len());
    for trigger in triggers {
        let path = directory.join(format!("{}.yml", trigger["name"].as_str().unwrap()));
        let existing = match tokio::fs::read_to_string(&path).await {
            Ok(text) => Some(text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        let yaml = trigger["yaml"].as_str().unwrap();
        if existing.as_deref().is_some_and(|text| text != yaml) && !flag(m, "force") {
            return Err(error(
                "HUB_EXPORT_CONFLICT",
                format!(
                    "{} already exists with different contents. Pass --force to replace it.",
                    path.display()
                ),
            ));
        }
        files.push((path, existing, yaml));
    }
    let mut exported = 0;
    for (path, existing, yaml) in &files {
        if existing.as_deref() == Some(*yaml) {
            continue;
        }
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true);
        if existing.is_none() {
            options.create_new(true);
        } else {
            options.truncate(true);
        }
        let mut file = options.open(path).await?;
        file.write_all(yaml.as_bytes()).await?;
        file.flush().await?;
        exported += 1;
    }
    Ok(Output::new(json!({"origin":origin,"directory":directory,"exported":exported,"unchanged":files.len()-exported})).id("directory").columns(&[("DIRECTORY","directory"),("EXPORTED","exported"),("UNCHANGED","unchanged"),("HUB","origin")]))
}
