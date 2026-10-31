//! Worktree disk ownership and Git operations. Durable workspace identities live in the registry.
use crate::git::{
    command,
    forge::{self, ForgeEngine},
};
use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    path::{Component, Path, PathBuf},
    time::{Duration, SystemTime},
};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

// Serialize branch selection and allocation inside this daemon. Git update-ref still performs
// compare-and-swap: an external Git process must never have its branch overwritten or removed.
static MUTATIONS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Debug, thiserror::Error)]
pub enum WorktreeError {
    #[error("Branch already checked out: {0}")]
    BranchAlreadyCheckedOut(String),
    #[error("Unknown branch: {0}")]
    UnknownBranch(String),
    #[error("action \"checkout\" requires refName or checkoutSource")]
    MissingCheckoutTarget,
}
pub fn wire_error(error: &anyhow::Error) -> Value {
    let code = match error.downcast_ref::<WorktreeError>() {
        Some(WorktreeError::BranchAlreadyCheckedOut(_)) => "branch_already_checked_out",
        Some(WorktreeError::UnknownBranch(_)) => "unknown_branch",
        Some(WorktreeError::MissingCheckoutTarget) => "missing_checkout_target",
        None => "unknown",
    };
    json!({"code":code,"message":error.to_string()})
}

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(|s| {
            if matches!(
                key,
                "cwd" | "path" | "rootPath" | "mainRepoRoot" | "worktreeRoot" | "worktreePath"
            ) {
                s
            } else {
                s.trim()
            }
        })
        .filter(|s| !s.is_empty())
}
fn required<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    text(value, key).ok_or_else(|| anyhow!("{key} is required"))
}
async fn git(cwd: &Path, args: &[&str], cancel: &CancellationToken) -> Result<String> {
    let bytes = command::checked(
        cwd,
        &args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
        &BTreeMap::new(),
        cancel.clone(),
        command::READ_LIMIT,
        command::MUTATION_TIMEOUT,
    )
    .await?;
    Ok(String::from_utf8(bytes)?
        .trim_end_matches(['\r', '\n'])
        .to_owned())
}
async fn optional(cwd: &Path, args: &[&str], cancel: &CancellationToken) -> Result<Option<String>> {
    command::optional(cwd, args, &BTreeMap::new(), cancel.clone()).await
}
fn lexical(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut result = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            other => result.push(other.as_os_str()),
        }
    }
    Ok(result)
}
/// Resolve existing ancestors as well as the final path, so missing children below an escaping
/// symlink cannot become owned merely because canonicalize(the whole path) failed.
pub async fn normalize_path(path: &Path) -> Result<PathBuf> {
    let mut ancestor = lexical(path)?;
    let mut suffix = Vec::new();
    loop {
        match tokio::fs::canonicalize(&ancestor).await {
            Ok(mut root) => {
                for component in suffix.iter().rev() {
                    root.push(component);
                }
                return Ok(root);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let name = ancestor.file_name().ok_or(error)?.to_owned();
                suffix.push(name);
                if !ancestor.pop() {
                    bail!("Unable to resolve path {}", path.display());
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
}
pub fn base_root(home: &Path, config: &Value) -> Result<PathBuf> {
    let Some(root) = text(config, "worktreesRoot") else {
        return lexical(&home.join("worktrees"));
    };
    let expanded = if root == "~" || root.starts_with("~/") {
        dirs::home_dir()
            .ok_or_else(|| anyhow!("User home directory is unavailable"))?
            .join(root.strip_prefix("~/").unwrap_or(""))
    } else {
        PathBuf::from(root)
    };
    lexical(&if expanded.is_absolute() {
        expanded
    } else {
        home.join(expanded)
    })
}
pub async fn common_dir(cwd: &Path, cancel: CancellationToken) -> Result<PathBuf> {
    let raw = git(
        cwd,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        &cancel,
    )
    .await?;
    normalize_path(Path::new(&raw)).await
}
pub async fn repo_root(cwd: &Path, cancel: CancellationToken) -> Result<PathBuf> {
    let common = common_dir(cwd, cancel).await?;
    Ok(if common.file_name().is_some_and(|name| name == ".git") {
        common.parent().unwrap_or(&common).to_owned()
    } else {
        common
    })
}
fn short_hash(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    let mut number = u64::from_be_bytes(digest[..8].try_into().expect("eight digest bytes"));
    let mut bytes = [b'0'; 13];
    for digit in bytes.iter_mut().rev() {
        *digit = b"0123456789abcdefghijklmnopqrstuvwxyz"[(number % 36) as usize];
        number /= 36;
    }
    String::from_utf8(bytes[..8].to_vec()).expect("base36 is ASCII")
}
pub async fn project_hash(cwd: &Path, cancel: CancellationToken) -> Result<String> {
    let root = match repo_root(cwd, cancel.clone()).await {
        Ok(root) => root,
        Err(error) if !cancel.is_cancelled() => {
            let _ = error;
            normalize_path(cwd).await?
        }
        Err(error) => return Err(error),
    };
    Ok(short_hash(
        root.to_str()
            .ok_or_else(|| anyhow!("Repository path is not UTF-8"))?,
    ))
}
pub async fn project_root(
    home: &Path,
    config: &Value,
    cwd: &Path,
    cancel: CancellationToken,
) -> Result<PathBuf> {
    Ok(base_root(home, config)?.join(project_hash(cwd, cancel).await?))
}
pub async fn ownership(
    home: &Path,
    config: &Value,
    cwd: &Path,
    cancel: CancellationToken,
) -> Result<Value> {
    let resolved = normalize_path(cwd).await?;
    let base = normalize_path(&base_root(home, config)?).await?;
    let mut result = json!({"allowed":false,"worktreePath":resolved});
    if let Ok(repo) = repo_root(cwd, cancel.clone()).await {
        result["repoRoot"] = json!(repo);
    }
    if cancel.is_cancelled() {
        bail!("Worktree operation cancelled");
    }
    if let Ok(relative) = resolved.strip_prefix(&base) {
        let mut parts = relative.components();
        if let (Some(Component::Normal(hash)), Some(Component::Normal(slug))) =
            (parts.next(), parts.next())
        {
            let root = base.join(hash);
            result["allowed"] = json!(true);
            result["worktreePath"] = json!(root.join(slug));
            result["worktreeRoot"] = json!(root);
        }
    }
    Ok(result)
}

fn parse_worktrees(bytes: &[u8]) -> Result<Vec<Value>> {
    let mut entries = Vec::new();
    let mut current = serde_json::Map::new();
    for field in bytes.split(|b| *b == 0) {
        let field = std::str::from_utf8(field)?;
        if field.is_empty() {
            if current.contains_key("path") {
                entries.push(Value::Object(std::mem::take(&mut current)));
            }
        } else if let Some(path) = field.strip_prefix("worktree ") {
            current.insert("path".into(), json!(path));
        } else if let Some(branch) = field.strip_prefix("branch ") {
            current.insert(
                "branchName".into(),
                json!(branch.strip_prefix("refs/heads/").unwrap_or(branch)),
            );
        } else if let Some(head) = field.strip_prefix("HEAD ") {
            current.insert("head".into(), json!(head));
        }
    }
    if current.contains_key("path") {
        entries.push(Value::Object(current));
    }
    Ok(entries)
}
async fn all_worktrees(cwd: &Path, cancel: &CancellationToken) -> Result<Vec<Value>> {
    let args = ["worktree", "list", "--porcelain", "-z"].map(String::from);
    parse_worktrees(
        &command::checked(
            cwd,
            &args,
            &BTreeMap::new(),
            cancel.clone(),
            command::READ_LIMIT,
            command::READ_TIMEOUT,
        )
        .await?,
    )
}
pub async fn list(
    home: &Path,
    config: &Value,
    cwd: &Path,
    cancel: CancellationToken,
) -> Result<Vec<Value>> {
    let root = normalize_path(&project_root(home, config, cwd, cancel.clone()).await?).await?;
    let mut entries = Vec::new();
    for mut entry in all_worktrees(cwd, &cancel).await? {
        let path = normalize_path(Path::new(required(&entry, "path")?)).await?;
        if !path.starts_with(&root) || path == root {
            continue;
        }
        let time = match tokio::fs::metadata(&path).await {
            Ok(metadata) => created_time(&metadata),
            Err(_) => SystemTime::UNIX_EPOCH,
        };
        entry["path"] = json!(path);
        entry["createdAt"] =
            json!(DateTime::<Utc>::from(time).to_rfc3339_opts(SecondsFormat::Millis, true));
        entries.push(entry);
    }
    Ok(entries)
}
fn created_time(metadata: &std::fs::Metadata) -> SystemTime {
    if let Ok(time) = metadata.created() {
        if time > SystemTime::UNIX_EPOCH {
            return time;
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let nanos = metadata.ctime() as i128 * 1_000_000_000 + metadata.ctime_nsec() as i128;
        if nanos >= 0 {
            return SystemTime::UNIX_EPOCH
                + Duration::from_nanos(nanos.min(u64::MAX as i128) as u64);
        }
    }
    metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH)
}
/// `mnemonic-id`'s `createNameId()` (MIT, Mattias E. O. Andersson): one random
/// adjective and one random noun from its word lists, joined by `-`.
fn create_name_id() -> String {
    use rand::seq::IteratorRandom;
    let mut rng = rand::thread_rng();
    let adjective = include_str!("name_id_adjectives.txt")
        .lines()
        .choose(&mut rng)
        .expect("compiled adjective list is non-empty");
    let noun = include_str!("name_id_nouns.txt")
        .lines()
        .choose(&mut rng)
        .expect("compiled noun list is non-empty");
    format!("{adjective}-{noun}")
}
pub fn slugify(input: &str) -> Result<String> {
    let mut slug = String::new();
    let mut separator = false;
    for c in input.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            if separator && !slug.is_empty() {
                slug.push('-');
            }
            slug.push(c);
            separator = false;
        } else {
            separator = true;
        }
    }
    if slug.len() > 50 {
        slug.truncate(50);
        if let Some(index) = slug.rfind('-').filter(|index| *index > 25) {
            slug.truncate(index);
        } else {
            slug.truncate(slug.trim_end_matches('-').len());
        }
    }
    if slug.is_empty() {
        bail!("Invalid worktree name: Branch name cannot be empty");
    }
    Ok(slug)
}
fn validate_slug(slug: &str) -> Result<()> {
    if slug.is_empty()
        || slug.len() > 100
        || slug.starts_with('-')
        || slug.ends_with('-')
        || slug.contains("--")
        || !slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        bail!("Invalid worktree name: {slug}");
    }
    Ok(())
}
/// `validateGitBranchName`: every rejection, including a name Git would parse
/// as an option, is the source's `InvalidGitBranchNameError`.
async fn validate_branch(cwd: &Path, branch: &str, cancel: &CancellationToken) -> Result<()> {
    if command::safe_ref(branch).is_err()
        || optional(cwd, &["check-ref-format", "--branch", branch], cancel)
            .await?
            .is_none()
    {
        bail!("Invalid branch name: Git rejected ref name '{branch}'");
    }
    Ok(())
}
/// `normalizeRequiredBaseBranch`. Malformed refs (`..`, `@{`) are rejected
/// later by the metadata writer, as in the source.
fn normalize_base(base: &str) -> Result<String> {
    let base = super::runtime::normalize_base_ref_name(base)?;
    if base.is_empty() {
        bail!("Base branch is required when creating a Paseo worktree");
    }
    if base == "HEAD" {
        bail!("Base branch cannot be HEAD when creating a Paseo worktree");
    }
    Ok(base.to_owned())
}
/// `resolveBaseBranchForWorktree`: an exact (`refs/…` or `origin/…`) request
/// must exist as given; a short name tries local, origin, then the raw name.
async fn resolve_base(cwd: &Path, base: &str, cancel: &CancellationToken) -> Result<String> {
    let requested = base.trim();
    let normalized = normalize_base(requested)?;
    let exact = if super::runtime::is_qualified_ref(requested) {
        Some(requested.to_owned())
    } else if requested.starts_with("origin/") {
        Some(format!("refs/remotes/{requested}"))
    } else {
        None
    };
    let candidates = match &exact {
        Some(exact) => vec![exact.clone()],
        None => vec![
            format!("refs/heads/{requested}"),
            format!("refs/remotes/origin/{requested}"),
            requested.to_owned(),
        ],
    };
    for candidate in candidates {
        // A candidate Git would parse as an option cannot name a ref.
        if command::safe_ref(&candidate).is_ok()
            && optional(
                cwd,
                &["rev-parse", "--verify", &format!("{candidate}^{{commit}}")],
                cancel,
            )
            .await?
            .is_some()
        {
            return Ok(candidate);
        }
    }
    bail!(
        "Base branch not found: {}",
        exact.as_deref().unwrap_or(&normalized)
    )
}
async fn default_branch(cwd: &Path, cancel: &CancellationToken) -> Result<String> {
    command::default_branch(cwd, &BTreeMap::new(), cancel.clone())
        .await?
        .ok_or_else(|| anyhow!("Unable to resolve repository default branch"))
}
async fn checked_out(cwd: &Path, branch: &str, cancel: &CancellationToken) -> Result<bool> {
    Ok(all_worktrees(cwd, cancel)
        .await?
        .iter()
        .any(|entry| text(entry, "branchName") == Some(branch)))
}
async fn branch_exists(cwd: &Path, branch: &str, cancel: &CancellationToken) -> Result<bool> {
    command::exists(
        cwd,
        &format!("refs/heads/{branch}"),
        &BTreeMap::new(),
        cancel.clone(),
    )
    .await
}
fn unique_branch(candidate: &str, existing: &HashSet<String>) -> String {
    let original: Vec<_> = candidate.split('/').collect();
    let mut segments: Vec<_> = original.iter().map(|s| (*s).to_owned()).collect();
    let mut suffix = 1;
    loop {
        let name = segments.join("/");
        let parent =
            (0..segments.len()).find(|index| existing.contains(&segments[..=*index].join("/")));
        let child = existing
            .iter()
            .any(|branch| branch.starts_with(&format!("{name}/")));
        if parent.is_none() && !child {
            return name;
        }
        let index = parent.unwrap_or(segments.len() - 1);
        segments[index] = format!("{}-{suffix}", original[index]);
        suffix += 1;
    }
}
async fn local_branches(cwd: &Path, cancel: &CancellationToken) -> Result<HashSet<String>> {
    let refs = git(
        cwd,
        &["for-each-ref", "--format=%(refname:strip=2)", "refs/heads/"],
        cancel,
    )
    .await?;
    Ok(refs.lines().map(str::to_owned).collect())
}
/// Claim the ref atomically. Only retry a proven name collision, never an ambiguous mutation.
async fn reserve_branch(
    cwd: &Path,
    candidate: &str,
    start: &str,
    cancel: &CancellationToken,
) -> Result<(String, String)> {
    let oid = git(
        cwd,
        &["rev-parse", "--verify", &format!("{start}^{{commit}}")],
        cancel,
    )
    .await?;
    loop {
        let branch = unique_branch(candidate, &local_branches(cwd, cancel).await?);
        match git(
            cwd,
            &["update-ref", &format!("refs/heads/{branch}"), &oid, ""],
            cancel,
        )
        .await
        {
            Ok(_) => return Ok((branch, oid)),
            Err(error) => {
                let after = local_branches(cwd, cancel).await?;
                if unique_branch(&branch, &after) == branch {
                    return Err(error);
                }
            }
        }
    }
}
/// `fetchWorktreeCheckoutRefs`: the first ref that fetches wins. `branch` is
/// the local branch the fetched tip is for, named in the failure message
/// together with every attempted ref and the last attempt's stderr.
async fn fetch_tip(
    cwd: &Path,
    branch: &str,
    refs: &[Value],
    cancel: &CancellationToken,
) -> Result<String> {
    // Never fetch directly into a user branch. The private ref is deleted with its observed OID.
    let temporary = format!("refs/paseo/worktree-create/{}", uuid::Uuid::new_v4());
    let mut attempted = Vec::new();
    let mut stderr = String::new();
    for reference in refs {
        let remote = text(reference, "remoteName").unwrap_or("origin");
        let source = required(reference, "remoteRef")?;
        command::safe_ref(remote)?;
        command::safe_ref(source)?;
        attempted.push(format!("{remote} {source}"));
        let fetched = git(
            cwd,
            &["fetch", remote, &format!("{source}:{temporary}")],
            cancel,
        )
        .await;
        let cleanup = CancellationToken::new();
        let oid = optional(cwd, &["rev-parse", "--verify", &temporary], &cleanup).await?;
        if let Some(oid) = oid {
            git(cwd, &["update-ref", "-d", &temporary, &oid], &cleanup).await?;
            if fetched.is_ok() {
                return Ok(oid);
            }
        }
        if cancel.is_cancelled() {
            bail!("Worktree operation cancelled");
        }
        stderr = match fetched {
            Ok(_) => String::new(),
            Err(error) => match error.downcast_ref::<command::GitCommandFailed>() {
                Some(failed) => failed.stderr.clone(),
                None => return Err(error),
            },
        };
    }
    let stderr = if stderr.is_empty() {
        String::new()
    } else {
        format!("\n{stderr}")
    };
    bail!(
        "Unable to fetch change request refs for worktree branch {branch}: {}{stderr}",
        attempted.join(", ")
    )
}
async fn refresh_base(cwd: &Path, base: &str, cancel: &CancellationToken) -> Result<()> {
    if !base.starts_with("refs/remotes/") {
        return Ok(());
    }
    // `refreshRemoteTrackingBaseRef` is best effort: any failure, including listing
    // remotes, branches from the cached ref.
    let remotes = git(cwd, &["remote"], cancel).await.unwrap_or_default();
    if let Some(remote) = remotes.lines().map(str::trim).find(|name| {
        !name.is_empty() && base.starts_with(&format!("refs/remotes/{name}/"))
    }) {
        let branch = &base["refs/remotes/".len() + remote.len() + 1..];
        let args = vec![
            "fetch".into(),
            remote.into(),
            format!("+refs/heads/{branch}:{base}"),
        ];
        let _ = command::run(
            cwd,
            &args,
            &BTreeMap::new(),
            cancel.clone(),
            command::READ_LIMIT,
            Duration::from_secs(15),
        )
        .await;
    }
    if cancel.is_cancelled() {
        bail!("Worktree operation cancelled");
    }
    Ok(())
}
struct Plan {
    branch: String,
    metadata: Value,
    owned_oid: Option<String>,
}
async fn plan(cwd: &Path, slug: &str, source: &Value, cancel: &CancellationToken) -> Result<Plan> {
    let kind = required(source, "kind")?;
    let requested = if matches!(kind, "checkout-change-request" | "checkout-github-pr") {
        text(source, "localBranchName").unwrap_or(required(source, "headRef")?)
    } else {
        required(source, "branchName")?
    };
    validate_branch(cwd, requested, cancel).await?;
    let mut exact = None;
    let mut hint = true;
    let base_name;
    let (branch, owned_oid) = match kind {
        "branch-off" | "restore-from-base" => {
            let requested_base = required(
                source,
                if kind == "branch-off" {
                    "baseBranch"
                } else {
                    "baseRef"
                },
            )?;
            base_name = normalize_base(requested_base)?;
            let resolved = resolve_base(cwd, requested_base, cancel).await?;
            let exists = kind == "branch-off" && branch_exists(cwd, requested, cancel).await?;
            let start = if exists { requested } else { &resolved };
            if kind == "branch-off" {
                refresh_base(cwd, start, cancel).await?;
            } else {
                hint = false;
            }
            let reserved =
                reserve_branch(cwd, if exists { slug } else { requested }, start, cancel).await?;
            exact = Some(resolved);
            (reserved.0, Some(reserved.1))
        }
        "checkout-branch" | "restore" => {
            // `ensureLocalBranch` comes first. A missing branch is fetched from origin into a
            // private ref; it is claimed only after every later check has passed, so those
            // errors cannot leak a newly created local branch.
            let fetched = if branch_exists(cwd, requested, cancel).await? {
                None
            } else {
                let refs =
                    [json!({"remoteName":"origin","remoteRef":format!("refs/heads/{requested}")})];
                Some(
                    fetch_tip(cwd, requested, &refs, cancel)
                        .await
                        .map_err(|error| {
                            if cancel.is_cancelled() {
                                error
                            } else {
                                WorktreeError::UnknownBranch(requested.to_owned()).into()
                            }
                        })?,
                )
            };
            let occupied = fetched.is_none() && checked_out(cwd, requested, cancel).await?;
            if kind == "restore" {
                if occupied {
                    return Err(WorktreeError::BranchAlreadyCheckedOut(requested.to_owned()).into());
                }
                // Restore the saved branch even when its comparison ref no longer exists.
                if let Some(base) =
                    text(source, "baseRef").filter(|base| super::runtime::is_qualified_ref(base))
                {
                    exact = resolve_base(cwd, base, cancel).await.ok();
                    if cancel.is_cancelled() {
                        bail!("Worktree operation cancelled");
                    }
                }
                base_name = normalize_base(text(source, "baseRef").unwrap_or(requested))?;
            } else {
                // `resolveCheckoutBranchBaseRef`: the default branch, else the main checkout's
                // current branch (`getCurrentBranch`, which reads through a rebase), else itself.
                base_name =
                    match command::default_branch(cwd, &BTreeMap::new(), cancel.clone()).await? {
                        Some(base) => base,
                        None => crate::git::current_branch(cwd, &BTreeMap::new(), cancel.clone())
                            .await
                            .ok()
                            .flatten()
                            .unwrap_or_else(|| requested.to_owned()),
                    };
            }
            match fetched {
                Some(oid) => {
                    let reserved = reserve_branch(cwd, requested, &oid, cancel).await?;
                    (reserved.0, Some(reserved.1))
                }
                None if occupied => {
                    let reserved = reserve_branch(cwd, requested, requested, cancel).await?;
                    (reserved.0, Some(reserved.1))
                }
                None => (requested.to_owned(), None),
            }
        }
        "checkout-change-request" | "checkout-github-pr" => {
            let local = unique_branch(requested, &local_branches(cwd, cancel).await?);
            base_name = normalize_base(required(source, "baseRefName")?)?;
            let number = source
                .get("changeRequestNumber")
                .or_else(|| source.get("githubPrNumber"))
                .and_then(Value::as_u64)
                .ok_or_else(|| anyhow!("Change request number is required"))?;
            let fallback =
                vec![json!({"remoteName":"origin","remoteRef":format!("refs/pull/{number}/head")})];
            let refs = source
                .get("checkoutRefs")
                .and_then(Value::as_array)
                .unwrap_or(&fallback);
            let tip = fetch_tip(cwd, &local, refs, cancel).await?;
            let reserved = reserve_branch(cwd, requested, &tip, cancel).await?;
            (reserved.0, Some(reserved.1))
        }
        _ => bail!("Unknown worktree source: {kind}"),
    };
    let mut metadata = json!({"baseRefName":base_name});
    if let Some(exact) = exact {
        metadata["baseRef"] = json!(exact);
    }
    if hint {
        let mut target = json!({"headRef":branch,"localBranchName":branch});
        if matches!(kind, "checkout-change-request" | "checkout-github-pr") {
            target["headRef"] = source["headRef"].clone();
            target["changeRequestNumber"] = source
                .get("changeRequestNumber")
                .or_else(|| source.get("githubPrNumber"))
                .cloned()
                .unwrap_or(Value::Null);
            if let Some(owner) = text(source, "headRepositoryOwner") {
                target["headRepositoryOwner"] = json!(owner);
            }
        }
        metadata["changeRequestLookupTarget"] = target;
    }
    Ok(Plan {
        branch,
        metadata,
        owned_oid,
    })
}
async fn release_branch(cwd: &Path, plan: &Plan) -> Result<()> {
    if let Some(oid) = &plan.owned_oid {
        let cancel = CancellationToken::new();
        if !checked_out(cwd, &plan.branch, &cancel).await? {
            // CAS refuses to delete a branch another process has advanced since allocation.
            git(
                cwd,
                &[
                    "update-ref",
                    "-d",
                    &format!("refs/heads/{}", plan.branch),
                    oid,
                ],
                &cancel,
            )
            .await?;
        }
    }
    Ok(())
}
struct ConfigChange {
    key: String,
    before: Vec<String>,
    after: Vec<String>,
}
async fn config_values(cwd: &Path, key: &str, cancel: &CancellationToken) -> Result<Vec<String>> {
    let args = ["config", "--null", "--get-all", key].map(String::from);
    let output = command::run(
        cwd,
        &args,
        &BTreeMap::new(),
        cancel.clone(),
        command::READ_LIMIT,
        command::READ_TIMEOUT,
    )
    .await?;
    if output.status == 1 {
        return Ok(Vec::new());
    }
    if output.status != 0 {
        bail!("Unable to read Git configuration: {}", output.stderr);
    }
    let bytes = output.stdout.strip_suffix(&[0]).unwrap_or(&output.stdout);
    if output.stdout.is_empty() {
        return Ok(Vec::new());
    }
    bytes
        .split(|b| *b == 0)
        .map(|value| Ok(std::str::from_utf8(value)?.to_owned()))
        .collect()
}
async fn set_config(
    cwd: &Path,
    key: &str,
    value: &str,
    append: bool,
    changes: &mut Vec<ConfigChange>,
    cancel: &CancellationToken,
) -> Result<()> {
    let before = config_values(cwd, key, cancel).await?;
    let mut after = if append { before.clone() } else { Vec::new() };
    after.push(value.to_owned());
    changes.push(ConfigChange {
        key: key.to_owned(),
        before,
        after,
    });
    git(
        cwd,
        &[
            "config",
            if append { "--add" } else { "--replace-all" },
            key,
            value,
        ],
        cancel,
    )
    .await?;
    Ok(())
}
async fn rollback_config(cwd: &Path, changes: &[ConfigChange]) -> Result<()> {
    let cancel = CancellationToken::new();
    for change in changes.iter().rev() {
        // A concurrently modified key belongs to its new writer; do not restore over it.
        if config_values(cwd, &change.key, &cancel).await? != change.after {
            continue;
        }
        git(cwd, &["config", "--unset-all", &change.key], &cancel).await?;
        for value in &change.before {
            git(cwd, &["config", "--add", &change.key, value], &cancel).await?;
        }
    }
    Ok(())
}
async fn track(
    cwd: &Path,
    remote: &str,
    head: &str,
    branch: &str,
    changes: &mut Vec<ConfigChange>,
    cancel: &CancellationToken,
) -> Result<()> {
    let exact = format!("refs/heads/{head}:refs/remotes/{remote}/{head}");
    let fetch = git(cwd, &["fetch", remote, &format!("+{exact}")], cancel).await;
    if cancel.is_cancelled() {
        bail!("Worktree operation cancelled");
    }
    if fetch.is_err() {
        return Ok(());
    }
    let key = format!("remote.{remote}.fetch");
    let wildcard = format!("refs/heads/*:refs/remotes/{remote}/*");
    let configured = optional(cwd, &["config", "--get-all", &key], cancel)
        .await?
        .unwrap_or_default();
    if !configured.lines().any(|line| {
        let line = line.trim_start_matches('+');
        line == exact || line == wildcard
    }) {
        set_config(cwd, &key, &format!("+{exact}"), true, changes, cancel).await?;
    }
    set_config(
        cwd,
        &format!("branch.{branch}.remote"),
        remote,
        false,
        changes,
        cancel,
    )
    .await?;
    set_config(
        cwd,
        &format!("branch.{branch}.merge"),
        &format!("refs/heads/{head}"),
        false,
        changes,
        cancel,
    )
    .await?;
    Ok(())
}
async fn configure_remotes(
    cwd: &Path,
    source: &Value,
    branch: &str,
    changes: &mut Vec<ConfigChange>,
    cancel: &CancellationToken,
) -> Result<()> {
    if !matches!(
        text(source, "kind"),
        Some("checkout-change-request" | "checkout-github-pr")
    ) {
        return Ok(());
    }
    let head = required(source, "headRef")?;
    let origin = source["trackOriginHead"].as_bool() == Some(true);
    let push_url = if let Some(url) = text(source, "pushRemoteUrl") {
        Some(url.to_owned())
    } else if origin && branch != head {
        optional(cwd, &["remote", "get-url", "--push", "origin"], cancel).await?
    } else {
        None
    };
    if let Some(url) = push_url {
        let number = source
            .get("changeRequestNumber")
            .or_else(|| source.get("githubPrNumber"))
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow!("Change request number is required"))?;
        // `configureWorktreePushRemote`: the remote is always `paseo-pr-<number>`;
        // checking the same change request out again re-points that remote.
        let remote = format!("paseo-pr-{number}");
        set_config(
            cwd,
            &format!("remote.{remote}.url"),
            &url,
            false,
            changes,
            cancel,
        )
        .await?;
        set_config(
            cwd,
            &format!("remote.{remote}.push"),
            &format!("HEAD:refs/heads/{head}"),
            false,
            changes,
            cancel,
        )
        .await?;
        set_config(
            cwd,
            &format!("branch.{branch}.pushRemote"),
            &remote,
            false,
            changes,
            cancel,
        )
        .await?;
        if text(source, "pushRemoteUrl").is_some() {
            set_config(
                cwd,
                &format!("remote.{remote}.fetch"),
                &format!("+refs/heads/{head}:refs/remotes/{remote}/{head}"),
                false,
                changes,
                cancel,
            )
            .await?;
            track(cwd, &remote, head, branch, changes, cancel).await?;
        }
    }
    if origin {
        track(cwd, "origin", head, branch, changes, cancel).await?;
    }
    Ok(())
}
pub async fn seed_paseo_config_file(source: &Path, target: &Path) -> Result<()> {
    let mut input = match tokio::fs::File::open(source.join("paseo.json")).await {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let mut output = match tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target.join("paseo.json"))
        .await
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    output
        .set_permissions(input.metadata().await?.permissions())
        .await?;
    tokio::io::copy(&mut input, &mut output).await?;
    output.flush().await?;
    Ok(())
}
fn same_directory(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        left.dev() == right.dev() && left.ino() == right.ino()
    }
    #[cfg(not(unix))]
    {
        left.created()
            .ok()
            .zip(right.created().ok())
            .is_some_and(|(a, b)| a == b)
    }
}
fn directory_identity(metadata: &std::fs::Metadata) -> Result<Value> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(json!({"device":metadata.dev(),"inode":metadata.ino()}))
    }
    #[cfg(not(unix))]
    {
        Ok(
            json!({"created":metadata.created()?.duration_since(SystemTime::UNIX_EPOCH)?.as_nanos().to_string()}),
        )
    }
}
pub async fn create_from_source(
    home: &Path,
    config: &Value,
    cwd: &Path,
    slug: &str,
    source: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    let mut result = create_with_identity(home, config, cwd, slug, source, cancel).await?;
    Ok(result["worktree"].take())
}
async fn create_with_identity(
    home: &Path,
    config: &Value,
    cwd: &Path,
    slug: &str,
    source: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    validate_slug(slug)?;
    let _guard = tokio::select! { guard = MUTATIONS.lock() => guard, _ = cancel.cancelled() => bail!("Worktree operation cancelled") };
    let base = normalize_path(&base_root(home, config)?).await?;
    let root = base.join(project_hash(cwd, cancel.clone()).await?);
    tokio::fs::create_dir_all(&root).await?;
    if normalize_path(&root).await? != root {
        bail!("Worktree project directory escapes its owned root");
    }
    let mut suffix = 0;
    let path = loop {
        let path = root.join(if suffix == 0 {
            slug.to_owned()
        } else {
            format!("{slug}-{suffix}")
        });
        match tokio::fs::create_dir(&path).await {
            Ok(()) => break path,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => suffix += 1,
            Err(error) => return Err(error.into()),
        }
    };
    let identity = tokio::fs::symlink_metadata(&path).await?;
    let plan = match plan(cwd, slug, source, &cancel).await {
        Ok(plan) => plan,
        Err(error) => {
            let _ = tokio::fs::remove_dir(&path).await;
            return Err(error);
        }
    };
    let path_string = path
        .to_str()
        .ok_or_else(|| anyhow!("Worktree path is not UTF-8"))?;
    let mut added = false;
    let mut config_changes = Vec::new();
    let result: Result<Value> = async {
        git(cwd, &["worktree", "add", path_string, &plan.branch], &cancel).await?;
        added = true;
        configure_remotes(cwd, source, &plan.branch, &mut config_changes, &cancel).await?;
        super::runtime::write_worktree_metadata(&path, &plan.metadata).await?;
        seed_paseo_config_file(cwd, &path).await?;
        let git_dir = command::git_dir(&path,&BTreeMap::new(),cancel.clone()).await?;
        let head = git(&path,&["rev-parse","HEAD"],&cancel).await?;
        Ok(json!({
            "worktree":{"branchName":plan.branch,"worktreePath":path,"comparisonBaseRef":plan.metadata.get("baseRef").unwrap_or(&plan.metadata["baseRefName"])},
            "_rollback":{"directory":directory_identity(&identity)?,"gitDir":git_dir,"head":head,"branchName":plan.branch}
        }))
    }.await;
    match result {
        Ok(result) => Ok(result),
        Err(error) => {
            let cleanup: Result<()> = async {
                if let Ok(current) = tokio::fs::symlink_metadata(&path).await {
                    if !current.is_dir() || !same_directory(&identity, &current) {
                        bail!("Reserved worktree path was replaced; leaving it untouched");
                    }
                    if added {
                        remove_disk(home, config, cwd, &path, CancellationToken::new()).await?;
                    } else {
                        // A failed/cancelled worktree add can have partially populated our directory.
                        // Remove it only if Git registered this precise path and branch; otherwise an
                        // empty-directory removal is the only safe cleanup.
                        let entries = all_worktrees(cwd, &CancellationToken::new()).await?;
                        let registered = entries.iter().any(|entry| {
                            text(entry, "path") == Some(path_string)
                                && text(entry, "branchName") == Some(plan.branch.as_str())
                        });
                        if registered {
                            remove_disk(home, config, cwd, &path, CancellationToken::new()).await?;
                        } else {
                            tokio::fs::remove_dir(&path).await?;
                        }
                    }
                }
                rollback_config(cwd, &config_changes).await?;
                release_branch(cwd, &plan).await
            }
            .await;
            match cleanup {
                Ok(()) => Err(error),
                Err(cleanup) => Err(anyhow!("{error}; rollback also failed: {cleanup}")),
            }
        }
    }
}

