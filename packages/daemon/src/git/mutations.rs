use super::{command::*, facts};
use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use tokio_util::sync::CancellationToken;

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct MergeConflict {
    pub message: String,
    pub files: Vec<String>,
}
pub async fn execute(
    cwd: &Path,
    args: &[&str],
    env: &BTreeMap<String, String>,
    cancel: CancellationToken,
) -> Result<()> {
    checked(
        cwd,
        &args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        env,
        cancel,
        READ_LIMIT,
        MUTATION_TIMEOUT,
    )
    .await?;
    Ok(())
}
pub async fn clean(
    cwd: &Path,
    env: &BTreeMap<String, String>,
    cancel: CancellationToken,
) -> Result<()> {
    if !text(cwd, &["status", "--porcelain"], env, cancel)
        .await?
        .is_empty()
    {
        bail!("Working directory has uncommitted changes.");
    }
    Ok(())
}
pub async fn checkout(
    cwd: &Path,
    branch: &str,
    env: &BTreeMap<String, String>,
    cancel: CancellationToken,
) -> Result<Value> {
    request_ref(branch, "branch")?;
    let resolution = facts::resolve_branch(cwd, branch, env, cancel.clone()).await?;
    let name = resolution["name"].as_str().unwrap_or(branch);
    if resolution["kind"] == "not-found" {
        bail!("Branch not found: {branch}");
    }
    if !text(cwd, &["status", "--porcelain"], env, cancel.clone())
        .await?
        .is_empty()
    {
        bail!(
            "Working directory has uncommitted changes. Commit or stash before switching branches."
        );
    }
    if facts::current_branch(cwd, env, cancel.clone())
        .await?
        .as_deref()
        == Some(name)
    {
        return Ok(json!({"source":"local","branch":branch}));
    }
    match resolution["kind"].as_str() {
        Some("local") => execute(cwd, &["checkout", name], env, cancel).await?,
        Some("remote-only") => {
            execute(
                cwd,
                &[
                    "checkout",
                    "-b",
                    name,
                    "--track",
                    resolution["remoteRef"]
                        .as_str()
                        .ok_or_else(|| anyhow!("Missing remote ref"))?,
                ],
                env,
                cancel,
            )
            .await?
        }
        _ => bail!("Branch not found: {branch}"),
    }
    Ok(json!({"source":if resolution["kind"]=="local"{"local"}else{"remote"},"branch":branch}))
}
pub async fn stage(
    cwd: &Path,
    paths: &[String],
    env: &BTreeMap<String, String>,
    cancel: CancellationToken,
) -> Result<()> {
    for path in paths {
        safe_path(path)?;
    }
    if paths.is_empty() {
        return Ok(());
    }
    let mut args = vec!["--literal-pathspecs".into(), "add".into(), "--".into()];
    args.extend_from_slice(paths);
    checked(cwd, &args, env, cancel, READ_LIMIT, MUTATION_TIMEOUT).await?;
    Ok(())
}
pub async fn unstage(
    cwd: &Path,
    paths: &[String],
    env: &BTreeMap<String, String>,
    cancel: CancellationToken,
) -> Result<()> {
    for path in paths {
        safe_path(path)?;
    }
    if paths.is_empty() {
        return Ok(());
    }
    let head = optional(cwd, &["rev-parse", "--verify", "HEAD"], env, cancel.clone()).await?;
    let mut args: Vec<String> = if head.is_some() {
        vec!["--literal-pathspecs", "reset", "-q", "HEAD", "--"]
    } else {
        vec![
            "--literal-pathspecs",
            "rm",
            "--cached",
            "-r",
            "-q",
            "--ignore-unmatch",
            "--",
        ]
    }
    .into_iter()
    .map(str::to_owned)
    .collect();
    args.extend_from_slice(paths);
    checked(cwd, &args, env, cancel, READ_LIMIT, MUTATION_TIMEOUT).await?;
    Ok(())
}
pub async fn discard(
    cwd: &Path,
    paths: &[String],
    env: &BTreeMap<String, String>,
    cancel: CancellationToken,
) -> Result<()> {
    let root =
        PathBuf::from(text(cwd, &["rev-parse", "--show-toplevel"], env, cancel.clone()).await?);
    unstage(&root, paths, env, cancel.clone()).await?;
    if paths.is_empty() {
        return Ok(());
    }
    let mut args = vec![
        "--literal-pathspecs".into(),
        "status".into(),
        "--porcelain=v1".into(),
        "-z".into(),
        "--".into(),
    ];
    args.extend_from_slice(paths);
    let out = checked(
        &root,
        &args,
        env,
        cancel.clone(),
        READ_LIMIT,
        MUTATION_TIMEOUT,
    )
    .await?;
    let mut tokens = out.split(|b| *b == 0);
    let (mut tracked, mut untracked) = (Vec::new(), Vec::new());
    while let Some(token) = tokens.next() {
        if token.len() < 4 {
            continue;
        }
        let name = String::from_utf8(token[3..].to_vec())?;
        if token[0] == b'R' || token[0] == b'C' {
            tokens.next();
        }
        if &token[..2] == b"??" {
            untracked.push(name);
        } else {
            tracked.push(name);
        }
    }
    for (names, prefix) in [
        (tracked, vec!["--literal-pathspecs", "checkout", "-q", "--"]),
        (
            untracked,
            vec!["--literal-pathspecs", "clean", "-fd", "-q", "--"],
        ),
    ] {
        if names.is_empty() {
            continue;
        }
        let mut args: Vec<String> = prefix.into_iter().map(str::to_owned).collect();
        args.extend(names);
        checked(
            &root,
            &args,
            env,
            cancel.clone(),
            READ_LIMIT,
            MUTATION_TIMEOUT,
        )
        .await?;
    }
    Ok(())
}
pub async fn commit(
    cwd: &Path,
    message: &str,
    add_all: bool,
    env: &BTreeMap<String, String>,
    cancel: CancellationToken,
) -> Result<()> {
    if message.trim().is_empty() {
        bail!("Commit message is required");
    }
    require_repo(cwd, env, cancel.clone()).await?;
    if add_all {
        execute(cwd, &["add", "-A"], env, cancel.clone()).await?;
    }
    execute(cwd, &["commit", "-m", message], env, cancel).await
}
pub async fn rename(
    cwd: &Path,
    name: &str,
    env: &BTreeMap<String, String>,
    cancel: CancellationToken,
) -> Result<Value> {
    if name.is_empty() {
        bail!("Branch name cannot be empty");
    }
    if name.encode_utf16().count() > 100 {
        bail!("Branch name too long (max 100 characters)");
    }
    if !name
        .bytes()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'-' | b'/'))
    {
        bail!(
            "Branch name must contain only lowercase letters, numbers, hyphens, and forward slashes"
        );
    }
    if name.starts_with('-') || name.ends_with('-') {
        bail!("Branch name cannot start or end with a hyphen");
    }
    if name.contains("--") {
        bail!("Branch name cannot have consecutive hyphens");
    }
    // The source validates the slug only; `git branch -m` reports any other refusal.
    require_worktree_root(cwd, env, cancel.clone()).await?;
    let old = facts::current_branch(cwd, env, cancel.clone())
        .await?
        .ok_or_else(|| anyhow!("Cannot rename branch in detached HEAD state"))?;
    execute(cwd, &["branch", "-m", name], env, cancel).await?;
    Ok(json!({"previousBranch":old,"currentBranch":name}))
}
/// The source's merge conflict detection: unmerged paths from three git views, or
/// conflict text in the failure. A failing detection command keeps the original error.
async fn merge_error(
    cwd: &Path,
    error: anyhow::Error,
    base: &str,
    current: &str,
    from: bool,
    env: &BTreeMap<String, String>,
    cancel: CancellationToken,
) -> anyhow::Error {
    let details = match error.downcast_ref::<GitCommandFailed>() {
        Some(failed) => format!("{}\n{}\n{}", failed.message, failed.stderr, failed.stdout),
        None => error.to_string(),
    };
    let mut outputs = Vec::with_capacity(3);
    for args in [
        &["diff", "--name-only", "--diff-filter=U"][..],
        &["ls-files", "-u"],
        &["status", "--porcelain"],
    ] {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        match run(cwd, &args, env, cancel.clone(), READ_LIMIT, READ_TIMEOUT).await {
            Ok(output) if output.status == 0 => {
                outputs.push(String::from_utf8_lossy(&output.stdout).into_owned())
            }
            _ => return error,
        }
    }
    let lines = |text: &str| -> Vec<String> {
        text.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect()
    };
    let mut files = lines(&outputs[0]);
    files.extend(
        lines(&outputs[1])
            .iter()
            .map(|line| line.rsplit('\t').next().unwrap_or("").to_owned()),
    );
    files.extend(lines(&outputs[2]).iter().filter_map(|line| {
        let conflicted = ["UU", "AA", "DD", "AU", "UA", "UD", "DU"]
            .iter()
            .any(|code| line.starts_with(code))
            && line.get(2..).is_some_and(|rest| rest.starts_with(char::is_whitespace));
        conflicted.then(|| line.get(3..).unwrap_or("").trim().to_owned())
    }));
    files.retain(|file| !file.is_empty());
    let lower = details.to_lowercase();
    if !files.is_empty() || lower.contains("conflict") || lower.contains("automatic merge failed")
    {
        let _ = execute(cwd, &["merge", "--abort"], env, cancel).await;
        return MergeConflict {
            message: if from {
                format!(
                    "Merge conflict while merging {base} into {current}. Please merge manually."
                )
            } else {
                format!("Merge conflict while merging {current} into {base}")
            },
            files,
        }
        .into();
    }
    error
}
pub async fn merge_to_base(
    cwd: &Path,
    requested: Option<&str>,
    squash: bool,
    env: &BTreeMap<String, String>,
    cancel: CancellationToken,
) -> Result<PathBuf> {
    require_repo(cwd, env, cancel.clone()).await?;
    let base = operation_base(cwd, requested, env, cancel.clone())
        .await?
        .ok_or_else(|| anyhow!("Unable to determine base branch for merge"))?;
    if base.starts_with("refs/remotes/") && !base.starts_with("refs/remotes/origin/") {
        bail!("No local merge target is recorded for base ref {base}");
    }
    let target = branch_name(&base);
    let current = facts::current_branch(cwd, env, cancel.clone())
        .await?
        .ok_or_else(|| anyhow!("Unable to determine current branch for merge"))?;
    let root =
        PathBuf::from(text(cwd, &["rev-parse", "--show-toplevel"], env, cancel.clone()).await?);
    if target == current {
        return Ok(root);
    }
    let reference = format!("refs/heads/{target}");
    // `getWorktreePathForBranch`: any lookup failure means "no other worktree".
    let worktree = text(
        cwd,
        &[
            "for-each-ref",
            "--format=%(refname)%00%(worktreepath)%00",
            &reference,
        ],
        env,
        cancel.clone(),
    )
    .await
    .unwrap_or_default();
    let operation = worktree
        .split("\0\n")
        .filter_map(|r| r.split_once('\0'))
        .find(|(r, _)| *r == reference)
        .and_then(|(_, p)| {
            let p = p.trim_end_matches('\0');
            (!p.is_empty()).then(|| PathBuf::from(p))
        })
        .unwrap_or_else(|| root.clone());
    let original = facts::current_branch(&operation, env, cancel.clone()).await?;
    let result: Result<()> = async {
        execute(&operation, &["checkout", target], env, cancel.clone()).await?;
        if squash {
            execute(
                &operation,
                &["merge", "--squash", &current],
                env,
                cancel.clone(),
            )
            .await?;
            execute(
                &operation,
                &[
                    "commit",
                    "-m",
                    &format!("Squash merge {current} into {target}"),
                ],
                env,
                cancel.clone(),
            )
            .await?;
        } else {
            execute(&operation, &["merge", &current], env, cancel.clone()).await?;
        }
        Ok(())
    }
    .await;
    let result = match result {
        Ok(()) => Ok(()),
        Err(error) => Err(merge_error(
            &operation,
            error,
            target,
            &current,
            false,
            env,
            cancel.clone(),
        )
        .await),
    };
    if operation == root {
        if let Some(original) = original.filter(|b| b != target) {
            let _ = execute(&operation, &["checkout", &original], env, cancel.clone()).await;
        }
    }
    result?;
    Ok(operation)
}
pub async fn merge_from_base(
    cwd: &Path,
    requested: Option<&str>,
    require_clean: bool,
    env: &BTreeMap<String, String>,
    cancel: CancellationToken,
) -> Result<()> {
    require_repo(cwd, env, cancel.clone()).await?;
    let current = facts::current_branch(cwd, env, cancel.clone())
        .await?
        .ok_or_else(|| anyhow!("Unable to determine current branch for merge"))?;
    let base = operation_base(cwd, requested, env, cancel.clone())
        .await?
        .ok_or_else(|| anyhow!("Unable to determine base branch for merge"))?;
    if require_clean {
        clean(cwd, env, cancel.clone()).await?;
    }
    let base = most_ahead_base(cwd, &base, env, cancel.clone()).await?;
    if base == current {
        return Ok(());
    }
    match execute(cwd, &["merge", &base], env, cancel.clone()).await {
        Ok(()) => Ok(()),
        Err(error) => Err(merge_error(cwd, error, &base, &current, true, env, cancel).await),
    }
}
pub async fn pull(
    cwd: &Path,
    env: &BTreeMap<String, String>,
    cancel: CancellationToken,
) -> Result<()> {
    require_repo(cwd, env, cancel.clone()).await?;
    if facts::current_branch(cwd, env, cancel.clone())
        .await?
        .is_none()
    {
        bail!("Unable to determine current branch for pull");
    }
    if optional(
        cwd,
        &["config", "--get", "remote.origin.url"],
        env,
        cancel.clone(),
    )
    .await?
    .is_none()
    {
        bail!("Remote 'origin' is not configured.");
    }
    let result = execute(cwd, &["pull"], env, cancel.clone()).await;
    if result.is_err() {
        let Ok(dir) = git_dir(cwd, env, cancel.clone()).await else {
            return result;
        };
        for (marker, operation) in [
            ("MERGE_HEAD", "merge"),
            ("rebase-merge", "rebase"),
            ("rebase-apply", "rebase"),
        ] {
            if tokio::fs::try_exists(dir.join(marker)).await? {
                let _ = execute(cwd, &[operation, "--abort"], env, cancel.clone()).await;
            }
        }
    }
    result
}
pub async fn push(
    cwd: &Path,
    env: &BTreeMap<String, String>,
    cancel: CancellationToken,
) -> Result<()> {
    require_repo(cwd, env, cancel.clone()).await?;
    let branch = facts::current_branch(cwd, env, cancel.clone())
        .await?
        .ok_or_else(|| anyhow!("Unable to determine current branch for push"))?;
    let push_remote = optional(
        cwd,
        &["config", "--get", &format!("branch.{branch}.pushRemote")],
        env,
        cancel.clone(),
    )
    .await?;
    let upstream_remote = optional(
        cwd,
        &["config", "--get", &format!("branch.{branch}.remote")],
        env,
        cancel.clone(),
    )
    .await?;
    let upstream_merge = optional(
        cwd,
        &["config", "--get", &format!("branch.{branch}.merge")],
        env,
        cancel.clone(),
    )
    .await?;
    if let Some(remote) = push_remote {
        let configured = optional(
            cwd,
            &["config", "--get", &format!("remote.{remote}.url")],
            env,
            cancel.clone(),
        )
        .await?
        .is_some();
        let spec = optional(
            cwd,
            &["config", "--get", &format!("remote.{remote}.push")],
            env,
            cancel.clone(),
        )
        .await?;
        if let Some(head) = spec
            .as_deref()
            .filter(|_| configured)
            .and_then(|s| s.trim_start_matches('+').strip_prefix("HEAD:refs/heads/"))
        {
            safe_ref(&remote)?;
            safe_ref(head)?;
            execute(
                cwd,
                &["push", &remote, &format!("HEAD:refs/heads/{head}")],
                env,
                cancel.clone(),
            )
            .await?;
            let tracking = upstream_remote.as_deref().unwrap_or(&remote);
            let tracking_url = optional(
                cwd,
                &["remote", "get-url", "--push", tracking],
                env,
                cancel.clone(),
            )
            .await?;
            let pushed_url = optional(
                cwd,
                &["remote", "get-url", "--push", &remote],
                env,
                cancel.clone(),
            )
            .await?;
            let same = tracking_url.is_some() && tracking_url == pushed_url;
            let tracking_matches = (upstream_remote.is_none() && upstream_merge.is_none())
                || (upstream_remote.is_some()
                    && upstream_merge
                        .as_deref()
                        .is_some_and(|reference| reference == format!("refs/heads/{head}")));
            if same && tracking_matches {
                let reference = format!("refs/remotes/{tracking}/{head}");
                let valid = run(
                    cwd,
                    &["check-ref-format".into(), reference.clone()],
                    env,
                    cancel.clone(),
                    READ_LIMIT,
                    READ_TIMEOUT,
                )
                .await?
                .status
                    == 0;
                if valid {
                    execute(
                        cwd,
                        &["update-ref", &reference, "HEAD"],
                        env,
                        cancel.clone(),
                    )
                    .await?;
                    if upstream_remote.is_none() && upstream_merge.is_none() {
                        execute(
                            cwd,
                            &["config", &format!("branch.{branch}.remote"), &remote],
                            env,
                            cancel.clone(),
                        )
                        .await?;
                        execute(
                            cwd,
                            &[
                                "config",
                                &format!("branch.{branch}.merge"),
                                &format!("refs/heads/{head}"),
                            ],
                            env,
                            cancel.clone(),
                        )
                        .await?;
                    }
                }
            }
            return Ok(());
        }
    }
    if let (Some(remote), Some(head)) = (
        upstream_remote,
        upstream_merge
            .as_deref()
            .and_then(|s| s.strip_prefix("refs/heads/")),
    ) {
        if optional(
            cwd,
            &["config", "--get", &format!("remote.{remote}.url")],
            env,
            cancel.clone(),
        )
        .await?
        .is_some()
        {
            safe_ref(&remote)?;
            safe_ref(head)?;
            return execute(
                cwd,
                &["push", "-u", &remote, &format!("HEAD:refs/heads/{head}")],
                env,
                cancel,
            )
            .await;
        }
    }
    if optional(
        cwd,
        &["config", "--get", "remote.origin.url"],
        env,
        cancel.clone(),
    )
    .await?
    .is_none()
    {
        bail!("Remote 'origin' is not configured.");
    }
    execute(cwd, &["push", "-u", "origin", &branch], env, cancel).await
}
pub async fn rebase(
    cwd: &Path,
    base: &str,
    env: &BTreeMap<String, String>,
    cancel: CancellationToken,
) -> Result<()> {
    safe_ref(base)?;
    execute(cwd, &["rebase", base], env, cancel).await
}
pub async fn revert(
    cwd: &Path,
    sha: &str,
    env: &BTreeMap<String, String>,
    cancel: CancellationToken,
) -> Result<()> {
    safe_ref(sha)?;
    execute(cwd, &["revert", "--no-edit", sha], env, cancel).await
}
