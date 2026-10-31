use std::{
    collections::HashMap,
    path::{Component, Path, PathBuf},
};

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use super::{runtime, store};
use crate::state::AppState;

/// Startup-only compatibility migration. Normal runtime ownership must never
/// infer a workspace ID from an agent's cwd.
pub async fn bootstrap(app: &AppState) -> Result<()> {
    let storage = app.projects.storage().await?;
    let (projects_exist, workspaces_exist) = tokio::try_join!(
        storage.projects_exist_on_disk(),
        storage.workspaces_exist_on_disk(),
    )?;
    let workspaces = storage.list_workspaces().await;
    for workspace in &workspaces {
        if store::is_archived(workspace) || workspace["isPaseoOwnedWorktree"] != true {
            continue;
        }
        let Some(root) = nonempty(workspace, "worktreeRoot") else {
            continue;
        };
        let Some(branch) = nonempty(workspace, "branch") else {
            continue;
        };
        if let Err(error) =
            runtime::pin_worktree_branch_identity_if_missing(Path::new(root), branch).await
        {
            tracing::warn!(%error, workspace_id = workspace["workspaceId"].as_str().unwrap_or(""),
                "Failed to pin legacy worktree branch identity; PR association remains disabled");
        }
    }
    if projects_exist && workspaces_exist {
        backfill(app, &workspaces).await?;
        return Ok(());
    }

    let mut existing_ids = HashMap::with_capacity(workspaces.len());
    for workspace in &workspaces {
        existing_ids.insert(
            resolve(required(workspace, "cwd")?)?,
            required(workspace, "workspaceId")?.to_owned(),
        );
    }
    let identity = app.transport.identity();
    let server_id = identity["serverId"].as_str();
    let records = app.agents.list_persisted_records().await?;
    let mut group_indices: HashMap<String, usize> = HashMap::new();
    let mut groups: Vec<Group> = Vec::new();
    for record in &records {
        if store::is_archived(record) {
            continue;
        }
        let cwd = resolve(required(record, "cwd")?)?;
        // Missing/non-directory agent roots are absence, not a Git observation.
        if !tokio::fs::metadata(&cwd)
            .await
            .is_ok_and(|meta| meta.is_dir())
        {
            continue;
        }
        let checkout = app.projects.checkout(&cwd).await?;
        let membership = classify(&cwd, checkout, server_id).await?;
        let created_at = agent_created_at(record);
        let updated_at = agent_updated_at(record);
        if let Some(index) = group_indices.get(&membership.directory_key).copied() {
            let group = &mut groups[index];
            group.created_at = min_date(&group.created_at, created_at).to_owned();
            group.updated_at = max_date(&group.updated_at, updated_at).to_owned();
        } else {
            group_indices.insert(membership.directory_key.clone(), groups.len());
            groups.push(Group {
                membership,
                created_at: created_at.to_owned(),
                updated_at: updated_at.to_owned(),
            });
        }
    }
    let mut ranges: HashMap<&str, (String, String)> = HashMap::new();
    for group in &groups {
        match ranges.get_mut(group.membership.project_key.as_str()) {
            Some((created, updated)) => {
                *created = min_date(created, &group.created_at).to_owned();
                *updated = max_date(updated, &group.updated_at).to_owned();
            }
            None => {
                ranges.insert(
                    &group.membership.project_key,
                    (group.created_at.clone(), group.updated_at.clone()),
                );
            }
        }
    }
    for group in &groups {
        let membership = &group.membership;
        let workspace_id = existing_ids
            .get(Path::new(&membership.cwd))
            .cloned()
            .unwrap_or_else(store::generate_workspace_id);
        let (created, updated) = &ranges[membership.project_key.as_str()];
        // The legacy bootstrap deliberately materializes its historical minimal
        // records; live placement reconciliation owns subsequent observations.
        storage
            .upsert_workspace(store::create_workspace(json!({
                "workspaceId":workspace_id,"projectId":membership.project_id,"cwd":membership.cwd,
                "kind":membership.workspace_kind,"displayName":membership.workspace_name,
                "createdAt":group.created_at,"updatedAt":group.updated_at,
            }))?)
            .await?;
        storage.upsert_project(store::create_project(json!({
            "projectId":membership.project_id,"rootPath":membership.project_root,"kind":membership.project_kind,
            "displayName":membership.project_name,"projectKey":membership.project_key,
            "createdAt":created,"updatedAt":updated,
        }))?).await?;
    }
    backfill(app, &storage.list_workspaces().await).await?;
    tracing::info!(
        materialized_projects = ranges.len(),
        materialized_workspaces = groups.len(),
        "Workspace registries bootstrapped from existing agent storage"
    );
    Ok(())
}