pub async fn create(
    home: &Path,
    config: &Value,
    cwd: &Path,
    input: &Value,
    forge_engine: &ForgeEngine,
    env: &BTreeMap<String, String>,
    cancel: CancellationToken,
) -> Result<Value> {
    let owned = ownership(home, config, cwd, cancel.clone()).await?;
    let repo = if owned["allowed"] == true {
        repo_root(cwd, cancel.clone()).await?
    } else if let Some(root) = optional(cwd, &["rev-parse", "--show-toplevel"], &cancel).await? {
        normalize_path(Path::new(&root)).await?
    } else {
        repo_root(cwd, cancel.clone())
            .await
            .context("Create worktree requires a git repository")?
    };
    let requested_slug = text(input, "worktreeSlug").map(slugify).transpose()?;
    let action = text(input, "action");
    let change = input.get("checkoutSource").filter(|v| !v.is_null());
    let number = change
        .and_then(|v| v["number"].as_u64())
        .or_else(|| input["githubPrNumber"].as_u64());
    let reference = text(input, "refName");
    let intent = if action != Some("branch-off") && number.is_some() {
        let number = number.expect("checked number");
        if number == 0 {
            bail!("Change request number must be positive");
        }
        let resolved = forge_engine.resolve(&repo, env, cancel.clone()).await?;
        let forge_name = resolved
            .as_ref()
            .map(|r| r.forge.as_str())
            .unwrap_or("github");
        if let Some(requested) = change.and_then(|v| text(v, "forge")) {
            if requested != forge_name {
                if resolved.is_none() {
                    bail!("Checkout from change request is not supported for {requested} yet");
                }
                bail!(
                    "Checkout source is for {requested}, but this workspace resolved to {forge_name}"
                );
            }
        }
        let target_input = json!({"number":number,"forge":forge_name});
        let target = forge_engine
            .execute(
                &repo,
                "getPullRequestCheckoutTarget",
                &target_input,
                env,
                cancel.clone(),
            )
            .await?;
        let head = match reference.or_else(|| text(&target, "headRefName")) {
            Some(head) => head.to_owned(),
            None => {
                let value = forge_engine
                    .execute(
                        &repo,
                        "getPullRequestHeadRef",
                        &target_input,
                        env,
                        cancel.clone(),
                    )
                    .await?;
                value
                    .as_str()
                    .ok_or_else(|| anyhow!("Forge returned an invalid pull request head ref"))?
                    .to_owned()
            }
        };
        let cross = target["isCrossRepository"].as_bool() == Some(true);
        let target_refs = target.get("checkoutRefs").and_then(Value::as_array);
        if cross
            && !target_refs.is_some_and(|v| !v.is_empty())
            && !forge::supports_cross_repo_checkout_without_refs(forge_name)
        {
            bail!("Checkout from change request is not supported for {forge_name} yet");
        }
        let base = match text(&target, "baseRefName") {
            Some(base) => base.to_owned(),
            None => default_branch(&repo, &cancel).await?,
        };
        let refs = match target_refs {
            Some(refs) => refs.clone(),
            None => {
                let defaults = forge::default_checkout_refs(forge_name, number);
                if defaults.is_empty() {
                    vec![json!({"remoteName":"origin","remoteRef":format!("refs/heads/{head}")})]
                } else {
                    defaults
                }
            }
        };
        let mut intent = json!({"kind":"checkout-change-request","forge":forge_name,"changeRequestNumber":number,"headRef":head,"baseRefName":base,"checkoutRefs":refs});
        if let Some(branch) = forge::build_pr_local_branch_name(forge_name, &head, &target)
            .filter(|name| name != &head)
        {
            intent["localBranchName"] = json!(branch);
        }
        if !cross && text(&target, "headRefKind") != Some("pull-ref") {
            intent["trackOriginHead"] = json!(true);
        }
        if cross {
            let owner = text(&target, "headOwnerLogin");
            if let Some(owner) = owner {
                intent["headRepositoryOwner"] = json!(owner);
            }
            let url = text(&target, "headRepositoryUrl")
                .or_else(|| text(&target, "headRepositorySshUrl"));
            let repository = if let Some(url) = url {
                let parts: Vec<_> = url
                    .trim_end_matches(".git")
                    .split(['/', ':'])
                    .filter(|s| !s.is_empty())
                    .collect();
                match (
                    owner.or_else(|| {
                        parts
                            .len()
                            .checked_sub(2)
                            .and_then(|i| parts.get(i).copied())
                    }),
                    parts.last(),
                ) {
                    (Some(owner), Some(name)) => format!("{owner}/{name}"),
                    (_, Some(name)) => (*name).to_owned(),
                    _ => "unknown repository".into(),
                }
            } else {
                owner.unwrap_or("unknown repository").to_owned()
            };
            intent["headRepository"] = json!(repository);
            if let Some(url) =
                text(&target, "headRepositorySshUrl").or_else(|| text(&target, "headRepositoryUrl"))
            {
                intent["pushRemoteUrl"] = json!(url);
            }
        }
        intent
    } else if action == Some("checkout") {
        json!({"kind":"checkout-branch","branchName":reference.ok_or(WorktreeError::MissingCheckoutTarget)?})
    } else {
        let base = match reference {
            Some(base) => base.to_owned(),
            None => default_branch(&repo, &cancel).await?,
        };
        // `createWorktreeCore`: an explicit branch name is only trimmed (an empty
        // one fails slug validation); otherwise the branch is the requested slug or
        // a fresh mnemonic `adjective-noun` id.
        let branch = match input["branchName"].as_str() {
            Some(branch) => branch.trim().to_owned(),
            None => match requested_slug.clone() {
                Some(slug) => slug,
                None => slugify(&create_name_id())?,
            },
        };
        json!({"kind":"branch-off","baseBranch":base,"branchName":branch})
    };
    let slug = match requested_slug {
        Some(slug) => slug,
        None => slugify(
            intent["branchName"]
                .as_str()
                .or_else(|| text(&intent, "localBranchName"))
                .or_else(|| text(&intent, "headRef"))
                .ok_or_else(|| anyhow!("Worktree branch is missing"))?,
        )?,
    };
    let mut result = create_with_identity(home, config, &repo, &slug, &intent, cancel).await?;
    result["intent"] = intent;
    result["repoRoot"] = json!(repo);
    result["created"] = json!(true);
    Ok(result)
}

