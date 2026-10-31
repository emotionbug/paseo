use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tokio::sync::mpsc::Sender;
use tokio_util::sync::CancellationToken;

use crate::{state::AppState, terminals::CreateTerminal};

pub(crate) fn definitions(_agent_scope: bool) -> Vec<Value> {
    let string = || json!({"type":"string"});
    let nonempty = || json!({"type":"string","minLength":1});
    let boolean = || json!({"type":"boolean"});
    let definition = |name: &str,
                      title: &str,
                      description: &str,
                      properties: Value,
                      required: &[&str]| {
        json!({"name":name,"title":title,"description":description,"inputSchema":{
            "type":"object","properties":properties,"required":required,"additionalProperties":true
        }})
    };
    vec![
        definition(
            "create_workspace",
            "Create workspace",
            "Create a workspace using an existing local checkout or a new Paseo-managed worktree.",
            json!({
                "isolation":{"type":"string","enum":["local","worktree"]},
                "path":{"type":"string","description":"Local directory or source checkout. Defaults to your current workspace. Local isolation adopts an existing directory and never creates one."},
                "projectId":{"type":"string","description":"Existing project id to own the workspace."},
                "title":{"type":"string","minLength":1,"x-trim":true},
                "background":{"type":"boolean","description":"True hides the workspace from default lists and the sidebar while its contents remain durable and accessible; omit background to inherit your current workspace’s value (false without a caller), and override it only when the user explicitly asks."},
                "mode":{"type":"string","enum":["branch-off","checkout-branch","checkout-pr"],"description":"Worktree creation mode. Defaults to branch-off."},
                "worktreeSlug":{"type":"string","minLength":1,"x-trim":true},
                "branchName":{"type":"string","minLength":1,"x-trim":true,"description":"New branch name for branch-off mode."},
                "baseBranch":{"type":"string","minLength":1,"x-trim":true,"description":"Base ref for branch-off mode."},
                "branch":{"type":"string","minLength":1,"x-trim":true,"description":"Existing branch for checkout-branch mode."},
                "prNumber":{"type":"integer","minimum":1,"description":"Pull request or change request number for checkout-pr mode."},
                "forge":{"type":"string","minLength":1,"x-trim":true,"description":"Forge for checkout-pr mode. Defaults to the source checkout."}
            }),
            &["isolation"],
        ),
        definition(
            "list_workspaces",
            "List workspaces",
            "List active workspaces.",
            json!({"includeBackground":{"type":"boolean","default":false}}),
            &[],
        ),
        definition(
            "archive_workspace",
            "Archive workspace",
            "Archive a workspace and everything it owns.",
            json!({"workspaceId":nonempty()}),
            &["workspaceId"],
        ),
        definition(
            "rename_workspace",
            "Rename workspace",
            "Rename a workspace by setting its user-visible title. Omit workspaceId to rename your current workspace.",
            json!({
                "workspaceId":{"type":"string","minLength":1,"x-trim":true,"description":"Workspace id to rename. Omit to rename your current workspace."},
                "title":{"type":"string","minLength":1,"x-trim":true,"description":"New user-visible workspace title."}
            }),
            &["title"],
        ),
        definition(
            "list_workspace_scripts",
            "List workspace scripts",
            "List configured workspace scripts and their lifecycle, service port, proxy URL, health, and terminal ID.",
            json!({"workspaceId":{"type":"string","description":"Workspace ID whose configured scripts to list."}}),
            &["workspaceId"],
        ),
        definition(
            "start_workspace_script",
            "Start workspace script",
            "Start one configured workspace script through Paseo's managed workspace-script launcher.",
            json!({"workspaceId":{"type":"string","description":"Workspace ID containing the configured script."},"scriptName":{"type":"string","minLength":1,"description":"Configured paseo.json script name to start."}}),
            &["workspaceId", "scriptName"],
        ),
        definition(
            "stop_workspace_script",
            "Stop workspace script",
            "Stop a running workspace script through its supervised terminal lifecycle.",
            json!({"workspaceId":{"type":"string","description":"Workspace ID containing the running script."},"scriptName":{"type":"string","minLength":1,"description":"Configured paseo.json script name to stop."}}),
            &["workspaceId", "scriptName"],
        ),
        definition(
            "list_terminals",
            "List terminals",
            "List terminals for a working directory or across all working directories.",
            json!({"cwd":{"type":"string","description":"Optional working directory. Defaults to your current working directory."},"all":{"type":"boolean","description":"List terminals across all working directories."}}),
            &[],
        ),
        definition(
            "create_terminal",
            "Create terminal",
            "Create a terminal session for a working directory.",
            json!({"cwd":{"type":"string","description":"Optional working directory. Defaults to your current working directory."},"name":{"type":"string","description":"Optional terminal name."}}),
            &[],
        ),
        definition(
            "kill_terminal",
            "Kill terminal",
            "Kill an existing terminal session.",
            json!({"terminalId":string()}),
            &["terminalId"],
        ),
        definition(
            "capture_terminal",
            "Capture terminal",
            "Capture plain-text terminal output lines from a terminal session.",
            json!({"terminalId":string(),"start":{"type":"number"},"end":{"type":"number"},"scrollback":boolean(),"stripAnsi":{"type":"boolean","default":true}}),
            &["terminalId"],
        ),
        definition(
            "send_terminal_keys",
            "Send terminal keys",
            "Send text or a key token: Enter, Tab, Escape, Space, BSpace, Up, Down, Left, Right, Home, End, Insert, Delete, PageUp, PageDown, or F1-F12. Combine C- (Ctrl), M- (Alt), and S- (Shift) prefixes, e.g. C-r, S-Tab, or C-M-Left. Set literal=true to send the text unchanged.",
            json!({"terminalId":string(),"keys":string(),"literal":boolean()}),
            &["terminalId", "keys"],
        ),
    ]
}

