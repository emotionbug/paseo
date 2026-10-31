use crate::process::{CommandOutput, CommandSpec};
use anyhow::{Result, anyhow, bail};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

pub const READ_LIMIT: usize = 20 * 1024 * 1024;
pub const READ_TIMEOUT: Duration = Duration::from_secs(30);
pub const MUTATION_TIMEOUT: Duration = Duration::from_secs(120);

/// The source's `NotGitRepoError`: the only error classified as `NOT_GIT_REPO`.
#[derive(Debug, thiserror::Error)]
#[error("Not a git repository: {0}")]
pub struct NotGitRepo(pub String);

/// A git process that exited unsuccessfully. The message mirrors the source's
/// `runGitCommand` failure text; stdout stays available for conflict detection.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct GitCommandFailed {
    pub message: String,
    pub stdout: String,
    pub stderr: String,
}

/// `requireGitRepo`: any failure to resolve the git dir means "not a repository".
pub async fn require_repo(
    cwd: &Path,
    env: &BTreeMap<String, String>,
    cancel: CancellationToken,
) -> Result<()> {
    let args = ["rev-parse".to_owned(), "--git-dir".to_owned()];
    match run(cwd, &args, env, cancel.clone(), READ_LIMIT, READ_TIMEOUT).await {
        Ok(output) if output.status == 0 => Ok(()),
        Err(error) if cancel.is_cancelled() => Err(error),
        _ => Err(NotGitRepo(cwd.display().to_string()).into()),
    }
}

/// `requireGitWorktreeRoot`: the checkout root, or `NotGitRepo` when there is none.
pub async fn require_worktree_root(
    cwd: &Path,
    env: &BTreeMap<String, String>,
    cancel: CancellationToken,
) -> Result<PathBuf> {
    let args = ["rev-parse".to_owned(), "--show-toplevel".to_owned()];
    match run(cwd, &args, env, cancel.clone(), READ_LIMIT, READ_TIMEOUT).await {
        Ok(output) if output.status == 0 => {
            let root = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if root.is_empty() {
                Err(NotGitRepo(cwd.display().to_string()).into())
            } else {
                Ok(PathBuf::from(root))
            }
        }
        Err(error) if cancel.is_cancelled() => Err(error),
        _ => Err(NotGitRepo(cwd.display().to_string()).into()),
    }
}

