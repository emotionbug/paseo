use crate::{
    cli::{Context, flag, option, output::Output, positional, required, string, strings, switch},
    client::{ClientError, DaemonClient},
};
use anyhow::{Result, anyhow, bail, ensure};
use clap::{ArgAction, ArgMatches, Command};
use serde_json::{Value, json};
use std::time::Duration;
const TIMEOUT: Duration = Duration::from_secs(30);
const TERMINALS: &[(&str, &str)] = &[
    ("ID", "id"),
    ("NAME", "name"),
    ("CWD", "cwd"),
    ("WORKSPACE", "workspaceId"),
];
const SCRIPTS: &[(&str, &str)] = &[
    ("NAME", "scriptName"),
    ("TYPE", "type"),
    ("LIFECYCLE", "lifecycle"),
    ("HEALTH", "health"),
    ("PORT", "port"),
    ("PROXY URL", "proxyUrl"),
    ("TERMINAL", "terminalId"),
];
pub fn commands() -> Vec<Command> {
    let terminal = Command::new("terminal")
        .about("Manage workspace terminals")
        .subcommand(
            Command::new("ls")
                .arg(switch("all", "all").conflicts_with_all(["cwd", "workspace"]))
                .arg(option("workspace", "workspace"))
                .arg(option("cwd", "cwd")),
        )
        .subcommand(
            Command::new("create")
                .arg(option("workspace", "workspace"))
                .arg(option("cwd", "cwd"))
                .arg(option("name", "name")),
        )
        .subcommand(Command::new("kill").arg(positional("terminal-id", true)))
        .subcommand(
            Command::new("capture")
                .arg(positional("terminal-id", true))
                .arg(option("start", "start").allow_hyphen_values(true))
                .arg(option("end", "end").allow_hyphen_values(true))
                .arg(switch("scrollback", "scrollback").short('S'))
                .arg(switch("ansi", "ansi")),
        )
        .subcommand(
            Command::new("send-keys")
                .arg(positional("terminal-id", true))
                .arg(
                    positional("keys", true)
                        .num_args(1..)
                        .action(ArgAction::Append),
                )
                .arg(switch("literal", "literal").short('l')),
        );
    let mut script = Command::new("script").about("Manage configured workspace scripts");
    for action in ["ls", "start", "stop"] {
        let mut child = Command::new(action)
            .arg(option("cwd", "cwd"))
            .arg(option("workspace", "workspace"));
        if action != "ls" {
            child = child.arg(positional("name", true));
        }
        script = script.subcommand(child);
    }
    vec![terminal, script]
}
pub async fn execute(path: &[&str], m: &ArgMatches, context: &Context) -> Result<Option<Output>> {
    if path.len() != 2 || !matches!(path[0], "terminal" | "script") {
        return Ok(None);
    }
    let client = context.connect().await?;
    let result = if path[0] == "terminal" {
        terminal(path[1], m, &client, context).await
    } else {
        script(path[1], m, &client, context).await
    };
    client.close().await;
    result.map(Some)
}
async fn request(
    client: &DaemonClient,
    message: Value,
    response: &str,
    context: &Context,
) -> Result<Value> {
    client
        .request_cancellable(message, response, TIMEOUT, context.cancel.clone())
        .await
}
fn project_terminal(value: &Value) -> Result<Value> {
    for key in ["id", "workspaceId", "cwd", "name"] {
        ensure!(value[key].is_string(), "Invalid terminal metadata: {key}");
    }
    Ok(
        json!({"id":value["id"],"workspaceId":value["workspaceId"],"cwd":value["cwd"],"name":value["name"]}),
    )
}
async fn terminal(
    action: &str,
    m: &ArgMatches,
    client: &DaemonClient,
    context: &Context,
) -> Result<Output> {
    client.require_feature(
        "workspaceTerminals",
        "Update the host to use workspace terminals through the SDK.",
    )?;
    if action == "ls" {
        let mut message = json!({"type":"list_terminals_request"});
        if let Some(id) = string(m, "workspace") {
            message["workspaceId"] = id.into();
        } else if !flag(m, "all") {
            message["cwd"] = string(m, "cwd")
                .map(str::to_owned)
                .unwrap_or(std::env::current_dir()?.to_string_lossy().into_owned())
                .into();
        }
        let result = request(client, message, "list_terminals_response", context).await?;
        let rows = result["terminals"]
            .as_array()
            .ok_or_else(|| anyhow!("Invalid terminal listing"))?
            .iter()
            .map(project_terminal)
            .collect::<Result<Vec<_>>>()?;
        return Ok(terminal_output(json!(rows)));
    }
    if action == "create" {
        let cwd = match string(m, "cwd") {
            Some(cwd) => Some(cwd.to_owned()),
            None if string(m, "workspace").is_none() => {
                Some(std::env::current_dir()?.to_string_lossy().into_owned())
            }
            None => None,
        };
        let (workspace, directory) = if let Some(workspace) = string(m, "workspace") {
            let directory = if let Some(cwd) = cwd.clone() {
                cwd
            } else {
                let rows = super::projects::list_workspaces(client, false).await?;
                rows.iter()
                    .find(|row| row["id"] == workspace)
                    .and_then(|row| row["workspaceDirectory"].as_str())
                    .ok_or_else(|| anyhow!("Workspace not found: {workspace}"))?
                    .into()
            };
            (workspace.to_owned(), directory)
        } else {
            let cwd = cwd
                .as_deref()
                .ok_or_else(|| anyhow!("Working directory unavailable"))?;
            let opened = request(
                client,
                json!({"type":"open_project_request","cwd":cwd}),
                "open_project_response",
                context,
            )
            .await?;
            let workspace = opened["workspace"]["id"]
                .as_str()
                .ok_or_else(|| anyhow!("The daemon did not open a workspace for {cwd}"))?;
            (workspace.into(), cwd.into())
        };
        let mut message =
            json!({"type":"create_terminal_request","workspaceId":workspace,"cwd":directory});
        if let Some(name) = string(m, "name") {
            message["name"] = name.into();
        }
        let result = request(client, message, "create_terminal_response", context).await?;
        return Ok(terminal_output(project_terminal(&result["terminal"])?));
    }
    let selector = required(m, "terminal-id")?;
    let rows = request(
        client,
        json!({"type":"list_terminals_request"}),
        "list_terminals_response",
        context,
    )
    .await?;
    let id=resolve_terminal(selector,rows["terminals"].as_array().ok_or_else(||anyhow!("Invalid terminal list"))?).ok_or_else(||ClientError::new("TERMINAL_NOT_FOUND",format!("No terminal found matching: {selector}. Use `paseo terminal ls --all` to list available terminals.")))?;
    match action {
        "kill" => {
            let result = request(
                client,
                json!({"type":"kill_terminal_request","terminalId":id}),
                "kill_terminal_response",
                context,
            )
            .await?;
            ensure!(result["success"] == true, "Failed to kill terminal {id}");
            Ok(Output::new(json!({"terminalId":id,"success":true}))
                .table_data(
                    json!({"terminalId":id.chars().take(8).collect::<String>(),"success":true}),
                )
                .id("terminalId")
                .columns(&[("ID", "terminalId"), ("SUCCESS", "success")])
                .widths(&[8, 8]))
        }
        "capture" => {
            let mut message = json!({"type":"capture_terminal_request","terminalId":id,"stripAnsi":!flag(m,"ansi")});
            if flag(m, "scrollback") {
                message["start"] = 0.into();
            } else if let Some(value) = string(m, "start") {
                message["start"] = parse_line(value, "--start")?.into();
            }
            if let Some(value) = string(m, "end") {
                message["end"] = parse_line(value, "--end")?.into();
            }
            let result = request(client, message, "capture_terminal_response", context).await?;
            let lines = result["lines"]
                .as_array()
                .ok_or_else(|| anyhow!("Invalid terminal capture"))?;
            let text = lines
                .iter()
                .map(|line| {
                    line.as_str()
                        .ok_or_else(|| anyhow!("Invalid terminal capture line"))
                })
                .collect::<Result<Vec<_>>>()?
                .join("\n");
            Ok(Output::new(json!({"terminalId":result["terminalId"],"lines":lines,"totalLines":result["totalLines"]})).human(text))
        }
        "send-keys" => {
            let keys = strings(m, "keys");
            let data = if flag(m, "literal") {
                keys.join("")
            } else {
                keys.iter().map(|key| key_token(key)).collect::<String>()
            };
            let count = data.encode_utf16().count();
            client.send(json!({"type":"terminal_input","terminalId":id,"message":{"type":"input","data":data}})).await?;
            client.flush().await?;
            Ok(Output::new(json!({"terminalId":id,"keysSent":count})).human(""))
        }
        _ => bail!("Unknown terminal command {action}"),
    }
}
fn resolve_terminal<'a>(selector: &str, rows: &'a [Value]) -> Option<&'a str> {
    if selector.is_empty() {
        return None;
    }
    if let Some(row) = rows.iter().find(|r| r["id"] == selector) {
        return row["id"].as_str();
    }
    let query = selector.to_lowercase();
    let prefix: Vec<_> = rows
        .iter()
        .filter(|r| {
            r["id"]
                .as_str()
                .is_some_and(|id| id.to_lowercase().starts_with(&query))
        })
        .collect();
    if prefix.len() == 1 {
        return prefix[0]["id"].as_str();
    }
    if prefix.len() > 1 {
        return None;
    }
    let exact: Vec<_> = rows
        .iter()
        .filter(|r| {
            r["name"]
                .as_str()
                .is_some_and(|s| s.to_lowercase() == query)
        })
        .collect();
    if exact.len() == 1 {
        return exact[0]["id"].as_str();
    }
    let partial: Vec<_> = rows
        .iter()
        .filter(|r| {
            r["name"]
                .as_str()
                .is_some_and(|s| s.to_lowercase().contains(&query))
        })
        .collect();
    if partial.len() == 1 {
        partial[0]["id"].as_str()
    } else {
        None
    }
}
fn parse_line(value: &str, flag: &str) -> Result<i64> {
    let value = value.trim_start();
    let digits = value
        .strip_prefix('-')
        .or_else(|| value.strip_prefix('+'))
        .unwrap_or(value)
        .bytes()
        .take_while(u8::is_ascii_digit)
        .count();
    ensure!(digits > 0, "Invalid {flag} value: {value}");
    let end = digits + usize::from(value.starts_with(['-', '+']));
    value[..end].parse().map_err(Into::into)
}
fn key_token(key: &str) -> &str {
    match key {
        "Enter" => "\r",
        "Tab" => "\t",
        "Escape" => "\u{1b}",
        "Space" => " ",
        "BSpace" => "\u{7f}",
        "C-c" => "\u{3}",
        "C-d" => "\u{4}",
        "C-z" => "\u{1a}",
        "C-l" => "\u{c}",
        "C-a" => "\u{1}",
        "C-e" => "\u{5}",
        _ => key,
    }
}
async fn script(
    action: &str,
    m: &ArgMatches,
    client: &DaemonClient,
    context: &Context,
) -> Result<Output> {
    client.require_feature(
        "workspaceScriptManagement",
        "Update the host to use workspace script management.",
    )?;
    let workspace = if let Some(workspace) = string(m, "workspace") {
        workspace.to_owned()
    } else {
        let cwd = super::projects::absolute(string(m, "cwd").unwrap_or("."))?;
        let rows = super::projects::list_workspaces(client, false).await?;
        let matches = rows
            .iter()
            .filter(|row| {
                row["workspaceDirectory"]
                    .as_str()
                    .and_then(|p| super::projects::absolute(p).ok())
                    .as_deref()
                    == Some(&cwd)
            })
            .collect::<Vec<_>>();
        match matches.as_slice(){[row]=>row["id"].as_str().ok_or_else(||anyhow!("Invalid workspace ID"))?.into(),[]=>return Err(ClientError::new("WORKSPACE_NOT_FOUND",format!("No Paseo workspace found for {cwd}. Open the directory in Paseo first, or pass --workspace <workspace-id>.")).into()),_=>return Err(ClientError::new("WORKSPACE_AMBIGUOUS",format!("Multiple workspaces use {cwd}. Pass --workspace <workspace-id> to select one.")).into())}
    };
    let (kind, response) = match action {
        "ls" => (
            "workspace.script.list.request",
            "workspace.script.list.response",
        ),
        "start" => (
            "workspace.script.start.request",
            "workspace.script.start.response",
        ),
        "stop" => (
            "workspace.script.stop.request",
            "workspace.script.stop.response",
        ),
        _ => bail!("Unknown script command {action}"),
    };
    let mut message = json!({"type":kind,"workspaceId":workspace});
    if action != "ls" {
        message["scriptName"] = required(m, "name")?.into();
    }
    let result = request(client, message, response, context).await?;
    let data = if action == "ls" {
        result.get("scripts").cloned().unwrap_or(json!([]))
    } else {
        ensure!(
            result["script"].is_object(),
            "Script did not return status metadata"
        );
        result["script"].clone()
    };
    Ok(script_output(data))
}
fn terminal_output(data: Value) -> Output {
    let mut table = data.clone();
    let shorten = |row: &mut Value| {
        if let Some(id) = row["id"].as_str() {
            row["id"] = id.chars().take(8).collect::<String>().into();
        }
    };
    if let Some(rows) = table.as_array_mut() {
        for row in rows {
            shorten(row);
        }
    } else {
        shorten(&mut table);
    }
    Output::new(data)
        .table_data(table)
        .columns(TERMINALS)
        .widths(&[8, 24, 48, 24])
}
fn script_output(data: Value) -> Output {
    let mut table = data.clone();
    let defaults = |row: &mut Value| {
        for key in ["health", "port", "proxyUrl", "terminalId"] {
            if row[key].is_null() {
                row[key] = "-".into();
            }
        }
    };
    if let Some(rows) = table.as_array_mut() {
        for row in rows {
            defaults(row);
        }
    } else {
        defaults(&mut table);
    }
    Output::new(data)
        .table_data(table)
        .id("scriptName")
        .columns(SCRIPTS)
        .widths(&[20, 9, 10, 10, 7, 42, 12])
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ambiguous_terminal_prefix_does_not_fall_back_to_title() {
        let rows = vec![
            json!({"id":"abc1","name":"abc"}),
            json!({"id":"abc2","name":"second"}),
        ];
        assert_eq!(resolve_terminal("abc", &rows), None);
        assert_eq!(resolve_terminal("abc1", &rows), Some("abc1"));
    }
    #[test]
    fn terminal_key_semantics_keep_unknown_tokens_literal() {
        assert_eq!(key_token("Enter"), "\r");
        assert_eq!(key_token("C-c"), "\u{3}");
        assert_eq!(key_token("C-x"), "C-x");
    }
}
