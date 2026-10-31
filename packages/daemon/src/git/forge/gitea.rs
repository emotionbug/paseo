use super::*;
use std::collections::{HashMap, HashSet};
const PR_FIELDS: &str =
    "index,state,author,url,title,body,mergeable,base,head,created,updated,labels,comments,ci";
const ISSUE_FIELDS: &str = "index,state,author,url,title,body,labels,comments,created,updated";
impl Context<'_> {
    async fn gitea_view(&self, n: u64) -> Result<Value> {
        let value = self
            .json(strings(&["pr", &n.to_string(), "-o", "json"]))
            .await?;
        number(&value, "index")?;
        Ok(value)
    }
    async fn gitea_rows(
        &self,
        pr: bool,
        input: &Value,
        state: &str,
        page: Option<u64>,
    ) -> Result<Vec<Value>> {
        let mut args = strings(&[
            if pr { "pr" } else { "issue" },
            "list",
            "--fields",
            if pr { PR_FIELDS } else { ISSUE_FIELDS },
            "--state",
            state,
            "-o",
            "json",
        ]);
        if let Some(limit) = input["limit"].as_u64() {
            args.extend(["--limit".into(), limit.to_string()]);
        }
        if let Some(page) = page {
            args.extend(["--page".into(), page.to_string()]);
        }
        let rows = self.json(args).await?;
        let query = s(input, "query").trim().to_lowercase();
        Ok(rows
            .as_array()
            .ok_or_else(|| anyhow!("tea JSON did not match the expected array schema"))?
            .iter()
            .filter(|v| query.is_empty() || s(v, "title").to_lowercase().contains(&query))
            .cloned()
            .collect())
    }
    pub(super) async fn gitea_list(&self, pr: bool, input: &Value) -> Result<Vec<Value>> {
        Ok(self
            .gitea_rows(pr, input, "open", None)
            .await?
            .iter()
            .map(|v| summary(v, pr))
            .collect())
    }
    async fn gitea_current_row(&self, input: &Value) -> Result<Option<Value>> {
        let head = required(input, "headRef")?;
        let identity = self
            .resolution
            .project_path
            .split_once('/')
            .filter(|(_, name)| !name.contains('/'));
        let owner = identity.map(|(owner, _)| owner);
        for page in 1..=4 {
            let rows = self
                .gitea_rows(true, &json!({"limit":50}), "open", Some(page))
                .await?;
            if let Some(row) = rows.iter().find(|v| matches_head(v, head, owner)) {
                return Ok(Some(row.clone()));
            }
            if rows.len() < 50 {
                break;
            }
        }
        let path = identity
            .map(|(o, n)| format!("repos/{}/{}", encode(o), encode(n)))
            .unwrap_or_else(|| "repos/{owner}/{repo}".into());
        let data = self
            .json_array(vec![
                "api".into(),
                format!("{path}/pulls?state=all&sort=recentupdate&page=1&limit=50"),
            ])
            .await?;
        let candidates: Vec<_> = array(&data)
            .iter()
            .filter(|v| {
                matches_head(&api_row(v), head, owner)
                    || (!s(input, "headSha").is_empty()
                        && s(&v["head"], "sha") == s(input, "headSha"))
            })
            .collect();
        Ok(candidates
            .iter()
            .find(|v| state(s(v, "state")) == "open")
            .or_else(|| {
                candidates.iter().find(|v| {
                    !s(input, "headSha").is_empty() && s(&v["head"], "sha") == s(input, "headSha")
                })
            })
            .map(|v| api_row(v)))
    }
    async fn gitea_combined(&self, owner: &str, name: &str, sha: &str) -> Result<Value> {
        self.api(format!(
            "repos/{}/{}/commits/{}/status",
            encode(owner),
            encode(name),
            encode(sha)
        ))
        .await
    }
    async fn gitea_action_pages(
        &self,
        owner: &str,
        name: &str,
        endpoint: &str,
        query: &str,
    ) -> Result<Vec<Value>> {
        let mut items = Vec::new();
        for page in 1..=5 {
            let prefix = if query.is_empty() {
                String::new()
            } else {
                format!("{query}&")
            };
            let value = self
                .api(format!(
                    "repos/{}/{}/actions/{endpoint}?{prefix}limit=50&page={page}",
                    encode(owner),
                    encode(name)
                ))
                .await?;
            if !value.is_object() {
                bail!("tea Actions JSON did not match expected schema");
            }
            // `workflow_runs` defaults to an empty page when omitted.
            let rows = array(&value["workflow_runs"]);
            if rows.is_empty() {
                break;
            }
            items.extend(rows.iter().cloned());
            if value["total_count"]
                .as_u64()
                .is_some_and(|n| items.len() as u64 >= n)
            {
                break;
            }
        }
        Ok(items)
    }
    async fn gitea_tasks(&self, owner: &str, name: &str, sha: &str) -> Result<Vec<Value>> {
        let data = match self.gitea_action_pages(owner, name, "tasks", "").await {
            Ok(v) => v,
            Err(e) if auth_state(&e) == "error" => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut tasks = latest_tasks(
            data.into_iter()
                .filter(|v| {
                    number_value(&v["id"]).is_some()
                        && v["status"].is_string()
                        && s(v, "head_sha") == sha
                })
                .collect(),
        );
        if tasks
            .iter()
            .any(|v| s(v, "status").eq_ignore_ascii_case("blocked"))
        {
            let runs = match self
                .gitea_action_pages(owner, name, "runs", &format!("head_sha={}", encode(sha)))
                .await
            {
                Ok(v) => v,
                Err(e) if auth_state(&e) == "error" => return Ok(tasks),
                Err(e) => return Err(e),
            };
            let run_numbers: HashSet<_> = runs
                .iter()
                .filter(|r| {
                    b(r, "need_approval")
                        && r["head_sha"].as_str().or(r["commit_sha"].as_str()) == Some(sha)
                })
                .filter_map(|r| {
                    number_value(&r["run_number"]).or_else(|| number_value(&r["index_in_repo"]))
                })
                .collect();
            for task in &mut tasks {
                if s(task, "status").eq_ignore_ascii_case("blocked")
                    && number_value(&task["run_number"]).is_some_and(|n| run_numbers.contains(&n))
                {
                    task["need_approval"] = true.into();
                }
            }
        }
        Ok(tasks)
    }
    async fn gitea_status(&self, input: &Value) -> Result<Value> {
        let Some(row) = self.gitea_current_row(input).await? else {
            return Ok(Value::Null);
        };
        let mut out = status(&row);
        // As in the source, optional checks cannot hide an otherwise resolved PR.
        let loaded: Result<Vec<Value>> = async {
            let n = number(&row, "index")?;
            let (owner, name) = repo_identity(s(&row, "url"))
                .ok_or_else(|| anyhow!("Missing repository identity"))?;
            let pr = self.gitea_view(n).await?;
            let sha = required(&pr, "headSha")?;
            let combined = match self.gitea_combined(&owner, &name, sha).await {
                Ok(v) => v,
                Err(e) if auth_state(&e) == "error" => json!({"statuses":[]}),
                Err(e) => return Err(e),
            };
            let tasks = self.gitea_tasks(&owner, &name, sha).await?;
            let ids: HashSet<_> = tasks.iter().filter_map(|v| run_id(s(v, "url"))).collect();
            let uncovered = array(&combined["statuses"]).iter().any(|v| {
                run_id(s(v, "target_url")).is_some_and(|id| !ids.contains(&id))
                    && commit_state(s(v, "status")) == "pending"
            });
            let mut required = HashSet::new();
            if uncovered {
                match self
                    .gitea_action_pages(&owner, &name, "runs", &format!("head_sha={}", encode(sha)))
                    .await
                {
                    Ok(runs) => {
                        for run in runs {
                            if run["head_sha"].as_str().or(run["commit_sha"].as_str()) != Some(sha)
                            {
                                continue;
                            }
                            let pr = &run["pull_requests"][0];
                            let cross = !pr["head"]["repo"]["id"].is_null()
                                && !pr["base"]["repo"]["id"].is_null()
                                && pr["head"]["repo"]["id"] != pr["base"]["repo"]["id"];
                            if b(&run, "need_approval")
                                || (matches!(
                                    s(&run, "status").to_lowercase().as_str(),
                                    "waiting" | "blocked"
                                ) && cross)
                            {
                                if let Some(id) = number_value(&run["id"]) {
                                    required.insert(id);
                                }
                            }
                        }
                    }
                    Err(e) if auth_state(&e) == "error" => {}
                    Err(e) => return Err(e),
                }
            }
            Ok(combine_checks(
                array(&combined["statuses"]),
                &tasks,
                &required,
            ))
        }
        .await;
        if let Ok(checks) = loaded {
            if !checks.is_empty() {
                out["checksStatus"] = checks_status(&checks).into();
                out["checks"] = checks.into();
            }
        }
        Ok(out)
    }
    pub(super) async fn gitea(&self, op: &str, input: &Value) -> Result<Value> {
        match op {
            "isAuthenticated" => Ok(self
                .json(strings(&["login", "list", "-o", "json"]))
                .await
                .ok()
                .is_some_and(|v| tea_login(&v, &self.resolution.host).is_some())
                .into()),
            "listPullRequests" => Ok(self.gitea_list(true, input).await?.into()),
            "listIssues" => Ok(self.gitea_list(false, input).await?.into()),
            "getCurrentPullRequestStatus" => self.gitea_status(input).await,
            "getPullRequest" | "getPullRequestHeadRef" | "getPullRequestCheckoutTarget" => {
                let n = number(input, "number")?;
                let view = self.gitea_view(n).await?;
                let summary = summary(&view, true);
                if op == "getPullRequest" {
                    return Ok(summary);
                }
                if op == "getPullRequestHeadRef" {
                    return Ok(summary["headRefName"].clone());
                }
                let mut cross = false;
                let mut kind = "branch";
                let mut head_owner = Value::Null;
                let mut head_ssh = Value::Null;
                let mut head_url = Value::Null;
                if let Some((owner, name)) = repo_identity(s(&summary, "url")) {
                    let raw = self
                        .api(format!(
                            "repos/{}/{}/pulls/{n}",
                            encode(&owner),
                            encode(&name)
                        ))
                        .await?;
                    let head = &raw["head"]["repo"];
                    let base = &raw["base"]["repo"];
                    if raw["flow"] == 1 || s(&raw["head"], "ref") == format!("refs/pull/{n}/head") {
                        kind = "pull-ref";
                    }
                    cross = !base["id"].is_null() && head["id"] != base["id"];
                    if cross {
                        head_owner = head["owner"]["login"].clone();
                        head_ssh = head["ssh_url"].clone();
                        head_url = head["html_url"].clone();
                    }
                }
                let mut refs = default_checkout_refs("gitea", n);
                if !cross && kind == "branch" && !s(&summary, "headRefName").is_empty() {
                    refs.insert(1,json!({"remoteName":"origin","remoteRef":format!("refs/heads/{}",s(&summary,"headRefName"))}));
                }
                Ok(
                    json!({"number":n,"baseRefName":summary["baseRefName"],"headRefName":summary["headRefName"],"checkoutRefs":refs,"headOwnerLogin":head_owner,"headRepositorySshUrl":head_ssh,"headRepositoryUrl":head_url,"isCrossRepository":cross,"headRefKind":kind}),
                )
            }
            "createPullRequest" => {
                let args = strings(&[
                    "pr",
                    "create",
                    "--title",
                    required(input, "title")?,
                    "--description",
                    s(input, "body"),
                    "--head",
                    required(input, "head")?,
                    "--base",
                    required(input, "base")?,
                ]);
                let out = self.run(args.clone()).await?;
                // Both failures are tea command errors over the redacted argv.
                let Some((url, number)) = created_url(&out, false) else {
                    return Err(self.command_error(
                        &args,
                        "tea reported a created pull request but returned no URL".into(),
                        String::new(),
                        None,
                    ));
                };
                let Some(number) = number else {
                    return Err(self.command_error(
                        &args,
                        format!("tea returned a pull request URL without an index: {url}"),
                        String::new(),
                        None,
                    ));
                };
                Ok(json!({"url":url,"number":number}))
            }
            "mergePullRequest" => {
                let facts = &input["status"]["forgeSpecific"];
                if s(facts, "forge") != "gitea" {
                    bail!("Gitea merge facts are unavailable for this pull request");
                }
                if b(facts, "hasMerged") {
                    bail!("This pull request is already merged");
                }
                if !b(facts, "mergeable") {
                    bail!("Gitea does not report this pull request as ready for direct merge");
                }
                self.run(strings(&[
                    "pr",
                    "merge",
                    &number(input, "prNumber")?.to_string(),
                    "--style",
                    merge_method(input)?,
                ]))
                .await?;
                Ok(json!({"success":true}))
            }
            // This is an explicit source capability boundary, not an unimplemented port.
            "enablePullRequestAutoMerge" | "disablePullRequestAutoMerge" => {
                bail!("{op} is not supported on Gitea yet")
            }
            "getCheckDetails" => self.gitea_check_details(input).await,
            _ => bail!("Unknown forge operation: {op}"),
        }
    }
    async fn gitea_timeline_pages(
        &self,
        owner: &str,
        name: &str,
        suffix: &str,
    ) -> Result<(Vec<Value>, bool)> {
        let mut items = Vec::new();
        for page in 1..=4 {
            let data = self
                .json_array(vec![
                    "api".into(),
                    format!(
                        "repos/{}/{}/{suffix}?page={page}&limit=50",
                        encode(owner),
                        encode(name)
                    ),
                ])
                .await?;
            let rows = data
                .as_array()
                .ok_or_else(|| anyhow!("tea timeline JSON did not match expected array schema"))?;
            items.extend(rows.iter().cloned());
            if rows.len() < 50 {
                return Ok((items, false));
            }
        }
        Ok((items, true))
    }
    pub(super) async fn gitea_timeline(&self, input: &Value) -> Result<Value> {
        let n = number(input, "prNumber")?;
        let owner = required(input, "repoOwner")?;
        let name = required(input, "repoName")?;
        let pr = self.gitea_view(n).await?;
        let comments_path = format!("issues/{n}/comments");
        let reviews_path = format!("pulls/{n}/reviews");
        let (comments, reviews) = tokio::join!(
            self.gitea_timeline_pages(owner, name, &comments_path),
            self.gitea_timeline_pages(owner, name, &reviews_path)
        );
        if comments.is_err() && reviews.is_err() {
            return Err(comments.err().unwrap());
        }
        let (comments, mut truncated) = comments.unwrap_or_default();
        let (reviews, review_truncated) = reviews.unwrap_or_default();
        truncated |= review_truncated;
        let mut items = Vec::new();
        for comment in comments {
            if comment.get("type").is_some() && s(&comment, "type") != "comment" {
                continue;
            }
            let mut item = timeline_item(&comment, false, false);
            if comment["html_url"].is_null() {
                item["url"] = format!("{}#issuecomment-{}", s(&pr, "url"), comment["id"]).into();
            }
            items.push(item);
        }
        for review in reviews {
            items.push(timeline_item(&review, true, false));
            if number_value(&review["comments_count"]).unwrap_or(0) > 0 {
                // Sequential fan-out is bounded below the source's five-process ceiling.
                if let Ok((comments, more)) = self
                    .gitea_timeline_pages(
                        owner,
                        name,
                        &format!("pulls/{n}/reviews/{}/comments", number(&review, "id")?),
                    )
                    .await
                {
                    truncated |= more;
                    items.extend(comments.iter().map(|v| timeline_item(v, false, true)));
                }
            }
        }
        Ok(timeline_result(input, items, truncated, None))
    }
    async fn gitea_check_details(&self, input: &Value) -> Result<Value> {
        let owner = s(input, "repoOwner");
        let name = s(input, "repoName");
        if owner.is_empty() || name.is_empty() {
            bail!("Gitea getCheckDetails requires repoOwner and repoName");
        }
        if input["checkRunId"].is_null() && input["workflowRunId"].is_null() {
            bail!("Gitea getCheckDetails requires a checkRunId or workflowRunId");
        }
        let n = if let Some(n) = number_value(&input["changeRequestNumber"]) {
            n
        } else {
            // `defaultResolveCurrentBranch`: a failed or empty read is "no branch".
            let branch = self
                .git(&["branch", "--show-current"])
                .await
                .unwrap_or_default();
            if branch.is_empty() {
                if self.cancel.is_cancelled() {
                    bail!("Forge request cancelled");
                }
                bail!("Gitea check details require a current branch");
            }
            let row = self
                .gitea_current_row(&json!({"headRef":branch}))
                .await?
                .ok_or_else(|| anyhow!("Gitea pull request for branch {branch} was not found"))?;
            number(&row, "index")?
        };
        let cache_key = format!("{}gitea-head:{n}", cache_prefix(self.cwd, self.env));
        let cell = self
            .engine
            .reads
            .lock()
            .await
            .cell(cache_key, |_| Some(Duration::from_secs(30)));
        let pr = cell.get_or_try_init(|| self.gitea_view(n)).await?;
        let sha = required(pr, "headSha")
            .map_err(|_| anyhow!("Gitea pull request #{n} did not include a head SHA"))?;
        let combined = match self.gitea_combined(owner, name, sha).await {
            Ok(v) => v,
            Err(e) if auth_state(&e) == "error" => json!({"statuses":[]}),
            Err(e) => return Err(e),
        };
        if let Some(status) = array(&combined["statuses"])
            .iter()
            .find(|v| !input["checkRunId"].is_null() && v["id"] == input["checkRunId"])
        {
            let context = if s(status, "context").is_empty() {
                format!("status-{}", status["id"])
            } else {
                s(status, "context").to_owned()
            };
            // `title: status.context || null`; `detailsUrl: status.url ?? null`.
            let title = if s(status, "context").is_empty() {
                Value::Null
            } else {
                status["context"].clone()
            };
            return Ok(
                json!({"checkRunId":status["id"],"name":context,"status":status["status"],"conclusion":commit_state(s(status,"status")),"url":status["target_url"],"detailsUrl":status["url"],"output":if status["description"].is_null() {Value::Null} else {json!({"title":title,"summary":status["description"],"text":null})},"annotations":[],"failedJobs":[],"truncated":false}),
            );
        }
        let tasks = self.gitea_tasks(owner, name, sha).await?;
        let id = number_value(&input["workflowRunId"])
            .or_else(|| number_value(&input["checkRunId"]))
            .ok_or_else(|| anyhow!("Invalid Gitea check ID"))?;
        let task = tasks
            .iter()
            .find(|v| number_value(&v["id"]) == Some(id))
            .ok_or_else(|| anyhow!("Gitea check {id} was not found"))?;
        // `display_title ?? name ?? workflow_id ?? null`: only null/absent falls through.
        let title = ["display_title", "name", "workflow_id"]
            .into_iter()
            .map(|key| &task[key])
            .find(|v| !v.is_null())
            .cloned()
            .unwrap_or(Value::Null);
        Ok(
            json!({"checkRunId":id,"workflowRunId":id,"name":task_name(task),"status":task["status"],"conclusion":task_state(s(task,"status")),"url":task["url"],"detailsUrl":task["url"],"output":{"title":title,"summary":task["workflow_id"],"text":null},"annotations":[],"failedJobs":[],"truncated":false}),
        )
    }
}
fn state(raw: &str) -> &'static str {
    if raw.eq_ignore_ascii_case("open") {
        "open"
    } else if raw.eq_ignore_ascii_case("merged") {
        "merged"
    } else {
        "closed"
    }
}
fn head(raw: &str) -> &str {
    raw.split_once(':').map_or(raw, |(_, head)| head)
}
fn summary(v: &Value, pr: bool) -> Value {
    let mut out = json!({"number":number_value(&v["index"]).unwrap_or(0),"title":s(v,"title"),"url":s(v,"url"),"state":if b(v,"hasMerged") {"merged"} else {state(s(v,"state"))},"body":if s(v,"body").is_empty() {Value::Null} else {v["body"].clone()},"labels":labels(&v["labels"]),"updatedAt":s(v,"updated")});
    if let Some((owner, name)) = repo_identity(s(v, "url")) {
        out["projectPath"] = format!("{owner}/{name}").into();
    }
    if pr {
        out["baseRefName"] = s(v, "base").into();
        out["headRefName"] = head(s(v, "head")).into();
    }
    out
}
fn matches_head(v: &Value, reference: &str, owner: Option<&str>) -> bool {
    if head(s(v, "head")) != reference {
        return false;
    }
    let actual = s(v, "head")
        .split_once(':')
        .map(|(owner, _)| owner.to_owned())
        .or_else(|| repo_identity(s(v, "url")).map(|(o, _)| o));
    owner.map_or_else(
        || !s(v, "head").contains(':'),
        |o| actual.as_deref() == Some(o),
    )
}
fn api_row(v: &Value) -> Value {
    let owner = s(&v["head"]["repo"]["owner"], "login");
    let base_owner = s(&v["base"]["repo"]["owner"], "login");
    let mut out = json!({"index":v["number"].to_string(),"state":if b(v,"merged") {"merged"} else {s(v,"state")},"url":s(v,"html_url"),"title":s(v,"title"),"body":s(v,"body"),"base":s(&v["base"],"ref"),"head":if !owner.is_empty() && owner!=base_owner {format!("{owner}:{}",s(&v["head"],"ref"))} else {s(&v["head"],"ref").to_owned()},"updated":s(v,"updated_at"),"labels":labels(&v["labels"]).join(",")});
    if !v["mergeable"].is_null() {
        out["mergeable"] = boolish(&v["mergeable"]).to_string().into();
    }
    out
}
fn ci_status(raw: &str) -> &'static str {
    match raw.to_lowercase().as_str() {
        "success" => "success",
        "failure" | "error" | "warning" => "failure",
        "pending" | "running" => "pending",
        _ => "none",
    }
}
/// `mapGiteaCommitState` (protocol `gitea-status`).
fn commit_state(raw: &str) -> &'static str {
    match raw.to_lowercase().as_str() {
        "success" => "success",
        "failure" | "error" | "warning" => "failure",
        "skipped" => "skipped",
        _ => "pending",
    }
}
fn task_state(raw: &str) -> &'static str {
    match raw.to_lowercase().as_str() {
        "success" => "success",
        "failure" | "failed" | "error" => "failure",
        "cancelled" | "canceled" => "cancelled",
        "skipped" => "skipped",
        _ => "pending",
    }
}
fn status(v: &Value) -> Value {
    let mut out = json!({"number":number_value(&v["index"]),"url":s(v,"url"),"title":s(v,"title"),"state":state(s(v,"state")),"baseRefName":s(v,"base"),"headRefName":head(s(v,"head")),"isMerged":state(s(v,"state"))=="merged","mergeable":if v.get("mergeable").is_none() {"UNKNOWN"} else if boolish(&v["mergeable"]) {"MERGEABLE"} else {"CONFLICTING"},"checks":[],"checksStatus":ci_status(s(v,"ci")),"reviewDecision":null,"forgeSpecific":{"forge":"gitea","mergeable":boolish(&v["mergeable"]),"hasMerged":state(s(v,"state"))=="merged","ciStatus":if s(v,"ci").is_empty() {Value::Null} else {v["ci"].clone()}}});
    insert_identity(&mut out, s(v, "url"), true);
    out
}
const RELATIVE_ORIGIN: &str = "https://gitea.invalid";
/// `getGiteaActionRunIdFromUrl`: resolves relative links against a placeholder origin.
fn run_id(raw: &str) -> Option<u64> {
    use std::sync::LazyLock;
    static RUN: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"/actions/runs/(\d+)(?:/jobs/\d+)?/?$").expect("valid run regex")
    });
    if raw.is_empty() {
        return None;
    }
    let url = url::Url::parse(RELATIVE_ORIGIN).ok()?.join(raw).ok()?;
    RUN.captures(url.path())?.get(1)?.as_str().parse().ok()
}
/// `isNativeActionJobUrl`: a job link of a fetched run on the tasks' own origin
/// (or a relative link).
fn native_job_url(raw: &str, origins: &HashSet<String>, runs: &HashSet<u64>) -> bool {
    use std::sync::LazyLock;
    static JOB: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"/actions/runs/\d+/jobs/\d+/?$").expect("valid job regex")
    });
    if raw.is_empty() {
        return false;
    }
    let Some(url) = url::Url::parse(RELATIVE_ORIGIN)
        .ok()
        .and_then(|base| base.join(raw).ok())
    else {
        return false;
    };
    if !JOB.is_match(url.path()) {
        return false;
    }
    let origin = url.origin().ascii_serialization();
    if origin != RELATIVE_ORIGIN && !origins.contains(&origin) {
        return false;
    }
    run_id(raw).is_some_and(|id| runs.contains(&id))
}
fn execution_id(task: &Value) -> String {
    if let Some(n) = number_value(&task["run_number"]) {
        format!("run-number\0{n}")
    } else if let Some(n) = run_id(s(task, "url")) {
        format!("run-id\0{n}")
    } else if !s(task, "url").is_empty() {
        format!("url\0{}", s(task, "url"))
    } else {
        format!("task\0{}", task["id"])
    }
}
fn workflow_id(task: &Value) -> String {
    if !s(task, "workflow_id").is_empty() {
        format!("workflow\0{}", s(task, "workflow_id"))
    } else if !s(task, "name").is_empty() || !s(task, "display_title").is_empty() {
        format!(
            "name\0{}",
            if !s(task, "name").is_empty() {
                s(task, "name")
            } else {
                s(task, "display_title")
            }
        )
    } else {
        format!("task\0{}", execution_id(task))
    }
}
fn latest_tasks(tasks: Vec<Value>) -> Vec<Value> {
    let mut latest: HashMap<String, &Value> = HashMap::new();
    let recency = |v: &Value| {
        (
            number_value(&v["run_number"]).unwrap_or(0),
            // `created_at ?? run_started_at`: only a null/absent field falls through.
            time(
                v.get("created_at")
                    .filter(|v| !v.is_null())
                    .unwrap_or(&v["run_started_at"]),
            ),
            number_value(&v["id"]).unwrap_or(0),
        )
    };
    for task in &tasks {
        let key = workflow_id(task);
        if latest
            .get(&key)
            .is_none_or(|old| recency(old) < recency(task))
        {
            latest.insert(key, task);
        }
    }
    let keep: HashMap<_, _> = latest
        .into_iter()
        .map(|(key, v)| (key, execution_id(v)))
        .collect();
    tasks
        .into_iter()
        .filter(|v| keep.get(&workflow_id(v)) == Some(&execution_id(v)))
        .collect()
}
fn task_name(task: &Value) -> String {
    ["name", "display_title", "workflow_id"]
        .into_iter()
        .map(|k| s(task, k))
        .find(|s| !s.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("actions-{}", task["id"]))
}
fn combine_checks(statuses: &[Value], tasks: &[Value], approvals: &HashSet<u64>) -> Vec<Value> {
    let origins: HashSet<_> = tasks
        .iter()
        .filter_map(|v| url::Url::parse(s(v, "url")).ok())
        .map(|u| u.origin().ascii_serialization())
        .collect();
    let runs: HashSet<_> = tasks.iter().filter_map(|v| run_id(s(v, "url"))).collect();
    let mut out = Vec::new();
    for status in statuses {
        let target = s(status, "target_url");
        let shadow = native_job_url(target, &origins, &runs);
        if shadow {
            continue;
        }
        let required = run_id(target).is_some_and(|id| approvals.contains(&id));
        let mut check = json!({"name":if s(status,"context").is_empty() {format!("status-{}",status["id"])} else {s(status,"context").to_owned()},"status":if required {"pending"} else {commit_state(s(status,"status"))},"url":status["target_url"],"checkRunId":status["id"]});
        if required {
            check["traits"] = json!(["action_required"]);
        } else if s(status, "status").eq_ignore_ascii_case("warning") {
            check["traits"] = json!(["warning"]);
        }
        out.push(check);
    }
    let mut seen = HashSet::new();
    for task in tasks {
        let Some(id) = number_value(&task["id"]) else {
            continue;
        };
        if !seen.insert(id) {
            continue;
        }
        let mut check = json!({"name":task_name(task),"status":task_state(s(task,"status")),"url":task["url"],"workflowRunId":id});
        if s(task, "status").eq_ignore_ascii_case("blocked") && b(task, "need_approval") {
            check["traits"] = json!(["action_required"]);
        }
        out.push(check);
    }
    out
}
fn timeline_item(v: &Value, review: bool, inline: bool) -> Value {
    let user = &v["user"];
    let timestamp = if review {
        v.get("submitted_at")
            .filter(|v| !v.is_null())
            .unwrap_or(&v["updated_at"])
    } else if inline {
        v.get("created_at")
            .filter(|v| !v.is_null())
            .unwrap_or(&v["updated_at"])
    } else {
        // Issue comments read `created_at` only.
        &v["created_at"]
    };
    let mut out = json!({"kind":if review {"review"} else {"comment"},"id":v["id"].to_string(),"author":user["login"].as_str().or(user["full_name"].as_str()).unwrap_or("unknown"),"authorUrl":user["html_url"],"avatarUrl":user["avatar_url"],"body":s(v,"body"),"createdAt":time(timestamp),"url":s(v,"html_url")});
    if review {
        out["reviewState"] = match s(v, "state").trim().to_uppercase().as_str() {
            "APPROVE" | "APPROVED" => "approved",
            "REQUEST_CHANGES" | "REQUESTED_CHANGES" | "CHANGES_REQUESTED" => "changes_requested",
            _ => "commented",
        }
        .into();
    }
    if inline {
        if !v["pull_request_review_id"].is_null() {
            out["reviewId"] = v["pull_request_review_id"].to_string().into();
        }
        if !s(v, "path").is_empty() {
            let position = v
                .get("position")
                .filter(|v| !v.is_null())
                .or(v.get("original_position").filter(|v| !v.is_null()));
            let anchor = position
                .map(|v| format!("pos-{v}"))
                .unwrap_or_else(|| format!("comment-{}", v["id"]));
            let mut location =
                json!({"path":s(v,"path"),"threadId":format!("{}#{anchor}",s(v,"path"))});
            if !v["position"].is_null() {
                location["line"] = v["position"].clone();
            }
            if let Some(resolver) = v.get("resolver") {
                location["isResolved"] = (!resolver.is_null()).into();
            }
            out["location"] = location;
        }
    }
    out
}
