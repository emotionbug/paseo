use std::{
    borrow::Cow,
    cmp::Ordering,
    collections::{HashMap, HashSet},
    path::Path,
    sync::Mutex,
};

use anyhow::{Result, anyhow};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Map, Value, json};

use crate::state::AppState;

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct DirectoryRequestError {
    pub code: &'static str,
    message: &'static str,
}

#[derive(Default)]
pub struct DirectoryState {
    state: Mutex<ProjectionState>,
}

#[derive(Default)]
struct ProjectionState {
    buckets: HashMap<String, (Bucket, String)>,
    archiving: HashMap<String, String>,
}

impl DirectoryState {
    pub fn mark_archiving(&self, ids: &[String], at: &str) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        for id in ids {
            state.archiving.insert(id.clone(), at.to_owned());
        }
    }

    pub fn clear_archiving(&self, ids: &[String]) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        for id in ids {
            state.archiving.remove(id);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Bucket {
    NeedsInput,
    Failed,
    Running,
    Attention,
    Done,
}

impl Bucket {
    fn name(self) -> &'static str {
        match self {
            Self::NeedsInput => "needs_input",
            Self::Failed => "failed",
            Self::Running => "running",
            Self::Attention => "attention",
            Self::Done => "done",
        }
    }
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}

fn enabled(value: &Value, key: &str) -> bool {
    value[key].as_bool() == Some(true)
}

fn nonnull<'a>(first: &'a Value, fallback: &'a Value) -> &'a Value {
    if first.is_null() { fallback } else { first }
}

fn project_name(record: &Value) -> &Value {
    nonnull(&record["customName"], &record["displayName"])
}

fn workspace_name(record: &Value) -> &Value {
    nonnull(&record["title"], &record["displayName"])
}

pub fn project_descriptor(record: &Value) -> Value {
    let mut result = json!({
        "projectId": record["projectId"],
        "projectDisplayName": project_name(record),
        "projectCustomName": record["customName"],
        "projectCustomIconRevision": record["customIconRevision"],
        "projectRootPath": record["rootPath"],
        "projectKind": record["kind"],
    });
    if record["projectKey"]
        .as_str()
        .is_some_and(|key| !key.is_empty())
    {
        result["projectKey"] = record["projectKey"].clone();
    }
    // Icon observation is owned by the project service, not inferred from a custom icon.
    if let Some(revision) = record.get("projectIconRevision") {
        result["projectIconRevision"] = revision.clone();
    }
    result
}

fn checkout_placement(workspace: &Value, git: &Value) -> Value {
    if text(workspace, "kind") == "directory" {
        return json!({"cwd":workspace["cwd"],"isGit":false,"currentBranch":null,
            "remoteUrl":null,"worktreeRoot":null,"isPaseoOwnedWorktree":false,"mainRepoRoot":null});
    }
    json!({
        "cwd": workspace["cwd"], "isGit": true,
        "currentBranch": nonnull(&workspace["branch"], &git["currentBranch"]),
        "remoteUrl": null,
        "worktreeRoot": nonnull(&workspace["worktreeRoot"], nonnull(&git["repoRoot"], &workspace["cwd"])),
        "isPaseoOwnedWorktree": enabled(workspace,"isPaseoOwnedWorktree") && !workspace["mainRepoRoot"].is_null(),
        "mainRepoRoot": workspace["mainRepoRoot"],
    })
}

fn setup_summary(workspace: &Value, snapshot: Option<&Value>) -> Value {
    match snapshot {
        None if !workspace["untrustedSource"].is_null() => json!("blocked"),
        None => Value::Null,
        Some(snapshot)
            if matches!(text(snapshot, "status"), "failed" | "blocked")
                || !snapshot["error"].is_null()
                || snapshot
                    .pointer("/detail/commands")
                    .and_then(Value::as_array)
                    .is_some_and(|commands| !commands.is_empty()) =>
        {
            snapshot["status"].clone()
        }
        Some(_) => Value::Null,
    }
}

fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or(path)
}

