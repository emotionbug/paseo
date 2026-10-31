use super::*;
use base64::Engine;
use std::io::IsTerminal;
use tokio::io::{AsyncBufReadExt, BufReader};

pub(super) fn run_command() -> Command {
    let mut command = Command::new("run")
        .about("Create and start an agent with a task")
        .arg(positional("prompt", true));
    for name in [
        "title",
        "name",
        "provider",
        "model",
        "thinking",
        "mode",
        "new-workspace",
        "worktree",
        "worktree-mode",
        "worktree-slug",
        "new-branch",
        "base",
        "branch",
        "pr-number",
        "forge",
        "workspace",
        "cwd",
        "wait-timeout",
        "output-schema",
    ] {
        command = command.arg(option(name, name));
    }
    for name in ["image", "env", "label"] {
        command = command.arg(multiple(name, name));
    }
    for name in ["no-wait", "detach", "attach"] {
        command = command.arg(super::super::switch(name, name));
    }
    command.arg(
        super::super::switch("background", "background")
            .short('d')
            .hide(true),
    )
}
pub(super) fn chat_command() -> Command {
    let mut command = Command::new("chat").about("Chat through line-oriented standard input");
    for name in ["provider", "model", "thinking", "mode", "cwd", "resume"] {
        command = command.arg(option(name, name));
    }
    command
}
pub(super) fn check_noninteractive(command: &str, m: &ArgMatches) -> Result<()> {
    if command == "chat" && std::io::stdin().is_terminal() {
        return Err(error(
            "INTERACTIVE_REQUIRED",
            "Interactive terminal chat is not included. Pipe newline-separated prompts to paseo chat, or use paseo run/send and Paseo Desktop.",
        ));
    }
    if command == "import" && string(m, "id").is_none() {
        return Err(error(
            "INTERACTIVE_REQUIRED",
            "Interactive session selection is not included. Specify --provider <provider> <session-id>.",
        ));
    }
    Ok(())
}
fn opt<'a>(m: &'a ArgMatches, key: &str) -> Option<&'a str> {
    m.try_get_one::<String>(key)
        .ok()
        .flatten()
        .map(String::as_str)
}
async fn launch_config(m: &ArgMatches, context: &Context, client: &DaemonClient) -> Result<Value> {
    let config = rpc(
        client,
        context,
        json!({"type":"get_daemon_config_request"}),
        "get_daemon_config_response",
    )
    .await?;
    if !config["config"].is_object() {
        return Err(error(
            "DAEMON_INVALID_RESPONSE",
            "Daemon response is missing its configuration object",
        ));
    }
    let profile = array(&config["config"]["agentProfiles"])
        .iter()
        .find(|p| p["cliDefault"] == true);
    let (provider, model) = super::super::provider_model(
        opt(m, "provider"),
        opt(m, "model"),
        profile.and_then(|p| p["provider"].as_str()),
    )?;
    let profile = profile.filter(|p| p["provider"] == provider);
    let same_model = model
        .as_deref()
        .is_none_or(|model| profile.and_then(|p| p["model"].as_str()) == Some(model));
    let mut launch = json!({"provider":provider});
    for (key, value) in [
        (
            "model",
            model
                .as_deref()
                .or_else(|| profile.and_then(|p| p["model"].as_str())),
        ),
        (
            "modeId",
            opt(m, "mode").or_else(|| profile.and_then(|p| p["modeId"].as_str())),
        ),
        (
            "thinkingOptionId",
            opt(m, "thinking").or_else(|| {
                if same_model {
                    profile.and_then(|p| p["thinkingOptionId"].as_str())
                } else {
                    None
                }
            }),
        ),
    ] {
        if let Some(value) = value {
            if key == "thinkingOptionId" && value.trim().is_empty() {
                return Err(error(
                    "INVALID_THINKING_OPTION",
                    "--thinking cannot be empty",
                ));
            }
            launch[key] = json!(if key == "thinkingOptionId" {
                value.trim()
            } else {
                value
            });
        }
    }
    if let Some(values) = profile.and_then(|p| p.get("featureValues")) {
        launch["featureValues"] = values.clone();
    }
    Ok(launch)
}
fn absent(m: &ArgMatches, names: &[&str], message: &str) -> Result<()> {
    if names.iter().any(|name| opt(m, name).is_some()) {
        return Err(error("INVALID_OPTIONS", message));
    }
    Ok(())
}
fn workspace_source(m: &ArgMatches, cwd: &str) -> Result<Value> {
    let kind = opt(m, "new-workspace")
        .or_else(|| opt(m, "worktree").map(|_| "worktree"))
        .unwrap_or("local");
    if opt(m, "new-workspace").is_some() && opt(m, "worktree").is_some() {
        return Err(error(
            "INVALID_OPTIONS",
            "--new-workspace and --worktree cannot be combined",
        ));
    }
    if (opt(m, "new-workspace").is_some() || opt(m, "worktree").is_some())
        && opt(m, "workspace").is_some()
    {
        return Err(error(
            "INVALID_OPTIONS",
            "A new workspace and --workspace cannot be combined",
        ));
    }
    if kind == "local" {
        absent(
            m,
            &[
                "worktree-mode",
                "worktree-slug",
                "new-branch",
                "base",
                "branch",
                "pr-number",
                "forge",
            ],
            "Worktree options require --new-workspace worktree",
        )?;
        return Ok(json!({"kind":"directory","path":cwd}));
    }
    if kind != "worktree" {
        return Err(error(
            "INVALID_OPTIONS",
            format!("Unsupported new workspace kind: {kind}"),
        ));
    }
    let mut source = json!({"kind":"worktree","cwd":cwd});
    if let Some(slug) = opt(m, "worktree-slug").or_else(|| opt(m, "worktree")) {
        source["worktreeSlug"] = json!(slug);
    }
    match opt(m, "worktree-mode").unwrap_or("branch-off") {
        "branch-off" => {
            absent(
                m,
                &["branch", "pr-number", "forge"],
                "--branch, --pr-number, and --forge require a checkout mode",
            )?;
            source["action"] = json!("branch-off");
            if let Some(branch) = opt(m, "new-branch") {
                source["branchName"] = json!(branch);
            }
            if let Some(base) = opt(m, "base") {
                source["baseBranch"] = json!(base);
            }
        }
        "checkout-branch" => {
            absent(
                m,
                &["new-branch", "base", "pr-number", "forge"],
                "--new-branch, --base, --pr-number, and --forge are not valid for checkout-branch",
            )?;
            let branch = opt(m, "branch").filter(|v| !v.is_empty()).ok_or_else(|| {
                error(
                    "INVALID_OPTIONS",
                    "--branch is required for checkout-branch",
                )
            })?;
            source["action"] = json!("checkout");
            source["refName"] = json!(branch);
        }
        "checkout-pr" => {
            absent(
                m,
                &["new-branch", "base", "branch"],
                "--new-branch, --base, and --branch are not valid for checkout-pr",
            )?;
            let number = opt(m, "pr-number")
                .and_then(|v| v.parse::<u64>().ok())
                .filter(|v| *v > 0)
                .ok_or_else(|| {
                    error("INVALID_OPTIONS", "--pr-number must be a positive integer")
                })?;
            source["action"] = json!("checkout");
            source["checkoutSource"] = json!({"kind":"change_request","number":number});
            if let Some(forge) = opt(m, "forge") {
                source["checkoutSource"]["forge"] = json!(forge);
            }
        }
        mode => {
            return Err(error(
                "INVALID_OPTIONS",
                format!("Unsupported worktree mode: {mode}"),
            ));
        }
    }
    Ok(source)
}
async fn caller(client: &DaemonClient, context: &Context) -> Result<Option<String>> {
    let Some(id) = std::env::var("PASEO_AGENT_ID")
        .ok()
        .filter(|s| !s.trim().is_empty())
    else {
        return Ok(None);
    };
    match fetch(client, context, id.trim()).await {
        Ok(agent) => Ok((agent["id"] == id.trim()).then(|| id.trim().into())),
        Err(e) => {
            if e.downcast_ref::<ClientError>().is_some_and(|e| {
                matches!(
                    e.code.as_str(),
                    "DAEMON_CONNECTION_LOST"
                        | "DAEMON_REQUEST_TIMEOUT"
                        | "DAEMON_REQUEST_CANCELLED"
                        | "DAEMON_UNREACHABLE"
                )
            }) {
                return Err(e);
            }
            Ok(None)
        }
    }
}
async fn workspace(
    m: &ArgMatches,
    context: &Context,
    client: &DaemonClient,
    caller: Option<&str>,
) -> Result<(String, Option<String>)> {
    let cwd = opt(m, "cwd")
        .map(str::to_owned)
        .unwrap_or(std::env::current_dir()?.to_string_lossy().into_owned());
    let source = workspace_source(m, &cwd)?;
    let new = opt(m, "new-workspace").is_some() || opt(m, "worktree").is_some();
    let ambient = std::env::var("PASEO_WORKSPACE_ID").ok();
    let existing = if new {
        None
    } else {
        opt(m, "workspace")
            .filter(|s| !s.trim().is_empty())
            .or_else(|| {
                if caller.is_some() {
                    None
                } else {
                    ambient.as_deref().filter(|s| !s.trim().is_empty())
                }
            })
    };
    if let Some(id) = existing {
        let result = rpc(
            client,
            context,
            json!({"type":"fetch_workspaces_request","filter":{"query":id},"page":{"limit":200}}),
            "fetch_workspaces_response",
        )
        .await?;
        let row = array(&result["entries"])
            .iter()
            .find(|w| w["id"] == id)
            .ok_or_else(|| error("WORKSPACE_NOT_FOUND", format!("Workspace not found: {id}")))?;
        eprintln!("Using workspace {id}");
        return Ok((text(row, "workspaceDirectory").into(), Some(id.into())));
    }
    if !new && caller.is_some() {
        return Ok((cwd, None));
    }
    let mut request = json!({"type":"workspace.create.request","source":source,"idempotencyKey":uuid::Uuid::new_v4().to_string()});
    if let Some(caller) = caller {
        request["callerAgentId"] = json!(caller);
    }
    let result = client
        .request_cancellable(
            request,
            "workspace.create.response",
            Duration::ZERO,
            context.cancel.clone(),
        )
        .await?;
    let row = &result["workspace"];
    if !row.is_object() {
        return Err(error(
            "WORKSPACE_CREATE_FAILED",
            result["error"]
                .as_str()
                .unwrap_or("Failed to create workspace for this run"),
        ));
    }
    eprintln!(
        "Created workspace {} - {}",
        text(row, "id"),
        text(row, "name")
    );
    if let Some(reason) = result["setupSkippedReason"].as_str() {
        eprintln!("{reason}");
    }
    eprintln!(
        "Tip: pass --workspace <id> (or set PASEO_WORKSPACE_ID) to run in an existing workspace."
    );
    Ok((
        row["workspaceDirectory"].as_str().unwrap_or(&cwd).into(),
        Some(text(row, "id").into()),
    ))
}
pub(super) async fn images(paths: Vec<String>, strict: bool) -> Result<Vec<Value>> {
    let mut images = Vec::with_capacity(paths.len());
    for path in paths {
        let mime = if strict {
            let mime = mime_guess::from_path(&path).first_or_octet_stream();
            if mime.type_() != "image" {
                return Err(error(
                    "IMAGE_READ_ERROR",
                    format!("File is not an image: {path} (detected type: {mime})"),
                ));
            }
            mime.to_string()
        } else {
            match Path::new(&path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase()
                .as_str()
            {
                "png" => "image/png",
                "gif" => "image/gif",
                "webp" => "image/webp",
                _ => "image/jpeg",
            }
            .to_owned()
        };
        let bytes = tokio::fs::read(&path).await.map_err(|e| {
            error(
                "IMAGE_READ_ERROR",
                format!("Failed to read image {path}: {e}"),
            )
        })?;
        images.push(
            json!({"data":base64::engine::general_purpose::STANDARD.encode(bytes),"mimeType":mime}),
        );
    }
    Ok(images)
}
async fn create(client: &DaemonClient, context: &Context, mut request: Value) -> Result<Value> {
    let modern = client.server_info()["features"]["creationLifecycle"] == true;
    request["type"] = json!(if modern {
        "agent.create.request"
    } else {
        "create_agent_request"
    });
    if modern {
        request["idempotencyKey"] = json!(uuid::Uuid::new_v4().to_string());
    }
    let result = client
        .request_cancellable(
            request,
            if modern {
                "agent.create.response"
            } else {
                "status"
            },
            Duration::ZERO,
            context.cancel.clone(),
        )
        .await?;
    result
        .get("agent")
        .filter(|a| a.is_object())
        .cloned()
        .ok_or_else(|| {
            error(
                "AGENT_CREATE_FAILED",
                result["error"]
                    .as_str()
                    .unwrap_or("Agent creation returned no agent"),
            )
        })
}
async fn send_turn(client: &DaemonClient, context: &Context, id: &str, prompt: &str) -> Result<()> {
    rpc(client,context,json!({"type":"send_agent_message_request","agentId":id,"text":prompt,"messageId":uuid::Uuid::new_v4().to_string()}),"send_agent_message_response").await?;
    Ok(())
}
pub(super) async fn run(
    m: &ArgMatches,
    context: &Context,
    client: &DaemonClient,
) -> Result<Output> {
    let prompt = string(m, "prompt")
        .filter(|p| !p.trim().is_empty())
        .ok_or_else(|| error("MISSING_PROMPT", "A prompt is required"))?;
    let waiting = !flag(m, "no-wait") && !flag(m, "background") && !flag(m, "detach");
    if flag(m, "background") {
        eprintln!("Warning: --background (-d) is deprecated. Use --no-wait instead.");
    }
    let schema = if let Some(schema) = string(m, "output-schema").filter(|s| !s.is_empty()) {
        if !waiting {
            return Err(error(
                "INVALID_OPTIONS",
                "--output-schema cannot be used with --no-wait",
            ));
        }
        if schema.trim().is_empty() {
            return Err(error(
                "INVALID_OUTPUT_SCHEMA",
                "--output-schema cannot be empty",
            ));
        }
        let source = if schema.trim().starts_with('{') {
            schema.to_owned()
        } else {
            tokio::fs::read_to_string(schema.trim()).await?
        };
        let schema: Value = serde_json::from_str(&source)
            .map_err(|e| error("INVALID_OUTPUT_SCHEMA", e.to_string()))?;
        if !schema.is_object() {
            return Err(error(
                "INVALID_OUTPUT_SCHEMA",
                "Output schema must be a JSON object",
            ));
        }
        Some(schema)
    } else {
        None
    };
    let validator = schema
        .as_ref()
        .map(|schema| {
            crate::agent::compile_schema(schema)
                .map_err(|e| error("INVALID_OUTPUT_SCHEMA", e.to_string()))
        })
        .transpose()?;
    let timeout = duration(string(m, "wait-timeout"))?;
    let mut config = launch_config(m, context, client).await?;
    let images = images(strings(m, "image"), true).await?;
    let labels = key_values(strings(m, "label"), false)?;
    let env =
        key_values(strings(m, "env"), false).map_err(|e| error("INVALID_ENV", e.to_string()))?;
    let caller = caller(client, context).await?;
    let (cwd, workspace) = workspace(m, context, client, caller.as_deref()).await?;
    config["cwd"] = json!(cwd);
    if let Some(title) = string(m, "title").or_else(|| string(m, "name")) {
        config["title"] = json!(title);
    }
    let initial = if let Some(schema) = &schema {
        format!(
            "{}\n\nYou must respond with JSON only that matches this JSON Schema:\n{}",
            prompt.trim(),
            serde_json::to_string_pretty(schema)?
        )
    } else {
        prompt.into()
    };
    let mut request = json!({"config":config,"initialPrompt":initial});
    if let Some(workspace) = workspace {
        request["workspaceId"] = json!(workspace);
    }
    if let Some(caller) = caller {
        request["callerAgentId"] = json!(caller);
    }
    if !images.is_empty() {
        request["images"] = json!(images);
    }
    for (key, values) in [("env", env), ("labels", labels)] {
        if values.as_object().is_some_and(|m| !m.is_empty()) {
            request[key] = values;
        }
    }
    if let Some(schema) = &schema {
        request["outputSchema"] = schema.clone();
    }
    let agent = create(client, context, request).await?;
    let id = text(&agent, "id");
    if !waiting {
        return Ok(presentation::run_result(&agent, None));
    }
    if let Some(validator) = validator {
        for attempt in 0..3 {
            let state = wait(client, context, id, timeout).await?;
            let status = text(&state, "status");
            if status != "idle" {
                return Err(error(
                    "OUTPUT_SCHEMA_FAILED",
                    match status {
                        "timeout" => "Timed out waiting for structured output",
                        "permission" => {
                            "Agent is waiting for permission before producing structured output"
                        }
                        _ => state["error"]
                            .as_str()
                            .unwrap_or("Agent failed before producing structured output"),
                    },
                ));
            }
            let response = if let Some(text) = state["lastMessage"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
            {
                text.to_owned()
            } else {
                let history=rpc(client,context,json!({"type":"fetch_agent_timeline_request","agentId":id,"direction":"tail","limit":200}),"fetch_agent_timeline_response").await?;
                array(&history["entries"])
                    .iter()
                    .rev()
                    .find_map(|e| {
                        if e["item"]["type"] == "assistant_message" {
                            e["item"]["text"].as_str().filter(|s| !s.trim().is_empty())
                        } else {
                            None
                        }
                    })
                    .unwrap_or("")
                    .to_owned()
            };
            if response.trim().is_empty() {
                return Err(error(
                    "OUTPUT_SCHEMA_FAILED",
                    "Agent finished without a structured output message",
                ));
            }
            let errors = match crate::agent::validate_response(&response, &validator) {
                Ok(value) => {
                    let mut output = presentation::run_result(
                        state
                            .get("final")
                            .filter(|a| a.is_object())
                            .unwrap_or(&agent),
                        Some("completed"),
                    )
                    .quiet_text(id.to_owned());
                    let mut options = context.output.clone();
                    options.format = super::super::output::Format::Table;
                    options.quiet = false;
                    let human = output.render(&options)?;
                    output.data = value;
                    output.human = Some(human);
                    return Ok(output);
                }
                Err(errors) => errors,
            };
            if attempt == 2 {
                return Err(error(
                    "OUTPUT_SCHEMA_FAILED",
                    format!(
                        "Agent response did not match the required output schema: {}",
                        errors.join("; ")
                    ),
                ));
            }
            send_turn(
                client,
                context,
                id,
                &crate::agent::retry_prompt(&initial, &errors),
            )
            .await?;
        }
        unreachable!("bounded structured attempts return or fail")
    }
    let state = if flag(m, "attach") {
        let stop = context.cancel.child_token();
        let watch = stream::attach_stream(client, context, id, stop.clone());
        tokio::pin!(watch);
        let waiting = wait(client, context, id, timeout);
        tokio::pin!(waiting);
        tokio::select! {state=&mut waiting=>{stop.cancel();let streamed=watch.await;streamed?;state?},streamed=&mut watch=>{streamed?;waiting.await?}}
    } else {
        wait(client, context, id, timeout).await?
    };
    let status = text(&state, "status");
    Ok(presentation::run_result(
        state
            .get("final")
            .filter(|a| a.is_object())
            .unwrap_or(&agent),
        Some(if status == "idle" {
            "completed"
        } else {
            status
        }),
    ))
}
pub(super) async fn import(
    m: &ArgMatches,
    context: &Context,
    client: &DaemonClient,
) -> Result<Output> {
    let id=string(m,"id").ok_or_else(||error("INTERACTIVE_REQUIRED","Interactive session selection is not included. Specify --provider <provider> <session-id>."))?.trim();
    if id.is_empty() {
        return Err(error("MISSING_SESSION_ID", "Session ID is required"));
    }
    let provider = string(m, "provider")
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            error(
                "MISSING_PROVIDER",
                "Provider is required. Use paseo import --provider <provider> <id>",
            )
        })?
        .trim();
    let cwd = string(m, "cwd")
        .map(str::trim)
        .map(str::to_owned)
        .unwrap_or(std::env::current_dir()?.to_string_lossy().into_owned());
    if cwd.is_empty() {
        return Err(error("INVALID_CWD", "--cwd cannot be empty"));
    }
    let mut labels = serde_json::Map::new();
    for value in strings(m, "label") {
        let (key, value) = value
            .split_once('=')
            .ok_or_else(|| error("INVALID_LABEL", "Labels must be in key=value format"))?;
        if key.trim().is_empty() {
            return Err(error(
                "INVALID_LABEL",
                "Labels must include a non-empty key",
            ));
        }
        labels.insert(key.trim().into(), json!(value));
    }
    let mut request =
        json!({"type":"import_agent_request","provider":provider,"sessionId":id,"cwd":cwd});
    if !labels.is_empty() {
        request["labels"] = json!(labels);
    }
    let result = rpc(client, context, request, "status").await?;
    if !result["agent"].is_object() {
        return Err(error(
            "AGENT_IMPORT_FAILED",
            result["error"]
                .as_str()
                .unwrap_or("Agent import returned no agent"),
        ));
    }
    Ok(presentation::run_result(&result["agent"], None))
}
pub(super) async fn chat(
    m: &ArgMatches,
    context: &Context,
    client: &DaemonClient,
) -> Result<Output> {
    let mut agent = if let Some(id) = string(m, "resume") {
        Some(fetch(client, context, id).await?)
    } else {
        None
    };
    let launch = if agent.is_none() {
        Some(launch_config(m, context, client).await?)
    } else {
        None
    };
    if let Some(agent) = &agent {
        for item in
            stream::history(client, context, text(agent, "id"), Duration::from_secs(60)).await?
        {
            if matches!(text(&item, "type"), "user_message" | "assistant_message") {
                super::super::output::stdout(&format!(
                    "{}: {}",
                    if item["type"] == "user_message" {
                        "You"
                    } else {
                        text(agent, "provider")
                    },
                    text(&item, "text")
                ))?;
            }
        }
    }
    let mut input = BufReader::new(tokio::io::stdin()).lines();
    loop {
        let line =
            tokio::select! {_=context.cancel.cancelled()=>break,line=input.next_line()=>line?};
        let Some(line) = line else { break };
        let message = line.trim();
        if matches!(message, "/exit" | "/quit") {
            break;
        }
        if message.is_empty() {
            continue;
        }
        if agent.is_none() {
            let (cwd, workspace) = workspace(m, context, client, None).await?;
            let mut config = launch
                .clone()
                .ok_or_else(|| anyhow!("Missing chat launch settings"))?;
            config["cwd"] = json!(cwd);
            let mut request = json!({"config":config});
            if let Some(workspace) = workspace {
                request["workspaceId"] = json!(workspace);
            }
            agent = Some(create(client, context, request).await?);
        }
        let current = agent.as_ref().unwrap();
        let id = text(current, "id");
        let mut events = client.subscribe();
        let subscription = rpc(
            client,
            context,
            json!({"type":"agent.timeline.set_subscription.request","agentIds":[id]}),
            "agent.timeline.set_subscription.response",
        )
        .await?;
        let subscription_id = subscription["subscriptionId"]
            .as_str()
            .ok_or_else(|| anyhow!("Host did not assign timeline ownership"))?;
        let turn:Result<()>=async{send_turn(client,context,id,message).await?;let waiting=wait(client,context,id,Duration::ZERO);tokio::pin!(waiting);loop{tokio::select!{state=&mut waiting=>{let state=state?;if context.output.format==super::super::output::Format::Json{super::super::output::stdout(&serde_json::to_string(&state)?)?;}else{if let Some(message)=state["lastMessage"].as_str(){super::super::output::stdout(&format!("{}: {message}",text(current,"provider")))?;}match text(&state,"status"){"error"=>eprintln!("{}",state["error"].as_str().unwrap_or("Agent failed")),"permission"=>super::super::output::stdout("Respond to the permission request in Paseo.")?,"timeout"=>super::super::output::stdout("Agent is still running; follow it in Paseo.")?,_=>{}}}break;},event=events.recv()=>{let event=event?;if event["payload"]["subscriptionId"]!=subscription_id{continue;}let update=&event["payload"]["event"];if context.output.format==super::super::output::Format::Json{super::super::output::stdout(&serde_json::to_string(&event)?)?;}else if update["type"]=="timeline"&&update["item"]["type"]=="tool_call"{super::super::output::stdout(&format!("[{}] {}",text(&update["item"],"name"),update["item"]["status"].as_str().unwrap_or("started")))?;}else if update["type"]=="permission_requested"{super::super::output::stdout(&format!("Permission needed: {}. Respond in Paseo.",text(&update["request"],"name")))?;}}}}Ok(())}.await;
        let released = client.release_subscription(subscription_id).await;
        if context.cancel.is_cancelled() {
            break;
        }
        turn?;
        released?;
    }
    Ok(Output::new(Value::Null).silent())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn structured_json_extraction_uses_shared_validator() {
        let schema = crate::agent::compile_schema(&json!({"type":"object"})).unwrap();
        assert_eq!(
            crate::agent::validate_response("prose {\"x\":\"}\\\"\",\"n\":1} trailing", &schema)
                .unwrap(),
            json!({"x":"}\"","n":1})
        );
        assert_eq!(
            crate::agent::validate_response("```json\n{\"ok\":true}\n```", &schema).unwrap(),
            json!({"ok":true})
        );
    }
    #[test]
    fn workspace_checkout_rejects_branch_off_options() {
        let m = run_command()
            .try_get_matches_from([
                "run",
                "hello",
                "--new-workspace",
                "worktree",
                "--worktree-mode",
                "checkout-branch",
                "--branch",
                "main",
                "--base",
                "other",
            ])
            .unwrap();
        assert!(workspace_source(&m, "/repo").is_err());
    }
}
