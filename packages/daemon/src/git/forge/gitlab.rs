use super::*;
impl Context<'_> {
    async fn gitlab_view(&self, selector: &str) -> Result<Value> {
        // `assertSafeGlabPositional`: a leading dash would be parsed as a flag.
        if selector.starts_with('-') {
            bail!("Refusing to pass a GitLab argument that begins with '-': {selector}");
        }
        let value = self
            .json(strings(&["mr", "view", selector, "-F", "json"]))
            .await?;
        number(&value, "iid")?;
        required(&value, "source_branch")?;
        required(&value, "target_branch")?;
        Ok(value)
    }
    /// `resolveCurrentMergeRequest` plus the best-effort approvals/pipeline reads.
    async fn gitlab_current(&self, input: &Value) -> Result<Value> {
        let head = required(input, "headRef")?;
        let list = self
            .json(strings(&[
                "mr",
                "list",
                "--all",
                "--source-branch",
                head,
                "--order",
                "updated_at",
                "--sort",
                "desc",
                "--per-page",
                "100",
                "-F",
                "json",
            ]))
            .await?;
        let candidates: Vec<_> = array(&list)
            .iter()
            .filter(|v| s(v, "source_branch") == head)
            .collect();
        let selected = candidates
            .iter()
            .find(|v| state(s(v, "state")) == "open")
            .or_else(|| {
                candidates.iter().find(|v| {
                    !s(input, "headSha").is_empty() && s(v, "sha") == s(input, "headSha")
                })
            });
        let Some(selected) = selected else {
            return Ok(Value::Null);
        };
        let mr = self
            .gitlab_view(&number(selected, "iid")?.to_string())
            .await?;
        let n = number(&mr, "iid")?;
        let approvals = if let Some(project) = project_path(&mr) {
            self.api(format!(
                "projects/{}/merge_requests/{n}/approvals",
                encode(project)
            ))
            .await
            .unwrap_or(Value::Null)
        } else {
            Value::Null
        };
        let pipeline = if !mr["head_pipeline"]["id"].is_null() {
            match self
                .json(strings(&[
                    "ci",
                    "get",
                    "--merge-request",
                    &n.to_string(),
                    "--with-job-details",
                    "-F",
                    "json",
                ]))
                .await
            {
                Ok(v) => v,
                Err(e) if auth_state(&e) == "error" => Value::Null,
                Err(e) => return Err(e),
            }
        } else {
            Value::Null
        };
        Ok(status(&mr, &approvals, &pipeline))
    }
    pub(super) async fn gitlab_list(&self, pr: bool, input: &Value) -> Result<Vec<Value>> {
        let mut args = strings(&[
            if pr { "mr" } else { "issue" },
            "list",
            if pr { "-F" } else { "-O" },
            "json",
        ]);
        let query = s(input, "query").trim();
        if !query.is_empty() {
            args.extend(strings(&["--search", query]));
        }
        if let Some(limit) = input["limit"].as_u64() {
            args.extend(["-P".into(), limit.to_string()]);
        }
        let data = self.json(args).await?;
        let mut rows = data
            .as_array()
            .ok_or_else(|| anyhow!("glab JSON did not match the expected array schema"))?
            .clone();
        if !query.is_empty() && query.bytes().all(|b| b.is_ascii_digit()) {
            let resource = if pr { "merge_requests" } else { "issues" };
            let matches = self
                .api(format!(
                    "projects/:id/{resource}?state=opened&iids[]={query}"
                ))
                .await?;
            let mut merged = array(&matches).to_vec();
            for row in rows {
                if !merged.iter().any(|v| v["iid"] == row["iid"]) {
                    merged.push(row);
                }
            }
            if let Some(limit) = input["limit"].as_u64() {
                merged.truncate(limit as usize);
            }
            rows = merged;
        }
        rows.iter().map(|v| summary(v, pr)).collect()
    }
    pub(super) async fn gitlab(&self, op: &str, input: &Value) -> Result<Value> {
        match op {
            "isAuthenticated" => Ok(self
                .run(strings(&[
                    "auth",
                    "status",
                    "--hostname",
                    &self.resolution.host,
                ]))
                .await
                .is_ok()
                .into()),
            "listPullRequests" => Ok(self.gitlab_list(true, input).await?.into()),
            "listIssues" => Ok(self.gitlab_list(false, input).await?.into()),
            "getPullRequest" | "getPullRequestHeadRef" | "getPullRequestCheckoutTarget" => {
                let mr = self
                    .gitlab_view(&number(input, "number")?.to_string())
                    .await?;
                if op == "getPullRequestHeadRef" {
                    return Ok(mr["source_branch"].clone());
                }
                if op == "getPullRequest" {
                    return summary(&mr, true);
                }
                let n = number(&mr, "iid")?;
                Ok(
                    json!({"number":n,"baseRefName":mr["target_branch"],"headRefName":mr["source_branch"],"checkoutRefs":[{"remoteName":"origin","remoteRef":format!("refs/merge-requests/{n}/head")},{"remoteName":"origin","remoteRef":format!("refs/heads/{}",s(&mr,"source_branch"))}],"headOwnerLogin":null,"headRepositorySshUrl":null,"headRepositoryUrl":null,"isCrossRepository":!mr["source_project_id"].is_null() && !mr["target_project_id"].is_null() && mr["source_project_id"] != mr["target_project_id"],"headRefKind":"branch"}),
                )
            }
            "getCurrentPullRequestStatus" => match self.gitlab_current(input).await {
                // glab's "no (open) merge request(s)" stderr anywhere in the lookup means
                // the branch has no MR; a 404/403 for the project still propagates.
                Err(error) if no_merge_request(&error) => Ok(Value::Null),
                result => result,
            },
            "createPullRequest" => {
                let out = self
                    .run(strings(&[
                        "mr",
                        "create",
                        "--title",
                        required(input, "title")?,
                        "--description",
                        s(input, "body"),
                        "--source-branch",
                        required(input, "head")?,
                        "--target-branch",
                        required(input, "base")?,
                        "--yes",
                    ]))
                    .await?;
                let Some((url, number)) = created_url(&out, true) else {
                    bail!("GitLab merge request was created but no URL was returned by glab");
                };
                let Some(number) = number else {
                    bail!("GitLab merge request URL did not contain an iid: {url}");
                };
                Ok(json!({"url":url,"number":number}))
            }
            "mergePullRequest" | "enablePullRequestAutoMerge" => {
                let auto = op == "enablePullRequestAutoMerge";
                let facts = &input["status"]["forgeSpecific"];
                if s(facts, "forge") != "gitlab" {
                    bail!(
                        "GitLab {} facts are unavailable for this merge request",
                        if auto { "auto-merge" } else { "merge" }
                    );
                }
                if b(facts, "mergeWhenPipelineSucceeds") {
                    bail!(
                        "{}",
                        if auto {
                            "Auto-merge is already enabled for this merge request"
                        } else {
                            "Direct merge is not available because auto-merge is already enabled"
                        }
                    );
                }
                if auto {
                    if !active_pipeline(s(facts, "pipelineStatus")) {
                        bail!(
                            "GitLab auto-merge requires an in-progress pipeline; without one the merge would run immediately"
                        );
                    }
                } else if !merge_ready(
                    &facts["detailedMergeStatus"],
                    s(facts, "mergeStatus"),
                    b(facts, "hasConflicts"),
                ) {
                    bail!("GitLab does not report this merge request as ready for direct merge");
                }
                let method = merge_method(input)?;
                let mut args = strings(&[
                    "mr",
                    "merge",
                    &number(input, "prNumber")?.to_string(),
                    if auto {
                        "--auto-merge"
                    } else {
                        "--auto-merge=false"
                    },
                    "--yes",
                ]);
                if method != "merge" {
                    args.push(format!("--{method}"));
                }
                self.run(args).await?;
                Ok(json!({"success":true}))
            }
            "disablePullRequestAutoMerge" => {
                self.run(strings(&[
                    "api",
                    "--method",
                    "POST",
                    &format!(
                        "projects/:fullpath/merge_requests/{}/cancel_merge_when_pipeline_succeeds",
                        number(input, "prNumber")?
                    ),
                ]))
                .await?;
                Ok(json!({"success":true}))
            }
            "getCheckDetails" => {
                let (flag, n) = if input
                    .get("changeRequestNumber")
                    .is_some_and(|v| !v.is_null())
                {
                    ("--merge-request", number(input, "changeRequestNumber")?)
                } else {
                    ("--pipeline-id", number(input, "checkRunId")?)
                };
                let pipeline = self
                    .json(strings(&[
                        "ci",
                        "get",
                        flag,
                        &n.to_string(),
                        "--with-job-details",
                        "-F",
                        "json",
                    ]))
                    .await?;
                number(&pipeline, "id")?;
                Ok(pipeline_details(&pipeline))
            }
            _ => bail!("Unknown forge operation: {op}"),
        }
    }
    pub(super) async fn gitlab_timeline(&self, input: &Value) -> Result<Value> {
        let n = number(input, "prNumber")?;
        let mr = self.gitlab_view(&n.to_string()).await?;
        let Some(project) = project_path(&mr) else {
            return Ok(timeline_result(
                input,
                vec![],
                false,
                Some(
                    json!({"kind":"not_found","message":"GitLab merge request project path is unavailable"}),
                ),
            ));
        };
        let path = format!(
            "projects/{}/merge_requests/{n}/discussions",
            encode(project)
        );
        let discussions = self.api(format!("{path}?per_page=100")).await?;
        let mut items = Vec::new();
        for discussion in array(&discussions) {
            let notes = array(&discussion["notes"]);
            let thread = discussion["individual_note"] == false
                || (discussion["individual_note"].is_null() && notes.len() > 1);
            for note in notes {
                if b(note, "system") {
                    continue;
                }
                let author = &note["author"];
                let mut item = json!({"kind":"comment","id":note["id"].to_string(),"author":author["username"].as_str().or(author["name"].as_str()).unwrap_or("unknown"),"authorUrl":author["web_url"],"avatarUrl":author["avatar_url"],"body":s(note,"body"),"createdAt":time(&note["created_at"]),"url":format!("{}#note_{}",s(&mr,"web_url"),note["id"])});
                if thread {
                    item["threadId"] = discussion["id"].clone();
                }
                let position = &note["position"];
                if let Some(path) = position["new_path"]
                    .as_str()
                    .or(position["old_path"].as_str())
                    .filter(|p| !p.is_empty())
                {
                    let line = position
                        .get("new_line")
                        .filter(|v| !v.is_null())
                        .or(position.get("old_line").filter(|v| !v.is_null()));
                    let start = &position["line_range"]["start"];
                    let start_line = start
                        .get("new_line")
                        .filter(|v| !v.is_null())
                        .or(start.get("old_line").filter(|v| !v.is_null()));
                    let mut location = json!({"path":path,"threadId":discussion["id"]});
                    if let Some(line) = line {
                        location["line"] = line.clone();
                    }
                    if let Some(start_line) = start_line.filter(|v| Some(*v) != line) {
                        location["startLine"] = start_line.clone();
                    }
                    if b(note, "resolvable") {
                        location["isResolved"] = b(note, "resolved").into();
                    }
                    item["location"] = location;
                } else if b(note, "resolvable") {
                    item["threadIsResolved"] = b(note, "resolved").into();
                }
                items.push(item);
            }
        }
        let truncated = if array(&discussions).len() >= 100 {
            self.api(format!("{path}?per_page=1&page=101"))
                .await
                .map_or(true, |v| !array(&v).is_empty())
        } else {
            false
        };
        Ok(timeline_result(input, items, truncated, None))
    }
}
/// `isNoMergeRequestText` on a glab command failure's stderr.
fn no_merge_request(error: &anyhow::Error) -> bool {
    use std::sync::LazyLock;
    static NO_MR: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"(?i)no (open )?merge requests?").expect("valid no-MR regex")
    });
    error
        .downcast_ref::<ForgeError>()
        .is_some_and(|e| e.auth_state == "error" && NO_MR.is_match(&e.stderr))
}
fn state(raw: &str) -> &'static str {
    match raw {
        "opened" => "open",
        "merged" => "merged",
        _ => "closed",
    }
}
fn project_path(v: &Value) -> Option<&str> {
    v["references"]["full"]
        .as_str()?
        .split(['!', '#'])
        .next()
        .filter(|v| !v.is_empty())
}
fn summary(v: &Value, pr: bool) -> Result<Value> {
    let mut out = json!({"number":number(v,"iid")?,"title":s(v,"title"),"url":s(v,"web_url"),"state":if pr { state(s(v,"state")) } else { s(v,"state") },"body":v["description"],"labels":labels(&v["labels"]),"updatedAt":s(v,"updated_at")});
    if pr {
        out["baseRefName"] = s(v, "target_branch").into();
        out["headRefName"] = s(v, "source_branch").into();
    }
    if let Some(path) = project_path(v) {
        out["projectPath"] = path.into();
    }
    Ok(out)
}
fn merge_ready(detailed: &Value, legacy: &str, conflicts: bool) -> bool {
    if !detailed.is_null() {
        detailed == "mergeable"
    } else {
        legacy == "can_be_merged" && !conflicts
    }
}
fn status(mr: &Value, approvals: &Value, pipeline: &Value) -> Value {
    let mut jobs: Vec<_> = array(&pipeline["jobs"]).iter().collect();
    jobs.sort_by_key(|v| number_value(&v["id"]).unwrap_or(0));
    let checks: Vec<_> = jobs.into_iter().map(|job| {
        let raw = s(job,"status").to_lowercase(); let allow = b(job,"allow_failure");
        let status = match raw.as_str() { "failed" if allow => "success", "manual" if allow => "skipped", "success"|"passed" => "success", "failed" => "failure", "canceled"|"cancelled" => "cancelled", "skipped" => "skipped", _ => "pending" };
        let mut check = json!({"name":s(job,"name"),"status":status,"url":job["web_url"],"workflow":s(job,"stage"),"checkRunId":job["id"]});
        if raw == "manual" { check["traits"] = if allow { json!(["manual"]) } else { json!(["manual","action_required"]) }; }
        else if raw == "failed" && allow { check["traits"] = json!(["warning"]); }
        check
    }).collect();
    let pipeline_state = pipeline["status"]
        .as_str()
        .or(mr["head_pipeline"]["status"].as_str())
        .unwrap_or("");
    let aggregate = match pipeline_state {
        "success" | "passed" => "success",
        "failed" => "failure",
        "manual" => "pending",
        v if active_pipeline(v) => "pending",
        _ => "none",
    };
    let required = approvals["approvals_required"]
        .as_i64()
        .or(mr["approvals_required"].as_i64())
        .unwrap_or(0);
    let given = approvals["approved_by"]
        .as_array()
        .map(|v| v.len() as i64)
        .or_else(|| {
            Some(
                (approvals["approvals_required"].as_i64()?
                    - approvals["approvals_left"].as_i64()?)
                .max(0),
            )
        })
        .or(mr["approvals_given"].as_i64())
        .unwrap_or(0);
    let mut out = json!({"number":mr["iid"],"url":s(mr,"web_url"),"title":s(mr,"title"),"state":state(s(mr,"state")),"baseRefName":s(mr,"target_branch"),"headRefName":s(mr,"source_branch"),"isMerged":s(mr,"state")=="merged" || !mr["merged_at"].is_null(),"isDraft":mr["draft"].as_bool().or(mr["work_in_progress"].as_bool()).unwrap_or(false),"mergeable":if b(mr,"has_conflicts") {"CONFLICTING"} else if merge_ready(&mr["detailed_merge_status"],s(mr,"merge_status"),b(mr,"has_conflicts")) {"MERGEABLE"} else {"UNKNOWN"},"checks":checks,"checksStatus":aggregate,"reviewDecision":null,"forgeSpecific":{"forge":"gitlab","detailedMergeStatus":mr["detailed_merge_status"],"mergeStatus":mr["merge_status"],"hasConflicts":b(mr,"has_conflicts"),"blockingDiscussionsResolved":mr["blocking_discussions_resolved"].as_bool().unwrap_or(true),"approvalsRequired":required,"approvalsGiven":given,"pipelineStatus":mr["head_pipeline"]["status"],"pipelineId":mr["head_pipeline"]["id"],"pipelineUrl":mr["head_pipeline"]["web_url"],"mergeWhenPipelineSucceeds":b(mr,"merge_when_pipeline_succeeds")}});
    if let Some(path) = project_path(mr) {
        out["projectPath"] = path.into();
        let parts: Vec<_> = path.split('/').filter(|s| !s.is_empty()).collect();
        if let (Some(owner), Some(name)) = (parts.first(), parts.last()) {
            out["repoOwner"] = (*owner).into();
            out["repoName"] = (*name).into();
        }
    }
    out
}
fn pipeline_details(pipeline: &Value) -> Value {
    let mut jobs: Vec<_> = array(&pipeline["jobs"]).iter().collect();
    jobs.sort_by_key(|v| number_value(&v["id"]).unwrap_or(0));
    let mut stages: Vec<Value> = Vec::new();
    for job in jobs {
        let name = s(job, "stage");
        let index = stages
            .iter()
            .position(|v| s(v, "name") == name)
            .unwrap_or_else(|| {
                stages.push(json!({"name":name,"status":"unknown","jobs":[]}));
                stages.len() - 1
            });
        stages[index]["jobs"].as_array_mut().unwrap().push(json!({"id":job["id"],"name":s(job,"name"),"stage":name,"status":normalized_pipeline(s(job,"status")),"rawStatus":s(job,"status"),"url":job["web_url"],"allowFailure":b(job,"allow_failure"),"durationSeconds":job["duration"]}));
    }
    for stage in &mut stages {
        let status = [
            "running", "failed", "pending", "created", "manual", "canceled", "skipped", "success",
        ]
        .into_iter()
        .find(|status| {
            array(&stage["jobs"]).iter().any(|j| {
                let raw = s(j, "status");
                let effective = if b(j, "allowFailure") && matches!(raw, "failed" | "manual") {
                    "success"
                } else {
                    raw
                };
                effective == *status
            })
        })
        .unwrap_or("unknown");
        stage["status"] = status.into();
    }
    json!({"checkRunId":pipeline["id"],"workflowRunId":null,"name":if s(pipeline,"ref").is_empty() {format!("Pipeline #{}",pipeline["id"])} else {format!("Pipeline ({})",s(pipeline,"ref"))},"status":null,"conclusion":null,"url":pipeline["web_url"],"detailsUrl":pipeline["web_url"],"output":null,"annotations":[],"failedJobs":[],"truncated":false,"pipeline":{"id":pipeline["id"],"status":normalized_pipeline(s(pipeline,"status")),"rawStatus":s(pipeline,"status"),"url":pipeline["web_url"],"ref":pipeline["ref"],"sha":pipeline["sha"],"stages":stages}})
}