fn base_descriptor(
    workspace: &Value,
    project: Option<&Value>,
    snapshot: Option<&Value>,
    scripts: Vec<Value>,
    setup: Option<&Value>,
) -> Value {
    let null = Value::Null;
    let project_record = project.unwrap_or(&null);
    let git = snapshot.map(|snapshot| &snapshot["git"]).unwrap_or(&null);
    let mut descriptor = json!({
        "id": workspace["workspaceId"], "projectId": workspace["projectId"],
        "projectDisplayName": project.map(project_name).unwrap_or(&workspace["projectId"]),
        "projectCustomName": project_record["customName"],
        "projectCustomIconRevision": project_record["customIconRevision"],
        "projectRootPath": project.map(|project| &project["rootPath"]).unwrap_or(&workspace["cwd"]),
        "workspaceDirectory": workspace["cwd"], "setupStatus": setup_summary(workspace,setup),
        "projectKind": if text(project_record,"kind") == "git" { "git" } else { "non_git" },
        "workspaceKind": workspace["kind"], "name": workspace_name(workspace),
        "title": workspace["title"], "pinnedAt": workspace["pinnedAt"],
        "background": enabled(workspace,"background"), "archivingAt": null,
        "status": "done", "statusEnteredAt": null, "activityAt": null,
        "createdAt": workspace["createdAt"], "lastUserMessageAt": null,
        "diffStat": git["diffStat"], "scripts": scripts,
    });
    if workspace["labels"]
        .as_array()
        .is_some_and(|labels| !labels.is_empty())
    {
        descriptor["labels"] = workspace["labels"].clone();
    }
    if enabled(workspace, "isPaseoOwnedWorktree") {
        if let Some(root) = workspace["worktreeRoot"]
            .as_str()
            .filter(|root| !root.is_empty())
        {
            descriptor["worktreeSlug"] = json!(basename(root));
        }
    }
    if let Some(project) = project {
        descriptor["project"] = json!({
            "projectKey": project["projectId"], "projectName": project_name(project),
            "workspaceName": workspace_name(workspace), "checkout": checkout_placement(workspace,git),
        });
    }
    if let Some(snapshot) = snapshot {
        let branch = text(git, "currentBranch").trim();
        let derived = if enabled(git, "isGit")
            && !branch.is_empty()
            && !branch.eq_ignore_ascii_case("HEAD")
        {
            branch
        } else {
            basename(text(workspace, "cwd"))
        };
        descriptor["name"] = workspace["title"]
            .as_str()
            .map_or_else(|| json!(derived), |title| json!(title));
        if enabled(git, "isGit") {
            let mut runtime = Map::new();
            for key in [
                "currentBranch",
                "remoteUrl",
                "isPaseoOwnedWorktree",
                "isDirty",
                "aheadBehind",
                "aheadOfOrigin",
                "behindOfOrigin",
            ] {
                runtime.insert(key.to_owned(), git[key].clone());
            }
            descriptor["gitRuntime"] = Value::Object(runtime);
        }
        let forge = &snapshot["forge"];
        descriptor["githubRuntime"] = json!({"featuresEnabled":forge["featuresEnabled"],
            "pullRequest":forge["pullRequest"],"error":forge["error"]});
        if let Some(forge) = forge["forge"].as_str() {
            descriptor["forge"] = json!(forge);
        }
    }
    descriptor
}

fn agent_bucket(agent: &Value) -> Bucket {
    if agent["pendingPermissions"]
        .as_array()
        .is_some_and(|permissions| !permissions.is_empty())
        || text(agent, "attentionReason") == "permission"
    {
        Bucket::NeedsInput
    } else if text(agent, "status") == "error" || text(agent, "attentionReason") == "error" {
        Bucket::Failed
    } else if text(agent, "status") == "running" {
        Bucket::Running
    } else if enabled(agent, "requiresAttention") {
        Bucket::Attention
    } else {
        Bucket::Done
    }
}

fn terminal_bucket(activity: &Value) -> Option<Bucket> {
    match text(activity, "attentionReason") {
        "needs_input" => Some(Bucket::NeedsInput),
        "finished" => Some(Bucket::Attention),
        _ => match text(activity, "state") {
            "working" => Some(Bucket::Running),
            "attention" => Some(Bucket::NeedsInput),
            _ => None,
        },
    }
}