pub(crate) async fn execute(
    app: &Arc<AppState>,
    caller: Option<&str>,
    name: &str,
    input: Value,
    cancel: CancellationToken,
    _updates: Option<Sender<Value>>,
) -> Result<Value> {
    // Once admitted, do not drop a mutating service future on cancellation: a PTY,
    // worktree, or archive may already have committed external effects.
    if cancel.is_cancelled() {
        bail!("Tool call cancelled before execution");
    }
    let result = match name {
        "create_workspace" => create_workspace(app, caller, &input).await?,
        "list_workspaces" => {
            let include_background = flag(&input, "includeBackground", false);
            let workspaces = app
                .projects
                .list_workspaces()
                .await?
                .into_iter()
                .filter(|workspace| {
                    !archived(workspace)
                        && (include_background || !flag(workspace, "background", false))
                })
                .map(|workspace| workspace_summary(&workspace))
                .collect::<Vec<_>>();
            json!({"workspaces":workspaces})
        }
        "archive_workspace" => {
            let id = required(&input, "workspaceId")?;
            let workspace = app
                .projects
                .workspace(id)
                .await?
                .filter(|workspace| !archived(workspace))
                .with_context(|| format!("Workspace not found: {id}"))?;
            let result = app
                .projects
                .archive_workspace(&super::actor(caller), required(&workspace, "workspaceId")?)
                .await?;
            json!({"workspaceId":id,"archivedAgentIds":result["archivedAgentIds"],"removedDirectory":result["removedDirectory"]})
        }
        "rename_workspace" => {
            if input.get("workspaceId").is_some() && trimmed(&input, "workspaceId").is_none() {
                return Err(super::input_error("workspaceId must not be empty"));
            }
            let id = if let Some(id) = trimmed(&input, "workspaceId") {
                id.to_owned()
            } else if let Some(caller) = caller {
                let agent = caller_agent(app, caller).await?;
                agent
                    .get("workspaceId")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        super::input_error(format!(
                            "Caller agent {caller} has no current workspace"
                        ))
                    })?
                    .to_owned()
            } else {
                return Err(super::input_error(
                    "workspaceId is required outside an agent-scoped session",
                ));
            };
            let title = required(&input, "title")?.trim();
            if title.is_empty() {
                return Err(super::input_error("title is required"));
            }
            let workspace = app
                .projects
                .workspace(&id)
                .await?
                .ok_or_else(|| super::input_error(format!("Workspace {id} not found")))?;
            if archived(&workspace) {
                return Err(super::input_error(format!("Workspace {id} is archived")));
            }
            if !app.projects.rename_workspace(&id, Some(title)).await? {
                return Err(super::input_error(format!("Workspace {id} not found")));
            }
            json!({"success":true,"workspaceId":id,"title":title})
        }
        "list_workspace_scripts" => {
            json!({"scripts":app.terminals.script_list(required(&input,"workspaceId")?).await?})
        }
        "start_workspace_script" => {
            json!({"script":app.terminals.script_start(required(&input,"workspaceId")?, required(&input,"scriptName")?).await?})
        }
        "stop_workspace_script" => {
            json!({"script":app.terminals.script_stop(required(&input,"workspaceId")?, required(&input,"scriptName")?).await?})
        }
        "list_terminals" => {
            let cwd = if flag(&input, "all", false) {
                None
            } else {
                Some(
                    resolve_scoped_cwd(app, caller, input.get("cwd").and_then(Value::as_str))
                        .await?,
                )
            };
            let cwd = cwd.as_ref().map(|path| path.to_string_lossy());
            let terminals = app
                .terminals
                .list(None, cwd.as_deref())
                .await
                .into_iter()
                .map(|terminal| terminal_summary(&terminal))
                .collect::<Vec<_>>();
            json!({"terminals":terminals})
        }
        "create_terminal" => {
            let cwd =
                resolve_scoped_cwd(app, caller, input.get("cwd").and_then(Value::as_str)).await?;
            let workspace_id = if let Some(caller) = caller {
                caller_agent(app, caller)
                    .await?
                    .get("workspaceId")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            } else {
                None
            };
            let workspace_id = match workspace_id {
                Some(id) => id,
                None => {
                    let workspace = app
                        .projects
                        .ensure_workspace_for_create(&cwd, &Value::Null, None)
                        .await?;
                    required(&workspace, "workspaceId")?.to_owned()
                }
            };
            let terminal = app
                .terminals
                .create(CreateTerminal {
                    cwd,
                    workspace_id,
                    name: trimmed(&input, "name").map(str::to_owned),
                    command: None,
                    args: Vec::new(),
                    env: BTreeMap::new(),
                    rows: 24,
                    cols: 80,
                })
                .await?;
            terminal_summary(&terminal.info())
        }
        "kill_terminal" => {
            let id = required(&input, "terminalId")?;
            existing_terminal(app, id).await?;
            app.terminals.kill(id).await?;
            json!({"success":true})
        }
        "capture_terminal" => {
            let id = required(&input, "terminalId")?;
            let terminal = existing_terminal(app, id).await?;
            let start = if flag(&input, "scrollback", false) {
                Some(0.0)
            } else {
                input.get("start").and_then(Value::as_f64)
            };
            let end = input.get("end").and_then(Value::as_f64);
            let (lines, total_lines) = terminal
                .capture(start, end, flag(&input, "stripAnsi", true))
                .await?;
            json!({"terminalId":id,"lines":lines,"totalLines":total_lines})
        }
        "send_terminal_keys" => {
            let terminal = existing_terminal(app, required(&input, "terminalId")?).await?;
            let keys = required(&input, "keys")?;
            if flag(&input, "literal", false) {
                terminal.input(keys.as_bytes()).await?;
            } else {
                let preamble = terminal.replay_preamble().await?;
                let encoded = encode_key(keys, &preamble);
                terminal.input(encoded.as_bytes()).await?;
            }
            json!({"success":true})
        }
        _ => bail!("Unknown workspace tool: {name}"),
    };
    Ok(json!({"content":[],"structuredContent":result,"isError":false}))
}

