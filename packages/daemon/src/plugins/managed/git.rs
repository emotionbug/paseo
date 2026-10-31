use super::{
    ManagedSources, command,
    reference::{is_commit, redact},
};
use anyhow::{Context, Result, bail};
use std::{collections::BTreeMap, path::Path, sync::LazyLock};

static SHORTHAND: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$").expect("constant Git grammar")
});
static SCP: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^[^/@\s]+@[^:\s]+:.+$").expect("constant SCP grammar"));

pub fn normalize(source: &str) -> Result<String> {
    let expands = source.starts_with("github:") || source.starts_with("git:");
    let source = if let Some(short) = source.strip_prefix("github:") {
        if !SHORTHAND.is_match(short) {
            bail!("Use github:owner/repository for a GitHub plugin source");
        }
        short
    } else if source.starts_with("git:") && !source.starts_with("git://") {
        &source[4..]
    } else {
        source
    };
    if expands && SHORTHAND.is_match(source) {
        return Ok(format!(
            "https://github.com/{}.git",
            source.trim_end_matches(".git")
        ));
    }
    if SCP.is_match(source)
        || url::Url::parse(source)
            .is_ok_and(|url| matches!(url.scheme(), "http" | "https" | "ssh" | "git" | "file"))
    {
        return Ok(source.to_owned());
    }
    bail!("Plugin source is neither an existing directory nor a Git URL: {source}")
}

fn environment(
    env: &BTreeMap<String, String>,
    remote: Option<&str>,
) -> (BTreeMap<String, String>, Option<String>) {
    let mut env = env.clone();
    env.insert("GIT_TERMINAL_PROMPT".into(), "0".into());
    let remote = remote.map(|remote| {
        if redact(remote) == remote {
            remote.to_owned()
        } else {
            let alias = "https://paseo.invalid/plugin.git";
            let index = env
                .get("GIT_CONFIG_COUNT")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(0);
            env.insert("GIT_CONFIG_COUNT".into(), (index + 1).to_string());
            env.insert(
                format!("GIT_CONFIG_KEY_{index}"),
                format!("url.{remote}.insteadOf"),
            );
            env.insert(format!("GIT_CONFIG_VALUE_{index}"), alias.into());
            alias.into()
        }
    });
    (env, remote)
}

fn redacted_error(error: anyhow::Error, remote: &str) -> anyhow::Error {
    let mut message = format!("{error:#}").replace(remote, &redact(remote));
    if let Ok(url) = url::Url::parse(remote) {
        for credential in [Some(url.username()), url.password()]
            .into_iter()
            .flatten()
            .filter(|s| !s.is_empty())
        {
            message = message.replace(credential, "[redacted]");
            let decoded = percent_encoding::percent_decode_str(credential).decode_utf8_lossy();
            message = message.replace(decoded.as_ref(), "[redacted]");
        }
    }
    anyhow::anyhow!(message)
}

pub async fn clone(remote: &str, checkout: &Path, env: &BTreeMap<String, String>) -> Result<()> {
    let (env, alias) = environment(env, Some(remote));
    command(
        "git",
        vec![
            "clone".into(),
            "--no-checkout".into(),
            "--".into(),
            alias.expect("remote supplied"),
            checkout.to_string_lossy().into_owned(),
        ],
        checkout.parent().context("Checkout parent is missing")?,
        env,
    )
    .await
    .map_err(|e| redacted_error(e, remote))?;
    Ok(())
}

pub async fn rev_parse(
    checkout: &Path,
    revision: &str,
    env: &BTreeMap<String, String>,
) -> Result<String> {
    if revision.starts_with('-') {
        bail!("Plugin Git ref cannot start with '-'");
    }
    let (env, _) = environment(env, None);
    let result = command(
        "git",
        vec!["rev-parse".into(), "--verify".into(), revision.into()],
        checkout,
        env,
    )
    .await?;
    let result = result.trim();
    if !is_commit(result) {
        bail!("Invalid Git revision returned by git");
    }
    Ok(result.to_owned())
}