fn workspace_root_agent<'a>(
    agent: &'a Value,
    agents: &HashMap<&str, &'a Value>,
) -> Option<&'a Value> {
    let mut current = agent;
    let mut seen = HashSet::new();
    seen.insert(text(current, "id"));
    loop {
        let Some(parent_id) = current
            .pointer("/labels/paseo.parent-agent-id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
        else {
            return Some(current);
        };
        if !seen.insert(parent_id) {
            return None;
        }
        let parent = *agents.get(parent_id)?;
        if parent["workspaceId"] != current["workspaceId"] {
            return Some(current);
        }
        current = parent;
    }
}

struct Contributions {
    bucket: Bucket,
    agents: Vec<usize>,
    activity: Vec<(Bucket, String)>,
}

fn resolve_entered_at(
    previous: Option<&(Bucket, String)>,
    bucket: Bucket,
    agents: &[usize],
    all_agents: &[Value],
    activity: &[(Bucket, String)],
    created_at: Option<&str>,
    now: &str,
) -> Option<String> {
    if let Some((previous_bucket, at)) = previous {
        return Some(
            if *previous_bucket == bucket {
                at.as_str()
            } else {
                now
            }
            .to_owned(),
        );
    }
    if agents.is_empty() && activity.is_empty() {
        return created_at.map(str::to_owned);
    }
    agents
        .iter()
        .map(|index| &all_agents[*index])
        .filter(|agent| agent_bucket(agent) == bucket)
        .filter_map(|agent| {
            agent["attentionTimestamp"]
                .as_str()
                .filter(|value| !value.is_empty())
                .or_else(|| {
                    agent["updatedAt"]
                        .as_str()
                        .filter(|value| !value.is_empty())
                })
        })
        .chain(
            activity
                .iter()
                .filter(|(candidate, _)| *candidate == bucket)
                .map(|(_, at)| at.as_str()),
        )
        .max()
        .or(Some(now))
        .map(str::to_owned)
}

pub async fn build_entries(
    app: &AppState,
    workspaces: Vec<Value>,
    projects: Vec<Value>,
) -> Result<Vec<Value>> {
    build_entries_ref(app, &workspaces, &projects).await
}

