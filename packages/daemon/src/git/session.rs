use super::{GitService, checkout_error, command, facts, mutations};
use crate::transport::RequestContext;
use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

impl GitService {
    pub(super) async fn handle_message(
        &self,
        context: &RequestContext,
        message: &Value,
    ) -> Result<bool> {
        let kind = message["type"].as_str().unwrap_or("");
        if kind == "unsubscribe_checkout_diff_request" {
            context
                .release_subscription(
                    message["subscriptionId"]
                        .as_str()
                        .ok_or_else(|| anyhow!("subscriptionId required"))?,
                )
                .await?;
            return Ok(true);
        }
        let response = match kind {
            "checkout_status_request" => "checkout_status_response",
            "validate_branch_request" => "validate_branch_response",
            "branch_suggestions_request" => "branch_suggestions_response",
            "checkout.commits.list.request" => "checkout.commits.list.response",
            "checkout.commits.file_diff.request" => "checkout.commits.file_diff.response",
            "checkout.diff.get.request" => "checkout.diff.get.response",
            "subscribe_checkout_diff_request" => "subscribe_checkout_diff_response",
            "checkout_switch_branch_request" => "checkout_switch_branch_response",
            "checkout.rename_branch.request" => "checkout.rename_branch.response",
            "checkout.discard_changes.request" => "checkout.discard_changes.response",
            "checkout_commit_request" => "checkout_commit_response",
            "checkout_merge_request" => "checkout_merge_response",
            "checkout_merge_from_base_request" => "checkout_merge_from_base_response",
            "checkout_pull_request" => "checkout_pull_response",
            "checkout_push_request" => "checkout_push_response",
            "checkout.refresh.request" => "checkout.refresh.response",
            "stash_save_request" => "stash_save_response",
            "stash_pop_request" => "stash_pop_response",
            "stash_list_request" => "stash_list_response",
            "checkout_pr_create_request"
            | "checkout_pr_merge_request"
            | "checkout_pr_status_request"
            | "pull_request_timeline_request"
            | "checkout.forge.set_auto_merge.request"
            | "checkout.github.set_auto_merge.request"
            | "checkout.forge.get_check_details.request"
            | "checkout.github.get_check_details.request"
            | "forge.search.request"
            | "github_search_request" => "",
            _ => return Ok(false),
        };
        let raw_cwd = message["cwd"]
            .as_str()
            .ok_or_else(|| anyhow!("cwd required"))?;
        let cwd = if raw_cwd == "~" || raw_cwd.starts_with("~/") {
            let app = self
                .app
                .upgrade()
                .ok_or_else(|| anyhow!("Daemon shutting down"))?;
            app.user_home.join(raw_cwd.strip_prefix("~/").unwrap_or(""))
        } else {
            PathBuf::from(raw_cwd)
        };
        let cancel = context.cancellation();
        if kind == "subscribe_checkout_diff_request" {
            self.subscribe_diff(context, message, &cwd).await?;
            return Ok(true);
        }
        if response.is_empty() {
            match self.forge_message(context, message, &cwd).await {
                Ok(Some(mut frame)) => {
                    if frame["payload"].get("cwd").is_some() {
                        frame["payload"]["cwd"] = json!(raw_cwd);
                    }
                    context.send(frame).await?;
                }
                Ok(None) => return Ok(false),
                Err(error) => {
                    let (response, mut payload) = match kind {
                        "checkout_pr_create_request" => (
                            "checkout_pr_create_response",
                            json!({"url":null,"number":null}),
                        ),
                        "checkout_pr_merge_request" => {
                            ("checkout_pr_merge_response", json!({"success":false}))
                        }
                        _ => return Err(error),
                    };
                    payload["cwd"] = json!(raw_cwd);
                    payload["requestId"] = message["requestId"].clone();
                    payload["error"] = checkout_error(&error);
                    context
                        .send(json!({"type":response,"payload":payload}))
                        .await?;
                }
            }
            return Ok(true);
        }
        let activity = context.current_activity();
        let result = self
            .operation(
                kind,
                message,
                &cwd,
                activity["focusedAgentId"].as_str(),
                cancel,
            )
            .await;
        let mut payload = match result {
            Ok(value) => value,
            Err(error) => {
                let mut payload = match kind {
                    "checkout_status_request" => facts::non_git(&cwd),
                    "validate_branch_request" => {
                        json!({"exists":false,"resolvedRef":null,"isRemote":false})
                    }
                    "branch_suggestions_request" => json!({"branches":[],"branchDetails":[]}),
                    "checkout.commits.list.request" => json!({"baseRef":null,"commits":[]}),
                    "checkout.commits.file_diff.request" => {
                        json!({"sha":message["sha"],"path":message["path"],"file":null})
                    }
                    "checkout.diff.get.request" => json!({"files":[]}),
                    "stash_list_request" => json!({"entries":[]}),
                    "checkout_switch_branch_request" => {
                        json!({"success":false,"branch":message["branch"]})
                    }
                    "checkout.rename_branch.request" => {
                        json!({"success":false,"currentBranch":null})
                    }
                    _ => json!({"success":false}),
                };
                payload["error"] =
                    if kind == "validate_branch_request" || kind == "branch_suggestions_request" {
                        json!(error.to_string())
                    } else {
                        checkout_error(&error)
                    };
                payload
            }
        };
        if !matches!(
            kind,
            "validate_branch_request" | "branch_suggestions_request"
        ) {
            payload["cwd"] = json!(raw_cwd);
        }
        payload["requestId"] = message["requestId"].clone();
        context
            .send(json!({"type":response,"payload":payload}))
            .await?;
        Ok(true)
    }
    async fn operation(
        &self,
        kind: &str,
        message: &Value,
        cwd: &Path,
        focused_agent_id: Option<&str>,
        cancel: CancellationToken,
    ) -> Result<Value> {
        let env = self.environment()?;
        match kind {
            "checkout_status_request" => {
                return Ok(super::status_projection(
                    &self.snapshot(cwd, false, cancel).await?,
                ));
            }
            "checkout.diff.get.request" => {
                return self.read_diff(cwd, &message["compare"], cancel).await;
            }
            "checkout.commits.list.request" => {
                let mut value = super::diff::list_commits(cwd, &env, cancel).await?;
                value["error"] = Value::Null;
                return Ok(value);
            }
            "checkout.commits.file_diff.request" => {
                let sha = string(message, "sha")?;
                let path = string(message, "path")?;
                return Ok(
                    json!({"sha":sha,"path":path,"file":super::diff::commit_file(cwd,sha,path,&env,cancel).await?,"error":null}),
                );
            }
            "validate_branch_request" => {
                let name = string(message, "branchName")?;
                command::request_ref(name, "branch")?;
                let resolution = facts::resolve_branch(cwd, name, &env, cancel).await?;
                let remote = resolution["kind"] == "remote-only";
                return Ok(
                    json!({"exists":resolution["kind"]!="not-found","resolvedRef":if remote{&resolution["remoteRef"]}else{&resolution["name"]},"isRemote":remote,"error":null}),
                );
            }
            "branch_suggestions_request" => {
                let branches = facts::branches(
                    cwd,
                    message["query"].as_str().unwrap_or(""),
                    message["limit"].as_u64().unwrap_or(50) as usize,
                    &env,
                    cancel,
                )
                .await?;
                return Ok(
                    json!({"branches":branches.iter().map(|v|v["name"].clone()).collect::<Vec<_>>(),"branchDetails":branches,"error":null}),
                );
            }
            "stash_list_request" => {
                return Ok(
                    json!({"entries":facts::stashes(cwd,message["paseoOnly"].as_bool().unwrap_or(true),&env,cancel).await?,"error":null}),
                );
            }
            "checkout.refresh.request" => {
                self.invalidate(cwd).await?;
                self.snapshot(cwd, true, cancel.clone()).await?;
                self.refresh_forge(cwd, cancel).await?;
                return Ok(json!({"success":true,"error":null}));
            }
            _ => {}
        }
        let missing = |error: &anyhow::Error| {
            error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
        };
        // The source's merge handlers inspect the snapshot before touching git; a
        // missing directory has no git facts.
        let snapshot_git = if matches!(
            kind,
            "checkout_merge_request" | "checkout_merge_from_base_request"
        ) {
            match self.snapshot(cwd, false, cancel.clone()).await {
                Ok(snapshot) => Some(super::status_projection(&snapshot)),
                Err(error) if missing(&error) => Some(Value::Null),
                Err(error) => return Err(error),
            }
        } else {
            None
        };
        let (_, entry) = match self.checkout_entry(cwd).await {
            Ok(entry) => entry,
            // `requireGitRepo` treats a missing directory like any other non-repository.
            Err(error)
                if !matches!(kind, "stash_save_request" | "stash_pop_request")
                    && missing(&error) =>
            {
                return Err(command::NotGitRepo(cwd.display().to_string()).into());
            }
            Err(error) => return Err(error),
        };
        let _mutation = tokio::select! {_=cancel.cancelled()=>bail!("Git mutation cancelled before admission"),guard=entry.mutation.lock()=>guard};
        let mut result = json!({"success":true,"error":null});
        match kind {
            "checkout_switch_branch_request" => {
                let value =
                    mutations::checkout(cwd, string(message, "branch")?, &env, cancel.clone())
                        .await?;
                result["source"] = value["source"].clone();
                result["branch"] = message["branch"].clone();
            }
            "checkout.rename_branch.request" => {
                let value =
                    mutations::rename(cwd, string(message, "branch")?, &env, cancel.clone())
                        .await?;
                crate::projects::runtime::rebind_worktree_change_request_hint(
                    cwd,
                    string(&value, "previousBranch")?,
                    string(&value, "currentBranch")?,
                )
                .await?;
                result["currentBranch"] = value["currentBranch"].clone();
            }
            "checkout.discard_changes.request" => {
                let paths = message["paths"]
                    .as_array()
                    .ok_or_else(|| anyhow!("paths required"))?
                    .iter()
                    .map(|p| {
                        p.as_str()
                            .map(str::to_owned)
                            .ok_or_else(|| anyhow!("Invalid path"))
                    })
                    .collect::<Result<Vec<_>>>()?;
                mutations::discard(cwd, &paths, &env, cancel.clone()).await?;
            }
            "checkout_commit_request" => {
                let generated;
                let text = match message["message"]
                    .as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    Some(value) => value,
                    None => {
                        generated = self
                            .generate_metadata(cwd, false, None, focused_agent_id, cancel.clone())
                            .await?;
                        string(&generated, "message")?
                    }
                };
                mutations::commit(
                    cwd,
                    text,
                    message["addAll"].as_bool().unwrap_or(true),
                    &env,
                    cancel.clone(),
                )
                .await?;
            }
            "checkout_merge_request" => {
                let git = snapshot_git.unwrap_or_default();
                if git["isGit"] != true {
                    bail!("Not a git repository: {}", cwd.display());
                }
                if message["requireCleanTarget"] == true && git["isDirty"] == true {
                    bail!("Working directory has uncommitted changes.");
                }
                let base = message["baseRef"]
                    .as_str()
                    .or_else(|| git["baseRef"].as_str())
                    .ok_or_else(|| anyhow!("Base branch is required for merge"))?;
                let target = mutations::merge_to_base(
                    cwd,
                    Some(base.strip_prefix("origin/").unwrap_or(base)),
                    message["strategy"] == "squash",
                    &env,
                    cancel.clone(),
                )
                .await?;
                if target != cwd {
                    self.notify_mutation(&target).await?;
                }
            }
            "checkout_merge_from_base_request" => {
                let require_clean = message["requireCleanTarget"].as_bool().unwrap_or(true);
                if require_clean && snapshot_git.unwrap_or_default()["isDirty"] == true {
                    bail!("Working directory has uncommitted changes.");
                }
                mutations::merge_from_base(
                    cwd,
                    message["baseRef"].as_str(),
                    require_clean,
                    &env,
                    cancel.clone(),
                )
                .await?
            }
            "checkout_pull_request" => mutations::pull(cwd, &env, cancel.clone()).await?,
            "checkout_push_request" => mutations::push(cwd, &env, cancel.clone()).await?,
            "stash_save_request" => {
                let branch = message["branch"].as_str().unwrap_or("").trim();
                mutations::execute(
                    cwd,
                    &[
                        "stash",
                        "push",
                        "--include-untracked",
                        "-m",
                        &format!(
                            "paseo-auto-stash: {}",
                            if branch.is_empty() { "unnamed" } else { branch }
                        ),
                    ],
                    &env,
                    cancel.clone(),
                )
                .await?;
            }
            "stash_pop_request" => {
                let index = message["stashIndex"]
                    .as_u64()
                    .ok_or_else(|| anyhow!("Invalid stash index"))?;
                mutations::execute(
                    cwd,
                    &["stash", "pop", &format!("stash@{{{index}}}")],
                    &env,
                    cancel.clone(),
                )
                .await?;
            }
            _ => bail!("Unknown Git mutation {kind}"),
        }
        if let Err(error) = self.notify_mutation(cwd).await {
            tracing::warn!(%error,"Mutation completed; refresh failed");
        }
        Ok(result)
    }
    async fn subscribe_diff(
        &self,
        context: &RequestContext,
        message: &Value,
        cwd: &Path,
    ) -> Result<()> {
        let requested = message["subscriptionId"].as_str();
        let slot = requested.map(|id| format!("diff:{id}"));
        let owner = context.begin_subscription("diffs", requested, slot.as_deref())?;
        let response_cwd = cwd.to_owned();
        let (cwd, entry) = match self.checkout_entry(cwd).await {
            Ok(entry) => entry,
            Err(error) => {
                owner.release().await?;
                return Err(error);
            }
        };
        let mut changes = entry.changes.subscribe();
        let compare = message["compare"].clone();
        if let Err(error) = self.retain_observation(&response_cwd).await {
            owner.release().await?;
            return Err(error);
        }
        let initial = match self.read_diff(&cwd, &compare, owner.cancellation()).await {
            Ok(value) => value,
            Err(error) => {
                let _ = self.release_observation(&response_cwd).await;
                owner.release().await?;
                return Err(error);
            }
        };
        let mut payload = initial.clone();
        payload["cwd"] = json!(response_cwd);
        payload["subscriptionId"] = json!(owner.response_id());
        payload["requestId"] = message["requestId"].clone();
        let bootstrap = async {
            context
                .send(json!({"type":"subscribe_checkout_diff_response","payload":payload}))
                .await?;
            owner.activate().await
        }
        .await;
        if let Err(error) = bootstrap {
            let _ = self.release_observation(&response_cwd).await;
            let _ = owner.release().await;
            return Err(error);
        }
        let app = self.app.clone();
        tokio::spawn(async move {
            let cancel = owner.cancellation();
            let mut previous = initial;
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    changed = changes.changed() => if changed.is_err() { break; }
                }
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = tokio::time::sleep(std::time::Duration::from_millis(150)) => {}
                }
                let Some(state) = app.upgrade() else { break };
                let next = state.git.read_diff(&cwd, &compare, cancel.clone()).await;
                drop(state);
                let mut value = match next {
                    Ok(value) => value,
                    Err(error) => json!({"cwd":cwd,"files":[],"error":checkout_error(&error)}),
                };
                if value == previous {
                    continue;
                }
                previous = value.clone();
                value["cwd"] = json!(response_cwd);
                value["subscriptionId"] = json!(owner.response_id());
                if owner
                    .emit(json!({"type":"checkout_diff_update","payload":value}))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            if let Some(app) = app.upgrade() {
                let _ = app.git.release_observation(&response_cwd).await;
            }
            let _ = owner.release().await;
        });
        Ok(())
    }
    async fn forge_message(
        &self,
        context: &RequestContext,
        message: &Value,
        cwd: &Path,
    ) -> Result<Option<Value>> {
        let env = self.environment()?;
        let cancel = context.cancellation();
        let mut input = message.clone();
        if message["type"] == "checkout_pr_create_request" {
            if self
                .forge
                .resolve(cwd, &env, cancel.clone())
                .await?
                .is_none()
            {
                bail!("No forge remote configured");
            }
            let activity = context.current_activity();
            let missing_title = input["title"].as_str().is_none_or(|s| s.trim().is_empty());
            let missing_body = input["body"].as_str().is_none_or(|s| s.trim().is_empty());
            if missing_title || missing_body {
                let generated = self
                    .generate_metadata(
                        cwd,
                        true,
                        input["baseRef"].as_str(),
                        activity["focusedAgentId"].as_str(),
                        cancel.clone(),
                    )
                    .await?;
                if missing_title {
                    input["title"] = generated["title"].clone();
                }
                if missing_body {
                    input["body"] = generated["body"].clone();
                }
            }
            let head = facts::current_branch(cwd, &env, cancel.clone())
                .await?
                .ok_or_else(|| anyhow!("Unable to determine head branch for PR"))?;
            let base =
                command::operation_base(cwd, input["baseRef"].as_str(), &env, cancel.clone())
                    .await?
                    .ok_or_else(|| anyhow!("Unable to determine base branch for PR"))?;
            command::safe_ref(&head)?;
            mutations::execute(cwd, &["push", "-u", "origin", &head], &env, cancel.clone()).await?;
            input["head"] = json!(head);
            input["base"] = json!(command::branch_name(&base));
        }
        let response = self
            .forge
            .handle_operation(&input, cwd, &env, cancel.clone())
            .await?;
        if matches!(
            message["type"].as_str(),
            Some(
                "checkout_pr_create_request"
                    | "checkout_pr_merge_request"
                    | "checkout.forge.set_auto_merge.request"
                    | "checkout.github.set_auto_merge.request"
            )
        ) {
            let _ = self.notify_mutation(cwd).await;
        }
        Ok(response)
    }
    pub async fn refresh_forge(&self, cwd: &Path, cancel: CancellationToken) -> Result<Value> {
        let env = self.environment()?;
        let payload = self.forge.status_payload(cwd, &env, cancel).await;
        let mut value = json!({"featuresEnabled":payload["githubFeaturesEnabled"],"pullRequest":payload["status"],"authState":payload["authState"],"error":payload["error"]});
        if let Some(forge) = payload.get("forge") {
            value["forge"] = forge.clone();
        }
        self.apply_forge_payload(cwd, &payload).await?;
        Ok(value)
    }
}
fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key].as_str().ok_or_else(|| anyhow!("{key} required"))
}
