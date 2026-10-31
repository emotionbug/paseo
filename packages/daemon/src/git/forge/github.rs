use super::*;
const SUMMARY_FIELDS: &str = "number,title,url,state,body,labels,baseRefName,headRefName,updatedAt";
const STATUS_FIELDS: &str = "number,url,title,state,isDraft,baseRefName,headRefName,headRefOid,mergedAt,reviewDecision,mergeable,headRepositoryOwner";
const CHECKOUT_QUERY: &str = "query PullRequestCheckoutTarget($owner: String!, $name: String!, $number: Int!) { repository(owner: $owner, name: $name) { pullRequest(number: $number) { number baseRefName headRefName isCrossRepository headRepositoryOwner { login } headRepository { sshUrl url } } } }";
const FACTS_QUERY: &str = "query PullRequestStatusFacts($owner: String!, $name: String!, $number: Int!) { repository(owner: $owner, name: $name) { autoMergeAllowed mergeCommitAllowed squashMergeAllowed rebaseMergeAllowed viewerDefaultMergeMethod pullRequest(number: $number) { mergeStateStatus autoMergeRequest { enabledAt mergeMethod enabledBy { login } } viewerCanEnableAutoMerge viewerCanDisableAutoMerge viewerCanMergeAsAdmin viewerCanUpdateBranch isMergeQueueEnabled isInMergeQueue } } }";
const TIMELINE_QUERY: &str = "query PullRequestTimeline($owner: String!, $name: String!, $number: Int!) { repository(owner: $owner, name: $name) { pullRequest(number: $number) { number reviews(first: 100) { nodes { id state body bodyHTML url submittedAt author { login url avatarUrl } } pageInfo { hasNextPage } } comments(first: 100) { nodes { id body bodyHTML url createdAt author { login url avatarUrl } } pageInfo { hasNextPage } } reviewThreads(first: 100) { nodes { id path line startLine isResolved isOutdated comments(first: 100) { nodes { id body bodyHTML url createdAt author { login url avatarUrl } pullRequestReview { id } } pageInfo { hasNextPage } } } pageInfo { hasNextPage } } } } }";
impl Context<'_> {
    async fn graphql(&self, query: &str, owner: &str, name: &str, number: u64) -> Result<Value> {
        self.json(strings(&[
            "api",
            "graphql",
            "-f",
            &format!("query={query}"),
            "-F",
            &format!("owner={owner}"),
            "-F",
            &format!("name={name}"),
            "-F",
            &format!("number={number}"),
        ]))
        .await
    }
    async fn github_repo(&self) -> Result<Value> {
        let args = strings(&["repo", "view", "--json", "owner,name,parent"]);
        match self.json(args).await {
            Ok(v) => Ok(v),
            Err(e) if auth_state(&e) == "error" => Ok(Value::Null),
            Err(e) => Err(e),
        }
    }
    pub(super) async fn github_list(&self, pr: bool, input: &Value) -> Result<Vec<Value>> {
        let value = self
            .json_array(strings(&[
                if pr { "pr" } else { "issue" },
                "list",
                "--search",
                s(input, "query"),
                "--json",
                if pr {
                    SUMMARY_FIELDS
                } else {
                    "number,title,url,state,body,labels,updatedAt"
                },
                "--limit",
                &input["limit"].as_u64().unwrap_or(20).to_string(),
            ]))
            .await?;
        value
            .as_array()
            .ok_or_else(|| anyhow!("gh JSON did not match the expected array schema"))?
            .iter()
            .map(|v| summary(v, pr))
            .collect()
    }
    pub(super) async fn github(&self, op: &str, input: &Value) -> Result<Value> {
        match op {
            "isAuthenticated" => {
                self.run(strings(&["auth", "status"])).await?;
                Ok(true.into())
            }
            "listPullRequests" => Ok(self.github_list(true, input).await?.into()),
            "listIssues" => Ok(self.github_list(false, input).await?.into()),
            "getPullRequest" | "getPullRequestHeadRef" => {
                let v = self
                    .json(strings(&[
                        "pr",
                        "view",
                        &number(input, "number")?.to_string(),
                        "--json",
                        SUMMARY_FIELDS,
                    ]))
                    .await?;
                let summary = summary(&v, true)?;
                Ok(if op == "getPullRequestHeadRef" {
                    summary["headRefName"].clone()
                } else {
                    summary
                })
            }
            "getPullRequestCheckoutTarget" => {
                let repo = self.github_repo().await?;
                let owner = s(&repo["owner"], "login");
                let name = s(&repo, "name");
                if owner.is_empty() || name.is_empty() {
                    bail!("Unable to resolve GitHub repository for pull request checkout");
                }
                let v = self
                    .graphql(CHECKOUT_QUERY, owner, name, number(input, "number")?)
                    .await?;
                let pr = &v["data"]["repository"]["pullRequest"];
                if pr.is_null() {
                    bail!("Pull request not found");
                }
                let n = number(pr, "number")?;
                Ok(
                    json!({"number":n,"baseRefName":s(pr,"baseRefName"),"headRefName":s(pr,"headRefName"),"checkoutRefs":default_checkout_refs("github",n),"headOwnerLogin":pr["headRepositoryOwner"]["login"],"headRepositorySshUrl":pr["headRepository"]["sshUrl"],"headRepositoryUrl":pr["headRepository"]["url"],"isCrossRepository":b(pr,"isCrossRepository"),"headRefKind":"branch"}),
                )
            }
            "getCurrentPullRequestStatus" => self.github_current(input).await,
            "getCheckDetails" => self.github_check_details(input).await,
            "searchRepositories" => self.github_repositories(input).await,
            "createPullRequest" => {
                let slug = if self.resolution.project_path.split('/').count() == 2 {
                    self.resolution.project_path.clone()
                } else {
                    let repo = self.github_repo().await?;
                    if s(&repo["owner"], "login").is_empty() || s(&repo, "name").is_empty() {
                        bail!("Unable to resolve GitHub repository for pull request creation");
                    }
                    format!("{}/{}", s(&repo["owner"], "login"), s(&repo, "name"))
                };
                let mut args = strings(&[
                    "api",
                    "-X",
                    "POST",
                    &format!("repos/{slug}/pulls"),
                    "-f",
                    &format!("title={}", required(input, "title")?),
                    "-f",
                    &format!("head={}", required(input, "head")?),
                    "-f",
                    &format!("base={}", required(input, "base")?),
                ]);
                if !s(input, "body").is_empty() {
                    args.extend(["-f".into(), format!("body={}", s(input, "body"))]);
                }
                let v = self.json(args).await?;
                Ok(json!({"url":required(&v,"url")?,"number":number(&v,"number")?}))
            }
            "mergePullRequest" | "enablePullRequestAutoMerge" | "disablePullRequestAutoMerge" => {
                guard_merge(op, input)?;
                let mut args = strings(&["pr", "merge", &number(input, "prNumber")?.to_string()]);
                if op == "disablePullRequestAutoMerge" {
                    args.push("--disable-auto".into());
                } else {
                    if op == "enablePullRequestAutoMerge" {
                        args.push("--auto".into());
                    }
                    args.push(format!("--{}", merge_method(input)?));
                }
                self.run(args).await?;
                Ok(json!({"success":true}))
            }
            _ => bail!("Unknown forge operation: {op}"),
        }
    }
    async fn github_status_command(&self, mut args: Vec<String>) -> Result<Value> {
        args.extend([
            "--json".into(),
            format!("{STATUS_FIELDS},statusCheckRollup"),
        ]);
        // `pr list` parses empty output as `[]`; `pr view` keeps the strict object read.
        let list = args.get(1).is_some_and(|v| v == "list");
        let read = |args: Vec<String>| async move {
            if list {
                self.json_array(args).await
            } else {
                self.json(args).await
            }
        };
        match read(args.clone()).await {
            Err(e)
                if auth_state(&e) == "error"
                    && error_text(&e).to_lowercase().contains("statuscheckrollup") =>
            {
                *args.last_mut().unwrap() = STATUS_FIELDS.into();
                read(args).await
            }
            result => result,
        }
    }
    async fn github_try_view(&self, args: Vec<String>) -> Result<Option<Value>> {
        match self.github_status_command(args).await {
            Ok(v) => Ok(Some(v)),
            Err(e)
                if auth_state(&e) == "error"
                    && error_text(&e)
                        .to_lowercase()
                        .contains("no pull requests found") =>
            {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }
    async fn github_current(&self, input: &Value) -> Result<Value> {
        let head = required(input, "headRef")?;
        let mut selected = None;
        if let Some(view) = self.github_try_view(strings(&["pr", "view"])).await? {
            if matches_head(&view, input, s(input, "headRepositoryOwner")) {
                selected = Some(view);
            }
        }
        if selected.is_none() {
            let mut owner = s(input, "headRepositoryOwner").to_owned();
            let mut candidates = Vec::new();
            if owner.is_empty() {
                let repo = self.github_repo().await?;
                owner = s(&repo["owner"], "login").to_owned();
                if owner.is_empty() {
                    return Ok(Value::Null);
                }
                let parent_owner = s(&repo["parent"]["owner"], "login");
                let parent_name = s(&repo["parent"], "name");
                if !parent_owner.is_empty() && !parent_name.is_empty() {
                    let parent = format!("{parent_owner}/{parent_name}");
                    let query = url::form_urlencoded::Serializer::new(String::new())
                        .append_pair("state", "all")
                        .append_pair("per_page", "100")
                        .append_pair("head", &format!("{owner}:{head}"))
                        .finish();
                    let refs = self
                        .json_array(vec!["api".into(), format!("repos/{parent}/pulls?{query}")])
                        .await?;
                    let mut refs: Vec<_> = array(&refs).iter().collect();
                    refs.sort_by_key(|v| {
                        !(s(input, "headSha") != "" && s(&v["head"], "sha") == s(input, "headSha"))
                    });
                    for row in refs.into_iter().take(3) {
                        if let Some(view) = self
                            .github_try_view(strings(&[
                                "pr",
                                "view",
                                &number(row, "number")?.to_string(),
                                "--repo",
                                &parent,
                            ]))
                            .await?
                        {
                            candidates.push(view);
                        }
                    }
                    selected = choose(candidates, input, &owner);
                    if let Some(value) = selected {
                        return self.github_status_facts(value, head).await;
                    }
                    return Ok(Value::Null);
                }
            }
            let values = match self
                .github_status_command(strings(&[
                    "pr", "list", "--state", "all", "--head", head, "--limit", "10",
                ]))
                .await
            {
                Ok(value) => value,
                Err(error)
                    if auth_state(&error) == "error"
                        && error_text(&error)
                            .to_lowercase()
                            .contains("no pull requests found") =>
                {
                    return Ok(Value::Null);
                }
                Err(error) => return Err(error),
            };
            candidates.extend(array(&values).iter().cloned());
            selected = choose(candidates, input, &owner);
        }
        match selected {
            Some(value) => self.github_status_facts(value, head).await,
            None => Ok(Value::Null),
        }
    }
    async fn github_status_facts(&self, value: Value, head: &str) -> Result<Value> {
        let mut out = status(&value, head);
        if out.is_null() {
            return Ok(out);
        }
        // `addCurrentPullRequestGithubFacts` needs the PR repo identity and a number.
        let Some((owner, name)) = pr_repo(s(&out, "url")) else {
            return Ok(out);
        };
        let Some(pr_number) = out["number"].as_u64() else {
            return Ok(out);
        };
        let data = match self.graphql(FACTS_QUERY, &owner, &name, pr_number).await
        {
            Ok(v) => v,
            Err(e) if auth_state(&e) == "error" => return Ok(out),
            Err(e) => return Err(e),
        };
        let repo = &data["data"]["repository"];
        let pr = &repo["pullRequest"];
        if repo.is_null() || pr.is_null() {
            return Ok(out);
        }
        let mut facts = json!({"forge":"github","mergeStateStatus":pr["mergeStateStatus"],"autoMergeRequest":null,"repository":{"autoMergeAllowed":b(repo,"autoMergeAllowed"),"mergeCommitAllowed":b(repo,"mergeCommitAllowed"),"squashMergeAllowed":b(repo,"squashMergeAllowed"),"rebaseMergeAllowed":b(repo,"rebaseMergeAllowed"),"viewerDefaultMergeMethod":repo["viewerDefaultMergeMethod"]}});
        for key in [
            "viewerCanEnableAutoMerge",
            "viewerCanDisableAutoMerge",
            "viewerCanMergeAsAdmin",
            "viewerCanUpdateBranch",
            "isMergeQueueEnabled",
            "isInMergeQueue",
        ] {
            facts[key] = b(pr, key).into();
        }
        let auto = &pr["autoMergeRequest"];
        if !auto.is_null() {
            facts["autoMergeRequest"] = json!({"enabledAt":auto["enabledAt"],"mergeMethod":auto["mergeMethod"],"enabledBy":auto["enabledBy"]["login"]});
        }
        out["forgeSpecific"] = facts;
        Ok(out)
    }
    pub(super) async fn github_timeline(&self, input: &Value) -> Result<Value> {
        let data = self
            .graphql(
                TIMELINE_QUERY,
                required(input, "repoOwner")?,
                required(input, "repoName")?,
                number(input, "prNumber")?,
            )
            .await?;
        let pr = &data["data"]["repository"]["pullRequest"];
        if pr.is_null() {
            return Ok(timeline_result(
                input,
                vec![],
                false,
                Some(json!({"kind":"not_found","message":"Pull request not found"})),
            ));
        }
        let mut items = Vec::new();
        let mut thread_ids = std::collections::HashSet::new();
        let mut truncated = ["reviews", "comments", "reviewThreads"]
            .iter()
            .any(|k| b(&pr[*k]["pageInfo"], "hasNextPage"));
        for thread in array(&pr["reviewThreads"]["nodes"]) {
            truncated |= b(&thread["comments"]["pageInfo"], "hasNextPage");
            for comment in array(&thread["comments"]["nodes"]) {
                thread_ids.insert(s(comment, "id").to_owned());
                let mut item = timeline_item(comment, false);
                if let Some(id) = comment["pullRequestReview"]["id"]
                    .as_str()
                    .filter(|v| !v.is_empty())
                {
                    item["reviewId"] = id.into();
                }
                let mut location = json!({"path":s(thread,"path"),"isResolved":b(thread,"isResolved"),"isOutdated":b(thread,"isOutdated")});
                for key in ["line", "startLine"] {
                    if let Some(value) = thread.get(key).filter(|v| !v.is_null()) {
                        location[key] = value.clone();
                    }
                }
                if !s(thread, "id").is_empty() {
                    location["threadId"] = thread["id"].clone();
                }
                item["location"] = location;
                items.push(item);
            }
        }
        for review in array(&pr["reviews"]["nodes"]) {
            let review_state = match s(review, "state") {
                "APPROVED" => "approved",
                "CHANGES_REQUESTED" => "changes_requested",
                "COMMENTED" => "commented",
                _ if !s(review, "body").trim().is_empty() => "commented",
                _ => continue,
            };
            let mut item = timeline_item(review, true);
            item["reviewState"] = review_state.into();
            items.push(item);
        }
        for comment in array(&pr["comments"]["nodes"]) {
            if !thread_ids.contains(s(comment, "id")) {
                items.push(timeline_item(comment, false));
            }
        }
        Ok(timeline_result(input, items, truncated, None))
    }
    async fn github_check_details(&self, input: &Value) -> Result<Value> {
        if s(input, "repoOwner").is_empty() || s(input, "repoName").is_empty() {
            bail!("GitHub getCheckDetails requires repoOwner and repoName");
        }
        let n = number(input, "checkRunId")
            .map_err(|_| anyhow!("GitHub getCheckDetails requires checkRunId"))?;
        let path = format!("repos/{}/{}", s(input, "repoOwner"), s(input, "repoName"));
        let check = self.api(format!("{path}/check-runs/{n}")).await?;
        number(&check, "id")?;
        let raw_annotations = self
            .json_array(strings(&[
                "api",
                &format!("{path}/check-runs/{n}/annotations"),
                "-f",
                "per_page=20",
            ]))
            .await?;
        let annotations: Vec<_> = array(&raw_annotations)
            .iter()
            .map(|a| {
                let mut out = json!({});
                // `toGitHubCheckAnnotations`: text fields only when non-empty, line
                // numbers whenever present (null included).
                for (target, source) in [
                    ("path", "path"),
                    ("annotationLevel", "annotation_level"),
                    ("message", "message"),
                    ("title", "title"),
                    ("rawDetails", "raw_details"),
                ] {
                    if !s(a, source).is_empty() {
                        out[target] = a[source].clone();
                    }
                }
                for (target, source) in [("startLine", "start_line"), ("endLine", "end_line")] {
                    if let Some(v) = a.get(source) {
                        out[target] = v.clone();
                    }
                }
                out
            })
            .collect();
        let workflow = input
            .get("workflowRunId")
            .filter(|v| !v.is_null())
            .unwrap_or(&check["check_suite"]["workflow_run"]["id"]);
        let mut truncated = annotations.len() >= 20;
        let mut failed_jobs = Vec::new();
        if let Some(id) = workflow.as_u64() {
            let raw = self
                .json(strings(&[
                    "api",
                    &format!("{path}/actions/runs/{id}/jobs"),
                    "-f",
                    "per_page=100",
                ]))
                .await?;
            let jobs = array(&raw["jobs"]);
            let failed: Vec<_> = jobs
                .iter()
                .filter(|j| {
                    matches!(
                        s(j, "conclusion"),
                        "failure" | "cancelled" | "timed_out" | "action_required"
                    )
                })
                .collect();
            truncated |= jobs.len() >= 100 || failed.len() > 5;
            for job in failed.into_iter().take(5) {
                let id = number(job, "id")?;
                let key = format!(
                    "{}:{}:{path}:{id}:{}",
                    environment_hash(self.env),
                    self.resolution.host,
                    s(job, "completed_at")
                );
                let cell = self.engine.logs.lock().await.cell(key, |_| None);
                let log = cell
                    .get_or_try_init(|| async {
                        let text = self
                            .run(strings(&["api", &format!("{path}/actions/jobs/{id}/logs")]))
                            .await?;
                        let (tail, truncated) = tail_log(&text);
                        Ok::<_, anyhow::Error>(json!({"logTail":tail,"logTruncated":truncated}))
                    })
                    .await?;
                truncated |= b(log, "logTruncated");
                failed_jobs.push(json!({"jobId":id,"name":s(job,"name"),"status":job["status"],"conclusion":job["conclusion"],"url":job["html_url"],"logTail":log["logTail"],"logTruncated":log["logTruncated"]}));
            }
        }
        Ok(
            json!({"checkRunId":check["id"],"workflowRunId":workflow,"name":s(&check,"name"),"status":check["status"],"conclusion":check["conclusion"],"url":check["html_url"],"detailsUrl":check["details_url"],"output":check["output"],"annotations":annotations,"failedJobs":failed_jobs,"truncated":truncated}),
        )
    }
    async fn github_repositories(&self, input: &Value) -> Result<Value> {
        let query = s(input, "query").trim();
        let limit = input["limit"].as_u64().unwrap_or(20).to_string();
        let args = if query.is_empty() {
            strings(&[
                "repo",
                "list",
                "--json",
                "id,name,nameWithOwner,description,isPrivate,updatedAt,sshUrl,url",
                "--limit",
                &limit,
            ])
        } else {
            strings(&[
                "search",
                "repos",
                query,
                "--json",
                "id,name,fullName,description,isPrivate,updatedAt,url",
                "--sort",
                "updated",
                "--order",
                "desc",
                "--limit",
                &limit,
            ])
        };
        let data = self.json(args).await?;
        let protocol = match self
            .run(strings(&[
                "config",
                "get",
                "git_protocol",
                "--host",
                "github.com",
            ]))
            .await
        {
            Ok(v) => v.trim().to_lowercase(),
            Err(e) if auth_state(&e) == "error" => "https".into(),
            Err(e) => return Err(e),
        };
        let values = array(&data).iter().map(|v| {
            let full = v["nameWithOwner"].as_str().or(v["fullName"].as_str()).unwrap_or("").trim();
            if !full.contains('/') { bail!("GitHub repository is missing owner identity: {full}"); }
            let ssh = v["sshUrl"].as_str().map(str::to_owned).unwrap_or_else(|| format!("git@github.com:{full}.git"));
            let id = v["id"].as_str().map(str::to_owned).unwrap_or_else(|| v["id"].to_string());
            Ok(json!({"id":id.trim(),"name":s(v,"name").trim(),"nameWithOwner":full,"description":v["description"],"visibility":if b(v,"isPrivate") {"private"} else {"public"},"updatedAt":s(v,"updatedAt"),"cloneUrl":if protocol=="ssh" {ssh.trim()} else {s(v,"url").trim()}}))
        }).collect::<Result<Vec<_>>>()?;
        Ok(values.into())
    }
}
fn summary(v: &Value, pr: bool) -> Result<Value> {
    let mut out = json!({"number":number(v,"number")?,"title":s(v,"title"),"url":s(v,"url"),"state":s(v,"state"),"body":v["body"],"labels":labels(&v["labels"]),"updatedAt":s(v,"updatedAt")});
    if pr {
        out["baseRefName"] = s(v, "baseRefName").into();
        out["headRefName"] = s(v, "headRefName").into();
    }
    Ok(out)
}
fn raw_state(v: &Value) -> String {
    if !s(v, "mergedAt").trim().is_empty() {
        "merged".into()
    } else {
        s(v, "state").to_lowercase()
    }
}
fn matches_head(v: &Value, input: &Value, owner: &str) -> bool {
    let head = if s(v, "headRefName").is_empty() {
        s(input, "headRef")
    } else {
        s(v, "headRefName")
    };
    head == s(input, "headRef")
        && pr_repo(s(v, "url")).is_some()
        && (raw_state(v) == "open"
            || (!s(input, "headSha").is_empty() && s(v, "headRefOid") == s(input, "headSha")))
        && (owner.is_empty() || s(&v["headRepositoryOwner"], "login") == owner)
}
pub(super) fn choose(mut values: Vec<Value>, input: &Value, owner: &str) -> Option<Value> {
    values.retain(|v| matches_head(v, input, owner));
    // `getPullRequestStateRank`: open or draft first, then merged, then closed.
    values.sort_by_key(|v| {
        (
            match raw_state(v).as_str() {
                "open" => 0,
                _ if b(v, "isDraft") => 0,
                "merged" => 1,
                _ => 2,
            },
            !(s(input, "headSha") != "" && s(v, "headRefOid") == s(input, "headSha")),
        )
    });
    values.into_iter().next()
}
pub(super) fn status(v: &Value, head: &str) -> Value {
    if s(v, "url").is_empty() || s(v, "title").is_empty() {
        return Value::Null;
    }
    let checks = checks(&v["statusCheckRollup"]);
    let decision = match s(v, "reviewDecision") {
        "APPROVED" => Some("approved"),
        "CHANGES_REQUESTED" => Some("changes_requested"),
        "REVIEW_REQUIRED" => Some("pending"),
        _ => None,
    };
    let mut out = json!({"url":s(v,"url"),"title":s(v,"title"),"state":raw_state(v),"baseRefName":s(v,"baseRefName"),"headRefName":if s(v,"headRefName").is_empty() {head} else {s(v,"headRefName")},"isMerged":!s(v,"mergedAt").trim().is_empty(),"isDraft":b(v,"isDraft"),"mergeable":match s(v,"mergeable") { x @ ("MERGEABLE"|"CONFLICTING")=>x,_=>"UNKNOWN" },"checksStatus":checks_status(&checks),"checks":checks,"reviewDecision":decision});
    if v["number"].is_number() {
        out["number"] = v["number"].clone();
    }
    if let Some((owner, name)) = pr_repo(s(v, "url")) {
        out["repoOwner"] = owner.into();
        out["repoName"] = name.into();
    }
    out
}
/// `parseGitHubPullRequestRepo`: host-agnostic `/owner/name/pull/N` identity.
pub(super) fn pr_repo(url: &str) -> Option<(String, String)> {
    let url = url::Url::parse(url).ok()?;
    let mut parts = url.path_segments()?.filter(|v| !v.is_empty());
    let owner = parts.next()?.to_owned();
    let name = parts.next()?.to_owned();
    (parts.next()? == "pull").then_some((owner, name))
}
pub(super) fn checks(value: &Value) -> Vec<Value> {
    let entries = if value.is_array() {
        array(value)
    } else {
        array(&value["contexts"])
    };
    let mut checks: Vec<(String, i64, Value)> = Vec::new();
    for v in entries {
        let (name, recency, check) = match s(v, "__typename") {
            "CheckRun" => {
                let status = if s(v, "status") != "COMPLETED" {
                    "pending"
                } else {
                    match s(v, "conclusion") {
                        "SUCCESS" => "success",
                        "FAILURE" | "TIMED_OUT" | "ACTION_REQUIRED" => "failure",
                        "CANCELLED" => "cancelled",
                        "SKIPPED" | "NEUTRAL" => "skipped",
                        _ => "pending",
                    }
                };
                let name = s(v, "name").to_owned();
                let run = &v["checkSuite"]["workflowRun"]["databaseId"];
                let recency = run.as_i64().unwrap_or_else(|| {
                    if !v["completedAt"].is_null() {
                        time(&v["completedAt"])
                    } else {
                        time(&v["startedAt"])
                    }
                });
                let mut check = json!({"name":name,"status":status,"url":v["detailsUrl"]});
                if !s(v, "workflowName").trim().is_empty() {
                    check["workflow"] = v["workflowName"].clone();
                }
                if v["databaseId"].is_number() {
                    check["checkRunId"] = v["databaseId"].clone();
                }
                if run.is_number() {
                    check["workflowRunId"] = run.clone();
                }
                let start = time(&v["startedAt"]);
                let end = time(&v["completedAt"]);
                let end = if end > 0 {
                    end
                } else {
                    chrono::Utc::now().timestamp_millis()
                };
                if start > 0 && end >= start {
                    check["duration"] = duration((end - start) / 1000).into();
                }
                (name, recency, check)
            }
            "StatusContext" => {
                let name = s(v, "context").to_owned();
                let status = match s(v, "state") {
                    "SUCCESS" => "success",
                    "FAILURE" | "ERROR" => "failure",
                    _ => "pending",
                };
                (
                    name.clone(),
                    time(&v["createdAt"]),
                    json!({"name":name,"status":status,"url":v["targetUrl"]}),
                )
            }
            _ => continue,
        };
        if let Some(old) = checks.iter_mut().find(|(n, _, _)| *n == name) {
            if recency > old.1 {
                *old = (name, recency, check);
            }
        } else {
            checks.push((name, recency, check));
        }
    }
    checks.into_iter().map(|(_, _, v)| v).collect()
}
fn duration(seconds: i64) -> String {
    let mut parts = Vec::new();
    if seconds >= 3600 {
        parts.push(format!("{}h", seconds / 3600));
    }
    if seconds % 3600 >= 60 {
        parts.push(format!("{}m", seconds % 3600 / 60));
    }
    if seconds % 60 > 0 || parts.is_empty() {
        parts.push(format!("{}s", seconds % 60));
    }
    parts.join(" ")
}
fn guard_merge(op: &str, input: &Value) -> Result<()> {
    let facts = &input["status"]["forgeSpecific"];
    let direct = op == "mergePullRequest";
    let disable = op == "disablePullRequestAutoMerge";
    if s(facts, "forge") != "github" {
        bail!(
            "GitHub {} facts are unavailable for this pull request",
            if direct { "merge" } else { "auto-merge" }
        );
    }
    let queue = b(facts, "isMergeQueueEnabled") || b(facts, "isInMergeQueue");
    let auto = !facts["autoMergeRequest"].is_null();
    if disable {
        if !auto {
            bail!("Auto-merge is not enabled for this pull request");
        }
        if !b(facts, "viewerCanDisableAutoMerge") {
            bail!("GitHub does not allow this viewer to disable auto-merge");
        }
        if queue {
            bail!("Auto-merge is not available because this repository uses a merge queue");
        }
        return Ok(());
    }
    let method = merge_method(input)?;
    let allowed = b(
        &facts["repository"],
        match method {
            "merge" => "mergeCommitAllowed",
            "squash" => "squashMergeAllowed",
            _ => "rebaseMergeAllowed",
        },
    );
    if direct {
        if !matches!(s(facts, "mergeStateStatus"), "CLEAN" | "HAS_HOOKS") {
            bail!("GitHub does not report this pull request as ready for direct merge");
        }
        if queue {
            bail!("Direct merge is not available because this repository uses a merge queue");
        }
        if auto {
            bail!("Direct merge is not available because auto-merge is already enabled");
        }
        if !allowed {
            bail!("Direct merge is not available because {method} is disabled");
        }
    } else {
        if s(facts, "mergeStateStatus") != "BLOCKED" {
            bail!("GitHub does not report this pull request as blocked for auto-merge");
        }
        if !b(facts, "viewerCanEnableAutoMerge") {
            bail!("GitHub does not allow this viewer to enable auto-merge");
        }
        if !b(&facts["repository"], "autoMergeAllowed") {
            bail!("Auto-merge is disabled for this repository");
        }
        if !allowed {
            bail!("Auto-merge is not available because {method} is disabled");
        }
        if auto {
            bail!("Auto-merge is already enabled for this pull request");
        }
        if queue {
            bail!("Auto-merge is not available because this repository uses a merge queue");
        }
        if s(&input["status"], "mergeable") == "CONFLICTING" {
            bail!("Auto-merge is not available because this pull request has conflicts");
        }
    }
    Ok(())
}
fn timeline_item(v: &Value, review: bool) -> Value {
    json!({"kind":if review {"review"} else {"comment"},"id":s(v,"id"),"author":v["author"]["login"].as_str().unwrap_or("unknown"),"authorUrl":v["author"]["url"],"avatarUrl":v["author"]["avatarUrl"],"body":normalize_body(s(v,"body"),s(v,"bodyHTML")),"createdAt":time(&v[if review {"submittedAt"} else {"createdAt"}]),"url":s(v,"url")})
}
/// `normalizeGitHubSearchQuery`: a pasted github.com (or Enterprise host) issue/PR
/// URL becomes its number; every other query passes through unchanged.
pub(super) fn normalize_search(query: &str, host: &str) -> String {
    use std::sync::LazyLock;
    static CLOUD: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(
            r"(?i)^https?://github\.com/[^/\s]+/[^/\s]+/(?:pull|issues)/(\d+)(?:[/?#].*)?$",
        )
        .expect("valid GitHub URL regex")
    });
    let trimmed = query.trim();
    if let Some(number) = CLOUD.captures(trimmed).and_then(|c| c.get(1)) {
        return number.as_str().to_owned();
    }
    // Only a non-cloud (Enterprise) host gets its own pattern.
    if cloud_forge(host) != Some("github") {
        let enterprise = regex::Regex::new(&format!(
            r"(?i)^https?://{}/[^/\s]+/[^/\s]+/(?:pull|issues)/(\d+)(?:[/?#].*)?$",
            regex::escape(host)
        ));
        if let Some(number) = enterprise
            .ok()
            .and_then(|re| re.captures(trimmed)?.get(1).map(|m| m.as_str().to_owned()))
        {
            return number;
        }
    }
    query.to_owned()
}
fn normalize_body(body: &str, html: &str) -> String {
    use std::sync::LazyLock;
    static MARKDOWN: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r#"!\[[^\]]*\]\(\s*([^\s)]+)(?:\s+[\"'][^)]*[\"'])?\s*\)"#).unwrap()
    });
    static HTML: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r#"(?i)<img\b[^>]*\bsrc\s*=\s*(?:\"([^\"]*)\"|'([^']*)')[^>]*>"#).unwrap()
    });
    let decode = |s: &str| {
        s.replace("&amp;", "&")
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
    };
    let mut sources: Vec<_> = MARKDOWN
        .captures_iter(body)
        .filter_map(|c| c.get(1))
        .map(|m| (m.start(), m.end(), m.as_str().to_owned()))
        .collect();
    // HTML `src` values that decode to nothing are not image references.
    sources.extend(
        HTML.captures_iter(body)
            .filter_map(|c| c.get(1).or_else(|| c.get(2)))
            .map(|m| (m.start(), m.end(), decode(m.as_str())))
            .filter(|(_, _, src)| !src.is_empty()),
    );
    sources.sort_by_key(|s| s.0);
    let rendered: Vec<_> = HTML
        .captures_iter(html)
        .filter_map(|c| c.get(1).or_else(|| c.get(2)))
        .map(|m| decode(m.as_str()))
        .filter(|src| !src.is_empty())
        .collect();
    if sources.is_empty() || sources.len() != rendered.len() {
        return body.to_owned();
    }
    let mut out = String::new();
    let mut cursor = 0;
    for ((start, end, raw), rendered) in sources.into_iter().zip(rendered) {
        let valid_raw = url::Url::parse(&raw).ok().is_some_and(|u| {
            u.scheme() == "https"
                && u.host_str() == Some("github.com")
                && u.path().starts_with("/user-attachments/assets/")
        });
        let valid_rendered = url::Url::parse(&rendered).ok().is_some_and(|u| {
            u.scheme() == "https"
                && matches!(
                    u.host_str(),
                    Some(
                        "camo.githubusercontent.com" | "private-user-images.githubusercontent.com"
                    )
                )
        });
        if !valid_raw || !valid_rendered || start < cursor {
            return body.to_owned();
        }
        out.push_str(&body[cursor..start]);
        out.push_str(&rendered);
        cursor = end;
    }
    out.push_str(&body[cursor..]);
    out
}