async fn existing_terminal(
    app: &Arc<AppState>,
    id: &str,
) -> Result<Arc<crate::terminals::Terminal>> {
    app.terminals
        .get(id)
        .await
        .map_err(|_| super::input_error(format!("Terminal {id} not found")))
}

async fn create_workspace(
    app: &Arc<AppState>,
    caller: Option<&str>,
    input: &Value,
) -> Result<Value> {
    for key in [
        "title",
        "worktreeSlug",
        "branchName",
        "baseBranch",
        "branch",
        "forge",
    ] {
        if input.get(key).is_some() && trimmed(input, key).is_none() {
            return Err(super::input_error(format!("{key} must not be empty")));
        }
    }
    let caller_workspace = if let Some(caller) = caller {
        caller_agent(app, caller)
            .await?
            .get("workspaceId")
            .and_then(Value::as_str)
            .map(str::to_owned)
    } else {
        None
    };
    let isolation = required(input, "isolation")?;
    let mut source = if isolation == "local" {
        let cwd =
            resolve_scoped_cwd(app, caller, input.get("path").and_then(Value::as_str)).await?;
        match tokio::fs::metadata(&cwd).await {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(super::input_error(format!(
                    "Directory not found: {}",
                    cwd.display()
                )));
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                return Err(super::input_error(format!(
                    "Directory not found: {}",
                    cwd.display()
                )));
            }
            Err(error) => return Err(error.into()),
        }
        absent(
            input,
            &[
                "mode",
                "worktreeSlug",
                "branchName",
                "baseBranch",
                "branch",
                "prNumber",
                "forge",
            ],
            "Worktree options require isolation worktree",
        )?;
        json!({"kind":"directory","path":cwd})
    } else {
        let cwd = if input.get("path").is_some()
            || input
                .get("projectId")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
        {
            resolve_scoped_cwd(app, caller, input.get("path").and_then(Value::as_str)).await?
        } else {
            let id = required(input, "projectId")?;
            let project = app
                .projects
                .project(id)
                .await?
                .filter(|project| !archived(project))
                .with_context(|| format!("Project not found: {id}"))?;
            PathBuf::from(required(&project, "rootPath")?)
        };
        let mut source = json!({"kind":"worktree","cwd":cwd});
        match input
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or("branch-off")
        {
            "branch-off" => {
                absent(
                    input,
                    &["branch", "prNumber", "forge"],
                    "branch, prNumber, and forge require a checkout mode",
                )?;
                source["action"] = json!("branch-off");
                copy_trimmed(input, "branchName", &mut source, "branchName")?;
                copy_trimmed(input, "baseBranch", &mut source, "refName")?;
            }
            "checkout-branch" => {
                let branch = trimmed(input, "branch").ok_or_else(|| {
                    super::input_error("branch is required for checkout-branch mode")
                })?;
                absent(
                    input,
                    &["branchName", "baseBranch", "prNumber", "forge"],
                    "branchName, baseBranch, prNumber, and forge are not valid for checkout-branch mode",
                )?;
                source["action"] = json!("checkout");
                source["refName"] = json!(branch);
            }
            "checkout-pr" => {
                let number = input.get("prNumber").ok_or_else(|| {
                    super::input_error("prNumber is required for checkout-pr mode")
                })?;
                absent(
                    input,
                    &["branchName", "baseBranch", "branch"],
                    "branchName, baseBranch, and branch are not valid for checkout-pr mode",
                )?;
                source["action"] = json!("checkout");
                let mut checkout = json!({"kind":"change_request","number":number});
                copy_trimmed(input, "forge", &mut checkout, "forge")?;
                source["checkoutSource"] = checkout;
            }
            mode => bail!("Invalid worktree mode: {mode}"),
        }
        copy_trimmed(input, "worktreeSlug", &mut source, "worktreeSlug")?;
        source
    };
    if let Some(project) = input.get("projectId") {
        source["projectId"] = project.clone();
    }
    let mut request = json!({"source":source});
    copy_trimmed(input, "title", &mut request, "title")?;
    if let Some(background) = input.get("background") {
        request["background"] = background.clone();
    }
    if let Some(id) = caller_workspace {
        request["callerWorkspaceId"] = json!(id);
    }
    let workspace = app.projects.create_workspace(&request).await?;
    Ok(workspace_summary(&workspace))
}