async fn build_entries_ref(
    app: &AppState,
    workspaces: &[Value],
    projects: &[Value],
) -> Result<Vec<Value>> {
    let (agents, message_times, provider_activity) = tokio::try_join!(
        app.agents.list_snapshots(),
        app.agents.workspace_last_user_message_times(),
        app.agents.list_provider_subagent_activity()
    )?;
    let project_map: HashMap<&str, &Value> = projects
        .iter()
        .map(|project| (text(project, "projectId"), project))
        .collect();
    let active_agents: HashMap<&str, &Value> = agents
        .iter()
        .filter(|agent| {
            agent["archivedAt"].is_null()
                && !enabled(agent, "internal")
                && !enabled(agent, "background")
                && (enabled(agent, "_live") || enabled(agent, "_providerAvailable"))
        })
        .map(|agent| (text(agent, "id"), agent))
        .collect();
    let mut entries = Vec::with_capacity(workspaces.len());
    let mut contributions = HashMap::new();
    let mut snapshots = HashMap::new();
    for workspace in workspaces {
        let project = project_map.get(text(workspace, "projectId")).copied();
        if !workspace["archivedAt"].is_null()
            || project.is_some_and(|project| !project["archivedAt"].is_null())
        {
            continue;
        }
        let id = text(workspace, "workspaceId");
        let cwd = text(workspace, "cwd");
        if text(workspace, "kind") != "directory" && !snapshots.contains_key(cwd) {
            let snapshot = match app
                .git
                .snapshot(Path::new(cwd), false, app.cancel.child_token())
                .await
            {
                Ok(snapshot) => Some(snapshot),
                Err(error) => {
                    if app.cancel.is_cancelled() {
                        return Err(error);
                    }
                    // A checkout observation failure does not erase durable workspace identity.
                    tracing::warn!(cwd, %error, "Workspace checkout observation unavailable");
                    app.git.cached_snapshot(Path::new(cwd)).await
                }
            };
            snapshots.insert(cwd, snapshot);
        }
        let (scripts, setup) = tokio::try_join!(
            app.terminals.script_list(id),
            app.terminals.setup_status(id)
        )?;
        let snapshot = if text(workspace, "kind") == "directory" {
            None
        } else {
            snapshots.get(cwd).and_then(Option::as_ref)
        };
        let mut descriptor = base_descriptor(workspace, project, snapshot, scripts, setup.as_ref());
        descriptor["lastUserMessageAt"] = message_times.get(id).map_or(Value::Null, |at| json!(at));
        let mut contribution = Contributions {
            bucket: Bucket::Done,
            agents: Vec::new(),
            activity: Vec::new(),
        };
        for terminal in app.terminals.list(Some(id), None).await {
            // Ownership is the workspace id, never its shared checkout directory.
            if text(&terminal, "workspaceId") != id {
                continue;
            }
            let activity = &terminal["activity"];
            if let Some(bucket) = terminal_bucket(activity) {
                contribution.bucket = contribution.bucket.min(bucket);
                if let Some(at) = activity["changedAt"]
                    .as_i64()
                    .and_then(DateTime::<Utc>::from_timestamp_millis)
                {
                    contribution
                        .activity
                        .push((bucket, at.to_rfc3339_opts(SecondsFormat::Millis, true)));
                }
            }
        }
        contributions.insert(id, contribution);
        entries.push(descriptor);
    }
    for (index, agent) in agents.iter().enumerate() {
        if !active_agents.contains_key(text(agent, "id")) {
            continue;
        }
        let Some(contribution) = contributions.get_mut(text(agent, "workspaceId")) else {
            continue;
        };
        // Timestamp grouping includes all active agents, even descendants whose idle state is masked.
        contribution.agents.push(index);
        let Some(root) = workspace_root_agent(agent, &active_agents) else {
            continue;
        };
        if root["id"] == agent["id"] {
            contribution.bucket = contribution.bucket.min(agent_bucket(agent));
        } else if text(agent, "status") == "running" {
            contribution.bucket = contribution.bucket.min(Bucket::Running);
        }
    }
    for subagent in &provider_activity {
        if text(subagent, "status") != "running" {
            continue;
        }
        let Some(parent) = active_agents.get(text(subagent, "parentAgentId")) else {
            continue;
        };
        let Some(root) = workspace_root_agent(parent, &active_agents) else {
            continue;
        };
        if let Some(contribution) = contributions.get_mut(text(root, "workspaceId")) {
            contribution.bucket = contribution.bucket.min(Bucket::Running);
            if let Some(at) = subagent["updatedAt"].as_str() {
                contribution.activity.push((Bucket::Running, at.to_owned()));
            }
        }
    }
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let mut state = app
        .projects
        .directory_state
        .state
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    for entry in &mut entries {
        let id = text(entry, "id");
        let contribution = &contributions[id];
        let at = resolve_entered_at(
            state.buckets.get(id),
            contribution.bucket,
            &contribution.agents,
            &agents,
            &contribution.activity,
            entry["createdAt"].as_str(),
            &now,
        );
        let archiving = state.archiving.get(id).map_or(Value::Null, |at| json!(at));
        if let Some(at) = &at {
            state
                .buckets
                .insert(id.to_owned(), (contribution.bucket, at.clone()));
        }
        entry["status"] = json!(contribution.bucket.name());
        entry["statusEnteredAt"] = json!(at);
        entry["archivingAt"] = archiving;
    }
    Ok(entries)
}

pub async fn descriptor(app: &AppState, workspace: &Value, project: &Value) -> Result<Value> {
    build_entries_ref(
        app,
        std::slice::from_ref(workspace),
        std::slice::from_ref(project),
    )
    .await?
    .into_iter()
    .next()
    .ok_or_else(|| {
        anyhow!(
            "Workspace is not active: {}",
            text(workspace, "workspaceId")
        )
    })
}

pub async fn placement(app: &AppState, workspace: &Value, project: &Value) -> Result<Value> {
    let snapshot = if text(workspace, "kind") == "directory" {
        Value::Null
    } else {
        app.git
            .cached_snapshot(Path::new(text(workspace, "cwd")))
            .await
            .unwrap_or(Value::Null)
    };
    Ok(
        json!({"projectKey":project["projectId"],"projectName":project_name(project),
        "workspaceName":workspace_name(workspace),"checkout":checkout_placement(workspace,&snapshot["git"])}),
    )
}