/// Roll back only the checkout created by this operation, never a later occupant of its path.
/// The internal `_rollback` capability returned by `create` must not be sent over the wire.
/// As in the source wrapper rollback, branch history remains available for later recovery.
pub async fn rollback_created(
    home: &Path,
    config: &Value,
    created: &Value,
    cancel: CancellationToken,
) -> Result<()> {
    let _guard = tokio::select! { guard = MUTATIONS.lock() => guard, _ = cancel.cancelled() => bail!("Worktree operation cancelled") };
    let worktree = created
        .get("worktree")
        .ok_or_else(|| anyhow!("Created worktree descriptor is missing"))?;
    let identity = created
        .get("_rollback")
        .ok_or_else(|| anyhow!("Created worktree rollback identity is missing"))?;
    let path = Path::new(required(worktree, "worktreePath")?);
    let repo = Path::new(required(created, "repoRoot")?);
    let current = match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if !current.is_dir() || directory_identity(&current)? != identity["directory"] {
        bail!("Created worktree directory was replaced; refusing rollback");
    }
    let git_dir = command::git_dir(path, &BTreeMap::new(), cancel.clone()).await?;
    if json!(git_dir) != identity["gitDir"]
        || git(path, &["branch", "--show-current"], &cancel).await?
            != required(identity, "branchName")?
        || git(path, &["rev-parse", "HEAD"], &cancel).await? != required(identity, "head")?
    {
        bail!("Created worktree identity changed; refusing rollback");
    }
    remove_disk(home, config, repo, path, cancel).await
}

