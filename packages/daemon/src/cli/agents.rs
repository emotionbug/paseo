use super::{Context, flag, multiple, option, output::Output, positional, string, strings};
use crate::client::{ClientError, DaemonClient};
use anyhow::{Result, anyhow};
use clap::{ArgMatches, Command};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};
mod launch;
mod presentation;
mod stream;

pub(super) fn error(code: &str, message: impl Into<String>) -> anyhow::Error {
    ClientError::new(code, message).into()
}
pub(super) fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or("")
}
pub(super) fn array(v: &Value) -> &[Value] {
    v.as_array().map(Vec::as_slice).unwrap_or(&[])
}
pub(super) fn response_array<'a>(value: &'a Value, key: &str) -> Result<&'a [Value]> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| {
            error(
                "DAEMON_INVALID_RESPONSE",
                format!("Daemon response is missing the {key} array"),
            )
        })
}
pub(super) fn short(id: &str) -> String {
    id.chars().take(7).collect()
}
pub(super) fn required<'a>(m: &'a ArgMatches, key: &str) -> Result<&'a str> {
    string(m, key)
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| error("MISSING_ARGUMENT", format!("{key} is required")))
}
pub(super) async fn rpc(
    client: &DaemonClient,
    context: &Context,
    message: Value,
    response: &str,
) -> Result<Value> {
    let payload = client
        .request_cancellable(
            message,
            response,
            Duration::from_secs(60),
            context.cancel.clone(),
        )
        .await?;
    if payload["accepted"] == false {
        return Err(error(
            "DAEMON_RPC_ERROR",
            payload["error"]
                .as_str()
                .unwrap_or("Daemon rejected the request"),
        ));
    }
    Ok(payload)
}
pub(super) async fn fetch(client: &DaemonClient, context: &Context, id: &str) -> Result<Value> {
    let payload = rpc(
        client,
        context,
        json!({"type":"fetch_agent_request","agentId":id}),
        "fetch_agent_response",
    )
    .await?;
    payload
        .get("agent")
        .filter(|v| v.is_object())
        .cloned()
        .ok_or_else(|| {
            ClientError::new("AGENT_NOT_FOUND", format!("No agent found matching: {id}"))
                .details("Use `paseo ls` to list available agents")
                .into()
        })
}
pub(super) async fn all(
    client: &DaemonClient,
    context: &Context,
    mut request: Value,
    history: bool,
) -> Result<Vec<Value>> {
    request["type"] = json!(if history {
        "fetch_agent_history_request"
    } else {
        "fetch_agents_request"
    });
    let response = if history {
        "fetch_agent_history_response"
    } else {
        "fetch_agents_response"
    };
    let mut result = Vec::new();
    let mut cursor = None;
    loop {
        request["page"] = json!({"limit":200});
        if let Some(cursor) = &cursor {
            request["page"]["cursor"] = json!(cursor);
        }
        let payload = rpc(client, context, request.clone(), response).await?;
        result.extend(
            response_array(&payload, "entries")?
                .iter()
                .map(|e| e["agent"].clone()),
        );
        let next = payload["pageInfo"]
            .get("nextCursor")
            .ok_or_else(|| {
                error(
                    "DAEMON_INVALID_RESPONSE",
                    "Agent directory response is missing pageInfo.nextCursor",
                )
            })?
            .as_str()
            .map(str::to_owned);
        if next.is_none() {
            break;
        }
        if next == cursor {
            return Err(anyhow!("Agent directory returned a repeated cursor"));
        }
        cursor = next;
    }
    Ok(result)
}
fn resolve_named<'a>(query: &str, agents: &'a [Value]) -> Option<&'a Value> {
    if query.is_empty() {
        return None;
    }
    if let Some(agent) = agents.iter().find(|a| a["id"] == query) {
        return Some(agent);
    }
    let query = query.to_lowercase();
    let prefixes: Vec<_> = agents
        .iter()
        .filter(|a| text(a, "id").to_lowercase().starts_with(&query))
        .collect();
    if prefixes.len() == 1 {
        return prefixes.first().copied();
    }
    for exact in [true, false] {
        let titles: Vec<_> = agents
            .iter()
            .filter(|a| {
                a["title"].as_str().is_some_and(|title| {
                    if exact {
                        title.to_lowercase() == query
                    } else {
                        title.to_lowercase().contains(&query)
                    }
                })
            })
            .collect();
        if titles.len() == 1 {
            return titles.first().copied();
        }
    }
    prefixes.first().copied()
}
fn same_or_descendant(base: &str, candidate: &str) -> bool {
    let mut base = base.replace('\\', "/");
    let mut candidate = candidate.replace('\\', "/");
    if base.ends_with('/') {
        base.pop();
    }
    if candidate.ends_with('/') {
        candidate.pop();
    }
    let windows = |path: &str| {
        path.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
            && path.as_bytes().get(1) == Some(&b':')
            && path.as_bytes().get(2) == Some(&b'/')
    };
    if windows(&base) || windows(&candidate) {
        base.make_ascii_lowercase();
        candidate.make_ascii_lowercase();
    }
    candidate == base
        || candidate
            .strip_prefix(&base)
            .is_some_and(|rest| rest.starts_with('/'))
}
pub(super) fn key_values(values: Vec<String>, split: bool) -> Result<Value> {
    let mut result = serde_json::Map::new();
    for raw in values {
        let segments: Vec<&str> = if split {
            raw.split(',').collect()
        } else {
            vec![raw.as_str()]
        };
        for segment in segments {
            let segment = if split { segment.trim() } else { segment };
            if split && segment.is_empty() {
                continue;
            }
            let (key, value) = segment.split_once('=').ok_or_else(|| {
                error(
                    "INVALID_LABEL",
                    format!("Invalid key=value format: {segment}"),
                )
            })?;
            let key = if split { key.trim() } else { key };
            if split && key.is_empty() {
                return Err(error(
                    "INVALID_LABEL",
                    "Labels must include a non-empty key",
                ));
            }
            result.insert(key.into(), json!(value));
        }
    }
    Ok(Value::Object(result))
}
pub(super) fn duration(raw: Option<&str>) -> Result<Duration> {
    let Some(raw) = raw else {
        return Ok(Duration::ZERO);
    };
    let duration =
        super::parse_duration(raw).map_err(|e| error("INVALID_TIMEOUT", e.to_string()))?;
    if duration.is_zero() {
        return Err(error("INVALID_TIMEOUT", "Timeout must be positive"));
    }
    Ok(duration)
}
pub(super) async fn wait(
    client: &DaemonClient,
    context: &Context,
    id: &str,
    timeout: Duration,
) -> Result<Value> {
    let mut request = json!({"type":"wait_for_finish_request","agentId":id});
    if !timeout.is_zero() {
        request["timeoutMs"] = json!(timeout.as_millis());
    }
    client
        .request_cancellable(
            request,
            "wait_for_finish_response",
            if timeout.is_zero() {
                Duration::ZERO
            } else {
                timeout + Duration::from_secs(5)
            },
            context.cancel.clone(),
        )
        .await
}
fn common_commands() -> Vec<Command> {
    vec![
        Command::new("ls")
            .about("List agents")
            .arg(super::switch("all", "all").short('a'))
            .arg(super::switch("global", "global").short('g'))
            .arg(super::switch("background", "background"))
            .arg(multiple("label", "label"))
            .arg(option("thinking", "thinking")),
        launch::run_command(),
        Command::new("import")
            .about("Import a provider session")
            .arg(positional("id", false))
            .arg(option("provider", "provider"))
            .arg(option("cwd", "cwd"))
            .arg(multiple("label", "label")),
        Command::new("logs")
            .about("View agent activity")
            .arg(positional("id", true))
            .arg(super::switch("follow", "follow").short('f'))
            .arg(option("tail", "tail"))
            .arg(option("filter", "filter"))
            .arg(option("since", "since")),
        Command::new("attach")
            .about("Attach to an agent output stream")
            .arg(positional("id", true)),
        Command::new("send")
            .arg(positional("id", true))
            .arg(positional("prompt-argument", false))
            .arg(option("prompt", "prompt"))
            .arg(option("prompt-file", "prompt-file"))
            .arg(multiple("image", "image"))
            .arg(option("behavior", "behavior").value_parser(["interrupt", "steer"]))
            .arg(super::switch("no-wait", "no-wait")),
        Command::new("inspect").arg(positional("id", true)),
        Command::new("stop")
            .arg(positional("id", false))
            .arg(super::switch("all", "all"))
            .arg(option("cwd", "cwd")),
        Command::new("delete")
            .arg(positional("id", false))
            .arg(super::switch("all", "all"))
            .arg(option("cwd", "cwd")),
        Command::new("wait")
            .arg(positional("id", true))
            .arg(option("timeout", "timeout")),
        Command::new("archive")
            .arg(positional("id", true))
            .arg(super::switch("force", "force")),
    ]
}
pub fn commands() -> Vec<Command> {
    let mut commands = common_commands();
    let mut nested = common_commands();
    nested.extend([
        Command::new("open")
            .arg(positional("id", true))
            .arg(option("server", "server")),
        Command::new("reload").arg(positional("id", true)),
        Command::new("detach").arg(positional("id", true)),
        Command::new("mode")
            .arg(positional("id", true))
            .arg(positional("mode", false))
            .arg(super::switch("list", "list")),
        Command::new("update")
            .arg(positional("id", true))
            .arg(option("name", "name"))
            .arg(multiple("label", "label"))
            .arg(option("thinking", "thinking")),
    ]);
    commands.push(
        Command::new("agent")
            .subcommand_required(true)
            .subcommands(nested),
    );
    commands.push(launch::chat_command());
    commands
}
pub async fn execute(path: &[&str], m: &ArgMatches, context: &Context) -> Result<Option<Output>> {
    let command = match path {
        ["agent", command] => *command,
        [command]
            if matches!(
                *command,
                "run"
                    | "ls"
                    | "import"
                    | "logs"
                    | "attach"
                    | "send"
                    | "inspect"
                    | "stop"
                    | "delete"
                    | "wait"
                    | "archive"
                    | "chat"
            ) =>
        {
            *command
        }
        _ => return Ok(None),
    };
    launch::check_noninteractive(command, m)?;
    if command == "open" {
        if let Some(server) = string(m, "server").map(str::trim).filter(|s| !s.is_empty()) {
            let id = required(m, "id")?;
            super::open_agent_desktop(server, id).await?;
            return Ok(Some(
                Output::new(json!({"agentId":id,"serverId":server,"status":"opened"}))
                    .id("agentId")
                    .columns(&[
                        ("AGENT ID", "agentId"),
                        ("SERVER ID", "serverId"),
                        ("STATUS", "status"),
                    ]),
            ));
        }
    }
    let client = context.connect().await?;
    let result = execute_connected(command, m, context, &client).await;
    client.close().await;
    result
        .map(Some)
        .map_err(|failure| command_error(command, m, failure))
}
fn command_error(command: &str, m: &ArgMatches, failure: anyhow::Error) -> anyhow::Error {
    if failure
        .downcast_ref::<ClientError>()
        .is_some_and(|e| e.code != "DAEMON_RPC_ERROR")
    {
        return failure;
    }
    let (code, operation) = match command {
        "run" => ("AGENT_CREATE_FAILED", "create agent"),
        "import" => ("AGENT_IMPORT_FAILED", "import agent"),
        "send" => ("SEND_FAILED", "send message"),
        "wait" => ("WAIT_FAILED", "wait for agent"),
        "ls" => ("LIST_AGENTS_FAILED", "list agents"),
        "inspect" => ("INSPECT_FAILED", "inspect agent"),
        "archive" => ("ARCHIVE_FAILED", "archive agent"),
        "reload" => ("RELOAD_FAILED", "reload agent"),
        "update" => ("UPDATE_FAILED", "update agent"),
        "delete" => ("DELETE_AGENT_FAILED", "delete agent(s)"),
        "stop" => ("STOP_AGENT_FAILED", "stop agent(s)"),
        "open" => ("OPEN_FAILED", "open agent"),
        "mode" => (
            "MODE_OPERATION_FAILED",
            if flag(m, "list") {
                "list modes"
            } else {
                "set mode"
            },
        ),
        _ => return failure,
    };
    error(code, format!("Failed to {operation}: {failure}"))
}
async fn execute_connected(
    command: &str,
    m: &ArgMatches,
    context: &Context,
    client: &DaemonClient,
) -> Result<Output> {
    match command {
        "run" => launch::run(m, context, client).await,
        "chat" => launch::chat(m, context, client).await,
        "import" => launch::import(m, context, client).await,
        "logs" | "attach" => stream::logs(m, context, client, command == "attach").await,
        "ls" => {
            let mut request = json!({"filter":{}});
            if !flag(m, "global") && !flag(m, "all") {
                request["scope"] = json!("active");
            }
            if flag(m, "all") {
                request["filter"]["includeArchived"] = json!(true);
            }
            if flag(m, "all") || flag(m, "background") {
                client.require_feature(
                    "backgroundWorkspaces",
                    "Update the host to use background workspaces.",
                )?;
                request["filter"]["includeBackground"] = json!(true);
            }
            let labels = key_values(
                strings(m, "label")
                    .into_iter()
                    .filter(|s| s.contains('='))
                    .collect(),
                false,
            )?;
            if labels.as_object().is_some_and(|v| !v.is_empty()) {
                request["filter"]["labels"] = labels;
            }
            if let Some(thinking) = string(m, "thinking") {
                if thinking.trim().is_empty() {
                    return Err(error(
                        "INVALID_THINKING_OPTION",
                        "--thinking cannot be empty",
                    ));
                }
                request["filter"]["thinkingOptionId"] = json!(thinking.trim());
            }
            let mut agents = all(client, context, request, false).await?;
            if !flag(m, "all") {
                agents.retain(|a| a["archivedAt"].is_null());
            }
            agents.sort_by(|a, b| {
                let rank = |a: &Value| match text(a, "status") {
                    "running" => 0,
                    "idle" => 1,
                    _ => 2,
                };
                rank(a)
                    .cmp(&rank(b))
                    .then_with(|| text(b, "createdAt").cmp(text(a, "createdAt")))
            });
            Ok(Output::new(json!(
                agents
                    .iter()
                    .map(presentation::list_item)
                    .collect::<Vec<_>>()
            ))
            .id("shortId")
            .columns(&[
                ("AGENT ID", "shortId"),
                ("NAME", "name"),
                ("PROVIDER", "provider"),
                ("THINKING", "thinking"),
                ("STATUS", "status"),
                ("CWD", "cwd"),
                ("CREATED", "created"),
            ])
            .widths(&[12, 20, 15, 12, 10, 30, 15])
            .color_column("status", presentation::status_color))
        }
        "inspect" => presentation::inspect(
            &fetch(client, context, required(m, "id")?).await?,
            &context.output,
        ),
        "send" => send(m, context, client).await,
        "wait" => {
            let timeout = duration(string(m, "timeout"))?;
            let state = wait(client, context, required(m, "id")?, timeout).await?;
            let id = state["final"]["id"].as_str().unwrap_or(required(m, "id")?);
            let status = text(&state, "status");
            let mut message = match status {
                "timeout" => {
                    if timeout.is_zero() {
                        format!(
                            "Agent wait timed out. Run `paseo wait {id}` again to keep waiting."
                        )
                    } else {
                        format!(
                            "Agent did not finish within {} second{}. Run `paseo wait {id}` again to keep waiting.",
                            timeout.as_secs(),
                            if timeout.as_secs() == 1 { "" } else { "s" }
                        )
                    }
                }
                "permission" => state["final"]["pendingPermissions"][0]["kind"]
                    .as_str()
                    .map(|kind| format!("Agent is waiting for permission: {kind}"))
                    .unwrap_or_else(|| "Agent is waiting for permission".into()),
                "error" => state["error"]
                    .as_str()
                    .unwrap_or("Agent finished with error")
                    .into(),
                _ => "Agent is idle.".into(),
            };
            if matches!(status, "idle" | "timeout") {
                if let Ok(items) =
                    stream::history(client, context, id, Duration::from_secs(2)).await
                {
                    let transcript = stream::transcript(&items, Some(5))?;
                    if !transcript.is_empty() && transcript != "No activity to display." {
                        message.push_str(&format!("\nLast 5 activity items:\n{transcript}"));
                    }
                }
            }
            Ok(message_output(
                json!({"agentId":id,"status":status,"message":message}),
            ))
        }
        "stop" | "delete" => stop_delete(command, m, context, client).await,
        "open" => {
            let id = required(m, "id")?;
            let agent = fetch(client, context, id).await?;
            let info = client.server_info();
            let server = text(&info, "serverId").trim();
            if server.is_empty() {
                return Err(error(
                    "SERVER_ID_UNAVAILABLE",
                    "Daemon did not provide a server ID",
                ));
            }
            let (id, server) = (text(&agent, "id").to_owned(), server.to_owned());
            super::open_agent_desktop(&server, &id).await?;
            Ok(
                Output::new(json!({"agentId":id,"serverId":server,"status":"opened"}))
                    .id("agentId")
                    .columns(&[
                        ("AGENT ID", "agentId"),
                        ("SERVER ID", "serverId"),
                        ("STATUS", "status"),
                    ]),
            )
        }
        "mode" => {
            if !flag(m, "list") && string(m, "mode").is_none_or(|s| s.trim().is_empty()) {
                return Err(error(
                    "MISSING_ARGUMENT",
                    "Mode argument required unless --list is specified",
                ));
            }
            let agent = fetch(client, context, required(m, "id")?).await?;
            if flag(m, "list") {
                let modes = array(&agent["availableModes"])
                    .iter()
                    .map(|mode| {
                        let mut value = json!({"id":mode["id"],"label":mode["label"]});
                        if let Some(description) = mode.get("description") {
                            value["description"] = description.clone();
                        }
                        value
                    })
                    .collect::<Vec<_>>();
                return Ok(Output::new(json!(modes))
                    .id("id")
                    .columns(&[
                        ("MODE", "id"),
                        ("LABEL", "label"),
                        ("DESCRIPTION", "description"),
                    ])
                    .widths(&[15, 25, 40]));
            }
            let mode = required(m, "mode")?.trim();
            rpc(
                client,
                context,
                json!({"type":"set_agent_mode_request","agentId":agent["id"],"modeId":mode}),
                "set_agent_mode_response",
            )
            .await?;
            Ok(
                Output::new(json!({"agentId":short(text(&agent,"id")),"mode":mode}))
                    .id("agentId")
                    .columns(&[("AGENT ID", "agentId"), ("MODE", "mode")])
                    .widths(&[12, 20]),
            )
        }
        "update" => update(m, context, client).await,
        "archive" | "reload" | "detach" => {
            let agents = all(
                client,
                context,
                json!({"filter":{"includeArchived":true}}),
                false,
            )
            .await?;
            let agent = resolve_named(required(m, "id")?, &agents).ok_or_else(|| {
                error(
                    "AGENT_NOT_FOUND",
                    format!("Agent not found: {}", required(m, "id").unwrap_or("")),
                )
            })?;
            let id = text(agent, "id");
            if command == "archive" {
                if !agent["archivedAt"].is_null() {
                    return Err(error(
                        "AGENT_ALREADY_ARCHIVED",
                        format!("Agent {} is already archived", short(id)),
                    ));
                }
                if agent["status"] == "running" && !flag(m, "force") {
                    return Err(error(
                        "AGENT_RUNNING",
                        "Agent is running. Use --force to archive it.",
                    ));
                }
            }
            let (request, response) = match command {
                "archive" => ("archive_agent_request", "agent_archived"),
                "reload" => ("refresh_agent_request", "status"),
                _ => ("agent.detach.request", "agent.detach.response"),
            };
            let result = rpc(
                client,
                context,
                json!({"type":request,"agentId":id}),
                response,
            )
            .await?;
            let value = match command {
                "archive" => {
                    json!({"agentId":id,"status":"archived","archivedAt":result["archivedAt"]})
                }
                "reload" => {
                    json!({"agentId":result["agentId"],"status":"reloaded","timelineSize":result["timelineSize"].as_u64().unwrap_or(0)})
                }
                _ => json!({"agentId":id,"status":"detached"}),
            };
            Ok(Output::new(value).id("agentId").columns(match command {
                "archive" => &[
                    ("AGENT ID", "agentId"),
                    ("STATUS", "status"),
                    ("ARCHIVED AT", "archivedAt"),
                ],
                "reload" => &[
                    ("AGENT ID", "agentId"),
                    ("STATUS", "status"),
                    ("TIMELINE", "timelineSize"),
                ],
                _ => &[("AGENT ID", "agentId"), ("STATUS", "status")],
            }))
        }
        _ => Err(anyhow!("Unknown agent command: {command}")),
    }
}
pub(super) fn message_output(value: Value) -> Output {
    Output::new(value)
        .id("agentId")
        .columns(&[
            ("AGENT ID", "agentId"),
            ("STATUS", "status"),
            ("MESSAGE", "message"),
        ])
        .widths(&[12, 12, 40])
}
async fn send(m: &ArgMatches, context: &Context, client: &DaemonClient) -> Result<Output> {
    let values = [
        string(m, "prompt-argument"),
        string(m, "prompt"),
        string(m, "prompt-file"),
    ];
    let provided: Vec<(usize, &str)> = values
        .into_iter()
        .enumerate()
        .filter_map(|(i, v)| v.filter(|v| !v.trim().is_empty()).map(|v| (i, v)))
        .collect();
    if provided.len() > 1 {
        return Err(error(
            "CONFLICTING_PROMPT_INPUT",
            "Provide exactly one of prompt argument, --prompt, or --prompt-file",
        ));
    }
    let (source, value) = provided
        .first()
        .copied()
        .ok_or_else(|| error("MISSING_PROMPT", "A prompt is required"))?;
    let prompt = if source == 2 {
        tokio::fs::read_to_string(value).await?
    } else {
        value.into()
    };
    let id = required(m, "id")?;
    let source = std::env::var("PASEO_AGENT_ID")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let mut request = json!({"type":"send_agent_message_request","agentId":id,"text":prompt,"messageId":uuid::Uuid::new_v4().to_string(),"activeTurnBehavior":string(m,"behavior").unwrap_or(if source.is_some(){"steer"}else{"interrupt"})});
    if let Some(source) = source {
        client.require_feature(
            "agentMessageProvenance",
            "Update the Paseo host to send messages with agent provenance.",
        )?;
        request["sourceAgentId"] = json!(source.trim());
    }
    let images = launch::images(strings(m, "image"), false).await?;
    if !images.is_empty() {
        request["images"] = json!(images);
    }
    let receipt = rpc(client, context, request, "send_agent_message_response").await?;
    if flag(m, "no-wait") {
        let queued = receipt["deliveryStatus"] == "queued";
        return Ok(message_output(
            json!({"agentId":id,"status":if queued{"queued"}else{"sent"},"message":if queued{"Message queued for delivery after two quiet seconds"}else{"Message sent, not waiting for completion"}}),
        ));
    }
    let state = wait(client, context, id, Duration::from_secs(600)).await?;
    let status = text(&state, "status");
    let message = match status {
        "timeout" => "Timed out waiting for agent to finish",
        "permission" => "Agent is waiting for permission",
        "error" => state["error"]
            .as_str()
            .unwrap_or("Agent finished with error"),
        _ => "Agent completed processing the message",
    };
    Ok(message_output(
        json!({"agentId":state["final"]["id"].as_str().unwrap_or(id),"status":if status=="idle"{"completed"}else{status},"message":message}),
    ))
}
async fn stop_delete(
    command: &str,
    m: &ArgMatches,
    context: &Context,
    client: &DaemonClient,
) -> Result<Output> {
    let id = string(m, "id");
    if id.is_none() && !flag(m, "all") && string(m, "cwd").is_none() {
        return Err(error(
            "MISSING_ARGUMENT",
            "Agent ID required unless --all or --cwd is specified",
        ));
    }
    let agents = if flag(m, "all") || string(m, "cwd").is_some() {
        let mut agents = all(
            client,
            context,
            if command == "delete" {
                json!({})
            } else {
                json!({"filter":{"includeArchived":true}})
            },
            command == "delete",
        )
        .await?;
        if command == "stop" {
            agents.retain(|a| a["archivedAt"].is_null());
        }
        if !flag(m, "all") {
            let cwd = required(m, "cwd")?;
            agents.retain(|a| same_or_descendant(cwd, text(a, "cwd")));
        }
        agents
    } else {
        vec![fetch(client, context, id.unwrap()).await?]
    };
    let mut ids = Vec::new();
    for agent in agents {
        let id = text(&agent, "id");
        let result: Result<bool> = async {
            if agent["status"] == "running" {
                let cancel = rpc(
                    client,
                    context,
                    json!({"type":"cancel_agent_request","agentId":id}),
                    "cancel_agent_response",
                )
                .await;
                if command == "stop" {
                    cancel?;
                    return Ok(true);
                }
            }
            if command == "delete" {
                rpc(
                    client,
                    context,
                    json!({"type":"delete_agent_request","agentId":id}),
                    "agent_deleted",
                )
                .await?;
                return Ok(true);
            }
            Ok(false)
        }
        .await;
        match result {
            Ok(true) => ids.push(id.to_owned()),
            Ok(false) => {}
            Err(e) => {
                if command == "delete"
                    && e.downcast_ref::<ClientError>()
                        .is_some_and(|e| e.code != "DAEMON_RPC_ERROR")
                {
                    return Err(e);
                }
                eprintln!("Warning: Failed to {command} agent {}: {e}", short(id));
            }
        }
    }
    Ok(Output::new(if command == "delete" {
        json!({"deletedCount":ids.len(),"agentIds":ids})
    } else {
        json!({"stoppedCount":ids.len(),"agentIds":ids})
    })
    .id("agentIds")
    .columns(if command == "delete" {
        &[("DELETED", "deletedCount")]
    } else {
        &[("INTERRUPTED", "stoppedCount")]
    }))
}
async fn update(m: &ArgMatches, context: &Context, client: &DaemonClient) -> Result<Output> {
    let labels = key_values(strings(m, "label"), true)?;
    let name = string(m, "name").map(str::trim);
    let thinking = string(m, "thinking").map(str::trim);
    if name == Some("") {
        return Err(error("INVALID_NAME", "Name cannot be empty"));
    }
    if thinking == Some("") {
        return Err(error(
            "INVALID_THINKING_OPTION",
            "--thinking cannot be empty",
        ));
    }
    let metadata = name.is_some() || labels.as_object().is_some_and(|v| !v.is_empty());
    if metadata && thinking.is_some() {
        return Err(error(
            "INVALID_OPTIONS",
            "--thinking cannot be combined with --name or --label",
        ));
    }
    if !metadata && thinking.is_none() {
        return Err(error("NO_CHANGES_PROVIDED", "Nothing to update"));
    }
    let agent = fetch(client, context, required(m, "id")?).await?;
    let id = text(&agent, "id");
    let notice = if let Some(thinking) = thinking {
        if client.server_info()["features"]["agentThinkingUpdate"] != true {
            return Err(error(
                "DAEMON_UPDATE_REQUIRED",
                "Update the host to use agent thinking updates.",
            ));
        }
        rpc(
            client,
            context,
            json!({"type":"set_agent_thinking_request","agentId":id,"thinkingOptionId":thinking}),
            "set_agent_thinking_response",
        )
        .await?["notice"]
            .clone()
    } else {
        let mut request = json!({"type":"update_agent_request","agentId":id});
        if let Some(name) = name {
            request["name"] = json!(name);
        }
        if labels.as_object().is_some_and(|v| !v.is_empty()) {
            request["labels"] = labels;
        }
        rpc(client, context, request, "update_agent_response").await?;
        Value::Null
    };
    let updated = fetch(client, context, id).await?;
    let labels = updated["labels"]
        .as_object()
        .map(|v| {
            v.iter()
                .map(|(k, v)| format!("{k}={}", v.as_str().unwrap_or("")))
                .collect::<Vec<_>>()
                .join(",")
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "-".into());
    Ok(Output::new(json!({"agentId":id,"name":updated["title"],"labels":labels,"thinkingOptionId":updated["effectiveThinkingOptionId"],"noticeType":notice["type"],"notice":notice["message"]})).id("agentId").columns(&[("AGENT ID","agentId"),("NAME","name"),("LABELS","labels"),("THINKING","thinkingOptionId"),("NOTICE","notice")]))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn durations_reject_nonpositive_and_unknown_units() {
        assert!(duration(Some("0s")).is_err());
        assert!(duration(Some("2fortnights")).is_err());
        assert_eq!(duration(Some("1h30m")).unwrap(), Duration::from_secs(5400));
    }
    #[test]
    fn metadata_labels_split_but_environment_values_do_not() {
        assert_eq!(
            key_values(vec!["a=one,b=two".into()], true).unwrap(),
            json!({"a":"one","b":"two"})
        );
        assert_eq!(
            key_values(vec!["a=one,b=two".into()], false).unwrap(),
            json!({"a":"one,b=two"})
        );
        assert!(key_values(vec![" =bad".into()], true).is_err());
    }
    #[test]
    fn named_lookup_prefers_unique_title_to_ambiguous_prefix() {
        let agents = vec![
            json!({"id":"abc-one","title":"first"}),
            json!({"id":"abc-two","title":"abc"}),
        ];
        assert_eq!(resolve_named("abc", &agents).unwrap()["id"], "abc-two");
        assert_eq!(resolve_named("ABC-O", &agents).unwrap()["id"], "abc-one");
    }
}