pub fn fetch_projects(app: &AppState, request: &Value, projects: Vec<Value>) -> Result<Value> {
    let projects = projects
        .iter()
        .filter(|project| project["archivedAt"].is_null())
        .map(project_descriptor)
        .collect::<Vec<_>>();
    if let Some(cursor) = request.get("sync").filter(|cursor| !cursor.is_null()) {
        app.agents
            .directory_sync
            .synchronize_projects(projects, cursor)
    } else {
        Ok(json!({"projects":projects}))
    }
}

pub async fn fetch(
    app: &AppState,
    request: &Value,
    workspaces: Vec<Value>,
    projects: Vec<Value>,
) -> Result<Value> {
    if let Some(cursor) = request.get("sync").filter(|cursor| !cursor.is_null()) {
        if request
            .get("filter")
            .is_some_and(|filter| !filter.is_null())
        {
            return Err(DirectoryRequestError {
                code: "invalid_request",
                message: "Sequenced workspace directory reads do not support filters.",
            }
            .into());
        }
        let entries = build_entries(app, workspaces, projects).await?;
        let background = enabled(request, "includeBackground");
        let mut result = app
            .agents
            .directory_sync
            .synchronize_workspaces(entries, cursor, background)?;
        if !background {
            if let Some(entries) = result["entries"].as_array_mut() {
                entries.retain(|entry| !enabled(entry, "background"));
            }
        }
        return Ok(result);
    }
    let entries = build_entries_ref(app, &workspaces, &projects).await?;
    paginate(request, entries, &projects)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SortSpec {
    key: String,
    descending: bool,
}
const SORT_KEYS: [&str; 4] = ["status_priority", "activity_at", "name", "project_id"];

fn sort_specs(request: &Value) -> Result<Vec<SortSpec>> {
    let mut result = Vec::new();
    if let Some(sort) = request["sort"].as_array() {
        for spec in sort {
            let key = text(spec, "key");
            if !SORT_KEYS.contains(&key) || !matches!(text(spec, "direction"), "asc" | "desc") {
                return Err(DirectoryRequestError {
                    code: "invalid_request",
                    message: "Invalid fetch_workspaces sort",
                }
                .into());
            }
            if !result.iter().any(|spec: &SortSpec| spec.key == key) {
                result.push(SortSpec {
                    key: key.to_owned(),
                    descending: text(spec, "direction") == "desc",
                });
            }
        }
    }
    if result.is_empty() {
        result.push(SortSpec {
            key: "activity_at".to_owned(),
            descending: true,
        });
    }
    Ok(result)
}

fn sort_value(entry: &Value, key: &str) -> Value {
    match key {
        "status_priority" => json!(match text(entry, "status") {
            "needs_input" => 0,
            "failed" => 1,
            "running" => 2,
            "attention" => 3,
            _ => 4,
        }),
        "activity_at" => entry["activityAt"]
            .as_str()
            .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
            .map_or(Value::Null, |at| json!(at.timestamp_millis())),
        "name" => json!(crate::json::locale_lowercase(text(entry, "name"))),
        "project_id" => json!(crate::json::locale_lowercase(text(entry, "projectId"))),
        _ => unreachable!("validated workspace sort key"),
    }
}

fn comparison_string(value: &Value) -> Cow<'_, str> {
    match value {
        Value::String(value) => Cow::Borrowed(value),
        Value::Object(_) => Cow::Borrowed("[object Object]"),
        Value::Array(values) => Cow::Owned(
            values
                .iter()
                .map(|value| {
                    if value.is_null() {
                        Cow::Borrowed("")
                    } else {
                        comparison_string(value)
                    }
                })
                .collect::<Vec<_>>()
                .join(","),
        ),
        _ => Cow::Owned(value.to_string()),
    }
}

fn compare_values(left: &Value, right: &Value) -> Ordering {
    match (left, right) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Null, _) => Ordering::Less,
        (_, Value::Null) => Ordering::Greater,
        (Value::Number(left), Value::Number(right)) => left
            .as_f64()
            .partial_cmp(&right.as_f64())
            .unwrap_or(Ordering::Equal),
        (Value::String(left), Value::String(right)) => crate::json::locale_compare(left, right),
        _ => crate::json::locale_compare(&comparison_string(left), &comparison_string(right)),
    }
}