pub async fn remove(
    home: &Path,
    config: &Value,
    repo: &Path,
    path: &Path,
    cancel: CancellationToken,
) -> Result<()> {
    let _guard = tokio::select! { guard = MUTATIONS.lock() => guard, _ = cancel.cancelled() => bail!("Worktree operation cancelled") };
    remove_disk(home, config, repo, path, cancel).await
}
async fn remove_disk(
    home: &Path,
    config: &Value,
    repo: &Path,
    path: &Path,
    cancel: CancellationToken,
) -> Result<()> {
    let owned = ownership(home, config, path, cancel.clone()).await?;
    if owned["allowed"] != true {
        bail!("Refusing to delete non-Paseo worktree");
    }
    let target = PathBuf::from(required(&owned, "worktreePath")?);
    // An available source repository must own this hash, not just some sibling under the base.
    let original = match tokio::fs::symlink_metadata(&target).await {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    if let Ok(root) = repo_root(repo, cancel.clone()).await {
        let expected =
            normalize_path(&project_root(home, config, &root, cancel.clone()).await?).await?;
        if target.parent() != Some(expected.as_path()) {
            bail!("Refusing to delete a worktree belonging to another project");
        }
        if let Ok(actual) = repo_root(&target, cancel.clone()).await {
            if actual != root {
                bail!("Refusing to delete a worktree belonging to another repository");
            }
        }
    }
    if cancel.is_cancelled() {
        bail!("Worktree operation cancelled");
    }
    let target_text = target
        .to_str()
        .ok_or_else(|| anyhow!("Worktree path is not UTF-8"))?;
    let _ = git(
        repo,
        &["worktree", "remove", target_text, "--force"],
        &cancel,
    )
    .await;
    if cancel.is_cancelled() {
        bail!("Worktree operation cancelled");
    }
    let mut last_error = None;
    for delay in [0, 100, 300, 700, 1500] {
        if delay != 0 {
            tokio::select! { _ = tokio::time::sleep(Duration::from_millis(delay)) => {}, _ = cancel.cancelled() => bail!("Worktree operation cancelled") }
        }
        // Recheck symlinks after git and between retry attempts. Never follow a replaced root.
        if normalize_path(&target).await? != target {
            bail!("Worktree path changed during removal");
        }
        match tokio::fs::symlink_metadata(&target).await {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                last_error = None;
                break;
            }
            Err(error) => return Err(error.into()),
            Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
                bail!("Refusing to remove replaced worktree path")
            }
            Ok(metadata) => {
                if original
                    .as_ref()
                    .is_none_or(|before| !same_directory(before, &metadata))
                {
                    bail!("Worktree directory was replaced during removal");
                }
            }
        }
        match tokio::fs::remove_dir_all(&target).await {
            Ok(()) => {
                last_error = None;
                break;
            }
            Err(error) => last_error = Some(error),
        }
    }
    if let Some(error) = last_error {
        return Err(error.into());
    }
    let _ = git(repo, &["worktree", "prune"], &cancel).await;
    Ok(())
}
async fn is_directory(path: &Path) -> Result<bool> {
    match tokio::fs::metadata(path).await {
        Ok(metadata) => Ok(metadata.is_dir()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}
pub async fn inspect_recovery(
    id: &str,
    workspace: Option<&Value>,
    project: Option<&Value>,
) -> Result<Value> {
    let unavailable = |reason: &str, message: &str| json!({"kind":"unavailable","workspaceId":id,"reason":reason,"message":message});
    let Some(workspace) = workspace else {
        return Ok(unavailable(
            "workspace_not_found",
            "This workspace is no longer known to the host.",
        ));
    };
    if text(workspace, "archivedAt").is_none() {
        return Ok(unavailable(
            "workspace_not_archived",
            "This workspace is not archived, but it is unavailable from the host.",
        ));
    }
    let Some(project) = project else {
        return Ok(unavailable(
            "project_not_found",
            "The project for this archived workspace no longer exists.",
        ));
    };
    let action = if is_directory(Path::new(required(workspace, "cwd")?)).await? {
        "unarchive"
    } else {
        if text(workspace, "kind") != Some("worktree") {
            return Ok(unavailable(
                "workspace_directory_missing",
                "The archived workspace directory no longer exists and cannot be recreated.",
            ));
        }
        let root = text(workspace, "mainRepoRoot").unwrap_or(required(project, "rootPath")?);
        if !is_directory(Path::new(root)).await? {
            return Ok(unavailable(
                "project_directory_missing",
                "The source repository needed to restore this worktree no longer exists.",
            ));
        }
        "restore"
    };
    let name = text(workspace, "title")
        .or_else(|| text(workspace, "displayName"))
        .or_else(|| text(workspace, "branch"))
        .unwrap_or(id);
    Ok(
        json!({"kind":"recoverable","workspaceId":id,"workspaceName":name,"action":action,"branch":workspace.get("branch").unwrap_or(&Value::Null)}),
    )
}
pub async fn map_workspace_cwd(source: &Path, cwd: &Path, target: &Path) -> Result<PathBuf> {
    let source = normalize_path(source).await?;
    let cwd = normalize_path(cwd).await?;
    let relative = cwd.strip_prefix(&source).with_context(|| {
        format!(
            "Workspace cwd is outside its source worktree: {}",
            cwd.display()
        )
    })?;
    let target = normalize_path(target).await?;
    let mapped = normalize_path(&target.join(relative)).await?;
    if !mapped.starts_with(&target) {
        bail!(
            "Workspace cwd escapes its target worktree: {}",
            relative.display()
        );
    }
    Ok(mapped)
}
pub async fn restore(
    home: &Path,
    config: &Value,
    workspace: &Value,
    project: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    let id = required(workspace, "workspaceId")?;
    let state = inspect_recovery(id, Some(workspace), Some(project)).await?;
    if text(&state, "kind") == Some("unavailable") {
        bail!("{}", required(&state, "message")?);
    }
    if text(&state, "action") == Some("unarchive") {
        return Ok(json!({"workspaceId":id,"action":"unarchive"}));
    }
    let root = Path::new(text(workspace, "mainRepoRoot").unwrap_or(required(project, "rootPath")?));
    let cwd = Path::new(required(workspace, "cwd")?);
    let previous = if let Some(path) = text(workspace, "worktreeRoot") {
        PathBuf::from(path)
    } else {
        let owned = ownership(home, config, cwd, cancel.clone()).await?;
        PathBuf::from(if owned["allowed"] == true {
            required(&owned, "worktreePath")?
        } else {
            required(workspace, "cwd")?
        })
    };
    let source = if let Some(branch) = text(workspace, "branch") {
        json!({"kind":"restore","branchName":branch,"baseRef":workspace.get("baseBranch").unwrap_or(&Value::Null)})
    } else {
        let recorded = text(workspace, "baseBranch");
        let valid = if let Some(base) = recorded {
            command::safe_ref(base)?;
            optional(
                root,
                &["rev-parse", "--verify", &format!("{base}^{{commit}}")],
                &cancel,
            )
            .await?
            .is_some()
        } else {
            false
        };
        let base = if valid {
            recorded.unwrap().to_owned()
        } else {
            default_branch(root,&cancel).await.context("The saved base is unavailable and the repository has no default branch to restore from.")?
        };
        json!({"kind":"restore-from-base","branchName":format!("restored/{}",&uuid::Uuid::new_v4().to_string()[..8]),"baseRef":base})
    };
    let _ = git(root, &["worktree", "prune"], &cancel).await;
    let slug = previous
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("Archived worktree has no valid directory name"))?;
    let worktree = create_from_source(home, config, root, slug, &source, cancel.clone()).await?;
    let created = Path::new(required(&worktree, "worktreePath")?);
    let check: Result<()> = async {
        let mapped = map_workspace_cwd(&previous, cwd, created).await?;
        if mapped != normalize_path(cwd).await? {
            bail!(
                "Recreated worktree diverged from {}: {}",
                cwd.display(),
                mapped.display()
            );
        }
        if !is_directory(&mapped).await? {
            bail!(
                "Selected project directory is missing from the restored worktree: {}",
                mapped.display()
            );
        }
        Ok(())
    }
    .await;
    if let Err(error) = check {
        return match remove(home, config, root, created, CancellationToken::new()).await {
            Ok(()) => Err(error),
            Err(cleanup) => Err(anyhow!("{error}; rollback also failed: {cleanup}")),
        };
    }
    Ok(json!({"workspaceId":id,"action":"restore","worktree":worktree}))
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn fixture() -> Result<(tempfile::TempDir, PathBuf, PathBuf)> {
        let temp = tempfile::tempdir()?;
        let repo = temp.path().join("repository");
        let home = temp.path().join("paseo");
        tokio::fs::create_dir(&repo).await?;
        let c = CancellationToken::new();
        git(&repo, &["init", "-b", "main"], &c).await?;
        git(
            &repo,
            &["config", "user.email", "worktree-test@example.invalid"],
            &c,
        )
        .await?;
        git(&repo, &["config", "user.name", "Worktree Test"], &c).await?;
        git(&repo, &["commit", "--allow-empty", "-m", "initial"], &c).await?;
        Ok((temp, repo, home))
    }
    #[test]
    fn namespace_collisions_preserve_branch_segments() {
        let names = ["feature".into(), "topic/child".into()]
            .into_iter()
            .collect();
        assert_eq!(unique_branch("feature/task", &names), "feature-1/task");
        assert_eq!(unique_branch("topic", &names), "topic-1");
        assert_eq!(
            slugify("Feature/My New Thing!").unwrap(),
            "feature-my-new-thing"
        );
    }
    #[test]
    fn default_worktree_names_are_mnemonic_adjective_noun_slugs() {
        let adjectives: Vec<_> = include_str!("name_id_adjectives.txt").lines().collect();
        let nouns: Vec<_> = include_str!("name_id_nouns.txt").lines().collect();
        for _ in 0..32 {
            let id = create_name_id();
            let (adjective, noun) = id.split_once('-').unwrap();
            assert!(adjectives.contains(&adjective) && nouns.contains(&noun), "{id}");
            assert_eq!(slugify(&id).unwrap(), id);
            assert!(validate_slug(&id).is_ok());
        }
    }
    #[tokio::test]
    async fn branch_off_records_exact_base_and_does_not_track() -> Result<()> {
        let (_temp, repo, home) = fixture().await?;
        let c = CancellationToken::new();
        git(
            &repo,
            &["update-ref", "refs/remotes/upstream/main", "HEAD"],
            &c,
        )
        .await?;
        let result = create_from_source(&home,&json!({}),&repo,"feature",&json!({"kind":"branch-off","baseBranch":"refs/remotes/upstream/main","branchName":"feature"}),c.clone()).await?;
        let path = Path::new(required(&result, "worktreePath")?);
        assert_eq!(result["comparisonBaseRef"], "refs/remotes/upstream/main");
        assert!(
            optional(&repo, &["config", "--get", "branch.feature.remote"], &c)
                .await?
                .is_none()
        );
        let metadata = super::super::runtime::read_worktree_metadata(path)
            .await?
            .unwrap();
        assert_eq!(metadata["baseRef"], "refs/remotes/upstream/main");
        assert_eq!(
            project_hash(path, c.clone()).await?,
            project_hash(&repo, c.clone()).await?
        );
        assert_eq!(list(&home, &json!({}), &repo, c).await?.len(), 1);
        Ok(())
    }
    #[tokio::test]
    async fn checkout_checked_out_branch_copies_without_modifying_original() -> Result<()> {
        let (_temp, repo, home) = fixture().await?;
        let c = CancellationToken::new();
        let original = git(&repo, &["rev-parse", "main"], &c).await?;
        let result = create_from_source(
            &home,
            &json!({}),
            &repo,
            "checkout",
            &json!({"kind":"checkout-branch","branchName":"main"}),
            c.clone(),
        )
        .await?;
        assert_eq!(result["branchName"], "main-1");
        assert_eq!(git(&repo, &["rev-parse", "main"], &c).await?, original);
        let path = Path::new(required(&result, "worktreePath")?);
        remove(&home, &json!({}), &repo, path, c.clone()).await?;
        assert!(branch_exists(&repo, "main-1", &c).await?);
        let restored = create_from_source(
            &home,
            &json!({}),
            &repo,
            "checkout",
            &json!({"kind":"restore","branchName":"main-1","baseRef":"refs/heads/main"}),
            c,
        )
        .await?;
        assert_eq!(restored["worktreePath"], result["worktreePath"]);
        Ok(())
    }
    #[tokio::test]
    async fn collisions_and_foreign_directories_are_not_removed() -> Result<()> {
        let (_temp, repo, home) = fixture().await?;
        let c = CancellationToken::new();
        let root = project_root(&home, &json!({}), &repo, c.clone()).await?;
        tokio::fs::create_dir_all(root.join("feature")).await?;
        tokio::fs::write(root.join("feature/user.txt"), b"keep me").await?;
        let result = create_from_source(
            &home,
            &json!({}),
            &repo,
            "feature",
            &json!({"kind":"branch-off","baseBranch":"main","branchName":"feature"}),
            c.clone(),
        )
        .await?;
        assert!(required(&result, "worktreePath")?.ends_with("feature-1"));
        assert!(
            remove(&home, &json!({}), &repo, &repo, c.clone())
                .await
                .is_err()
        );
        assert_eq!(
            tokio::fs::read(root.join("feature/user.txt")).await?,
            b"keep me"
        );
        #[cfg(unix)]
        {
            let escape = root.join("escape");
            std::os::unix::fs::symlink(&repo, &escape)?;
            assert_eq!(
                ownership(&home, &json!({}), &escape.join("missing"), c.clone()).await?["allowed"],
                false
            );
            assert!(remove(&home, &json!({}), &repo, &escape, c).await.is_err());
        }
        Ok(())
    }
    #[tokio::test]
    async fn change_request_fetch_uses_private_ref_and_preserves_existing_branch() -> Result<()> {
        let (_temp, repo, home) = fixture().await?;
        let c = CancellationToken::new();
        git(&repo, &["branch", "feature"], &c).await?;
        git(
            &repo,
            &["remote", "add", "origin", repo.to_str().unwrap()],
            &c,
        )
        .await?;
        let before = git(&repo, &["rev-parse", "feature"], &c).await?;
        let result = create_from_source(&home,&json!({}),&repo,"request",&json!({"kind":"checkout-change-request","forge":"github","changeRequestNumber":7,"headRef":"feature","baseRefName":"main","checkoutRefs":[{"remoteName":"origin","remoteRef":"refs/heads/feature"}]}),c.clone()).await?;
        assert_eq!(result["branchName"], "feature-1");
        assert_eq!(git(&repo, &["rev-parse", "feature"], &c).await?, before);
        assert!(
            git(
                &repo,
                &[
                    "for-each-ref",
                    "--format=%(refname)",
                    "refs/paseo/worktree-create/"
                ],
                &c
            )
            .await?
            .is_empty()
        );
        let metadata = super::super::runtime::read_worktree_metadata(Path::new(required(
            &result,
            "worktreePath",
        )?))
        .await?
        .unwrap();
        assert_eq!(
            metadata["changeRequestLookupTarget"]["localBranchName"],
            "feature-1"
        );
        Ok(())
    }
    #[tokio::test]
    async fn base_and_branch_validation_use_source_error_texts() -> Result<()> {
        let (_temp, repo, _home) = fixture().await?;
        let c = CancellationToken::new();
        let error = |result: Result<String>| result.unwrap_err().to_string();
        assert_eq!(
            error(resolve_base(&repo, "origin/missing", &c).await),
            "Base branch not found: refs/remotes/origin/missing"
        );
        assert_eq!(
            error(resolve_base(&repo, " missing ", &c).await),
            "Base branch not found: missing"
        );
        assert_eq!(resolve_base(&repo, " main ", &c).await?, "refs/heads/main");
        assert_eq!(error(normalize_base("  ")), "Base branch is required");
        assert_eq!(
            error(normalize_base("refs/heads/")),
            "Base branch is required when creating a Paseo worktree"
        );
        assert_eq!(
            error(normalize_base("origin/HEAD")),
            "Base branch cannot be HEAD when creating a Paseo worktree"
        );
        assert_eq!(
            validate_branch(&repo, "-x", &c)
                .await
                .unwrap_err()
                .to_string(),
            "Invalid branch name: Git rejected ref name '-x'"
        );
        Ok(())
    }
    #[tokio::test]
    async fn change_request_failures_and_push_remotes_match_source() -> Result<()> {
        let (_temp, repo, home) = fixture().await?;
        let c = CancellationToken::new();
        git(&repo, &["branch", "feature"], &c).await?;
        git(
            &repo,
            &["remote", "add", "origin", repo.to_str().unwrap()],
            &c,
        )
        .await?;
        let missing = create_from_source(&home,&json!({}),&repo,"missing",&json!({"kind":"checkout-change-request","forge":"github","changeRequestNumber":7,"headRef":"feature","baseRefName":"main","checkoutRefs":[{"remoteName":"origin","remoteRef":"refs/heads/absent"}]}),c.clone()).await.unwrap_err().to_string();
        assert!(
            missing.starts_with(
                "Unable to fetch change request refs for worktree branch feature-1: origin refs/heads/absent\n"
            ),
            "{missing}"
        );
        // Checking the same change request out twice re-points one `paseo-pr-<n>` remote.
        for slug in ["first", "second"] {
            create_from_source(&home,&json!({}),&repo,slug,&json!({"kind":"checkout-change-request","forge":"github","changeRequestNumber":8,"headRef":"feature","baseRefName":"main","pushRemoteUrl":repo.to_str().unwrap(),"checkoutRefs":[{"remoteName":"origin","remoteRef":"refs/heads/feature"}]}),c.clone()).await?;
        }
        let remotes = git(&repo, &["remote"], &c).await?;
        assert_eq!(
            remotes.lines().collect::<Vec<_>>(),
            ["origin", "paseo-pr-8"]
        );
        Ok(())
    }
    #[tokio::test]
    async fn failed_post_create_rolls_back_only_allocated_branch() -> Result<()> {
        let (_temp, repo, home) = fixture().await?;
        let c = CancellationToken::new();
        // Opening a directory as the source config makes the real copy fail after worktree add.
        tokio::fs::create_dir(repo.join("paseo.json")).await?;
        git(&repo, &["branch", "feature"], &c).await?;
        let before = git(&repo, &["rev-parse", "feature"], &c).await?;
        let result = create_from_source(
            &home,
            &json!({}),
            &repo,
            "feature",
            &json!({"kind":"branch-off","baseBranch":"main","branchName":"feature"}),
            c.clone(),
        )
        .await;
        assert!(result.is_err());
        assert_eq!(git(&repo, &["rev-parse", "feature"], &c).await?, before);
        assert!(!branch_exists(&repo, "feature-1", &c).await?);
        assert!(list(&home, &json!({}), &repo, c).await?.is_empty());
        Ok(())
    }
    #[tokio::test]
    async fn recovery_restores_saved_subdirectory_and_branchless_workspace() -> Result<()> {
        let (_temp, repo, home) = fixture().await?;
        let c = CancellationToken::new();
        tokio::fs::create_dir(repo.join("nested")).await?;
        tokio::fs::write(repo.join("nested/file.txt"), b"saved").await?;
        git(&repo, &["add", "nested/file.txt"], &c).await?;
        git(&repo, &["commit", "-m", "nested project"], &c).await?;
        let created = create_from_source(&home,&json!({}),&repo,"recover",&json!({"kind":"restore-from-base","branchName":"restored/original","baseRef":"refs/heads/main"}),c.clone()).await?;
        let path = Path::new(required(&created, "worktreePath")?);
        let workspace = json!({"workspaceId":"recovery","projectId":"project","kind":"worktree","cwd":path.join("nested"),"worktreeRoot":path,"mainRepoRoot":repo,"archivedAt":"2026-01-01T00:00:00.000Z","displayName":"Recovery","branch":null,"baseBranch":"refs/heads/main"});
        let project = json!({"rootPath":repo});
        remove(&home, &json!({}), &repo, path, c.clone()).await?;
        let inspected = inspect_recovery("recovery", Some(&workspace), Some(&project)).await?;
        assert_eq!(inspected["action"], "restore");
        let recovered = restore(&home, &json!({}), &workspace, &project, c).await?;
        assert_eq!(recovered["action"], "restore");
        assert_eq!(
            tokio::fs::read(path.join("nested/file.txt")).await?,
            b"saved"
        );
        assert_eq!(
            inspect_recovery("recovery", Some(&workspace), Some(&project)).await?["action"],
            "unarchive"
        );
        Ok(())
    }
    #[test]
    fn porcelain_preserves_newlines_in_worktree_paths() -> Result<()> {
        let entries =
            parse_worktrees(b"worktree /tmp/a\nb\0HEAD abc\0branch refs/heads/feature\0\0")?;
        assert_eq!(entries[0]["path"], "/tmp/a\nb");
        assert_eq!(entries[0]["branchName"], "feature");
        Ok(())
    }
    #[tokio::test]
    async fn wrapper_rollback_preserves_history_and_rejects_replaced_directory() -> Result<()> {
        let (_temp, repo, home) = fixture().await?;
        let c = CancellationToken::new();
        let engine = ForgeEngine::new();
        let input = json!({"action":"branch-off","refName":"main","branchName":"wrapper","worktreeSlug":"wrapper"});
        let first = create(
            &home,
            &json!({}),
            &repo,
            &input,
            &engine,
            &BTreeMap::new(),
            c.clone(),
        )
        .await?;
        let path = PathBuf::from(required(&first["worktree"], "worktreePath")?);
        rollback_created(&home, &json!({}), &first, c.clone()).await?;
        assert!(!is_directory(&path).await?);
        assert!(branch_exists(&repo, "wrapper", &c).await?);
        let second = create(
            &home,
            &json!({}),
            &repo,
            &input,
            &engine,
            &BTreeMap::new(),
            c.clone(),
        )
        .await?;
        let path = PathBuf::from(required(&second["worktree"], "worktreePath")?);
        tokio::fs::rename(&path, path.with_file_name("moved-worktree")).await?;
        tokio::fs::create_dir(&path).await?;
        tokio::fs::write(path.join("user.txt"), b"unrelated").await?;
        assert!(
            rollback_created(&home, &json!({}), &second, c)
                .await
                .is_err()
        );
        assert_eq!(tokio::fs::read(path.join("user.txt")).await?, b"unrelated");
        Ok(())
    }
    #[tokio::test]
    async fn seeding_project_configuration_never_overwrites_target() -> Result<()> {
        let (_temp, repo, home) = fixture().await?;
        tokio::fs::create_dir(&home).await?;
        tokio::fs::write(repo.join("paseo.json"), b"{\"source\":true}").await?;
        seed_paseo_config_file(&repo, &home).await?;
        assert_eq!(
            tokio::fs::read(home.join("paseo.json")).await?,
            b"{\"source\":true}"
        );
        tokio::fs::write(home.join("paseo.json"), b"{\"target\":true}").await?;
        seed_paseo_config_file(&repo, &home).await?;
        assert_eq!(
            tokio::fs::read(home.join("paseo.json")).await?,
            b"{\"target\":true}"
        );
        Ok(())
    }
}