pub(crate) async fn resolve_scoped_cwd(
    app: &Arc<AppState>,
    caller: Option<&str>,
    requested: Option<&str>,
) -> Result<PathBuf> {
    if let Some(id) = caller {
        let agent = caller_agent(app, id).await?;
        let context = app.mcp.caller_context(id).await;
        if let Some(locked) = context
            .as_ref()
            .and_then(|value| trimmed(value, "lockedCwd"))
        {
            return expand_path(app, locked, None);
        }
        let parent = Path::new(required(&agent, "cwd")?);
        let requested = requested.map(str::trim).filter(|value| !value.is_empty());
        if context
            .as_ref()
            .is_some_and(|context| !flag(context, "allowCustomCwd", true))
            || requested.is_none()
        {
            return Ok(parent.to_path_buf());
        }
        return expand_path(app, requested.unwrap(), Some(parent));
    }
    let requested = requested
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| super::input_error("cwd is required"))?;
    expand_path(app, requested, None)
}

pub(super) fn expand_path(app: &AppState, value: &str, base: Option<&Path>) -> Result<PathBuf> {
    let expanded = if value == "~" {
        app.user_home.clone()
    } else if let Some(suffix) = value.strip_prefix("~/") {
        app.user_home.join(suffix)
    } else {
        PathBuf::from(value)
    };
    let absolute = if expanded.is_absolute() {
        expanded
    } else {
        match base {
            Some(base) => base.join(expanded),
            None => std::env::current_dir()?.join(expanded),
        }
    };
    // Match path.resolve, not canonicalize: do not impose existence or symlink resolution.
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            component => normalized.push(component.as_os_str()),
        }
    }
    Ok(normalized)
}

