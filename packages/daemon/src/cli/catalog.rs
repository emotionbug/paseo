use super::agents::{all, array, error, fetch, required, response_array, rpc, short, text};
use super::{
    Context, flag, option,
    output::{Format, Output},
    positional, string, switch,
};
use crate::client::DaemonClient;
use anyhow::{Result, anyhow};
use clap::{ArgMatches, Command};
use serde_json::{Value, json};
use std::time::Duration;
mod plugins;
mod usage;

pub fn commands() -> Vec<Command> {
    vec![
        Command::new("provider")
            .about("Agent providers")
            .subcommand_required(true)
            .subcommands([
                Command::new("ls"),
                Command::new("models")
                    .arg(positional("provider", true))
                    .arg(switch("thinking", "thinking")),
                Command::new("diagnostic").arg(positional("provider", true)),
            ]),
        Command::new("permit")
            .about("Manage permission requests")
            .subcommand_required(true)
            .subcommands([
                Command::new("ls"),
                Command::new("allow")
                    .arg(positional("agent", true))
                    .arg(positional("request", false))
                    .arg(switch("all", "all"))
                    .arg(option("input", "input")),
                Command::new("deny")
                    .arg(positional("agent", true))
                    .arg(positional("request", false))
                    .arg(switch("all", "all"))
                    .arg(option("message", "message"))
                    .arg(switch("interrupt", "interrupt")),
            ]),
        Command::new("usage")
            .about("Show account usage reports")
            .arg(option("agent", "agent"))
            .arg(switch("refresh", "refresh"))
            .subcommands([
                Command::new("ls")
                    .arg(option("agent", "agent"))
                    .arg(switch("refresh", "refresh")),
                Command::new("inspect")
                    .arg(positional("id", true))
                    .arg(switch("refresh", "refresh")),
            ]),
        Command::new("speech").about("Speech commands"),
        plugins::command(),
    ]
}
pub async fn execute(path: &[&str], m: &ArgMatches, context: &Context) -> Result<Option<Output>> {
    let Some(family) = path.first() else {
        return Ok(None);
    };
    if !matches!(*family, "provider" | "permit" | "usage" | "plugin") {
        return Ok(None);
    }
    let command = path.get(1).copied().unwrap_or("ls");
    if *family == "plugin" && command == "init" {
        return plugins::init(m).await.map(Some);
    }
    let client = context.connect().await?;
    let result = match *family {
        "provider" => provider(command, m, context, &client).await,
        "permit" => permit(command, m, context, &client).await,
        "usage" => usage::execute(command, m, context, &client).await,
        "plugin" => plugins::execute(command, m, context, &client).await,
        _ => unreachable!(),
    };
    client.close().await;
    result.map(Some)
}
async fn provider(
    command: &str,
    m: &ArgMatches,
    context: &Context,
    client: &DaemonClient,
) -> Result<Output> {
    match command {
        "ls" => {
            let snapshot = rpc(
                client,
                context,
                json!({"type":"get_providers_snapshot_request"}),
                "get_providers_snapshot_response",
            )
            .await?;
            let entries = response_array(
                if snapshot["compactSnapshot"].is_object() {
                    &snapshot["compactSnapshot"]
                } else {
                    &snapshot
                },
                "entries",
            )?;
            let data:Vec<Value>=entries.iter().map(|entry|json!({"provider":entry["provider"],"label":entry["label"].as_str().unwrap_or(text(entry,"provider")),"status":if entry["status"]=="ready"{"available"}else{text(entry,"status")},"enabled":if entry["enabled"]==true{"Enabled"}else{"Disabled"},"defaultMode":entry["defaultModeId"].as_str().unwrap_or("default"),"modes":array(&entry["modes"]).iter().map(|m|text(m,"label")).collect::<Vec<_>>().join(", ")})).collect();
            Ok(Output::new(json!(data))
                .id("provider")
                .columns(&[
                    ("PROVIDER", "provider"),
                    ("LABEL", "label"),
                    ("STATUS", "status"),
                    ("ENABLED", "enabled"),
                    ("DEFAULT MODE", "defaultMode"),
                    ("MODES", "modes"),
                ])
                .widths(&[12, 16, 12, 10, 14, 30])
                .color_column("status", |value, _| match value.as_str() {
                    Some("available") => Some(32),
                    Some("unavailable") => Some(31),
                    _ => None,
                }))
        }
        "models" => {
            let provider = required(m, "provider")?.to_lowercase();
            let result = client
                .request_cancellable(
                    json!({"type":"list_provider_models_request","provider":provider}),
                    "list_provider_models_response",
                    Duration::from_secs(90),
                    context.cancel.clone(),
                )
                .await?;
            let data:Vec<Value>=array(&result["models"]).iter().map(|model|{let ids:Vec<&str>=array(&model["thinkingOptions"]).iter().map(|v|text(v,"id")).collect();json!({"model":model["label"],"id":model["id"],"description":model["description"].as_str().unwrap_or(""),"thinkingOptionIds":ids,"defaultThinkingOptionId":model["defaultThinkingOptionId"],"thinkingOptions":if ids.is_empty(){"none".into()}else{ids.join(", ")}})}).collect();
            let mut output = Output::new(json!(data));
            if flag(m, "thinking") {
                output = output
                    .columns(&[
                        ("ID", "id"),
                        ("MODEL", "model"),
                        ("THINKING IDS", "thinkingOptions"),
                        ("DEFAULT THINKING", "defaultThinkingOptionId"),
                    ])
                    .widths(&[30, 30, 40, 18]);
                if context.output.format == Format::Table && !context.output.quiet {
                    let mut display = output.data.clone();
                    for row in display.as_array_mut().unwrap() {
                        if row["defaultThinkingOptionId"].is_null() {
                            row["defaultThinkingOptionId"] = json!("auto");
                        }
                    }
                    let human = Output::new(display)
                        .columns(&output.columns)
                        .widths(&[30, 30, 40, 18])
                        .render(&context.output)?;
                    output = output.human(human);
                }
            } else {
                output = output
                    .columns(&[
                        ("ID", "id"),
                        ("MODEL", "model"),
                        ("DESCRIPTION", "description"),
                    ])
                    .widths(&[30, 30, 40]);
            }
            Ok(output)
        }
        "diagnostic" => {
            let provider = required(m, "provider")?.trim().to_lowercase();
            let result = client
                .request_cancellable(
                    json!({"type":"provider_diagnostic_request","provider":provider}),
                    "provider_diagnostic_response",
                    Duration::from_secs(180),
                    context.cancel.clone(),
                )
                .await?;
            Ok(Output::new(
                json!({"provider":result["provider"],"diagnostic":result["diagnostic"]}),
            )
            .id("provider")
            .human(text(&result, "diagnostic")))
        }
        _ => Err(anyhow!("Unknown provider command: {command}")),
    }
}
async fn permit(
    command: &str,
    m: &ArgMatches,
    context: &Context,
    client: &DaemonClient,
) -> Result<Output> {
    if command == "ls" {
        let agents = all(
            client,
            context,
            json!({"filter":{"includeArchived":true}}),
            false,
        )
        .await?;
        let rows:Vec<Value>=agents.iter().flat_map(|agent|array(&agent["pendingPermissions"]).iter().map(move|p|json!({"id":p["id"],"agentId":agent["id"],"agentShortId":short(text(agent,"id")),"name":p["name"],"description":p["description"].as_str().unwrap_or("-")}))).collect();
        return permission_output(rows, false, context);
    }
    if command != "allow" && command != "deny" {
        return Err(anyhow!("Unknown permit command: {command}"));
    }
    let request_id = string(m, "request");
    if command == "deny" && request_id.is_none() && !flag(m, "all") {
        return Err(error(
            "MISSING_ARGUMENT",
            "Request ID is required unless --all is specified",
        ));
    }
    let mut response = json!({"behavior":command});
    if command == "allow" {
        if let Some(input) = string(m, "input") {
            response["updatedInput"] = serde_json::from_str(input)
                .map_err(|e| error("INVALID_JSON", format!("Invalid JSON for --input: {e}")))?;
        }
    } else {
        if let Some(message) = string(m, "message") {
            response["message"] = json!(message);
        }
        if flag(m, "interrupt") {
            response["interrupt"] = json!(true);
        }
    }
    let agent = fetch(client, context, required(m, "agent")?).await?;
    let permissions = array(&agent["pendingPermissions"]);
    if permissions.is_empty() {
        return Err(error(
            "NO_PENDING_PERMISSIONS",
            format!(
                "No pending permissions for agent {}",
                short(text(&agent, "id"))
            ),
        ));
    }
    let selected: Vec<&Value> = if flag(m, "all") || (command == "allow" && request_id.is_none()) {
        permissions.iter().collect()
    } else {
        let prefix = request_id.unwrap();
        vec![
            permissions
                .iter()
                .find(|p| text(p, "id").starts_with(prefix))
                .ok_or_else(|| {
                    error(
                        "PERMISSION_NOT_FOUND",
                        format!("Permission request not found: {prefix}"),
                    )
                })?,
        ]
    };
    let mut rows = Vec::with_capacity(selected.len());
    for permission in selected {
        let id = text(permission, "id");
        let ack=client.request_cancellable(json!({"type":"agent_permission_response","agentId":agent["id"],"requestId":id,"response":response}),"agent_permission_resolved",Duration::from_secs(15),context.cancel.clone()).await.map_err(|e|error("PERMISSION_RESPONSE_UNCONFIRMED",format!("Permission response unconfirmed for request {id}: {e}. Use `paseo permit ls` to check whether the request is still pending")))?;
        if ack["agentId"] != agent["id"] {
            return Err(error(
                "PERMISSION_RESPONSE_UNCONFIRMED",
                "Permission response acknowledged a different agent",
            ));
        }
        rows.push(json!({"requestId":id,"agentId":agent["id"],"agentShortId":short(text(&agent,"id")),"name":permission["name"],"result":if command=="allow"{"allowed"}else{"denied"}}));
    }
    permission_output(rows, true, context)
}
fn permission_output(rows: Vec<Value>, response: bool, context: &Context) -> Result<Output> {
    let key = if response { "requestId" } else { "id" };
    let mut output = Output::new(json!(rows)).id(key);
    if context.output.format == Format::Table && !context.output.quiet {
        let mut display = output.data.clone();
        for row in display.as_array_mut().unwrap() {
            row[key] = json!(text(row, key).chars().take(8).collect::<String>());
        }
        let columns: &[(&'static str, &'static str)] = if response {
            &[
                ("REQUEST ID", "requestId"),
                ("AGENT", "agentShortId"),
                ("TOOL", "name"),
                ("RESULT", "result"),
            ]
        } else {
            &[
                ("AGENT", "agentShortId"),
                ("REQ_ID", "id"),
                ("TOOL", "name"),
                ("DESCRIPTION", "description"),
            ]
        };
        let human = Output::new(display)
            .columns(columns)
            .widths(if response {
                &[12, 10, 20, 10]
            } else {
                &[12, 12, 20, 50]
            })
            .color_column("result", |value, _| match value.as_str() {
                Some("allowed") => Some(32),
                Some("denied") => Some(31),
                _ => None,
            })
            .render(&context.output)?;
        output = output.human(human);
    }
    Ok(output)
}