fn sort_json(sort: &[SortSpec]) -> Value {
    Value::Array(
        sort.iter()
            .map(
                |spec| json!({"key":spec.key,"direction":if spec.descending {"desc"} else {"asc"}}),
            )
            .collect(),
    )
}

struct RankedEntry {
    entry: Value,
    values: Map<String, Value>,
}

fn compare_ranked(left: &RankedEntry, right: &RankedEntry, sort: &[SortSpec]) -> Ordering {
    compare_ranked_cursor(left, &right.values, text(&right.entry, "id"), sort)
}

fn compare_ranked_cursor(
    left: &RankedEntry,
    values: &Map<String, Value>,
    id: &str,
    sort: &[SortSpec],
) -> Ordering {
    for spec in sort {
        let ordering = compare_values(
            &left.values[&spec.key],
            values.get(&spec.key).unwrap_or(&Value::Null),
        );
        if ordering != Ordering::Equal {
            return if spec.descending {
                ordering.reverse()
            } else {
                ordering
            };
        }
    }
    crate::json::locale_compare(text(&left.entry, "id"), id)
}

fn decode_cursor(token: &str, sort: &[SortSpec]) -> Result<Value> {
    let invalid = || DirectoryRequestError {
        code: "invalid_cursor",
        message: "Invalid fetch_workspaces cursor",
    };
    let bytes = URL_SAFE_NO_PAD
        .decode(token.trim_end_matches('='))
        .map_err(|_| invalid())?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    let specs = value["sort"].as_array().ok_or_else(invalid)?;
    if value["id"].as_str().is_none() || !value["values"].is_object() {
        return Err(invalid().into());
    }
    if specs.iter().any(|spec| {
        !SORT_KEYS.contains(&text(spec, "key"))
            || !matches!(text(spec, "direction"), "asc" | "desc")
    }) {
        return Err(invalid().into());
    }
    if specs.len() != sort.len()
        || specs.iter().zip(sort).any(|(entry, spec)| {
            text(entry, "key") != spec.key
                || (text(entry, "direction") == "desc") != spec.descending
        })
    {
        return Err(DirectoryRequestError {
            code: "invalid_cursor",
            message: "fetch_workspaces cursor does not match current sort",
        }
        .into());
    }
    Ok(value)
}