struct Group {
    membership: Membership,
    created_at: String,
    updated_at: String,
}
struct Membership {
    cwd: String,
    directory_key: String,
    workspace_kind: &'static str,
    workspace_name: String,
    project_id: String,
    project_key: String,
    project_name: String,
    project_root: String,
    project_kind: &'static str,
}
async fn classify(cwd: &Path, mut checkout: Value, server_id: Option<&str>) -> Result<Membership> {
    let cwd = cwd.to_string_lossy().into_owned();
    checkout["cwd"] = json!(cwd);
    let remote = checkout["remoteUrl"].as_str();
    let worktree_root = checkout["worktreeRoot"].as_str();
    let main_root = nonempty(&checkout, "mainRepoRoot");
    let project_key =
        store::derive_project_key(&cwd, remote, worktree_root, main_root, server_id).await?;
    let project_root = if checkout["isGit"] == true {
        if let Some(main) = main_root {
            match worktree_root.and_then(parse_git_path) {
                Some(root) => match store::relative_inside(root, &cwd)
                    .await
                    .filter(|p| !p.is_empty())
                {
                    Some(relative) => resolve(&Path::new(main).join(relative).to_string_lossy())?
                        .to_string_lossy()
                        .into_owned(),
                    None => main.to_owned(),
                },
                None => main.to_owned(),
            }
        } else {
            cwd.clone()
        }
    } else {
        cwd.clone()
    };
    let directory_key = match worktree_root.and_then(parse_git_path) {
        Some(root) if store::relative_inside(root, &cwd).await.as_deref() == Some("") => {
            root.to_owned()
        }
        _ => cwd.clone(),
    };
    let project_name =
        store::derive_project_grouping_display_name(&cwd, remote, worktree_root).await;
    Ok(Membership {
        workspace_kind: store::derive_workspace_kind(&checkout),
        workspace_name: store::derive_workspace_display_name(&cwd, &checkout),
        project_kind: store::derive_project_kind(&checkout),
        project_id: if project_key.starts_with("remote:") {
            project_key.clone()
        } else {
            project_root.clone()
        },
        cwd,
        directory_key,
        project_key,
        project_name,
        project_root,
    })
}

