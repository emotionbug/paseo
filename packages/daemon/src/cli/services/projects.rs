use crate::{
    cli::{Context, flag, number, option, output::Output, positional, required, string, switch},
    client::{ClientError, DaemonClient, DaemonTarget},
};
use anyhow::{Result, anyhow, bail, ensure};
use clap::{ArgMatches, Command};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
const RPC: Duration = Duration::from_secs(30);
const PROJECT: &[(&str, &str)] = &[
    ("PROJECT ID", "projectId"),
    ("NAME", "name"),
    ("KIND", "kind"),
    ("PATH", "path"),
];
const WORKSPACE: &[(&str, &str)] = &[
    ("WORKSPACE ID", "workspaceId"),
    ("PROJECT", "project"),
    ("NAME", "name"),
    ("ISOLATION", "isolation"),
    ("CWD", "cwd"),
];
pub fn commands() -> Vec<Command> {
    let project = Command::new("project")
        .about("Manage projects")
        .subcommand(Command::new("create").arg(positional("path", false)))
        .subcommand(Command::new("ls"))
        .subcommand(
            Command::new("rename")
                .arg(positional("project-id", true))
                .arg(positional("name", false))
                .arg(switch("reset", "reset")),
        )
        .subcommand(Command::new("delete").arg(positional("project-id", true)));
    let mut create = Command::new("create").arg(
        option("isolation", "isolation")
            .required(true)
            .value_parser(["local", "worktree"]),
    );
    for key in [
        "path",
        "project",
        "title",
        "mode",
        "worktree-slug",
        "new-branch",
        "base",
        "branch",
        "pr-number",
        "forge",
    ] {
        create = create.arg(option(key, key));
    }
    let workspace = Command::new("workspace")
        .about("Manage workspaces")
        .subcommand(
            create
                .arg(switch("background", "background").conflicts_with("no-background"))
                .arg(switch("no-background", "no-background")),
        )
        .subcommand(Command::new("ls").arg(switch("background", "background")))
        .subcommand(
            Command::new("rename")
                .arg(positional("workspace-id", true))
                .arg(positional("title", false))
                .arg(switch("reset", "reset")),
        )
        .subcommand(Command::new("setup").arg(positional("workspace-id", true)))
        .subcommand(Command::new("archive").arg(positional("workspace-id", true)));
    let mut wt_create = Command::new("create");
    for key in ["cwd", "mode", "new-branch", "base", "branch", "pr-number"] {
        wt_create = wt_create.arg(option(key, key));
    }
    let worktree = Command::new("worktree")
        .hide(true)
        .about("Manage Paseo-managed git worktrees")
        .subcommand(Command::new("ls").arg(option("cwd", "cwd")))
        .subcommand(wt_create)
        .subcommand(
            Command::new("archive")
                .arg(positional("name", true))
                .arg(option("cwd", "cwd")),
        );
    let clone = Command::new("clone")
        .about("Clone a repository and register its workspace")
        .arg(positional("repo", true))
        .arg(option("dir", "dir").required(true))
        .arg(option("protocol", "protocol").value_parser(["https", "ssh"]));
    vec![project, workspace, worktree, clone]
}
pub async fn execute(
    path: &[&str],
    matches: &ArgMatches,
    context: &Context,
) -> Result<Option<Output>> {
    if !matches!(
        path.first(),
        Some(&"project" | &"workspace" | &"worktree" | &"clone")
    ) {
        return Ok(None);
    }
    if path.len() == 1 && path[0] != "clone" {
        return Ok(None);
    }
    let client = context.connect().await?;
    let result = match path[0] {
        "project" => project(path[1], matches, &client, context).await,
        "workspace" => workspace(path[1], matches, &client, context).await,
        "worktree" => worktree(path[1], matches, &client, context).await,
        "clone" => clone_repo(matches, &client).await,
        _ => unreachable!(),
    };
    client.close().await;
    result.map(Some)
}
async fn rpc(
    client: &DaemonClient,
    mut message: Value,
    response: &str,
    context: Option<&Context>,
) -> Result<Value> {
    if message.get("requestId").is_none() {
        message["requestId"] = uuid::Uuid::new_v4().to_string().into();
    }
    let result = if let Some(context) = context {
        client
            .request_cancellable(message, response, RPC, context.cancel.clone())
            .await?
    } else {
        client.request(message, response, RPC).await?
    };
    if let Some(error) = result.get("error").filter(|e| !e.is_null()) {
        if let Some(message) = error["message"].as_str() {
            return Err(ClientError::new(
                error["code"].as_str().unwrap_or("DAEMON_RPC_ERROR"),
                message,
            )
            .into());
        }
    }
    Ok(result)
}
fn project_row(value: &Value) -> Value {
    json!({"projectId":value["projectId"],"name":value["projectDisplayName"],"kind":value["projectKind"],"path":value["projectRootPath"]})
}
fn workspace_row(value: &Value) -> Value {
    json!({"workspaceId":value["id"],"project":value["projectDisplayName"],"name":value["name"],"isolation":if value["workspaceKind"]=="worktree"{"worktree"}else{"local"},"cwd":value["workspaceDirectory"],"background":value["background"]==true})
}
fn rename_value(matches: &ArgMatches, key: &str) -> Result<Value> {
    let value = string(matches, key);
    if flag(matches, "reset") {
        ensure!(value.is_none(), "--reset cannot be combined with a {key}");
        Ok(Value::Null)
    } else {
        let value = value.unwrap_or("").trim();
        ensure!(!value.is_empty(), "{key} cannot be empty");
        Ok(value.into())
    }
}
async fn project(
    action: &str,
    m: &ArgMatches,
    client: &DaemonClient,
    context: &Context,
) -> Result<Output> {
    match action {
        "ls" => {
            let payload = rpc(
                client,
                json!({"type":"project.list.request"}),
                "project.list.response",
                Some(context),
            )
            .await?;
            let rows = payload["projects"]
                .as_array()
                .ok_or_else(|| anyhow!("Invalid project list"))?
                .iter()
                .map(project_row)
                .collect::<Vec<_>>();
            Ok(Output::new(json!(rows))
                .id("projectId")
                .columns(PROJECT)
                .widths(&[20, 24, 10, 42]))
        }
        "create" => {
            let raw = string(m, "path");
            let cwd = match &context.target {
                DaemonTarget::Endpoint { .. } => raw
                    .ok_or_else(|| {
                        ClientError::new(
                            "MISSING_PATH",
                            "Project path is required when targeting a daemon explicitly",
                        )
                    })?
                    .to_owned(),
                _ => absolute(raw.unwrap_or("."))?,
            };
            let payload = rpc(
                client,
                json!({"type":"project.add.request","cwd":cwd}),
                "project.add.response",
                Some(context),
            )
            .await?;
            ensure!(payload["project"].is_object(), "Project creation failed");
            Ok(Output::new(project_row(&payload["project"]))
                .id("projectId")
                .columns(PROJECT)
                .widths(&[20, 24, 10, 42]))
        }
        "rename" => {
            let id = required(m, "project-id")?;
            let payload=rpc(client,json!({"type":"project.rename.request","projectId":id,"customName":rename_value(m,"name")?}),"project.rename.response",Some(context)).await?;
            ensure!(payload["accepted"] == true, "renameProject rejected");
            Ok(
                Output::new(json!({"projectId":id,"name":payload["customName"]}))
                    .id("projectId")
                    .columns(&[("PROJECT ID", "projectId"), ("NAME", "name")]),
            )
        }
        "delete" => {
            let id = required(m, "project-id")?;
            let payload = rpc(
                client,
                json!({"type":"project.remove.request","projectId":id}),
                "project.remove.response",
                Some(context),
            )
            .await?;
            ensure!(payload["accepted"] == true, "removeProject rejected");
            let count = payload["removedWorkspaceIds"]
                .as_array()
                .map(Vec::len)
                .unwrap_or(0);
            Ok(Output::new(
                json!({"projectId":id,"removedWorkspaceIds":payload["removedWorkspaceIds"]}),
            )
            .table_data(json!({"projectId":id,"removedWorkspaceCount":count}))
            .id("projectId")
            .columns(&[
                ("PROJECT ID", "projectId"),
                ("REMOVED WORKSPACES", "removedWorkspaceCount"),
            ])
            .widths(&[20, 18]))
        }
        _ => bail!("Unknown project command {action}"),
    }
}
pub(super) async fn list_workspaces(client: &DaemonClient, background: bool) -> Result<Vec<Value>> {
    let mut rows = Vec::new();
    let mut cursor = None;
    let mut seen = std::collections::HashSet::new();
    loop {
        let mut message = json!({"type":"fetch_workspaces_request","page":{"limit":200}});
        if background {
            message["filter"] = json!({"includeBackground":true});
        }
        if let Some(cursor) = &cursor {
            message["page"]["cursor"] = json!(cursor);
        }
        let payload = rpc(client, message, "fetch_workspaces_response", None).await?;
        rows.extend(
            payload["entries"]
                .as_array()
                .ok_or_else(|| anyhow!("Invalid workspace directory"))?
                .iter()
                .cloned(),
        );
        cursor = payload["pageInfo"]["nextCursor"]
            .as_str()
            .map(str::to_owned);
        let Some(next) = &cursor else { break };
        ensure!(
            seen.insert(next.clone()),
            "Daemon repeated a workspace page cursor"
        );
    }
    Ok(rows)
}
async fn caller(client: &DaemonClient) -> Result<Option<String>> {
    let Some(id) = std::env::var("PASEO_AGENT_ID")
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
    else {
        return Ok(None);
    };
    match client
        .request(
            json!({"type":"fetch_agent_request","agentId":id}),
            "fetch_agent_response",
            RPC,
        )
        .await
    {
        Ok(value) => Ok((value["agent"]["id"] == id).then_some(id)),
        Err(error) => {
            if error.downcast_ref::<ClientError>().is_some_and(|e| {
                matches!(
                    e.code.as_str(),
                    "DAEMON_CONNECTION_LOST" | "DAEMON_REQUEST_TIMEOUT"
                )
            }) {
                Err(error)
            } else {
                Ok(None)
            }
        }
    }
}
async fn workspace(
    action: &str,
    m: &ArgMatches,
    client: &DaemonClient,
    context: &Context,
) -> Result<Output> {
    match action {
        "ls" => {
            let rows = list_workspaces(client, flag(m, "background"))
                .await?
                .iter()
                .map(workspace_row)
                .collect::<Vec<_>>();
            let mut output = Output::new(json!(rows))
                .id("workspaceId")
                .columns(WORKSPACE)
                .widths(&[20, 20, 22, 10, 42]);
            if flag(m, "background") {
                let mut table = output.data.clone();
                if let Some(rows) = table.as_array_mut() {
                    for row in rows {
                        row["background"] =
                            (if row["background"] == true { "yes" } else { "" }).into();
                    }
                }
                output = output.table_data(table);
                output.columns.push(("BACKGROUND", "background"));
                output.min_widths.push(8);
            }
            Ok(output)
        }
        "create" => {
            let source = workspace_source(m)?;
            let mut message = json!({"type":"workspace.create.request","source":source});
            if client.server_info()["features"]["creationLifecycle"] == true
                || client.server_info()["features"]["workspaceRequestReceipts"] == true
            {
                message["idempotencyKey"] = uuid::Uuid::new_v4().to_string().into();
            }
            copy(m, &mut message, &[("title", "title")]);
            if flag(m, "background") || flag(m, "no-background") {
                client.require_feature(
                    "backgroundWorkspaces",
                    "Update the host to create background workspaces.",
                )?;
                message["background"] = flag(m, "background").into();
            }
            if let Some(id) = caller(client).await? {
                client.require_feature(
                    "backgroundWorkspaces",
                    "Update the host to create background workspaces.",
                )?;
                message["callerAgentId"] = id.into();
            }
            let payload = client
                .request_cancellable(
                    message,
                    "workspace.create.response",
                    Duration::ZERO,
                    context.cancel.clone(),
                )
                .await?;
            ensure!(
                payload["workspace"].is_object(),
                "Workspace creation failed"
            );
            Ok(Output::new(workspace_row(&payload["workspace"]))
                .id("workspaceId")
                .columns(WORKSPACE)
                .widths(&[20, 20, 22, 10, 42]))
        }
        "rename" => {
            let id = required(m, "workspace-id")?;
            let payload=rpc(client,json!({"type":"workspace.title.set.request","workspaceId":id,"title":rename_value(m,"title")?}),"workspace.title.set.response",Some(context)).await?;
            ensure!(payload["accepted"] == true, "setWorkspaceTitle rejected");
            Ok(
                Output::new(json!({"workspaceId":id,"title":payload["title"]}))
                    .id("workspaceId")
                    .columns(&[("WORKSPACE ID", "workspaceId"), ("TITLE", "title")]),
            )
        }
        "setup" => {
            client.require_feature(
                "workspaceSetupRun",
                "Update the host to run workspace setup.",
            )?;
            let id = required(m, "workspace-id")?;
            let payload = rpc(
                client,
                json!({"type":"workspace.setup.run.request","workspaceId":id}),
                "workspace.setup.run.response",
                Some(context),
            )
            .await?;
            Ok(Output::new(json!({"workspaceId":id,"status":if payload["started"]==true{"started"}else{"already_allowed"}})).id("workspaceId").columns(&[("WORKSPACE ID","workspaceId"),("STATUS","status")]))
        }
        "archive" => {
            let id = required(m, "workspace-id")?;
            let payload = rpc(
                client,
                json!({"type":"archive_workspace_request","workspaceId":id}),
                "archive_workspace_response",
                Some(context),
            )
            .await?;
            ensure!(
                payload["archivedAt"].is_string(),
                "Workspace archive did not return an archive timestamp"
            );
            Ok(Output::new(
                json!({"workspaceId":id,"status":"archived","archivedAt":payload["archivedAt"]}),
            )
            .id("workspaceId")
            .columns(&[
                ("WORKSPACE ID", "workspaceId"),
                ("STATUS", "status"),
                ("ARCHIVED AT", "archivedAt"),
            ]))
        }
        _ => bail!("Unknown workspace command {action}"),
    }
}
fn absent(m: &ArgMatches, keys: &[&str], message: &str) -> Result<()> {
    ensure!(keys.iter().all(|key| string(m, key).is_none()), "{message}");
    Ok(())
}
fn copy(m: &ArgMatches, target: &mut Value, keys: &[(&str, &str)]) {
    for (from, to) in keys {
        if let Some(value) = string(m, from) {
            target[*to] = value.into();
        }
    }
}
fn workspace_source(m: &ArgMatches) -> Result<Value> {
    let cwd = std::env::current_dir()?.to_string_lossy().into_owned();
    let path = string(m, "path");
    let project = string(m, "project");
    if required(m, "isolation")? == "local" {
        absent(
            m,
            &[
                "mode",
                "worktree-slug",
                "new-branch",
                "base",
                "branch",
                "pr-number",
                "forge",
            ],
            "Worktree options require --isolation worktree",
        )?;
        let mut source = json!({"kind":"directory","path":path.unwrap_or(&cwd)});
        copy(m, &mut source, &[("project", "projectId")]);
        return Ok(source);
    }
    let mut source = json!({"kind":"worktree"});
    if let Some(path) = path.or_else(|| project.is_none().then_some(cwd.as_str())) {
        source["cwd"] = path.into();
    }
    copy(
        m,
        &mut source,
        &[("project", "projectId"), ("worktree-slug", "worktreeSlug")],
    );
    match string(m, "mode").unwrap_or("branch-off") {
        "branch-off" => {
            absent(
                m,
                &["branch", "pr-number", "forge"],
                "--branch, --pr-number, and --forge require a checkout mode",
            )?;
            source["action"] = "branch-off".into();
            copy(
                m,
                &mut source,
                &[("new-branch", "branchName"), ("base", "baseBranch")],
            );
        }
        "checkout-branch" => {
            absent(
                m,
                &["new-branch", "base", "pr-number", "forge"],
                "--new-branch, --base, --pr-number, and --forge are not valid for --mode checkout-branch",
            )?;
            source["action"] = "checkout".into();
            source["refName"] = required(m, "branch")?.into();
        }
        "checkout-pr" => {
            absent(
                m,
                &["new-branch", "base", "branch"],
                "--new-branch, --base, and --branch are not valid for --mode checkout-pr",
            )?;
            let number = number(m, "pr-number")?
                .filter(|n| *n > 0)
                .ok_or_else(|| anyhow!("--pr-number must be a positive integer"))?;
            source["action"] = "checkout".into();
            source["checkoutSource"] = json!({"kind":"change_request","number":number});
            copy(m, &mut source["checkoutSource"], &[("forge", "forge")]);
        }
        mode => bail!("Unsupported worktree mode: {mode}"),
    }
    Ok(source)
}
async fn worktree(
    action: &str,
    m: &ArgMatches,
    client: &DaemonClient,
    context: &Context,
) -> Result<Output> {
    let current = std::env::current_dir()?.to_string_lossy().into_owned();
    let cwd = string(m, "cwd").unwrap_or(&current);
    if action != "create"
        && matches!(context.target, DaemonTarget::Endpoint { .. })
        && string(m, "cwd").is_none()
    {
        bail!("--cwd is required when targeting a daemon explicitly");
    }
    if action == "create" {
        let mut request = json!({"type":"create_paseo_worktree_request","cwd":cwd});
        match required(m, "mode")? {
            "branch-off" => {
                request["action"] = "branch-off".into();
                request["worktreeSlug"] = required(m, "new-branch")?.into();
                copy(m, &mut request, &[("base", "refName")]);
            }
            "checkout-branch" => {
                request["action"] = "checkout".into();
                request["refName"] = required(m, "branch")?.into();
            }
            "checkout-pr" => {
                request["action"] = "checkout".into();
                request["githubPrNumber"] = number(m, "pr-number")?
                    .filter(|n| *n > 0)
                    .ok_or_else(|| anyhow!("--pr-number must be a positive integer"))?
                    .into();
            }
            mode => bail!("Invalid --mode: {mode}"),
        }
        let payload = rpc(
            client,
            request,
            "create_paseo_worktree_response",
            Some(context),
        )
        .await?;
        let workspace = &payload["workspace"];
        let path = workspace["workspaceDirectory"].as_str().ok_or_else(|| {
            anyhow!("Failed to create worktree: workspace directory missing from daemon response")
        })?;
        return Ok(Output::new(
            json!({"name":basename(path),"branchName":workspace["name"],"worktreePath":path}),
        )
        .id("worktreePath")
        .columns(&[
            ("NAME", "name"),
            ("BRANCH", "branchName"),
            ("PATH", "worktreePath"),
        ]));
    }
    let payload = rpc(
        client,
        json!({"type":"paseo_worktree_list_request","cwd":cwd}),
        "paseo_worktree_list_response",
        Some(context),
    )
    .await?;
    let worktrees = payload["worktrees"]
        .as_array()
        .ok_or_else(|| anyhow!("Invalid worktree list"))?;
    if action == "archive" {
        let name = required(m, "name")?;
        ensure!(!name.trim().is_empty(), "Worktree name is required");
        let tree = worktrees
            .iter()
            .find(|tree| {
                tree["branchName"] == name
                    || tree["worktreePath"]
                        .as_str()
                        .is_some_and(|p| basename(p) == name)
            })
            .ok_or_else(|| anyhow!("Worktree not found: {name}"))?;
        let result=rpc(client,json!({"type":"paseo_worktree_archive_request","worktreePath":tree["worktreePath"],"scope":"worktree"}),"paseo_worktree_archive_response",Some(context)).await?;
        let data = json!({"name":basename(tree["worktreePath"].as_str().unwrap_or(name)),"status":"archived","removedAgents":result.get("removedAgents").cloned().unwrap_or(json!([]))});
        let mut table = data.clone();
        table["removedAgents"] = data["removedAgents"]
            .as_array()
            .filter(|agents| !agents.is_empty())
            .map(|agents| {
                agents
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_else(|| "-".into())
            .into();
        return Ok(Output::new(data).table_data(table).id("name").columns(&[
            ("NAME", "name"),
            ("STATUS", "status"),
            ("REMOVED AGENTS", "removedAgents"),
        ]));
    }
    let agents = rpc(
        client,
        json!({"type":"fetch_agents_request","filter":{"includeArchived":true}}),
        "fetch_agents_response",
        Some(context),
    )
    .await?;
    let agents = agents["entries"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let home = std::env::var("HOME").ok();
    let rows:Vec<_>=worktrees.iter().map(|tree|{let path=tree["worktreePath"].as_str().unwrap_or("");let agent=agents.iter().rev().find(|a|a["agent"]["cwd"]==path).and_then(|a|a["agent"]["id"].as_str()).map(|s|s.chars().take(7).collect::<String>()).unwrap_or_else(||"-".into());let display=home.as_deref().and_then(|home|path.strip_prefix(home)).map(|rest|format!("~{rest}")).unwrap_or_else(||path.into());json!({"name":basename(path),"branch":tree["branchName"].as_str().unwrap_or("-"),"cwd":display,"agent":agent})}).collect();
    Ok(Output::new(json!(rows)).id("name").columns(&[
        ("NAME", "name"),
        ("BRANCH", "branch"),
        ("CWD", "cwd"),
        ("AGENT", "agent"),
    ]))
}
async fn clone_repo(m: &ArgMatches, client: &DaemonClient) -> Result<Output> {
    let repo = required(m, "repo")?;
    let dir = required(m, "dir")?.trim();
    ensure!(!dir.is_empty(), "--dir is required");
    let complete = repo.contains("://")
        || repo
            .split_once(':')
            .is_some_and(|(host, path)| host.contains('@') && !path.is_empty());
    ensure!(
        complete || string(m, "protocol").is_some(),
        "--protocol is required for owner/repo repository names"
    );
    client.require_feature("projectGithubClone","This daemon does not support cloning GitHub repos. Update the host to a newer Paseo version.")?;
    let mut request =
        json!({"type":"project.github.clone.request","repo":repo,"targetDirectory":dir});
    if !complete {
        copy(m, &mut request, &[("protocol", "cloneProtocol")]);
    }
    let response = client
        .request(
            request,
            "project.github.clone.response",
            Duration::from_secs(600),
        )
        .await?;
    ensure!(
        response["project"].is_object() && response["checkoutPath"].is_string(),
        "Failed to clone GitHub repo: no project returned"
    );
    Ok(Output::new(json!({"repo":response["repo"],"checkoutPath":response["checkoutPath"],"projectId":response["project"]["projectId"],"projectName":response["project"]["projectDisplayName"]})).id("projectId").columns(&[("REPO","repo"),("PROJECT","projectName"),("PATH","checkoutPath")]))
}
pub(super) fn absolute(path: &str) -> Result<String> {
    let path = PathBuf::from(path);
    Ok(if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    }
    .components()
    .fold(PathBuf::new(), |mut out, part| {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
        out
    })
    .to_string_lossy()
    .into_owned())
}
fn basename(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn branch_modes_reject_conflicting_intent() {
        let command = commands()
            .into_iter()
            .find(|c| c.get_name() == "workspace")
            .unwrap();
        let matches = command
            .try_get_matches_from([
                "workspace",
                "create",
                "--isolation",
                "worktree",
                "--mode",
                "checkout-branch",
                "--branch",
                "main",
                "--new-branch",
                "unsafe",
            ])
            .unwrap();
        assert!(workspace_source(matches.subcommand().unwrap().1).is_err());
    }
    #[test]
    fn local_source_cannot_silently_ignore_worktree_options() {
        let command = commands()
            .into_iter()
            .find(|c| c.get_name() == "workspace")
            .unwrap();
        let matches = command
            .try_get_matches_from([
                "workspace",
                "create",
                "--isolation",
                "local",
                "--base",
                "main",
            ])
            .unwrap();
        assert!(workspace_source(matches.subcommand().unwrap().1).is_err());
    }
}