/// Legacy pagination is stateless; sequenced reads use AgentManager's shared directory_sync.
pub fn paginate(request: &Value, entries: Vec<Value>, projects: &[Value]) -> Result<Value> {
    let sort = sort_specs(request)?;
    let mut filter = request.get("filter").cloned().unwrap_or_else(|| json!({}));
    if enabled(request, "includeBackground") {
        filter["includeBackground"] = json!(true);
    }
    let background = enabled(&filter, "includeBackground");
    let occupied: HashSet<&str> = entries
        .iter()
        .filter(|entry| background || !enabled(entry, "background"))
        .map(|entry| text(entry, "projectId"))
        .collect();
    let token = request
        .pointer("/page/cursor")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty());
    let empty_projects = if token.is_some() {
        Vec::new()
    } else {
        let project_id = text(&filter, "projectId").trim();
        projects
            .iter()
            .filter(|project| {
                project["archivedAt"].is_null()
                    && !occupied.contains(text(project, "projectId"))
                    && (project_id.is_empty() || text(project, "projectId") == project_id)
            })
            .map(project_descriptor)
            .collect()
    };
    let mut entries: Vec<RankedEntry> = entries
        .into_iter()
        .filter(|entry| crate::directory_sync::matches_workspace_filter(entry, Some(&filter)))
        .map(|entry| {
            let values = sort
                .iter()
                .map(|spec| (spec.key.clone(), sort_value(&entry, &spec.key)))
                .collect();
            RankedEntry { entry, values }
        })
        .collect();
    entries.sort_by(|left, right| compare_ranked(left, right, &sort));
    if let Some(token) = token {
        let cursor = decode_cursor(token, &sort)?;
        let values = cursor["values"]
            .as_object()
            .expect("validated cursor values");
        entries.retain(|entry| {
            compare_ranked_cursor(entry, values, text(&cursor, "id"), &sort) == Ordering::Greater
        });
    }
    let limit = request
        .pointer("/page/limit")
        .and_then(Value::as_u64)
        .unwrap_or(200) as usize;
    let has_more = entries.len() > limit;
    entries.truncate(limit);
    let next_cursor = if has_more {
        entries.last().map(|last| {
            URL_SAFE_NO_PAD.encode(
                json!({"sort":sort_json(&sort),"values":last.values,"id":last.entry["id"]})
                    .to_string()
                    .as_bytes(),
            )
        })
    } else {
        None
    };
    Ok(
        json!({"entries":entries.into_iter().map(|entry| entry.entry).collect::<Vec<_>>(),"emptyProjects":empty_projects,
        "pageInfo":{"nextCursor":next_cursor,"prevCursor":request.pointer("/page/cursor").unwrap_or(&Value::Null),"hasMore":has_more}}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_attention_priority_and_terminal_states() {
        assert_eq!(
            agent_bucket(&json!({"status":"running","attentionReason":"error"})),
            Bucket::Failed
        );
        assert_eq!(
            agent_bucket(&json!({"status":"error","pendingPermissions":[{}]})),
            Bucket::NeedsInput
        );
        assert_eq!(
            agent_bucket(&json!({"status":"initializing"})),
            Bucket::Done
        );
        assert_eq!(
            terminal_bucket(&json!({"state":"attention","attentionReason":"finished"})),
            Some(Bucket::Attention)
        );
        assert_eq!(terminal_bucket(&json!({"state":"idle"})), None);
    }

    #[test]
    fn parent_ownership_is_not_checkout_ownership() {
        let parent = json!({"id":"p","workspaceId":"a","cwd":"/same"});
        let child = json!({"id":"c","workspaceId":"b","cwd":"/same","labels":{"paseo.parent-agent-id":"p"}});
        let agents = HashMap::from([("p", &parent), ("c", &child)]);
        assert_eq!(workspace_root_agent(&child, &agents).unwrap()["id"], "c");
        assert!(workspace_root_agent(&child, &HashMap::new()).is_none());
        let cycle = json!({"id":"p","workspaceId":"b","labels":{"paseo.parent-agent-id":"c"}});
        assert!(
            workspace_root_agent(&child, &HashMap::from([("p", &cycle), ("c", &child)])).is_none()
        );
    }

    #[test]
    fn priority_unmask_uses_transition_time_and_then_retains_it() {
        let agents = vec![json!({"status":"running","updatedAt":"old"})];
        let previous = (Bucket::NeedsInput, "permission-time".to_owned());
        assert_eq!(
            resolve_entered_at(
                Some(&previous),
                Bucket::Running,
                &[0],
                &agents,
                &[],
                Some("created"),
                "now"
            ),
            Some("now".into())
        );
        let previous = (Bucket::Running, "unmasked".to_owned());
        assert_eq!(
            resolve_entered_at(
                Some(&previous),
                Bucket::Running,
                &[0],
                &agents,
                &[],
                None,
                "later"
            ),
            Some("unmasked".into())
        );
        assert_eq!(
            resolve_entered_at(None, Bucket::Done, &[], &[], &[], Some("created"), "now"),
            Some("created".into())
        );
        assert_eq!(
            resolve_entered_at(
                Some(&previous),
                Bucket::Done,
                &[],
                &[],
                &[],
                Some("created"),
                "now"
            ),
            Some("now".into())
        );
    }

    #[test]
    fn placement_preserves_stored_branch_and_title_over_live_checkout() {
        let workspace = json!({"workspaceId":"a","projectId":"p","kind":"worktree","cwd":"/worktree",
            "branch":"saved","title":"Human title","displayName":"saved","isPaseoOwnedWorktree":true,
            "mainRepoRoot":"/main","worktreeRoot":"/worktree","createdAt":"created"});
        let project = json!({"projectId":"p","kind":"git","displayName":"repo","customName":"Custom","rootPath":"/main"});
        let snapshot = json!({"git":{"isGit":true,"currentBranch":"live","remoteUrl":"ssh://repo","diffStat":{"additions":7,"deletions":3}},
            "forge":{"featuresEnabled":true,"pullRequest":{"url":"https://forge/pr/1"},"error":null,"forge":"gitlab"}});
        let entry = base_descriptor(&workspace, Some(&project), Some(&snapshot), vec![], None);
        assert_eq!(entry["name"], "Human title");
        assert_eq!(entry["project"]["checkout"]["currentBranch"], "saved");
        assert_eq!(entry["gitRuntime"]["currentBranch"], "live");
        assert_eq!(
            entry["githubRuntime"]["pullRequest"]["url"],
            "https://forge/pr/1"
        );
        assert_eq!(entry["forge"], "gitlab");
        assert_eq!(entry["worktreeSlug"], "worktree");
        assert_eq!(entry["projectDisplayName"], "Custom");
    }

    #[test]
    fn no_setup_commands_do_not_claim_completed_setup() {
        assert_eq!(
            setup_summary(
                &json!({}),
                Some(&json!({"status":"completed","error":null,"detail":{"commands":[]}}))
            ),
            Value::Null
        );
        assert_eq!(
            setup_summary(&json!({"untrustedSource":{"kind":"change_request"}}), None),
            "blocked"
        );
    }

    #[test]
    fn pagination_filters_background_and_carries_empty_projects_only_once() {
        let entries = vec![
            json!({"id":"b","name":"Beta","projectId":"p","activityAt":null}),
            json!({"id":"a","name":"Alpha","projectId":"p","activityAt":null}),
            json!({"id":"hidden","name":"Hidden","projectId":"q","background":true}),
        ];
        let projects = vec![json!({"projectId":"p"}), json!({"projectId":"q"})];
        let request = json!({"sort":[{"key":"name","direction":"asc"}],"page":{"limit":1}});
        let first = paginate(&request, entries.clone(), &projects).unwrap();
        assert_eq!(first["entries"][0]["id"], "a");
        assert_eq!(first["emptyProjects"][0]["projectId"], "q");
        let mut next = request.clone();
        next["page"]["cursor"] = first["pageInfo"]["nextCursor"].clone();
        let second = paginate(&next, entries.clone(), &projects).unwrap();
        assert_eq!(second["entries"][0]["id"], "b");
        assert_eq!(second["emptyProjects"], json!([]));
        assert_eq!(second["pageInfo"]["hasMore"], false);
        next["sort"][0]["direction"] = json!("desc");
        assert_eq!(
            paginate(&next, entries.clone(), &projects)
                .unwrap_err()
                .downcast_ref::<DirectoryRequestError>()
                .unwrap()
                .code,
            "invalid_cursor"
        );
        let exact = paginate(&json!({"filter":{"query":"hidden"}}), entries, &projects).unwrap();
        assert_eq!(exact["entries"][0]["id"], "hidden");
    }

    #[test]
    fn pagination_uses_locale_collation_and_null_activity_order() {
        let entries = vec![
            json!({"id":"z","name":"zebra","activityAt":null}),
            json!({"id":"e","name":"éclair","activityAt":"2026-01-01T00:00:00.000Z"}),
            json!({"id":"a","name":"apple","activityAt":null}),
        ];
        let by_name = paginate(
            &json!({"sort":[{"key":"name","direction":"asc"}]}),
            entries.clone(),
            &[],
        )
        .unwrap();
        let ids: Vec<_> = by_name["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| text(entry, "id"))
            .collect();
        assert_eq!(ids, vec!["a", "e", "z"]);
        let by_activity = paginate(&json!({}), entries, &[]).unwrap();
        assert_eq!(by_activity["entries"][0]["id"], "e");
        assert_eq!(by_activity["entries"][1]["id"], "a");
    }

    #[test]
    fn missing_checkout_keeps_persisted_identity_without_fake_runtime() {
        let workspace = json!({"workspaceId":"a","projectId":"p","kind":"worktree","cwd":"/missing",
            "displayName":"saved branch","branch":"saved","worktreeRoot":"/missing"});
        let project = json!({"projectId":"p","kind":"git","displayName":"repo","rootPath":"/main"});
        let entry = base_descriptor(&workspace, Some(&project), None, vec![], None);
        assert_eq!(entry["name"], "saved branch");
        assert_eq!(entry["project"]["checkout"]["isGit"], true);
        assert!(entry.get("gitRuntime").is_none());
        assert!(entry.get("githubRuntime").is_none());
        assert_eq!(entry["diffStat"], Value::Null);
    }
}