pub async fn run(
    cwd: &Path,
    args: &[String],
    env: &BTreeMap<String, String>,
    cancel: CancellationToken,
    limit: usize,
    timeout: Duration,
) -> Result<CommandOutput> {
    let _permit = super::scheduler::acquire(&cancel).await?;
    let mut environment = env.clone();
    environment.insert("GIT_TERMINAL_PROMPT".into(), "0".into());
    environment.insert("GIT_OPTIONAL_LOCKS".into(), "0".into());
    // Repository/error classification must not depend on the desktop locale.
    environment.insert("LC_ALL".into(), "C".into());
    crate::process::run(
        CommandSpec {
            program: "git".into(),
            args: args.to_vec(),
            cwd: Some(cwd.to_owned()),
            env: environment,
            clear_env: false,
        },
        limit,
        timeout,
        cancel,
    )
    .await
}
pub async fn checked(
    cwd: &Path,
    args: &[String],
    env: &BTreeMap<String, String>,
    cancel: CancellationToken,
    limit: usize,
    timeout: Duration,
) -> Result<Vec<u8>> {
    let output = run(cwd, args, env, cancel, limit, timeout).await?;
    if output.status != 0 {
        let stderr = output.stderr.trim();
        return Err(GitCommandFailed {
            message: format!(
                "Git command failed: git {} (exit code: {}, signal: none)\n{}",
                args.join(" "),
                output.status,
                if stderr.is_empty() {
                    "(no stderr)"
                } else {
                    stderr
                }
            ),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: output.stderr,
        }
        .into());
    }
    Ok(output.stdout)
}
pub async fn text(
    cwd: &Path,
    args: &[&str],
    env: &BTreeMap<String, String>,
    cancel: CancellationToken,
) -> Result<String> {
    let bytes = checked(
        cwd,
        &args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        env,
        cancel,
        READ_LIMIT,
        READ_TIMEOUT,
    )
    .await?;
    Ok(String::from_utf8(bytes)?.trim().to_string())
}
pub async fn optional(
    cwd: &Path,
    args: &[&str],
    env: &BTreeMap<String, String>,
    cancel: CancellationToken,
) -> Result<Option<String>> {
    let result = run(
        cwd,
        &args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        env,
        cancel,
        READ_LIMIT,
        READ_TIMEOUT,
    )
    .await?;
    if result.status != 0 {
        return Ok(None);
    }
    let text = String::from_utf8(result.stdout)?.trim().to_owned();
    Ok((!text.is_empty()).then_some(text))
}
pub fn safe_ref(reference: &str) -> Result<()> {
    if reference.is_empty() || reference.starts_with('-') || reference.chars().any(char::is_control)
    {
        bail!("Invalid git ref: {reference}");
    }
    Ok(())
}
/// The source's `assertSafeGitRef(ref, label)`. A leading `-` is additionally
/// rejected so a client value can never be parsed as a git option.
pub fn request_ref(reference: &str, label: &str) -> Result<()> {
    if reference.is_empty()
        || reference.starts_with('-')
        || reference.contains("..")
        || !reference
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'/' | b'-'))
    {
        bail!("Invalid {label}: {reference}");
    }
    Ok(())
}
pub fn safe_path(path: &str) -> Result<()> {
    if path.is_empty()
        || Path::new(path).is_absolute()
        || path.contains('\0')
        || path.split(['/', '\\']).any(|s| s == "..")
    {
        bail!("Invalid path: {path}");
    }
    Ok(())
}
pub fn branch_name(reference: &str) -> &str {
    if let Some(rest) = reference.strip_prefix("refs/remotes/") {
        return rest.split_once('/').map(|(_, name)| name).unwrap_or(rest);
    }
    reference
        .strip_prefix("refs/heads/")
        .or_else(|| reference.strip_prefix("origin/"))
        .unwrap_or(reference)
}
pub async fn git_dir(
    cwd: &Path,
    env: &BTreeMap<String, String>,
    cancel: CancellationToken,
) -> Result<PathBuf> {
    Ok(PathBuf::from(
        text(cwd, &["rev-parse", "--absolute-git-dir"], env, cancel).await?,
    ))
}
pub async fn metadata(
    cwd: &Path,
    env: &BTreeMap<String, String>,
    cancel: CancellationToken,
) -> Result<Option<Value>> {
    let path = git_dir(cwd, env, cancel).await?.join("paseo/worktree.json");
    match tokio::fs::read(path).await {
        Ok(bytes) => Ok(Some(crate::projects::runtime::parse_metadata(
            &serde_json::from_slice::<Value>(&bytes)?,
        )?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
pub async fn exists(
    cwd: &Path,
    reference: &str,
    env: &BTreeMap<String, String>,
    cancel: CancellationToken,
) -> Result<bool> {
    safe_ref(reference)?;
    let output = run(
        cwd,
        &[
            "show-ref".into(),
            "--verify".into(),
            "--quiet".into(),
            reference.into(),
        ],
        env,
        cancel,
        READ_LIMIT,
        READ_TIMEOUT,
    )
    .await?;
    match output.status {
        0 => Ok(true),
        1 => Ok(false),
        _ => Err(anyhow!("{}", output.stderr.trim())),
    }
}
pub async fn default_branch(
    cwd: &Path,
    env: &BTreeMap<String, String>,
    cancel: CancellationToken,
) -> Result<Option<String>> {
    if let Some(head) = optional(
        cwd,
        &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
        env,
        cancel.clone(),
    )
    .await?
    {
        let name = branch_name(&head);
        if exists(cwd, &format!("refs/heads/{name}"), env, cancel.clone()).await? {
            return Ok(Some(name.into()));
        }
        if exists(cwd, &head, env, cancel.clone()).await? {
            return Ok(Some(head.trim_start_matches("refs/remotes/").into()));
        }
    }
    for name in ["main", "master"] {
        if exists(cwd, &format!("refs/heads/{name}"), env, cancel.clone()).await? {
            return Ok(Some(name.into()));
        }
    }
    Ok(None)
}
pub async fn operation_base(
    cwd: &Path,
    requested: Option<&str>,
    env: &BTreeMap<String, String>,
    cancel: CancellationToken,
) -> Result<Option<String>> {
    if let Some(value) = requested {
        safe_ref(value)?;
    }
    let meta = metadata(cwd, env, cancel.clone()).await?;
    let stored = meta.as_ref().and_then(|m| {
        m.get("baseRef")
            .and_then(Value::as_str)
            .or_else(|| m.get("baseRefName").and_then(Value::as_str))
    });
    if let (Some(stored), Some(requested)) = (stored, requested) {
        if stored != requested && branch_name(stored) != requested {
            bail!("Base ref mismatch: stored {stored}, requested {requested}");
        }
    }
    if let Some(base) = stored.or(requested) {
        safe_ref(base)?;
        return Ok(Some(base.to_string()));
    }
    default_branch(cwd, env, cancel).await
}
/// The source's `resolveMostAheadBaseRef` for an already selected base ref.
pub async fn most_ahead_base(
    cwd: &Path,
    base: &str,
    env: &BTreeMap<String, String>,
    cancel: CancellationToken,
) -> Result<String> {
    if base.starts_with("refs/heads/") || base.starts_with("refs/remotes/") {
        if !exists(cwd, base, env, cancel).await? {
            bail!("Base ref not found: {base}");
        }
        return Ok(base.to_owned());
    }
    let name = branch_name(base);
    let local = exists(cwd, &format!("refs/heads/{name}"), env, cancel.clone()).await?;
    let remote = exists(
        cwd,
        &format!("refs/remotes/origin/{name}"),
        env,
        cancel.clone(),
    )
    .await?;
    match (local, remote) {
        (false, false) => bail!("Base branch not found locally or on origin: {name}"),
        (true, false) => Ok(name.into()),
        (false, true) => Ok(format!("origin/{name}")),
        (true, true) => {
            let counts = text(
                cwd,
                &[
                    "rev-list",
                    "--left-right",
                    "--count",
                    &format!("{name}...origin/{name}"),
                ],
                env,
                cancel,
            )
            .await?;
            // Like the source, unparsable divergence counts fall back to the local branch.
            let mut counts = counts.split_whitespace();
            let left = counts.next().unwrap_or("0").parse::<u64>();
            let right = counts.next().unwrap_or("0").parse::<u64>();
            Ok(match (left, right) {
                (Ok(left), Ok(right)) if right > left => format!("origin/{name}"),
                _ => name.into(),
            })
        }
    }
}

pub async fn resolve_diff_base(
    cwd: &Path,
    requested: Option<&str>,
    env: &BTreeMap<String, String>,
    cancel: CancellationToken,
) -> Result<Option<String>> {
    let Some(base) = operation_base(cwd, requested, env, cancel.clone()).await? else {
        return Ok(None);
    };
    if base.starts_with("refs/heads/") || base.starts_with("refs/remotes/") {
        if !exists(cwd, &base, env, cancel).await? {
            bail!("Base ref not found: {base}");
        }
        return Ok(Some(base));
    }
    let name = branch_name(&base);
    if exists(
        cwd,
        &format!("refs/remotes/origin/{name}"),
        env,
        cancel.clone(),
    )
    .await?
    {
        return Ok(Some(format!("origin/{name}")));
    }
    if exists(cwd, &format!("refs/heads/{name}"), env, cancel).await? {
        return Ok(Some(name.into()));
    }
    bail!("Base branch not found locally or on origin: {name}")
}
