//! Read-only checkout and commit diffs. All git execution is cancellable and local.
use super::command;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

mod history;
mod parse;
#[cfg(test)]
mod tests;
pub use history::list_commits;

const FILE_LIMIT: usize = 1024 * 1024;
const TOTAL_LIMIT: usize = 2 * FILE_LIMIT;
const INVENTORY_LIMIT: usize = 20 * FILE_LIMIT;
const TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug)]
struct Change {
    path: String,
    old_path: Option<String>,
    status: char,
    untracked: bool,
}
#[derive(Clone, Copy, Debug, Default)]
struct Stat {
    additions: u64,
    deletions: u64,
    binary: bool,
}

async fn git(
    cwd: &Path,
    args: &[String],
    env: &BTreeMap<String, String>,
    cancel: &CancellationToken,
    limit: usize,
) -> Result<Vec<u8>> {
    command::checked(cwd, args, env, cancel.clone(), limit, TIMEOUT).await
}
fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| (*v).to_owned()).collect()
}
fn readonly(env: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut result = env.clone();
    result.insert("GIT_OPTIONAL_LOCKS".into(), "0".into());
    result
}
async fn root(
    cwd: &Path,
    env: &BTreeMap<String, String>,
    cancel: &CancellationToken,
) -> Result<PathBuf> {
    command::require_worktree_root(cwd, env, cancel.clone()).await
}
fn diff_args(refs: &[String], whitespace: bool, option: &str) -> Vec<String> {
    let mut result = args(&[
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
        "--src-prefix=a/",
        "--dst-prefix=b/",
        "-M",
    ]);
    if whitespace {
        result.push("-w".into());
    }
    result.push(option.into());
    result.extend_from_slice(refs);
    result
}
fn file(change: &Change, stat: Stat, status: &str) -> Value {
    let mut result = json!({"path":change.path,"isNew":change.status == 'A',"isDeleted":change.status == 'D',"additions":stat.additions,"deletions":stat.deletions,"hunks":[],"status":status});
    if let Some(old) = &change.old_path {
        result["oldPath"] = json!(old);
    }
    result
}
fn overflow(error: &anyhow::Error) -> bool {
    error.chain().any(|e| {
        e.to_string()
            .starts_with("subprocess stdout exceeded output limit")
    })
}
fn structured_limit() -> usize {
    // Invert both legacy base64 and Noise protected-record expansion, then
    // reserve the same 1 MiB outer-envelope headroom as the source daemon.
    let capacity = (32 * FILE_LIMIT / 4) * 3;
    let mut protected = capacity;
    while protected + protected.div_ceil(65535 - 16 - 6).max(1) * 24 > capacity {
        protected = capacity - protected.div_ceil(65535 - 16 - 6).max(1) * 24;
    }
    protected.min(capacity - 40) - FILE_LIMIT
}

/// Response cwd remains the caller's cwd; Git paths are rooted at the checkout.
/// Operational errors are source-compatible typed payloads; cancellation propagates.
pub async fn read(
    cwd: &Path,
    compare: &Value,
    env: &BTreeMap<String, String>,
    cancel: CancellationToken,
) -> Result<Value> {
    let result = read_inner(cwd, compare, env, &cancel).await;
    if cancel.is_cancelled() {
        bail!("git diff cancelled");
    }
    match result {
        Ok(mut files) => {
            files.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
            if serde_json::to_vec(&files)?.len() > structured_limit() {
                Ok(
                    json!({"cwd":cwd,"files":[],"diffTooLarge":true,"error":{"code":"UNKNOWN","message":"Diff too large to display"}}),
                )
            } else {
                Ok(json!({"cwd":cwd,"files":files,"error":null}))
            }
        }
        Err(error) => {
            let message = error.to_string();
            let code = if error.downcast_ref::<command::NotGitRepo>().is_some() {
                "NOT_GIT_REPO"
            } else {
                "UNKNOWN"
            };
            Ok(json!({"cwd":cwd,"files":[],"error":{"code":code,"message":message}}))
        }
    }
}