async fn caller_agent(app: &Arc<AppState>, id: &str) -> Result<Value> {
    app.agents
        .agent_snapshot(id)
        .await?
        .ok_or_else(|| super::input_error(format!("Parent agent {id} not found")))
}

fn required<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| super::input_error(format!("{key} is required")))
}

fn trimmed<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn flag(value: &Value, key: &str, default: bool) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn archived(value: &Value) -> bool {
    value
        .get("archivedAt")
        .is_some_and(|value| !value.is_null() && value.as_str() != Some(""))
}

fn absent(input: &Value, fields: &[&str], message: &str) -> Result<()> {
    if fields.iter().any(|field| input.get(*field).is_some()) {
        return Err(super::input_error(message));
    }
    Ok(())
}

fn copy_trimmed(input: &Value, key: &str, output: &mut Value, target: &str) -> Result<()> {
    if let Some(value) = input.get(key) {
        let value = value
            .as_str()
            .ok_or_else(|| super::input_error(format!("{key} must be a string")))?
            .trim();
        if value.is_empty() {
            return Err(super::input_error(format!("{key} is required")));
        }
        output[target] = json!(value);
    }
    Ok(())
}

fn workspace_summary(workspace: &Value) -> Value {
    let mut summary = json!({
        "workspaceId":workspace["workspaceId"],"projectId":workspace["projectId"],"cwd":workspace["cwd"],
        "isolation":if workspace["kind"] == "worktree" { "worktree" } else { "local" },
        "kind":workspace["kind"],"title":workspace["title"]
    });
    if let Some(background) = workspace.get("background") {
        summary["background"] = background.clone();
    }
    summary
}

fn terminal_summary(terminal: &Value) -> Value {
    json!({"id":terminal["id"],"name":terminal["name"],"cwd":terminal["cwd"]})
}

