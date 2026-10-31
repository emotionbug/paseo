use super::*;
use std::collections::{BTreeMap, HashSet};

/// Complete workspace history plus ten base-context commits. All statistics
/// come from --numstat/--raw; this operation never requests patch bodies.
pub async fn list_commits(
    cwd: &Path,
    env: &BTreeMap<String, String>,
    cancel: CancellationToken,
) -> Result<Value> {
    let env = readonly(env);
    let cwd = root(cwd, &env, &cancel).await?;
    let branch = command::run(
        &cwd,
        &args(&["symbolic-ref", "--quiet", "--short", "HEAD"]),
        &env,
        cancel.clone(),
        INVENTORY_LIMIT,
        TIMEOUT,
    )
    .await?;
    if branch.status == 1 {
        return Ok(json!({"baseRef":null,"commits":[]}));
    }
    if branch.status != 0 {
        bail!("{}", branch.stderr);
    }
    let branch = String::from_utf8(branch.stdout)?.trim().to_owned();
    let base = comparison_base(&cwd, &branch, &env, &cancel).await?;
    let mut commits = Vec::new();
    let mut base_revision = Some("HEAD".to_owned());
    if let Some(base) = &base {
        commits = records(&cwd, &format!("{base}..HEAD"), None, &env, &cancel).await?;
        let merge = command::run(
            &cwd,
            &args(&["merge-base", base, "HEAD"]),
            &env,
            cancel.clone(),
            INVENTORY_LIMIT,
            TIMEOUT,
        )
        .await?;
        base_revision = if merge.status == 0 {
            Some(String::from_utf8(merge.stdout)?.trim().to_owned())
        } else {
            None
        };
    }
    let workspace: HashSet<String> = commits
        .iter()
        .filter_map(|v| v["sha"].as_str().map(str::to_owned))
        .collect();
    if let Some(revision) = base_revision {
        commits.extend(records(&cwd, &revision, Some(10), &env, &cancel).await?);
    }
    if !commits.is_empty() {
        let unpushed = git(
            &cwd,
            &args(&["rev-list", "HEAD", "--not", "--remotes"]),
            &env,
            &cancel,
            INVENTORY_LIMIT,
        )
        .await?;
        let unpushed = String::from_utf8(unpushed)?;
        let unpushed: HashSet<&str> = unpushed.lines().collect();
        for commit in &mut commits {
            let sha = commit["sha"].as_str().context("Missing commit SHA")?;
            let on_remote = !unpushed.contains(sha);
            let on_base = !workspace.contains(sha);
            commit["isOnRemote"] = json!(on_remote);
            commit["isOnBase"] = json!(on_base);
        }
    }
    Ok(json!({"baseRef":base,"commits":commits}))
}

async fn records(
    cwd: &Path,
    revision: &str,
    limit: Option<usize>,
    env: &BTreeMap<String, String>,
    cancel: &CancellationToken,
) -> Result<Vec<Value>> {
    let mut command = args(&[
        "log",
        revision,
        "--diff-merges=first-parent",
        "--format=%x1e%H%x00%h%x00%an%x00%aI%x00%s%x00",
        "--raw",
        "--numstat",
        "-z",
        "-M",
        "--no-ext-diff",
        "--no-textconv",
    ]);
    if let Some(limit) = limit {
        command.push(format!("--max-count={limit}"));
    }
    parse_records(&git(cwd, &command, env, cancel, INVENTORY_LIMIT).await?)
}

async fn comparison_base(
    cwd: &Path,
    branch: &str,
    env: &BTreeMap<String, String>,
    cancel: &CancellationToken,
) -> Result<Option<String>> {
    let Some(base) = command::operation_base(cwd, None, env, cancel.clone()).await? else {
        return Ok(None);
    };
    if command::branch_name(&base) == branch {
        return Ok(None);
    }
    if let Some(resolved) = most_ahead(cwd, &base, env, cancel).await? {
        return Ok(Some(resolved));
    }
    let Some(default) = command::default_branch(cwd, env, cancel.clone()).await? else {
        return Ok(None);
    };
    if command::branch_name(&default) == branch {
        return Ok(None);
    }
    most_ahead(cwd, &default, env, cancel).await
}