async fn read_inner(
    cwd: &Path,
    compare: &Value,
    env: &BTreeMap<String, String>,
    cancel: &CancellationToken,
) -> Result<Vec<Value>> {
    let mut env = readonly(env);
    let cwd = root(cwd, &env, cancel).await?;
    let mode = compare["mode"]
        .as_str()
        .context("Missing diff comparison mode")?;
    let whitespace = compare["ignoreWhitespace"].as_bool() == Some(true);
    let summary = compare["detail"].as_str() == Some("summary");
    let mut refs = Vec::new();
    match mode {
        "uncommitted" => {
            let head = command::run(
                &cwd,
                &args(&["rev-parse", "--verify", "HEAD"]),
                &env,
                cancel.clone(),
                INVENTORY_LIMIT,
                TIMEOUT,
            )
            .await?;
            if head.status == 0 {
                refs.push("HEAD".into());
            } else {
                // hash-object computes the correct empty-tree ID for SHA-1 and
                // SHA-256 repositories, without writing an object or the index.
                let hash = git(
                    &cwd,
                    &args(&["hash-object", "-t", "tree", "--", null_device()]),
                    &env,
                    cancel,
                    1024,
                )
                .await?;
                refs.push(String::from_utf8(hash)?.trim().to_owned());
            }
        }
        "base" | "all" => {
            let requested = compare["baseRef"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let Some(base) =
                command::resolve_diff_base(&cwd, requested, &env, cancel.clone()).await?
            else {
                return Ok(Vec::new());
            };
            let merged = command::run(
                &cwd,
                &args(&["merge-base", &base, "HEAD"]),
                &env,
                cancel.clone(),
                INVENTORY_LIMIT,
                TIMEOUT,
            )
            .await?;
            refs.push(if merged.status == 0 {
                String::from_utf8(merged.stdout)?.trim().to_owned()
            } else {
                base
            });
            if mode == "base" {
                refs.push("HEAD".into());
            }
        }
        _ => bail!("Invalid diff comparison mode: {mode}"),
    }
    // This guard owns the private index through the entire read, including all
    // error/cancellation paths. The real index is never written.
    let _index = if mode == "all" {
        private_index(&cwd, &refs[0], &mut env, cancel).await?
    } else {
        None
    };
    let mut inventory_args = diff_args(&refs, whitespace, "--name-status");
    inventory_args.push("-z".into());
    let mut changes =
        parse::changes(&git(&cwd, &inventory_args, &env, cancel, INVENTORY_LIMIT).await?)?;
    let mut stat_args = diff_args(&refs, whitespace, "--numstat");
    stat_args.push("-z".into());
    let stats = parse::stats(&git(&cwd, &stat_args, &env, cancel, TOTAL_LIMIT).await?)?;
    if mode != "base" {
        let untracked = git(
            &cwd,
            &args(&["ls-files", "-z", "--others", "--exclude-standard"]),
            &env,
            cancel,
            INVENTORY_LIMIT,
        )
        .await?;
        for path in untracked.split(|b| *b == 0).filter(|p| !p.is_empty()) {
            let path = String::from_utf8_lossy(path).into_owned();
            if !changes.iter().any(|c| c.path == path) {
                changes.push(Change {
                    path,
                    old_path: None,
                    status: 'A',
                    untracked: true,
                });
            }
        }
    }
    changes.sort_by(|a, b| a.path.cmp(&b.path));
    let format = if !summary && changes.iter().any(|change| change.untracked) {
        let config = command::run(
            &cwd,
            &args(&[
                "config",
                "-z",
                "--get-regexp",
                "^(core\\.quotepath|core\\.abbrev|extensions\\.objectformat)$",
            ]),
            &env,
            cancel.clone(),
            INVENTORY_LIMIT,
            TIMEOUT,
        )
        .await?;
        if config.status != 0 && config.status != 1 {
            bail!("{}", config.stderr);
        }
        parse::patch_format(&config.stdout)
    } else {
        parse::PatchFormat::default()
    };
    let mut files = Vec::with_capacity(changes.len());
    let mut total = 0;
    let mut serialized = 2;
    for change in changes {
        if cancel.is_cancelled() {
            bail!("git diff cancelled");
        }
        let value = if change.untracked {
            untracked_file(&cwd, &change, summary, &mut total, &format, cancel).await?
        } else {
            let stat = stats.get(&change.path).copied().unwrap_or_default();
            if whitespace
                && change.status == 'M'
                && !stat.binary
                && stat.additions == 0
                && stat.deletions == 0
            {
                continue;
            }
            if summary || stat.binary {
                file(&change, stat, if stat.binary { "binary" } else { "ok" })
            } else if total >= TOTAL_LIMIT {
                file(&change, stat, "too_large")
            } else {
                let mut patch_args = diff_args(&refs, whitespace, "--patch");
                patch_args.push("--".into());
                if let Some(old) = &change.old_path {
                    patch_args.push(format!(":(literal){old}"));
                }
                patch_args.push(format!(":(literal){}", change.path));
                match git(&cwd, &patch_args, &env, cancel, FILE_LIMIT).await {
                    Ok(bytes) if total + bytes.len() <= TOTAL_LIMIT => {
                        total += bytes.len();
                        let mut value = file(&change, stat, "ok");
                        parse::apply_patch(&mut value, &bytes, &change.path)?;
                        value
                    }
                    Ok(_) => file(&change, stat, "too_large"),
                    Err(error) if overflow(&error) => file(&change, stat, "too_large"),
                    Err(error) => return Err(error),
                }
            }
        };
        serialized += serde_json::to_vec(&value)?.len() + usize::from(!files.is_empty());
        files.push(value);
        // Avoid building arbitrarily many JSON line objects when the wire
        // envelope can no longer carry this response. read maps this to the
        // source's all-or-nothing diffTooLarge result.
        if serialized > structured_limit() {
            break;
        }
    }
    Ok(files)
}

fn null_device() -> &'static str {
    if cfg!(windows) { "NUL" } else { "/dev/null" }
}

