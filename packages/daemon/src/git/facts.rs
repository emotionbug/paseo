use super::command::*;
use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use tokio_util::sync::CancellationToken;

pub fn non_git(cwd: &Path) -> Value {
    json!({"cwd":cwd,"isGit":false,"repoRoot":null,"mainRepoRoot":null,"diffStat":null,"currentBranch":null,"isDirty":null,"baseRef":null,"aheadBehind":null,"upstreamRef":null,"aheadOfOrigin":null,"behindOfOrigin":null,"hasRemote":false,"remoteUrl":null,"isPaseoOwnedWorktree":false,"error":null})
}
pub async fn current_branch(
    cwd: &Path,
    env: &BTreeMap<String, String>,
    cancel: CancellationToken,
) -> Result<Option<String>> {
    if let Some(branch) = optional(
        cwd,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
        env,
        cancel.clone(),
    )
    .await?
    {
        return Ok(Some(branch));
    }
    let dir = git_dir(cwd, env, cancel).await?;
    for name in ["rebase-merge/head-name", "rebase-apply/head-name"] {
        match tokio::fs::read_to_string(dir.join(name)).await {
            Ok(s) => return Ok(Some(s.trim().trim_start_matches("refs/heads/").into())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(None)
}
pub async fn divergence(
    cwd: &Path,
    left: &str,
    right: &str,
    env: &BTreeMap<String, String>,
    cancel: CancellationToken,
) -> Result<Option<(u64, u64)>> {
    let Some(counts) = optional(
        cwd,
        &[
            "rev-list",
            "--left-right",
            "--count",
            &format!("{left}...{right}"),
        ],
        env,
        cancel,
    )
    .await?
    else {
        return Ok(None);
    };
    let mut values = counts.split_whitespace();
    Ok(Some((
        values
            .next()
            .ok_or_else(|| anyhow!("Missing ahead count"))?
            .parse()?,
        values
            .next()
            .ok_or_else(|| anyhow!("Missing behind count"))?
            .parse()?,
    )))
}
pub async fn snapshot(
    cwd: &Path,
    home: &Path,
    config: &Value,
    env: &BTreeMap<String, String>,
    cancel: CancellationToken,
) -> Result<Value> {
    let output = run(
        cwd,
        &["rev-parse".into(), "--show-toplevel".into()],
        env,
        cancel.clone(),
        READ_LIMIT,
        READ_TIMEOUT,
    )
    .await?;
    if output.status != 0 {
        if output
            .stderr
            .to_lowercase()
            .contains("not a git repository")
        {
            return Ok(non_git(cwd));
        }
        return Err(anyhow!("{}", output.stderr.trim()));
    }
    let root = PathBuf::from(String::from_utf8(output.stdout)?.trim());
    let current = current_branch(&root, env, cancel.clone()).await?;
    let remote = optional(
        &root,
        &["config", "--get", "remote.origin.url"],
        env,
        cancel.clone(),
    )
    .await?;
    let dirty = !checked(
        &root,
        &["status".into(), "--porcelain=v1".into(), "-z".into()],
        env,
        cancel.clone(),
        READ_LIMIT,
        READ_TIMEOUT,
    )
    .await?
    .is_empty();
    let base = operation_base(&root, None, env, cancel.clone()).await?;
    let upstream = optional(
        &root,
        &["rev-parse", "--symbolic-full-name", "@{upstream}"],
        env,
        cancel.clone(),
    )
    .await?;
    let upstream_counts = if let Some(u) = &upstream {
        divergence(&root, "HEAD", u, env, cancel.clone()).await?
    } else {
        None
    };
    let mut ahead_behind = Value::Null;
    if let (Some(base), Some(branch)) = (&base, &current) {
        if branch_name(base) != branch {
            let comparison = if base.starts_with("refs/") {
                Some(base.clone())
            } else {
                let remote = format!("refs/remotes/origin/{}", branch_name(base));
                if exists(&root, &remote, env, cancel.clone()).await? {
                    Some(remote)
                } else {
                    Some(base.clone())
                }
            };
            if let Some(comparison) = comparison {
                if let Some((ahead, behind)) =
                    divergence(&root, "HEAD", &comparison, env, cancel.clone()).await?
                {
                    ahead_behind = json!({"ahead":ahead,"behind":behind});
                }
            }
        }
    }
    let main = crate::projects::worktrees::repo_root(&root, cancel.clone()).await?;
    let ownership = crate::projects::worktrees::ownership(home, config, &root, cancel).await?;
    let owned = ownership["allowed"].as_bool().unwrap_or(false) && base.is_some();
    let display_base = base.as_deref().map(branch_name);
    Ok(
        json!({"cwd":cwd,"isGit":true,"repoRoot":root,"mainRepoRoot":main,"currentBranch":current,"isDirty":dirty,"baseRef":display_base,"aheadBehind":ahead_behind,"upstreamRef":upstream,"aheadOfOrigin":upstream_counts.map(|c|c.0),"behindOfOrigin":upstream_counts.map(|c|c.1),"hasRemote":remote.is_some(),"remoteUrl":remote,"isPaseoOwnedWorktree":owned,"error":null}),
    )
}
/// The source's `normalizeBranchSuggestionName`: a short branch name, or None for
/// blanks and the `HEAD`/`origin` pseudo names.
pub fn normalize_branch(raw: &str) -> Option<&str> {
    let trimmed = raw.trim();
    let name = trimmed
        .strip_prefix("refs/heads/")
        .or_else(|| trimmed.strip_prefix("refs/remotes/"))
        .unwrap_or(trimmed);
    let name = name.strip_prefix("origin/").unwrap_or(name);
    (!name.is_empty() && name != "HEAD" && name != "origin").then_some(name)
}
/// The source's `resolveBranchCheckout`.
pub async fn resolve_branch(
    cwd: &Path,
    name: &str,
    env: &BTreeMap<String, String>,
    cancel: CancellationToken,
) -> Result<Value> {
    safe_ref(name)?;
    require_repo(cwd, env, cancel.clone()).await?;
    let Some(name) = normalize_branch(name) else {
        return Ok(json!({"kind":"not-found"}));
    };
    if exists(cwd, &format!("refs/heads/{name}"), env, cancel.clone()).await? {
        return Ok(json!({"kind":"local","name":name}));
    }
    if exists(cwd, &format!("refs/remotes/origin/{name}"), env, cancel).await? {
        return Ok(json!({"kind":"remote-only","name":name,"remoteRef":format!("origin/{name}")}));
    }
    Ok(json!({"kind":"not-found"}))
}
/// The source's `listBranchSuggestions`.
pub async fn branches(
    cwd: &Path,
    query: &str,
    limit: usize,
    env: &BTreeMap<String, String>,
    cancel: CancellationToken,
) -> Result<Vec<Value>> {
    require_repo(cwd, env, cancel.clone()).await?;
    let refs = text(
        cwd,
        &[
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(refname)%09%(committerdate:unix)%09%(objectname)",
            "refs/heads",
            "refs/remotes/origin",
        ],
        env,
        cancel.clone(),
    )
    .await?;
    let mut entries: BTreeMap<String, Value> = BTreeMap::new();
    for row in refs.lines() {
        let fields: Vec<_> = row.split('\t').collect();
        if fields.len() != 3 {
            continue;
        }
        let Some(name) = normalize_branch(fields[0]) else {
            continue;
        };
        let local = fields[0].starts_with("refs/heads/");
        let entry = entries.entry(name.into()).or_insert_with(
            || json!({"name":name,"committerDate":0,"hasLocal":false,"hasRemote":false}),
        );
        entry[if local { "hasLocal" } else { "hasRemote" }] = json!(true);
        entry["committerDate"] = json!(
            entry["committerDate"]
                .as_i64()
                .unwrap_or(0)
                .max(fields[1].parse().unwrap_or(0))
        );
    }
    let raw_query = query.trim().to_lowercase();
    let query = normalize_branch(&raw_query)
        .map(str::to_owned)
        .unwrap_or_else(|| raw_query.clone());
    let mut values: Vec<_> = entries
        .into_values()
        .filter(|v| {
            v["name"]
                .as_str()
                .unwrap_or("")
                .to_lowercase()
                .contains(&query)
        })
        .collect();
    values.sort_by(|a, b| {
        let an = a["name"].as_str().unwrap_or("");
        let bn = b["name"].as_str().unwrap_or("");
        bn.to_lowercase()
            .starts_with(&query)
            .cmp(&an.to_lowercase().starts_with(&query))
            .then_with(|| {
                b["committerDate"]
                    .as_i64()
                    .cmp(&a["committerDate"].as_i64())
            })
            .then_with(|| crate::json::locale_compare(an, bn))
    });
    values.truncate(limit.clamp(1, 200));
    for value in &mut values {
        if value["hasLocal"] == true && value["hasRemote"] == true {
            let name = value["name"].as_str().unwrap_or("");
            // A ref may vanish between listing and comparison; keep the branch
            // without divergence metadata, like the source.
            if let Some((a, b)) = divergence(
                cwd,
                &format!("refs/heads/{name}"),
                &format!("refs/remotes/origin/{name}"),
                env,
                cancel.clone(),
            )
            .await
            .ok()
            .flatten()
            {
                value["localAhead"] = json!(a);
                value["localBehind"] = json!(b);
            }
        }
    }
    Ok(values)
}
pub async fn stashes(
    cwd: &Path,
    paseo_only: bool,
    env: &BTreeMap<String, String>,
    cancel: CancellationToken,
) -> Result<Vec<Value>> {
    let out = text(cwd, &["stash", "list", "--format=%gd%x00%gs"], env, cancel).await?;
    Ok(out
        .lines()
        .filter_map(|line| {
            let (reference, subject) = line.split_once('\0')?;
            let branch = subject
                .split_once("paseo-auto-stash:")
                .map(|(_, s)| s.trim())
                .filter(|s| !s.is_empty());
            let is_paseo = subject.contains("paseo-auto-stash:");
            if paseo_only && !is_paseo {
                return None;
            }
            let index = reference
                .strip_prefix("stash@{")?
                .strip_suffix('}')?
                .parse::<u64>()
                .ok()?;
            Some(json!({"index":index,"message":subject,"branch":branch,"isPaseo":is_paseo}))
        })
        .collect())
}