fn encode_key<'a>(original: &'a str, preamble: &str) -> std::borrow::Cow<'a, str> {
    use std::borrow::Cow;
    // JavaScript's source token regex is not dot-all: multiline text is literal.
    if original.contains(['\n', '\r', '\u{2028}', '\u{2029}']) {
        return Cow::Borrowed(original);
    }
    let mut token = original;
    let (mut ctrl, mut alt, mut shift) = (false, false, false);
    loop {
        if let Some(rest) = token.strip_prefix("C-") {
            ctrl = true;
            token = rest;
        } else if let Some(rest) = token.strip_prefix("M-") {
            alt = true;
            token = rest;
        } else if let Some(rest) = token.strip_prefix("S-") {
            shift = true;
            token = rest;
        } else {
            break;
        }
    }
    if token.is_empty() {
        return Cow::Borrowed(original);
    }
    let token = match token {
        "Up" => "ArrowUp",
        "Down" => "ArrowDown",
        "Left" => "ArrowLeft",
        "Right" => "ArrowRight",
        "BSpace" => "Backspace",
        "Space" => " ",
        other => other,
    };
    let modifier = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(ctrl);
    let prefix = |value: &str| {
        if alt {
            format!("\x1b{value}")
        } else {
            value.to_owned()
        }
    };
    let csi = |final_byte: char| {
        if modifier == 1 {
            format!("\x1b[{final_byte}")
        } else {
            format!("\x1b[1;{modifier}{final_byte}")
        }
    };
    let tilde = |number: u8| {
        if modifier == 1 {
            format!("\x1b[{number}~")
        } else {
            format!("\x1b[{number};{modifier}~")
        }
    };
    if token
        .chars()
        .next()
        .is_some_and(|character| character.len_utf16() == 1 && character.len_utf8() == token.len())
    {
        if !ctrl && !alt && !shift {
            return Cow::Borrowed(token);
        }
        let character = if shift {
            token.to_uppercase()
        } else {
            token.to_owned()
        };
        let encoded = if ctrl {
            let upper = character.to_uppercase();
            let control = if upper.len() == 1 && upper.as_bytes()[0].is_ascii_uppercase() {
                Some(upper.as_bytes()[0] - 64)
            } else {
                match character.as_str() {
                    " " | "@" | "2" => Some(0),
                    "[" | "3" => Some(27),
                    "\\" | "4" => Some(28),
                    "]" | "5" => Some(29),
                    "^" | "6" => Some(30),
                    "_" | "/" | "7" => Some(31),
                    "8" | "?" => Some(127),
                    _ if character.encode_utf16().count() == 1 => {
                        Some((character.encode_utf16().next().unwrap() & 31) as u8)
                    }
                    _ => None,
                }
            };
            match control {
                Some(code) => prefix(&char::from(code).to_string()),
                None => prefix(&character),
            }
        } else {
            prefix(&character)
        };
        return Cow::Owned(encoded);
    }
    let encoded = match token {
        "Enter" if modifier > 1 && preamble.contains("\x1b[?9001h") => {
            let state = 16 * u8::from(shift) + 2 * u8::from(alt) + 8 * u8::from(ctrl);
            format!("\x1b[13;28;13;1;{state};1_")
        }
        "Enter"
            if modifier > 1
                && preamble.split("\x1b[=").skip(1).any(|part| {
                    part.split(';')
                        .next()
                        .and_then(|number| number.parse::<u64>().ok())
                        .is_some_and(|number| number > 0)
                }) =>
        {
            format!("\x1b[13;{modifier}u")
        }
        "Enter" => "\r".to_owned(),
        "Tab" if shift && !ctrl && !alt => "\x1b[Z".to_owned(),
        "Tab" => prefix("\t"),
        "Backspace" => prefix("\x7f"),
        "Escape" => "\x1b".to_owned(),
        "ArrowUp" | "ArrowDown" | "ArrowRight" | "ArrowLeft" => {
            let final_byte = match token {
                "ArrowUp" => 'A',
                "ArrowDown" => 'B',
                "ArrowRight" => 'C',
                _ => 'D',
            };
            if modifier == 1 && preamble.contains("\x1b[?1h") {
                format!("\x1bO{final_byte}")
            } else {
                csi(final_byte)
            }
        }
        "Home" => csi('H'),
        "End" => csi('F'),
        "Insert" => tilde(2),
        "Delete" => tilde(3),
        "PageUp" => tilde(5),
        "PageDown" => tilde(6),
        "F1" | "F2" | "F3" | "F4" => {
            let final_byte = match token {
                "F1" => 'P',
                "F2" => 'Q',
                "F3" => 'R',
                _ => 'S',
            };
            if modifier == 1 {
                format!("\x1bO{final_byte}")
            } else {
                csi(final_byte)
            }
        }
        "F5" => tilde(15),
        "F6" => tilde(17),
        "F7" => tilde(18),
        "F8" => tilde(19),
        "F9" => tilde(20),
        "F10" => tilde(21),
        "F11" => tilde(23),
        "F12" => tilde(24),
        _ => return Cow::Borrowed(original),
    };
    Cow::Owned(encoded)
}