async fn most_ahead(
    cwd: &Path,
    base: &str,
    env: &BTreeMap<String, String>,
    cancel: &CancellationToken,
) -> Result<Option<String>> {
    if base.starts_with("refs/heads/") || base.starts_with("refs/remotes/") {
        if !command::exists(cwd, base, env, cancel.clone()).await? {
            bail!("Base ref not found: {base}");
        }
        return Ok(Some(base.to_owned()));
    }
    let name = command::branch_name(base);
    let local = command::exists(cwd, &format!("refs/heads/{name}"), env, cancel.clone()).await?;
    let remote = command::exists(
        cwd,
        &format!("refs/remotes/origin/{name}"),
        env,
        cancel.clone(),
    )
    .await?;
    match (local, remote) {
        (false, false) => Ok(None),
        (true, false) => Ok(Some(name.to_owned())),
        (false, true) => Ok(Some(format!("origin/{name}"))),
        (true, true) => {
            let text = command::text(
                cwd,
                &[
                    "rev-list",
                    "--left-right",
                    "--count",
                    &format!("{name}...origin/{name}"),
                ],
                env,
                cancel.clone(),
            )
            .await?;
            let mut counts = text.split_whitespace();
            let local: u64 = counts
                .next()
                .context("Missing local commit count")?
                .parse()?;
            let remote: u64 = counts
                .next()
                .context("Missing origin commit count")?
                .parse()?;
            Ok(Some(if remote > local {
                format!("origin/{name}")
            } else {
                name.to_owned()
            }))
        }
    }
}
pub(super) fn parse_records(bytes: &[u8]) -> Result<Vec<Value>> {
    let mut result = Vec::new();
    for record in bytes
        .split(|b| *b == 0x1e)
        .filter(|r| !r.iter().all(u8::is_ascii_whitespace))
    {
        let mut fields = record.split(|b| *b == 0);
        let mut header = Vec::with_capacity(5);
        for _ in 0..5 {
            header.push(
                String::from_utf8_lossy(fields.next().context("Incomplete git commit header")?)
                    .into_owned(),
            );
        }
        let mut statuses = BTreeMap::new();
        let mut stats = Vec::new();
        while let Some(field) = fields.next() {
            let field = field.strip_prefix(b"\n").unwrap_or(field);
            if field.is_empty() {
                continue;
            }
            if field.starts_with(b":") {
                let status = field
                    .rsplit(|b| *b == b' ')
                    .next()
                    .and_then(|s| s.first())
                    .copied()
                    .context("Missing raw change status")?;
                let first = fields.next().context("Missing raw change path")?;
                let path = if status == b'R' || status == b'C' {
                    fields.next().context("Missing raw rename destination")?
                } else {
                    first
                };
                let status = match status {
                    b'A' | b'C' => Some("added"),
                    b'M' | b'T' => Some("modified"),
                    b'D' => Some("deleted"),
                    b'R' => Some("renamed"),
                    _ => None,
                };
                if let Some(status) = status {
                    statuses.insert(String::from_utf8_lossy(path).into_owned(), status);
                }
            } else {
                let mut parts = field.splitn(3, |b| *b == b'\t');
                let additions = parts.next().context("Missing history additions")?;
                let deletions = parts.next().context("Missing history deletions")?;
                let mut path = parts.next().context("Missing history numstat path")?;
                if path.is_empty() {
                    fields.next().context("Missing history rename source")?;
                    path = fields
                        .next()
                        .context("Missing history rename destination")?;
                }
                let binary = additions == b"-" || deletions == b"-";
                let additions = if binary {
                    0
                } else {
                    std::str::from_utf8(additions)?.parse::<u64>()?
                };
                let deletions = if binary {
                    0
                } else {
                    std::str::from_utf8(deletions)?.parse::<u64>()?
                };
                stats.push((
                    String::from_utf8_lossy(path).into_owned(),
                    additions,
                    deletions,
                ));
            }
        }
        let files: Vec<Value> = stats
            .into_iter()
            .map(|(path, additions, deletions)| {
                let mut file = json!({"path":path,"additions":additions,"deletions":deletions});
                if let Some(status) = statuses.get(&path) {
                    file["status"] = json!(status);
                }
                file
            })
            .collect();
        result.push(json!({"sha":header[0].trim(),"shortSha":header[1].trim(),"authorName":header[2],"authorDate":header[3].trim(),"subject":header[4],"files":files}));
    }
    Ok(result)
}