pub async fn requested(
    checkout: &Path,
    requested: Option<&str>,
    env: &BTreeMap<String, String>,
) -> Result<String> {
    let Some(requested) = requested else {
        return rev_parse(checkout, "refs/remotes/origin/HEAD^{commit}", env).await;
    };
    if requested.starts_with('-') {
        bail!("Plugin Git ref cannot start with '-'");
    }
    for name in [
        format!("refs/remotes/origin/{requested}"),
        format!("refs/tags/{requested}"),
    ] {
        // A missing candidate is not a fallback for a failed process launch or transport.
        let (env_git, _) = environment(env, None);
        let result = crate::process::run(
            crate::process::CommandSpec {
                program: "git".into(),
                args: vec![
                    "show-ref".into(),
                    "--verify".into(),
                    "--quiet".into(),
                    name.clone(),
                ],
                cwd: Some(checkout.to_owned()),
                env: env_git,
                clear_env: true,
            },
            1024 * 1024,
            std::time::Duration::from_secs(120),
            tokio_util::sync::CancellationToken::new(),
        )
        .await?;
        if result.status == 0 {
            return rev_parse(checkout, &format!("{name}^{{commit}}"), env).await;
        }
        if result.status != 1 {
            bail!(
                "git show-ref failed (exit {}): {}",
                result.status,
                result.stderr
            );
        }
    }
    rev_parse(checkout, &format!("{requested}^{{commit}}"), env).await
}

pub async fn checkout(root: &Path, commit: &str, env: &BTreeMap<String, String>) -> Result<()> {
    let (env, _) = environment(env, None);
    command(
        "git",
        vec!["checkout".into(), "--detach".into(), commit.into()],
        root,
        env.clone(),
    )
    .await?;
    command(
        "git",
        vec![
            "submodule".into(),
            "update".into(),
            "--init".into(),
            "--recursive".into(),
        ],
        root,
        env,
    )
    .await?;
    Ok(())
}

pub async fn remote_commit(
    sources: &ManagedSources,
    remote: &str,
    requested: Option<&str>,
) -> Result<String> {
    if requested.is_some_and(|r| r.starts_with('-')) {
        bail!("Plugin Git ref cannot start with '-'");
    }
    if let Some(commit) = requested.filter(|r| is_commit(r)) {
        return Ok(commit.to_owned());
    }
    let names = requested
        .map(|r| {
            vec![
                format!("refs/heads/{r}"),
                format!("refs/tags/{r}^{{}}"),
                format!("refs/tags/{r}"),
                r.to_owned(),
            ]
        })
        .unwrap_or_else(|| vec!["HEAD".into()]);
    let (env, alias) = environment(&sources.env, Some(remote));
    let mut args = vec![
        "ls-remote".into(),
        "--symref".into(),
        "--".into(),
        alias.expect("remote supplied"),
    ];
    args.extend(names.iter().cloned());
    let output = command("git", args, &sources.root, env)
        .await
        .map_err(|e| redacted_error(e, remote))?;
    for name in &names {
        if let Some(commit) = output
            .lines()
            .filter_map(|line| line.split_once('\t'))
            .find_map(|(commit, found)| (name == found && is_commit(commit)).then_some(commit))
        {
            return Ok(commit.to_owned());
        }
    }
    if let Some(requested_ref) = requested {
        let staging = sources.staging().await?;
        let mut abandoned = super::StagingCleanup(Some(staging.clone()));
        let checkout = staging.join("checkout");
        let result = async {
            clone(remote, &checkout, &sources.env).await?;
            self::requested(&checkout, Some(requested_ref), &sources.env).await
        }
        .await;
        let cleanup = super::remove_tree(&staging).await;
        if cleanup.is_ok() {
            abandoned.0 = None;
        }
        return match (result, cleanup) {
            (Ok(revision), Ok(())) => Ok(revision),
            (Err(error), _) => Err(error),
            (_, Err(error)) => Err(error),
        };
    }
    bail!("Cannot resolve Git default HEAD")
}
