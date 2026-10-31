use crate::{
    cli::{
        Context, flag, option,
        output::{Output, OutputOptions},
        parse_duration, positional, provider_model, required, string, switch,
    },
    client::{ClientError, DaemonClient, DaemonTarget},
};
use anyhow::{Result, anyhow, bail, ensure};
use clap::{ArgMatches, Command};
use serde_json::{Value, json};
use std::time::Duration;
const RPC: Duration = Duration::from_secs(30);
const SCHEDULE: &[(&str, &str)] = &[
    ("ID", "id"),
    ("NAME", "name"),
    ("CADENCE", "cadence"),
    ("TARGET", "target"),
    ("STATUS", "status"),
    ("NEXT RUN", "nextRunAt"),
];
pub fn commands() -> Vec<Command> {
    let mut create = Command::new("create")
        .arg(positional("prompt", true))
        .arg(switch("run-now", "run-now"));
    for key in [
        "every",
        "cron",
        "timezone",
        "name",
        "target",
        "provider",
        "mode",
        "thinking",
        "cwd",
        "max-runs",
        "expires-in",
    ] {
        create = create.arg(option(key, key).hide(key == "target"));
    }
    let mut update = Command::new("update")
        .arg(positional("id", true))
        .arg(switch("no-max-runs", "no-max-runs").conflicts_with("max-runs"))
        .arg(switch("no-expires-in", "no-expires-in").conflicts_with("expires-in"));
    for key in [
        "every",
        "cron",
        "timezone",
        "name",
        "prompt",
        "provider",
        "model",
        "mode",
        "cwd",
        "max-runs",
        "expires-in",
    ] {
        update = update.arg(option(key, key));
    }
    let mut schedule = Command::new("schedule")
        .about("Manage recurring schedules")
        .subcommand(create)
        .subcommand(update)
        .subcommand(Command::new("ls"));
    for name in ["inspect", "logs", "pause", "resume", "delete", "run-once"] {
        schedule = schedule.subcommand(Command::new(name).arg(positional("id", true)));
    }
    let mut heartbeat_create = Command::new("create")
        .arg(positional("prompt", true))
        .arg(option("cron", "cron").required(true));
    for key in ["timezone", "name", "max-runs", "expires-in"] {
        heartbeat_create = heartbeat_create.arg(option(key, key));
    }
    let heartbeat = Command::new("heartbeat")
        .about("Manage this agent's heartbeats")
        .subcommand(heartbeat_create)
        .subcommand(
            Command::new("update")
                .arg(positional("id", true))
                .arg(option("cron", "cron").required(true))
                .arg(option("timezone", "timezone")),
        )
        .subcommand(Command::new("delete").arg(positional("id", true)));
    vec![schedule, heartbeat]
}
pub async fn execute(path: &[&str], m: &ArgMatches, context: &Context) -> Result<Option<Output>> {
    if path.len() != 2 || !matches!(path[0], "schedule" | "heartbeat") {
        return Ok(None);
    }
    let heartbeat = path[0] == "heartbeat";
    let action = path[1];
    let caller = if heartbeat {
        Some(
            std::env::var("PASEO_AGENT_ID")
                .ok()
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| anyhow!("Heartbeat commands must run inside a Paseo agent"))?,
        )
    } else {
        None
    };
    let message = match action {
        "create" => Some(create_input(m, context, caller.as_deref())?),
        "update" => Some(update_input(m, heartbeat)?),
        _ => None,
    };
    let client = context.connect().await?;
    let result = run(action, m, context, &client, message, caller.as_deref()).await;
    client.close().await;
    result.map(Some)
}
async fn rpc(
    client: &DaemonClient,
    message: Value,
    kind: &str,
    context: &Context,
) -> Result<Value> {
    client
        .request_cancellable(
            message,
            &format!("schedule/{kind}/response"),
            RPC,
            context.cancel.clone(),
        )
        .await
}
async fn run(
    action: &str,
    m: &ArgMatches,
    context: &Context,
    client: &DaemonClient,
    message: Option<Value>,
    caller: Option<&str>,
) -> Result<Output> {
    if !matches!(action, "ls" | "create") {
        let id = required(m, "id")?;
        let result = rpc(
            client,
            json!({"type":"schedule/inspect","scheduleId":id}),
            "inspect",
            context,
        )
        .await?;
        let record = &result["schedule"];
        ensure!(record.is_object(), "Schedule not found: {id}");
        if let Some(caller) = caller {
            ensure!(
                record["target"]["type"] == "agent" && record["target"]["agentId"] == caller,
                "Heartbeat {id} does not belong to agent {caller}"
            );
        } else {
            ensure!(
                record["target"]["type"] == "new-agent",
                "Schedule not found: {id}"
            );
        }
        if action == "inspect" {
            return inspect_output(record.clone(), &context.output);
        }
    }
    let request = message.unwrap_or_else(|| {
        let mut message =
            json!({"type":format!("schedule/{}",if action=="ls"{"list"}else{action})});
        if let Some(id) = string(m, "id") {
            message["scheduleId"] = id.into();
        }
        message
    });
    let response = rpc(
        client,
        request,
        if action == "ls" { "list" } else { action },
        context,
    )
    .await?;
    match action {
        "ls" => {
            let rows = response["schedules"]
                .as_array()
                .ok_or_else(|| anyhow!("Invalid schedule listing"))?
                .iter()
                .filter(|s| s["target"]["type"] == "new-agent")
                .map(schedule_row)
                .collect::<Vec<_>>();
            Ok(Output::new(json!(rows))
                .columns(SCHEDULE)
                .widths(&[10, 20, 20, 20, 12, 24]))
        }
        "delete" => Ok(
            Output::new(json!({"id":response["scheduleId"],"status":"deleted"}))
                .columns(&[("ID", "id"), ("STATUS", "status")]),
        ),
        "logs" => {
            let rows=response["runs"].as_array().ok_or_else(||anyhow!("Invalid schedule log listing"))?.iter().map(|r|json!({"id":r["id"],"status":r["status"],"startedAt":r["startedAt"],"agentId":r["agentId"].as_str().map(|id|id.chars().take(7).collect::<String>()),"output":r["output"],"error":r["error"]})).collect::<Vec<_>>();
            Ok(Output::new(json!(rows)).columns(&[
                ("RUN ID", "id"),
                ("STATUS", "status"),
                ("STARTED", "startedAt"),
                ("AGENT", "agentId"),
                ("OUTPUT", "output"),
                ("ERROR", "error"),
            ]))
        }
        "create" | "pause" | "resume" | "run-once" | "update" => {
            ensure!(
                response["schedule"].is_object(),
                "Schedule operation did not return the schedule"
            );
            if action == "update" && caller.is_none() {
                inspect_output(response["schedule"].clone(), &context.output)
            } else {
                Ok(Output::new(schedule_row(&response["schedule"]))
                    .columns(SCHEDULE)
                    .widths(&[10, 20, 20, 20, 12, 24]))
            }
        }
        _ => bail!("Unknown schedule command {action}"),
    }
}
fn cadence(m: &ArgMatches) -> Result<Option<Value>> {
    let every = string(m, "every");
    let cron = string(m, "cron");
    let timezone = string(m, "timezone");
    ensure!(
        !(every.is_some() && cron.is_some()),
        "Specify at most one of --every or --cron"
    );
    if let Some(timezone) = timezone {
        ensure!(!timezone.trim().is_empty(), "--timezone cannot be empty");
        ensure!(cron.is_some(), "--timezone can only be used with --cron");
    }
    let Some(expression) = every
        .map(every_cron)
        .transpose()?
        .or_else(|| cron.map(|s| s.trim().to_owned()))
    else {
        return Ok(None);
    };
    let mut cadence = json!({"type":"cron","expression":expression});
    if let Some(timezone) = timezone {
        cadence["timezone"] = timezone.trim().into();
    }
    Ok(Some(cadence))
}
fn every_cron(value: &str) -> Result<String> {
    let seconds = parse_duration(value)?.as_secs();
    let minutes = seconds / 60;
    let result = if seconds == 0 || seconds % 60 != 0 {
        None
    } else if minutes < 60 && 60 % minutes == 0 {
        Some(format!("*/{minutes} * * * *"))
    } else if minutes == 60 {
        Some("0 * * * *".into())
    } else if minutes % 60 != 0 {
        None
    } else {
        let hours = minutes / 60;
        if hours < 24 && 24 % hours == 0 {
            Some(format!("0 */{hours} * * *"))
        } else if hours == 24 {
            Some("0 0 * * *".into())
        } else {
            None
        }
    };
    result.ok_or_else(||ClientError::new("UNREPRESENTABLE_CADENCE",format!("{value} cannot be represented faithfully by five-field cron. Use --cron for calendar schedules")).into())
}
fn create_input(m: &ArgMatches, context: &Context, heartbeat: Option<&str>) -> Result<Value> {
    let prompt = required(m, "prompt")?.trim();
    ensure!(!prompt.is_empty(), "Schedule prompt cannot be empty");
    let cadence = cadence(m)?.ok_or_else(|| anyhow!("Specify exactly one of --every or --cron"))?;
    let target = if let Some(agent) = heartbeat {
        json!({"type":"agent","agentId":agent})
    } else {
        let cwd = string(m, "cwd").map(str::trim).filter(|s| !s.is_empty());
        if matches!(context.target, DaemonTarget::Endpoint { .. }) && cwd.is_none() {
            return Err(ClientError::new("MISSING_CWD","--cwd is required when --host is specified (the local working directory will not exist on the remote daemon)").into());
        }
        let target = string(m, "target").map(str::trim).unwrap_or("");
        if !target.is_empty() && target != "new-agent" {
            ensure!(
                ["provider", "mode", "thinking"]
                    .iter()
                    .all(|key| string(m, key).is_none()),
                "--provider/--mode/--thinking can only be used with a new-agent target"
            );
            if target == "self" {
                let agent = std::env::var("PASEO_AGENT_ID")
                    .ok()
                    .filter(|s| !s.trim().is_empty())
                    .ok_or_else(|| {
                        anyhow!("--target self requires running inside a Paseo agent")
                    })?;
                json!({"type":"self","agentId":agent.trim()})
            } else {
                json!({"type":"agent","agentId":target})
            }
        } else {
            let (provider, model) = provider_model(string(m, "provider"), None, None)?;
            let mut config = json!({"provider":provider,"cwd":match cwd{Some(cwd)=>cwd.to_owned(),None=>std::env::current_dir()?.to_string_lossy().into_owned()}});
            if let Some(model) = model {
                config["model"] = model.into();
            }
            if let Some(mode) = string(m, "mode").map(str::trim).filter(|v| !v.is_empty()) {
                config["modeId"] = mode.into();
            }
            if let Some(thinking) = string(m, "thinking") {
                ensure!(!thinking.trim().is_empty(), "--thinking cannot be empty");
                config["thinkingOptionId"] = thinking.trim().into();
            }
            json!({"type":"new-agent","config":config})
        }
    };
    let mut message =
        json!({"type":"schedule/create","prompt":prompt,"cadence":cadence,"target":target});
    if heartbeat.is_none() {
        message["runOnCreate"] = flag(m, "run-now").into();
    }
    if let Some(name) = string(m, "name").map(str::trim).filter(|s| !s.is_empty()) {
        message["name"] = name.into();
    }
    limits(m, &mut message)?;
    Ok(message)
}
fn update_input(m: &ArgMatches, heartbeat: bool) -> Result<Value> {
    let id = required(m, "id")?.trim();
    ensure!(!id.is_empty(), "Schedule id cannot be empty");
    let mut message = json!({"type":"schedule/update","scheduleId":id});
    if let Some(cadence) = cadence(m)? {
        message["cadence"] = cadence;
    }
    if heartbeat {
        return Ok(message);
    }
    if let Some(name) = string(m, "name") {
        message["name"] = if name.trim().is_empty() {
            Value::Null
        } else {
            name.trim().into()
        };
    }
    if let Some(prompt) = string(m, "prompt") {
        ensure!(!prompt.trim().is_empty(), "--prompt cannot be empty");
        message["prompt"] = prompt.trim().into();
    }
    let mut config = json!({});
    if string(m, "provider").is_some() || string(m, "model").is_some() {
        let (provider, model) = provider_model(string(m, "provider"), string(m, "model"), None)?;
        config["provider"] = provider.into();
        if let Some(model) = model {
            config["model"] = model.into();
        }
    }
    if let Some(mode) = string(m, "mode") {
        config["modeId"] = if mode.trim().is_empty() {
            Value::Null
        } else {
            mode.trim().into()
        };
    }
    if let Some(cwd) = string(m, "cwd") {
        ensure!(!cwd.trim().is_empty(), "--cwd cannot be empty");
        config["cwd"] = cwd.trim().into();
    }
    if config.as_object().is_some_and(|m| !m.is_empty()) {
        message["newAgentConfig"] = config;
    }
    limits(m, &mut message)?;
    if flag(m, "no-max-runs") {
        message["maxRuns"] = Value::Null;
    }
    if flag(m, "no-expires-in") {
        message["expiresAt"] = Value::Null;
    }
    ensure!(
        message.as_object().unwrap().len() > 2,
        "Specify at least one field to update"
    );
    Ok(message)
}
fn limits(m: &ArgMatches, message: &mut Value) -> Result<()> {
    if let Some(max) = string(m, "max-runs") {
        let digits = max
            .trim_start()
            .trim_start_matches('+')
            .bytes()
            .take_while(u8::is_ascii_digit)
            .count();
        let text = max.trim_start().trim_start_matches('+');
        let value = text[..digits]
            .parse::<u64>()
            .ok()
            .filter(|v| *v > 0 && *v <= 9007199254740991)
            .ok_or_else(|| anyhow!("--max-runs must be a positive integer"))?;
        message["maxRuns"] = value.into();
    }
    if let Some(expires) = string(m, "expires-in") {
        let millis: i64 = parse_duration(expires)?.as_millis().try_into()?;
        let expires = chrono::Utc::now()
            .checked_add_signed(chrono::Duration::milliseconds(millis))
            .ok_or_else(|| anyhow!("Schedule expiration is out of range"))?;
        message["expiresAt"] = expires
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
            .into();
    }
    Ok(())
}
fn format_cadence(value: &Value) -> String {
    if value["type"] == "cron" {
        let expression = value["expression"].as_str().unwrap_or("");
        let timezone = value["timezone"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|s| format!(" ({s})"))
            .unwrap_or_default();
        format!("cron:{expression}{timezone}")
    } else {
        let millis = value["everyMs"].as_u64().unwrap_or(0);
        let mut seconds = millis / 1000;
        let hours = seconds / 3600;
        seconds %= 3600;
        let minutes = seconds / 60;
        seconds %= 60;
        let mut result = String::from("every:");
        if hours > 0 {
            result.push_str(&format!("{hours}h"));
        }
        if minutes > 0 {
            result.push_str(&format!("{minutes}m"));
        }
        if seconds > 0 || (hours == 0 && minutes == 0) {
            result.push_str(&format!("{seconds}s"));
        }
        result
    }
}
fn format_target(value: &Value) -> String {
    if value["type"] == "new-agent" {
        let provider = value["config"]["provider"].as_str().unwrap_or("");
        let model = value["config"]["model"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|s| format!("/{s}"))
            .unwrap_or_default();
        format!("new-agent:{provider}{model}")
    } else {
        format!(
            "{}:{}",
            value["type"].as_str().unwrap_or("agent"),
            value["agentId"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(7)
                .collect::<String>()
        )
    }
}
fn schedule_row(value: &Value) -> Value {
    json!({"id":value["id"],"name":value["name"],"cadence":format_cadence(&value["cadence"]),"target":format_target(&value["target"]),"status":value["status"],"nextRunAt":value["nextRunAt"],"lastRunAt":value["lastRunAt"]})
}
fn inspect_output(value: Value, options: &OutputOptions) -> Result<Output> {
    let mut rows = Vec::new();
    for (key, field) in [
        ("Id", "id"),
        ("Name", "name"),
        ("Prompt", "prompt"),
        ("Cadence", "cadence"),
        ("Target", "target"),
        ("Status", "status"),
        ("CreatedAt", "createdAt"),
        ("UpdatedAt", "updatedAt"),
        ("NextRunAt", "nextRunAt"),
        ("LastRunAt", "lastRunAt"),
        ("PausedAt", "pausedAt"),
        ("ExpiresAt", "expiresAt"),
        ("MaxRuns", "maxRuns"),
        ("RunCount", "runs"),
    ] {
        let text = match field {
            "cadence" => {
                if value[field]["type"] == "cron" {
                    format_cadence(&value[field])
                } else {
                    format!("every:{}ms", value[field]["everyMs"])
                }
            }
            "target" => format_target(&value[field]),
            "runs" => value[field]
                .as_array()
                .map(Vec::len)
                .unwrap_or(0)
                .to_string(),
            _ => value[field]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| value[field].to_string()),
        };
        rows.push(json!({"key":key,"value":text}));
    }
    let human = Output::new(json!(rows))
        .id("key")
        .columns(&[("KEY", "key"), ("VALUE", "value")])
        .render(&OutputOptions {
            format: super::super::output::Format::Table,
            quiet: false,
            ..options.clone()
        })?;
    if options.quiet {
        return Ok(Output::new(json!(rows)).id("key"));
    }
    Ok(Output::new(value).human(human))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn presets_do_not_change_rolling_intervals() {
        assert_eq!(every_cron("5m").unwrap(), "*/5 * * * *");
        assert_eq!(every_cron("1d").unwrap(), "0 0 * * *");
        assert!(every_cron("90m").is_err());
        assert!(every_cron("2d").is_err());
        assert!(every_cron("30s").is_err());
    }
    #[test]
    fn model_update_requires_explicit_provider() {
        let schedule = commands().remove(0);
        let parsed = schedule
            .try_get_matches_from(["schedule", "update", "id", "--model", "m"])
            .unwrap();
        assert!(update_input(parsed.subcommand().unwrap().1, false).is_err());
    }
}