async fn private_index(
    cwd: &Path,
    base: &str,
    env: &mut BTreeMap<String, String>,
    cancel: &CancellationToken,
) -> Result<Option<tempfile::TempDir>> {
    let untracked = git(
        cwd,
        &args(&["ls-files", "-z", "--others", "--exclude-standard"]),
        env,
        cancel,
        INVENTORY_LIMIT,
    )
    .await?;
    let paths: Vec<_> = untracked
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .collect();
    let mut entries = Vec::new();
    for chunk in paths.chunks(64) {
        let mut command = args(&["ls-tree", "-r", "-z", base, "--"]);
        command.extend(
            chunk
                .iter()
                .map(|p| format!(":(literal){}", String::from_utf8_lossy(p))),
        );
        entries.extend(git(cwd, &command, env, cancel, INVENTORY_LIMIT).await?);
    }
    if entries.is_empty() {
        return Ok(None);
    }
    let directory = tokio::task::spawn_blocking(|| {
        tempfile::Builder::new()
            .prefix("paseo-all-changes-")
            .tempdir()
    })
    .await??;
    let index = directory.path().join("index");
    let original = git(
        cwd,
        &args(&["rev-parse", "--git-path", "index"]),
        env,
        cancel,
        INVENTORY_LIMIT,
    )
    .await?;
    let original = PathBuf::from(String::from_utf8(original)?.trim_end_matches(['\r', '\n']));
    let original = if original.is_absolute() {
        original
    } else {
        cwd.join(original)
    };
    match tokio::fs::copy(original, &index).await {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    env.insert(
        "GIT_INDEX_FILE".into(),
        index.to_string_lossy().into_owned(),
    );
    for entry in entries.split(|b| *b == 0).filter(|p| !p.is_empty()) {
        let line = String::from_utf8_lossy(entry);
        let (header, path) = line.split_once('\t').context("Malformed git tree entry")?;
        let mut fields = header.split_whitespace();
        let mode = fields.next().context("Missing tree mode")?;
        let _kind = fields.next().context("Missing tree kind")?;
        let object = fields.next().context("Missing tree object")?;
        // The three separate cacheinfo arguments preserve commas in paths.
        git(
            cwd,
            &args(&["update-index", "--add", "--cacheinfo", mode, object, path]),
            env,
            cancel,
            INVENTORY_LIMIT,
        )
        .await?;
    }
    Ok(Some(directory))
}

async fn untracked_file(
    cwd: &Path,
    change: &Change,
    summary: bool,
    total: &mut usize,
    format: &parse::PatchFormat,
    cancel: &CancellationToken,
) -> Result<Value> {
    let path = cwd.join(&change.path);
    let meta = tokio::fs::symlink_metadata(&path).await?;
    let mut content = Vec::new();
    let oversized = meta.len() > FILE_LIMIT as u64;
    if meta.file_type().is_symlink() {
        content = tokio::fs::read_link(&path)
            .await?
            .as_os_str()
            .as_encoded_bytes()
            .to_vec();
    } else if meta.is_file() {
        let mut options = tokio::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        options.custom_flags(libc::O_NOFOLLOW);
        let file = options.open(&path).await?;
        let maximum = if oversized { 8192 } else { FILE_LIMIT + 1 };
        content.reserve((meta.len() as usize).min(maximum));
        let mut reader = file.take(maximum as u64);
        tokio::select! {
            _ = cancel.cancelled() => bail!("git diff cancelled"),
            result = reader.read_to_end(&mut content) => { result?; }
        }
    } else {
        bail!("Cannot read untracked non-file: {}", change.path);
    }
    let sample = &content[..content.len().min(8192)];
    let suspicious = sample
        .iter()
        .filter(|&&b| b < 7 || (b > 14 && b < 32) || b == 127)
        .count();
    let binary = sample.contains(&0) || (!sample.is_empty() && suspicious * 10 > sample.len() * 3);
    if binary {
        return Ok(file(
            change,
            Stat {
                binary: true,
                ..Stat::default()
            },
            "binary",
        ));
    }
    if oversized || content.len() > FILE_LIMIT {
        return Ok(file(change, Stat::default(), "too_large"));
    }
    let text = String::from_utf8_lossy(&content);
    let count = if text.is_empty() {
        0
    } else {
        text.split_terminator('\n').count()
    };
    let mut value = file(
        change,
        Stat {
            additions: count as u64,
            ..Stat::default()
        },
        "ok",
    );
    if summary {
        return Ok(value);
    }
    // Native added-file parsing is exact: every source line is an addition;
    // missing-final-newline markers do not become displayed lines.
    let header = if count == 1 {
        "@@ -0,0 +1 @@".to_owned()
    } else {
        format!("@@ -0,0 +1,{count} @@")
    };
    let raw_length = parse::added_patch_len(&change.path, &content, count, &header, format);
    if raw_length > FILE_LIMIT || *total + raw_length > TOTAL_LIMIT {
        value["status"] = json!("too_large");
        return Ok(value);
    }
    *total += raw_length;
    if count > 0 {
        let mut lines = Vec::with_capacity(count + 1);
        lines.push(json!({"type":"header","content":header}));
        lines.extend(
            text.split_terminator('\n')
                .map(|line| json!({"type":"add","content":line})),
        );
        value["hunks"] =
            json!([{"oldStart":0,"oldCount":0,"newStart":1,"newCount":count,"lines":lines}]);
    }
    Ok(value)
}

/// One commit against its first parent, including root commits. Binary-only
/// entries return None; metadata-only mode/rename changes remain real entries.
pub async fn commit_file(
    cwd: &Path,
    sha: &str,
    path: &str,
    env: &BTreeMap<String, String>,
    cancel: CancellationToken,
) -> Result<Option<Value>> {
    command::request_ref(sha, "commit")?;
    command::safe_path(path)?;
    let env = readonly(env);
    let cwd = root(cwd, &env, &cancel).await?;
    let inventory = git(
        &cwd,
        &args(&[
            "show",
            sha,
            "--format=",
            "--diff-merges=first-parent",
            "--name-status",
            "-z",
            "-M",
        ]),
        &env,
        &cancel,
        INVENTORY_LIMIT,
    )
    .await?;
    let Some(change) = parse::changes(&inventory)?
        .into_iter()
        .find(|c| c.path == path)
    else {
        return Ok(None);
    };
    let mut command = args(&[
        "show",
        sha,
        "--format=",
        "--diff-merges=first-parent",
        "-M",
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
        "--src-prefix=a/",
        "--dst-prefix=b/",
        "--",
    ]);
    if let Some(old) = &change.old_path {
        command.push(format!(":(literal){old}"));
    }
    command.push(format!(":(literal){path}"));
    let bytes = match git(&cwd, &command, &env, &cancel, FILE_LIMIT).await {
        Ok(bytes) => bytes,
        Err(error) if overflow(&error) => {
            command.truncate(command.iter().position(|a| a == "--").unwrap());
            command.push("--numstat".into());
            command.push("-z".into());
            let stats = parse::stats(&git(&cwd, &command, &env, &cancel, TOTAL_LIMIT).await?)?;
            return Ok(Some(file(
                &change,
                stats.get(path).copied().unwrap_or_default(),
                "too_large",
            )));
        }
        Err(error) => return Err(error),
    };
    if bytes.is_empty() {
        return Ok(None);
    }
    let mut value = file(&change, Stat::default(), "ok");
    if !parse::apply_patch(&mut value, &bytes, path)? {
        return Ok(None);
    }
    if value["status"] == "binary" {
        return Ok(None);
    }
    // Source commit-file parser does not add a status field to successful files.
    value.as_object_mut().unwrap().remove("status");
    Ok(Some(value))
}

/// Snapshot shortstat uses the comparison branch, not an all-diff inventory:
/// an untracked-only checkout with no comparison ref has no shortstat.
pub async fn shortstat(
    cwd: &Path,
    env: &BTreeMap<String, String>,
    cancel: CancellationToken,
) -> Result<Option<Value>> {
    let env = readonly(env);
    let cwd = match root(cwd, &env, &cancel).await {
        Ok(cwd) => cwd,
        Err(error) if cancel.is_cancelled() => return Err(error),
        Err(_) => return Ok(None),
    };
    let Some(branch) = super::facts::current_branch(&cwd, &env, cancel.clone()).await? else {
        return Ok(None);
    };
    let base = command::operation_base(&cwd, None, &env, cancel.clone()).await?;
    let comparison = if let Some(base) = base.filter(|base| base != &branch) {
        if base.starts_with("refs/heads/") || base.starts_with("refs/remotes/") {
            if !command::exists(&cwd, &base, &env, cancel.clone()).await? {
                return Ok(None);
            }
            base
        } else {
            let name = command::branch_name(&base);
            if command::exists(
                &cwd,
                &format!("refs/remotes/origin/{name}"),
                &env,
                cancel.clone(),
            )
            .await?
            {
                format!("origin/{name}")
            } else if command::exists(&cwd, &format!("refs/heads/{name}"), &env, cancel.clone())
                .await?
            {
                name.to_owned()
            } else {
                return Ok(None);
            }
        }
    } else {
        if !command::exists(
            &cwd,
            &format!("refs/remotes/origin/{branch}"),
            &env,
            cancel.clone(),
        )
        .await?
        {
            return Ok(None);
        }
        format!("origin/{branch}")
    };
    let merge = command::run(
        &cwd,
        &args(&["merge-base", "HEAD", &comparison]),
        &env,
        cancel.clone(),
        1024,
        TIMEOUT,
    )
    .await?;
    if merge.status != 0 {
        return Ok(None);
    }
    let merge = String::from_utf8(merge.stdout)?;
    let merge = merge.trim();
    if merge.is_empty() {
        return Ok(None);
    }
    let tracked = git(
        &cwd,
        &args(&[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--shortstat",
            merge,
        ]),
        &env,
        &cancel,
        FILE_LIMIT,
    )
    .await?;
    let mut additions = 0u64;
    let mut deletions = 0u64;
    let text = String::from_utf8(tracked)?;
    let mut previous = "";
    for word in text.split_whitespace() {
        if word.starts_with("insertion") {
            additions = previous.parse()?;
        }
        if word.starts_with("deletion") {
            deletions = previous.parse()?;
        }
        previous = word;
    }
    let paths = git(
        &cwd,
        &args(&["ls-files", "-z", "--others", "--exclude-standard"]),
        &env,
        &cancel,
        INVENTORY_LIMIT,
    )
    .await?;
    for path in paths.split(|b| *b == 0).filter(|p| !p.is_empty()).take(500) {
        if cancel.is_cancelled() {
            bail!("Git shortstat cancelled");
        }
        let path = cwd.join(String::from_utf8_lossy(path).as_ref());
        // Unlike a patch, source shortstat counts a symlink target's contents.
        // Missing/unreadable/special/oversized files contribute no additions.
        let read=async {
            let metadata=tokio::fs::metadata(&path).await?;
            if !metadata.is_file() || metadata.len()>FILE_LIMIT as u64{return Ok::<_,anyhow::Error>(0);}
            let file=tokio::fs::File::open(&path).await?;
            let mut reader=file.take((FILE_LIMIT+1) as u64);
            let mut bytes=Vec::with_capacity((metadata.len() as usize).saturating_add(1).min(FILE_LIMIT+1));
            tokio::select!{_=cancel.cancelled()=>bail!("Git shortstat cancelled"),result=reader.read_to_end(&mut bytes)=>{result?;}}
            if bytes.len()>FILE_LIMIT{return Ok(0);}
            let sample=&bytes[..bytes.len().min(8192)];
            let suspicious=sample.iter().filter(|&&b|b<7||(b>14&&b<32)||b==127).count();
            if sample.contains(&0)||(!sample.is_empty()&&suspicious*10>sample.len()*3){return Ok(0);}
            Ok(bytes.iter().filter(|&&b|b==b'\n').count() as u64+u64::from(!bytes.is_empty()&&!bytes.ends_with(b"\n")))
        }.await;
        if cancel.is_cancelled() {
            bail!("Git shortstat cancelled");
        }
        if let Ok(count) = read {
            additions += count;
        }
    }
    if additions == 0 && deletions == 0 {
        Ok(None)
    } else {
        Ok(Some(json!({"additions":additions,"deletions":deletions})))
    }
}
