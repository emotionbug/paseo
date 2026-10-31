use super::*;
impl ForgeEngine {
    async fn optional_git(
        &self,
        cwd: &Path,
        env: &BTreeMap<String, String>,
        cancel: &CancellationToken,
        args: &[&str],
    ) -> Result<Option<String>> {
        let result = self.raw(cwd, env, cancel, "git", strings(args), 30).await?;
        Ok((result.status == 0)
            .then(|| String::from_utf8_lossy(&result.stdout).trim().to_owned())
            .filter(|v| !v.is_empty()))
    }
    /// Resolves tracked/pushed head and persisted fork binding, never client-supplied facts.
    pub async fn lookup_target(
        &self,
        cwd: &Path,
        env: &BTreeMap<String, String>,
        cancel: CancellationToken,
    ) -> Result<Option<Value>> {
        let branch = self
            .local_git(cwd, env, &cancel, &["branch", "--show-current"])
            .await?;
        if branch.is_empty() {
            return Ok(None);
        }
        let gitdir = self
            .local_git(cwd, env, &cancel, &["rev-parse", "--absolute-git-dir"])
            .await?;
        let metadata = match tokio::fs::read(Path::new(&gitdir).join("paseo/worktree.json")).await {
            Ok(bytes) => {
                crate::projects::runtime::parse_metadata(&serde_json::from_slice::<Value>(&bytes)?)?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Value::Null,
            Err(e) => return Err(e.into()),
        };
        let hint = &metadata["changeRequestLookupTarget"];
        let owner = s(hint, "headRepositoryOwner");
        let bound = if !s(hint, "localBranchName").is_empty() {
            s(hint, "localBranchName") == branch
        } else if owner.is_empty() {
            s(hint, "headRef") == branch
        } else {
            format!("{owner}/{}", s(hint, "headRef")) == branch
                || (owner
                    .trim()
                    .to_lowercase()
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-')
                    && format!("{}/{}", owner.trim().to_lowercase(), s(hint, "headRef")) == branch)
        };
        let mut target = json!({"headRef":branch});
        if bound && !s(hint, "headRef").is_empty() {
            target["headRef"] = hint["headRef"].clone();
            if !owner.is_empty() {
                target["headRepositoryOwner"] = owner.into();
            }
        } else {
            let remote = self
                .optional_git(
                    cwd,
                    env,
                    &cancel,
                    &["config", "--get", &format!("branch.{branch}.remote")],
                )
                .await?;
            let merge = if remote.is_some() {
                self.optional_git(
                    cwd,
                    env,
                    &cancel,
                    &["config", "--get", &format!("branch.{branch}.merge")],
                )
                .await?
            } else {
                None
            };
            let origin = self
                .optional_git(cwd, env, &cancel, &["config", "--get", "remote.origin.url"])
                .await?;
            let mut base = metadata["baseRef"]
                .as_str()
                .or(metadata["baseRefName"].as_str())
                .map(str::to_owned);
            // `storedBaseRef ?? resolveBaseRef` (the repository default branch).
            if base.is_none() {
                base = crate::git::command::default_branch(cwd, env, cancel.clone()).await?;
            }
            let base = base.as_deref().map(crate::git::command::branch_name);
            if let (Some(remote), Some(tracked)) = (
                remote.as_deref(),
                // `parseBranchMergeHeadRef`: a trimmed, non-empty refs/heads/ name.
                merge
                    .as_deref()
                    .and_then(|v| v.strip_prefix("refs/heads/"))
                    .map(str::trim)
                    .filter(|v| !v.is_empty()),
            ) {
                let url = self
                    .optional_git(cwd, env, &cancel, &["remote", "get-url", remote])
                    .await?;
                let remote_repo = url
                    .as_deref()
                    .and_then(parse_remote)
                    .map(|r| r.path)
                    .filter(|p| p.split('/').count() == 2);
                let origin_repo = origin.as_deref().and_then(parse_remote).map(|r| r.path);
                let same = remote_repo
                    .as_ref()
                    .zip(origin_repo.as_ref())
                    .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b));
                let owner = remote_repo
                    .as_deref()
                    .filter(|_| !same)
                    .and_then(|v| v.split('/').next());
                let identifies = branch == tracked
                    || owner.is_some_and(|o| {
                        branch == format!("{o}/{tracked}")
                            || branch == format!("{}/{tracked}", o.to_lowercase())
                    });
                if Some(tracked) != base || identifies {
                    target["headRef"] = tracked.into();
                    if let Some(owner) = owner {
                        target["headRepositoryOwner"] = owner.into();
                    }
                }
            }
            if s(&target, "headRef") == branch && s(&target, "headRepositoryOwner").is_empty() {
                if let Some(remote) = self
                    .optional_git(
                        cwd,
                        env,
                        &cancel,
                        &["config", "--get", &format!("branch.{branch}.pushRemote")],
                    )
                    .await?
                {
                    let spec = self
                        .optional_git(
                            cwd,
                            env,
                            &cancel,
                            &["config", "--get", &format!("remote.{remote}.push")],
                        )
                        .await?;
                    // `parseHeadPushRefspec`: only `[+]HEAD:refs/heads/<name>` names a head.
                    let pushed = spec
                        .as_deref()
                        .map(|v| v.trim())
                        .map(|v| v.strip_prefix('+').unwrap_or(v))
                        .and_then(|v| v.strip_prefix("HEAD:refs/heads/"))
                        .map(str::trim)
                        .filter(|v| !v.is_empty());
                    if let Some(pushed) = pushed.filter(|p| *p != branch) {
                        let url = self
                            .optional_git(cwd, env, &cancel, &["remote", "get-url", &remote])
                            .await?;
                        let remote_repo = url
                            .as_deref()
                            .and_then(parse_remote)
                            .filter(|r| cloud_forge(&r.host) == Some("github"))
                            .map(|r| r.path);
                        let origin_repo = origin
                            .as_deref()
                            .and_then(parse_remote)
                            .filter(|r| cloud_forge(&r.host) == Some("github"))
                            .map(|r| r.path);
                        let same = remote_repo
                            .as_ref()
                            .zip(origin_repo.as_ref())
                            .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b));
                        let owner = remote_repo
                            .as_deref()
                            .filter(|_| !same)
                            .and_then(|v| v.split('/').next());
                        if Some(pushed) != base || owner.is_some() {
                            target["headRef"] = pushed.into();
                            if let Some(owner) = owner {
                                target["headRepositoryOwner"] = owner.into();
                            }
                        }
                    }
                }
            }
        }
        if let Some(sha) = self
            .optional_git(cwd, env, &cancel, &["rev-parse", "HEAD"])
            .await?
        {
            target["headSha"] = sha.into();
        }
        Ok(Some(target))
    }
}