async fn backfill(app: &AppState, workspaces: &[Value]) -> Result<()> {
    let records = app.agents.list_persisted_records().await?;
    let user_home = resolve(&app.user_home.to_string_lossy())?;
    let owners = owners(workspaces)?;
    let mut migrated = 0;
    for record in records {
        if nonempty(&record, "workspaceId").is_some() {
            continue;
        }
        let cwd = resolve(required(&record, "cwd")?)?;
        let include_archived = record.get("archivedAt").is_some_and(|v| !v.is_null());
        if let Some(id) = legacy_owner(&cwd, &user_home, &owners, include_archived) {
            if app
                .agents
                .backfill_workspace_id(required(&record, "id")?, id)
                .await?
            {
                migrated += 1;
            }
        }
    }
    if migrated > 0 {
        tracing::info!(migrated, "Backfilled workspaceId for legacy agent records");
    }
    Ok(())
}
struct Owner<'a> {
    id: &'a str,
    cwd: PathBuf,
    created_at: &'a str,
    archived: bool,
}
fn owners(workspaces: &[Value]) -> Result<Vec<Owner<'_>>> {
    workspaces
        .iter()
        .map(|workspace| {
            Ok(Owner {
                id: required(workspace, "workspaceId")?,
                cwd: resolve(required(workspace, "cwd")?)?,
                created_at: required(workspace, "createdAt")?,
                archived: store::is_archived(workspace),
            })
        })
        .collect()
}
fn legacy_owner<'a>(
    cwd: &Path,
    home: &Path,
    workspaces: &'a [Owner<'a>],
    include_archived: bool,
) -> Option<&'a str> {
    // Equal-time ties preserve registry order, as Array.reduce does in source.
    let mut exact: Option<&Owner<'_>> = None;
    let mut enclosing: Option<(&Owner<'_>, usize)> = None;
    for workspace in workspaces
        .iter()
        .filter(|w| include_archived || !w.archived)
    {
        if workspace.cwd == cwd {
            if exact.is_none_or(|old| workspace.created_at < old.created_at) {
                exact = Some(workspace);
            }
            continue;
        }
        if workspace.cwd == home {
            continue;
        }
        // Source tests `${cwd}${sep}` rather than Path::starts_with: notably a
        // filesystem root does not acquire all descendants through this rule.
        let prefix = format!(
            "{}{}",
            workspace.cwd.to_string_lossy(),
            std::path::MAIN_SEPARATOR
        );
        if !cwd.to_string_lossy().starts_with(&prefix) {
            continue;
        }
        let length = workspace.cwd.to_string_lossy().encode_utf16().count();
        if enclosing.is_none_or(|(old, old_length)| {
            length > old_length || (length == old_length && workspace.created_at < old.created_at)
        }) {
            enclosing = Some((workspace, length));
        }
    }
    exact.map(|w| w.id).or_else(|| enclosing.map(|(w, _)| w.id))
}
fn required<'a>(record: &'a Value, key: &str) -> Result<&'a str> {
    record[key]
        .as_str()
        .ok_or_else(|| anyhow!("Legacy registry bootstrap: missing {key}"))
}
fn nonempty<'a>(record: &'a Value, key: &str) -> Option<&'a str> {
    record[key].as_str().filter(|s| !s.is_empty())
}
fn resolve(value: &str) -> Result<PathBuf> {
    let path = Path::new(value);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            part => normalized.push(part.as_os_str()),
        }
    }
    Ok(normalized)
}
fn parse_git_path(output: &str) -> Option<&str> {
    let path = output
        .strip_suffix("\r\n")
        .or_else(|| output.strip_suffix('\n'))
        .unwrap_or(output);
    (!path.is_empty() && !path.contains(['\r', '\n']) && !path.starts_with("--")).then_some(path)
}
fn agent_created_at(record: &Value) -> &str {
    nonempty(record, "createdAt")
        .or_else(|| nonempty(record, "updatedAt"))
        .unwrap_or("1970-01-01T00:00:00.000Z")
}
fn agent_updated_at(record: &Value) -> &str {
    nonempty(record, "lastActivityAt")
        .or_else(|| nonempty(record, "updatedAt"))
        .or_else(|| nonempty(record, "createdAt"))
        .unwrap_or("1970-01-01T00:00:00.000Z")
}
fn date_ms(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|d| d.timestamp_millis())
}
fn min_date<'a>(left: &'a str, right: &'a str) -> &'a str {
    if matches!((date_ms(left), date_ms(right)), (Some(a),Some(b)) if a <= b) {
        left
    } else {
        right
    }
}
fn max_date<'a>(left: &'a str, right: &'a str) -> &'a str {
    if matches!((date_ms(left), date_ms(right)), (Some(a),Some(b)) if a >= b) {
        left
    } else {
        right
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn workspace(id: &str, cwd: &str, created: &str, archived: bool) -> Value {
        json!({"workspaceId":id,"cwd":cwd,"createdAt":created,"archivedAt":if archived { json!("2026-01-01") } else { Value::Null }})
    }
    #[test]
    fn legacy_owner_prefers_exact_then_deepest_then_oldest_and_excludes_home() -> Result<()> {
        let records = vec![
            workspace("home", "/home/user", "0", false),
            workspace("parent", "/home/user/repo", "1", false),
            workspace("deep-new", "/home/user/repo/src", "3", false),
            workspace("deep-old", "/home/user/repo/src", "2", false),
        ];
        let owners = owners(&records)?;
        let home = Path::new("/home/user");
        assert_eq!(
            legacy_owner(Path::new("/home/user/elsewhere"), home, &owners, false),
            None
        );
        assert_eq!(legacy_owner(home, home, &owners, false), Some("home"));
        assert_eq!(
            legacy_owner(Path::new("/home/user/repo/src"), home, &owners, false),
            Some("deep-old")
        );
        assert_eq!(
            legacy_owner(
                Path::new("/home/user/repo/src/nested"),
                home,
                &owners,
                false
            ),
            Some("deep-old")
        );
        assert_eq!(
            legacy_owner(Path::new("/home/user/repository"), home, &owners, false),
            None
        );
        Ok(())
    }
    #[test]
    fn archived_agents_may_retain_archived_workspace_ownership() -> Result<()> {
        let records = vec![
            workspace("live", "/repo", "2", false),
            workspace("archived", "/repo", "1", true),
        ];
        let owners = owners(&records)?;
        assert_eq!(
            legacy_owner(Path::new("/repo"), Path::new("/home"), &owners, false),
            Some("live")
        );
        assert_eq!(
            legacy_owner(Path::new("/repo"), Path::new("/home"), &owners, true),
            Some("archived")
        );
        Ok(())
    }
    #[tokio::test]
    async fn legacy_remote_subdirectory_identity_preserves_historical_project_id() -> Result<()> {
        let membership = classify(Path::new("/worktree/packages/app"), json!({"isGit":true,
            "worktreeRoot":"/worktree", "mainRepoRoot":"/main", "remoteUrl":"git@github.com:Owner/Repo.git",
            "currentBranch":"feature", "isPaseoOwnedWorktree":true}), Some("server")).await?;
        assert_eq!(
            membership.project_id,
            "remote:github.com/owner/repo#subdir:packages/app"
        );
        assert_eq!(membership.project_key, membership.project_id);
        assert_eq!(membership.project_root, "/main/packages/app");
        assert_eq!(membership.directory_key, "/worktree/packages/app");
        assert_eq!(membership.workspace_kind, "worktree");
        assert_eq!(membership.project_name, "app");
        Ok(())
    }
}
