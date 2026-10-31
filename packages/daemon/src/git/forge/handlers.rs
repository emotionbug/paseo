use super::*;
impl ForgeEngine {
    /// Produces source session envelopes without retaining transport state.
    /// PR creation input must carry the head/base prepared by checkout-git's push/preflight flow.
    pub async fn handle_operation(
        &self,
        message: &Value,
        cwd: &Path,
        env: &BTreeMap<String, String>,
        cancel: CancellationToken,
    ) -> Result<Option<Value>> {
        let kind = s(message, "type");
        let response = match kind {
            "checkout_pr_create_request" => "checkout_pr_create_response",
            "checkout_pr_merge_request" => "checkout_pr_merge_response",
            "checkout_pr_status_request" => "checkout_pr_status_response",
            "pull_request_timeline_request" => "pull_request_timeline_response",
            "checkout.forge.set_auto_merge.request" => "checkout.forge.set_auto_merge.response",
            "checkout.github.set_auto_merge.request" => "checkout.github.set_auto_merge.response",
            "checkout.forge.get_check_details.request" => {
                "checkout.forge.get_check_details.response"
            }
            "checkout.github.get_check_details.request" => {
                "checkout.github.get_check_details.response"
            }
            "forge.search.request" => "forge.search.response",
            "github_search_request" => "github_search_response",
            _ => return Ok(None),
        };
        let request = message
            .get("payload")
            .filter(|v| v.is_object())
            .unwrap_or(message);
        let mut input = request.clone();
        if let Some(object) = input.as_object_mut() {
            object.remove("forge");
            object.remove("status");
        }
        let mut payload = json!({"cwd":cwd.to_string_lossy(),"requestId":request["requestId"]});
        match kind {
            "forge.search.request" | "github_search_request" => {
                let legacy = kind == "github_search_request";
                if legacy {
                    input["forge"] = "github".into();
                }
                let result = self
                    .execute(cwd, "searchIssuesAndPrs", &input, env, cancel.clone())
                    .await;
                payload.as_object_mut().unwrap().remove("cwd");
                match result {
                    Ok(mut value) => {
                        if legacy {
                            for item in value["items"].as_array_mut().into_iter().flatten() {
                                if item["kind"] == "change_request" {
                                    item["kind"] = "pr".into();
                                }
                            }
                        }
                        payload["items"] = value["items"].take();
                        payload["authState"] = value["authState"].take();
                        payload["error"] = Value::Null;
                        if legacy {
                            payload["featuresEnabled"] = value["featuresEnabled"].clone();
                            payload["githubFeaturesEnabled"] = value["featuresEnabled"].clone();
                        }
                    }
                    Err(error) => {
                        payload["items"] = json!([]);
                        // `resolveAuthStateForError`: "error" once a forge resolves,
                        // otherwise "no_remote".
                        payload["authState"] = match self.resolve(cwd, env, cancel.clone()).await
                        {
                            Ok(Some(_)) => "error",
                            Ok(None) => "no_remote",
                            Err(_) => "error",
                        }
                        .into();
                        payload["error"] = error.to_string().into();
                        if legacy {
                            payload["featuresEnabled"] = false.into();
                            payload["githubFeaturesEnabled"] = false.into();
                        }
                    }
                }
            }
            "checkout_pr_status_request" => {
                payload = self.status_payload(cwd, env, cancel.clone()).await;
                payload["cwd"] = cwd.to_string_lossy().as_ref().into();
                payload["requestId"] = request["requestId"].clone();
            }
            "pull_request_timeline_request" => {
                payload["prNumber"] = request["prNumber"].clone();
                payload["items"] = json!([]);
                payload["truncated"] = false.into();
                payload["githubFeaturesEnabled"] = true.into();
                if number(request, "prNumber").is_err()
                    || !valid_repo_segment(s(request, "repoOwner"))
                    || !valid_repo_segment(s(request, "repoName"))
                {
                    payload["error"] = json!({"kind":"unknown","message":"Pull request timeline request has invalid PR identity"});
                } else {
                    match self.resolve(cwd, env, cancel.clone()).await? {
                        None => {
                            payload["githubFeaturesEnabled"] = false.into();
                            payload["authState"] = "no_remote".into();
                            payload["error"] = json!({"kind":"unknown","message":"No supported forge remote is configured for this workspace"});
                        }
                        Some(resolution) => {
                            let auth = self
                                .execute(cwd, "isAuthenticated", &json!({}), env, cancel.clone())
                                .await;
                            if !matches!(auth, Ok(Value::Bool(true))) {
                                payload["githubFeaturesEnabled"] = false.into();
                                if let Err(error) = auth {
                                    // `forgeAuthStateFromError`: anything but a missing CLI
                                    // reads as a sign-in problem.
                                    payload["authState"] = probe_auth_state(&error).into();
                                }
                                let brand = match resolution.forge.as_str() {
                                    "github" => "GitHub",
                                    "gitlab" => "GitLab",
                                    "codeberg" => "Codeberg",
                                    "forgejo" => "Forgejo",
                                    _ => "Gitea",
                                };
                                payload["error"] = json!({"kind":"unknown","message":format!("{brand} CLI is unavailable or not authenticated")});
                            } else {
                                match self
                                    .execute(
                                        cwd,
                                        "getPullRequestTimeline",
                                        &input,
                                        env,
                                        cancel.clone(),
                                    )
                                    .await
                                {
                                    Ok(value) => {
                                        for field in ["prNumber", "items", "truncated", "error"] {
                                            payload[field] = value[field].clone();
                                        }
                                    }
                                    Err(error) => {
                                        let state = auth_state(&error);
                                        payload["error"] =
                                            json!({"kind":"unknown","message":error.to_string()});
                                        payload["githubFeaturesEnabled"] =
                                            (!matches!(state, "cli_missing" | "unauthenticated"))
                                                .into();
                                        payload["authState"] = state.into();
                                    }
                                }
                            }
                        }
                    }
                }
            }
            "checkout.forge.get_check_details.request"
            | "checkout.github.get_check_details.request" => {
                let result = if request["checkRunId"].is_null()
                    && request["workflowRunId"].is_null()
                {
                    Err(anyhow!(
                        "Check details request must address a check by checkRunId or workflowRunId"
                    ))
                } else {
                    self.execute(cwd, "getCheckDetails", &input, env, cancel.clone())
                        .await
                };
                payload["success"] = result.is_ok().into();
                match result {
                    Ok(value) => {
                        payload["details"] = value;
                        payload["error"] = Value::Null;
                    }
                    Err(error) => {
                        payload["details"] = Value::Null;
                        payload["error"] = checkout_error(&error);
                    }
                }
            }
            "checkout_pr_create_request" => {
                if input["base"].is_null() && input["baseRef"].is_string() {
                    input["base"] = input["baseRef"].clone();
                }
                match self
                    .execute(cwd, "createPullRequest", &input, env, cancel.clone())
                    .await
                {
                    Ok(value) => {
                        payload["url"] = value["url"].clone();
                        payload["number"] = value["number"].clone();
                        payload["error"] = Value::Null;
                    }
                    Err(error) => {
                        payload["url"] = Value::Null;
                        payload["number"] = Value::Null;
                        payload["error"] = checkout_error(&error);
                    }
                }
            }
            _ => {
                let auto = kind != "checkout_pr_merge_request";
                if auto {
                    payload["enabled"] = request["enabled"].clone();
                }
                let result: Result<Value> = async {
                    let mut lookup = self
                        .lookup_target(cwd, env, cancel.clone())
                        .await?
                        .ok_or_else(|| {
                            anyhow!(
                                "Unable to determine current change request number for {}",
                                if auto { "auto-merge" } else { "merge" }
                            )
                        })?;
                    lookup["force"] = true.into();
                    lookup["reason"] = if auto {
                        "auto-merge-validation"
                    } else {
                        "merge-pr-validation"
                    }
                    .into();
                    // `resolveCurrentPullRequest` reads the forced snapshot, whose forge
                    // slice degrades every lookup failure to "no pull request".
                    let status = self
                        .execute(
                            cwd,
                            "getCurrentPullRequestStatus",
                            &lookup,
                            env,
                            cancel.clone(),
                        )
                        .await
                        .unwrap_or(Value::Null);
                    let n = number(&status, "number").map_err(|_| {
                        anyhow!(
                            "Unable to determine current change request number for {}",
                            if auto { "auto-merge" } else { "merge" }
                        )
                    })?;
                    input["prNumber"] = n.into();
                    input["status"] = status;
                    let op = if auto {
                        if b(request, "enabled") {
                            if s(request, "mergeMethod").is_empty() {
                                bail!("mergeMethod is required when enabling auto-merge");
                            }
                            "enablePullRequestAutoMerge"
                        } else {
                            if !request["mergeMethod"].is_null() {
                                bail!("mergeMethod is not allowed when disabling auto-merge");
                            }
                            "disablePullRequestAutoMerge"
                        }
                    } else {
                        "mergePullRequest"
                    };
                    self.execute(cwd, op, &input, env, cancel.clone()).await
                }
                .await;
                payload["success"] = result.is_ok().into();
                payload["error"] = result
                    .err()
                    .map(|e| checkout_error(&e))
                    .unwrap_or(Value::Null);
            }
        }
        Ok(Some(json!({"type":response,"payload":payload})))
    }
    pub(super) async fn local_git(
        &self,
        cwd: &Path,
        env: &BTreeMap<String, String>,
        cancel: &CancellationToken,
        args: &[&str],
    ) -> Result<String> {
        let result = self.raw(cwd, env, cancel, "git", strings(args), 30).await?;
        if result.status != 0 {
            bail!("{}", result.stderr);
        }
        Ok(String::from_utf8(result.stdout)?.trim().to_owned())
    }
    /// Snapshot owners may use this projection directly without constructing an RPC request.
    pub async fn status_payload(
        &self,
        cwd: &Path,
        env: &BTreeMap<String, String>,
        cancel: CancellationToken,
    ) -> Value {
        let mut payload = json!({"status":null,"githubFeaturesEnabled":false,"authState":"no_remote","error":null});
        let mut probe_failed = false;
        let result: Result<()> = async {
            let Some(resolution) = self.resolve(cwd, env, cancel.clone()).await? else {
                // `buildUnresolvedRemoteForgeSnapshot`: a remote no forge claimed exposes its
                // host as the forge id and reads as unauthenticated.
                if let Some(host) = self.origin_host(cwd, env, &cancel).await? {
                    payload["forge"] = host.into();
                    payload["authState"] = "unauthenticated".into();
                }
                return Ok(());
            };
            payload["forge"] = resolution.forge.clone().into();
            // GitHub's throwing auth probe runs before the PR lookup (`loadForgeSnapshot`).
            if resolution.forge == "github" {
                if let Err(error) = self
                    .execute(cwd, "isAuthenticated", &json!({}), env, cancel.clone())
                    .await
                {
                    probe_failed = true;
                    return Err(error);
                }
            }
            let Some(lookup) = self.lookup_target(cwd, env, cancel.clone()).await? else {
                // `getUnavailablePullRequestStatus`: a Paseo-owned worktree without a lookup
                // target is an authenticated "no PR"; any other checkout has no branch to ask.
                if crate::git::command::metadata(cwd, env, cancel.clone())
                    .await
                    .ok()
                    .flatten()
                    .is_some()
                {
                    payload["githubFeaturesEnabled"] = true.into();
                    payload["authState"] = "authenticated".into();
                }
                return Ok(());
            };
            let mut status = self
                .execute(
                    cwd,
                    "getCurrentPullRequestStatus",
                    &lookup,
                    env,
                    cancel.clone(),
                )
                .await?;
            if status.is_object() {
                status["forge"] = resolution.forge.into();
                if status["isDraft"].is_null() {
                    status["isDraft"] = false.into();
                }
                if status["projectPath"].is_null()
                    && !s(&status, "repoOwner").is_empty()
                    && !s(&status, "repoName").is_empty()
                {
                    status["projectPath"] =
                        format!("{}/{}", s(&status, "repoOwner"), s(&status, "repoName")).into();
                }
                if s(&status["forgeSpecific"], "forge") == "github" {
                    let mut facts = status["forgeSpecific"].clone();
                    facts.as_object_mut().unwrap().remove("forge");
                    status["github"] = facts;
                }
            }
            payload["status"] = status;
            payload["githubFeaturesEnabled"] = true.into();
            payload["authState"] = "authenticated".into();
            Ok(())
        }
        .await;
        if let Err(error) = result {
            let state = auth_state(&error);
            if probe_failed || matches!(state, "cli_missing" | "unauthenticated") {
                payload["authState"] = probe_auth_state(&error).into();
                payload["githubFeaturesEnabled"] = false.into();
                payload["error"] = Value::Null;
            } else {
                payload["authState"] = "authenticated".into();
                payload["githubFeaturesEnabled"] = true.into();
                payload["error"] = checkout_error(&error);
            }
        }
        payload
    }
}
/// `toCheckoutError`: only a `NotGitRepoError` carries a specific code here.
fn checkout_error(error: &anyhow::Error) -> Value {
    let code = if error
        .downcast_ref::<crate::git::command::NotGitRepo>()
        .is_some()
    {
        "NOT_GIT_REPO"
    } else {
        "UNKNOWN"
    };
    json!({"code":code,"message":error.to_string()})
}
/// `forgeAuthStateFromError`: a missing CLI, otherwise a sign-in problem.
fn probe_auth_state(error: &anyhow::Error) -> &'static str {
    if auth_state(error) == "cli_missing" {
        "cli_missing"
    } else {
        "unauthenticated"
    }
}
fn valid_repo_segment(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

pub fn normalize_clone_repository(repo: &str, protocol: Option<&str>) -> Result<Value> {
    let trimmed = repo.trim();
    if trimmed.is_empty() {
        bail!("Repository is required");
    }
    if let Some(remote) = parse_remote(trimmed) {
        let name = remote.path.rsplit('/').next().unwrap_or("");
        if !valid_repo_segment(name) {
            bail!("Repository name contains invalid characters");
        }
        return Ok(json!({"name":name,"displayName":remote.path,"cloneUrl":trimmed}));
    }
    let parts: Vec<_> = trimmed.split('/').collect();
    if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
        bail!("Repository must use owner/repo format or a git remote URL");
    }
    let owner = parts[0];
    let name = parts[1].strip_suffix(".git").unwrap_or(parts[1]);
    if !valid_repo_segment(owner) || !valid_repo_segment(name) {
        bail!("Repository contains invalid characters");
    }
    let protocol = protocol
        .ok_or_else(|| anyhow!("Clone protocol is required for owner/repo repository names"))?;
    let url = if protocol == "ssh" {
        format!("git@github.com:{owner}/{name}.git")
    } else {
        format!("https://github.com/{owner}/{name}.git")
    };
    Ok(json!({"name":name,"displayName":format!("{owner}/{name}"),"cloneUrl":url}))
}
