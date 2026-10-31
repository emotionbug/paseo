use super::*;
use std::path::PathBuf;
struct Target {
    cwd: PathBuf,
    resolution: ForgeResolution,
    lookup: Value,
    /// The checkout's own lookup, before any fork-redirect owner hint: the per-target
    /// fallback re-resolves from it exactly like `runGitHubPoll(entry.target)`.
    target_lookup: Value,
    owner: String,
    name: String,
    key: String,
    cell: Arc<OnceCell<Value>>,
    visited: std::collections::HashSet<String>,
}
impl ForgeEngine {
    /// The observation owner supplies every due checkout in one call (five-second alignment).
    /// GitHub groups by authenticated host and batches at most 25 aliases per query.
    pub async fn poll_statuses(
        &self,
        cwds: &[PathBuf],
        env: &BTreeMap<String, String>,
        cancel: CancellationToken,
    ) -> Vec<(PathBuf, Value)> {
        let mut output = Vec::new();
        let mut groups: BTreeMap<String, Vec<Target>> = BTreeMap::new();
        for cwd in cwds {
            if cancel.is_cancelled() {
                break;
            }
            let prepared: Result<Option<Target>> = async {
                let Some(resolution) = self.resolve(cwd, env, cancel.clone()).await? else {
                    return Ok(None);
                };
                if resolution.forge != "github" {
                    return Ok(None);
                }
                let Some(mut lookup) = self.lookup_target(cwd, env, cancel.clone()).await? else {
                    return Ok(None);
                };
                let Some((owner, name)) = resolution
                    .project_path
                    .split_once('/')
                    .filter(|(_, n)| !n.contains('/'))
                else {
                    return Ok(None);
                };
                let mut owner = owner.to_owned();
                let mut name = name.to_owned();
                let key = read_key(cwd, env, "getCurrentPullRequestStatus", &lookup);
                let target_lookup = lookup.clone();
                let redirect_cell = self.redirects.lock().await.cell(
                    format!(
                        "{}:{}:{}",
                        environment_hash(env),
                        resolution.host,
                        resolution.project_path
                    ),
                    |_| None,
                );
                if let Some(redirect) = redirect_cell.get() {
                    owner = s(redirect, "owner").to_owned();
                    name = s(redirect, "name").to_owned();
                    if s(&lookup, "headRepositoryOwner").is_empty() {
                        lookup["headRepositoryOwner"] = redirect["headRepositoryOwner"].clone();
                    }
                }
                let cell = self
                    .reads
                    .lock()
                    .await
                    .cell(key.clone(), |_| Some(Duration::from_secs(30)));
                let mut visited = std::collections::HashSet::new();
                visited.insert(format!("{owner}/{name}"));
                Ok(Some(Target {
                    cwd: cwd.clone(),
                    resolution,
                    lookup,
                    target_lookup,
                    owner,
                    name,
                    key,
                    cell,
                    visited,
                }))
            }
            .await;
            match prepared {
                Ok(Some(target)) => groups
                    .entry(target.resolution.host.clone())
                    .or_default()
                    .push(target),
                Ok(None) => output.push((
                    cwd.clone(),
                    self.status_payload(cwd, env, cancel.clone()).await,
                )),
                Err(e) => output.push((cwd.clone(), error_payload(&e, None))),
            }
        }
        for (_, targets) in groups {
            let mut pending = targets.into_iter();
            loop {
                let batch: Vec<_> = pending.by_ref().take(25).collect();
                if batch.is_empty() {
                    break;
                }
                self.poll_github_batch(batch, env, &cancel, &mut output)
                    .await;
                if cancel.is_cancelled() {
                    break;
                }
            }
        }
        output
    }
    async fn poll_github_batch(
        &self,
        mut entries: Vec<Target>,
        env: &BTreeMap<String, String>,
        cancel: &CancellationToken,
        output: &mut Vec<(PathBuf, Value)>,
    ) {
        while !entries.is_empty() && !cancel.is_cancelled() {
            let query = status_query(&entries);
            let first = &entries[0];
            let response = self.poll_query(first, query, env, cancel, true).await;
            let response = match response {
                Ok(v) => v,
                Err(e) => {
                    for entry in entries {
                        output.push((entry.cwd, error_payload(&e, Some("github"))));
                    }
                    return;
                }
            };
            let mut redirects = Vec::new();
            let mut checks = Vec::new();
            for (index, mut entry) in entries.into_iter().enumerate() {
                let repo = &response["data"][format!("t{index}")];
                if !repo.is_object() || !repo["pullRequests"]["nodes"].is_array() {
                    let error = anyhow!(
                        "GitHub batch PR status response is missing repository {}/{}",
                        entry.owner,
                        entry.name
                    );
                    output.push((entry.cwd, error_payload(&error, Some("github"))));
                    continue;
                }
                let parent = &repo["parent"];
                if b(repo, "isFork")
                    && !s(&parent["owner"], "login").is_empty()
                    && !s(parent, "name").is_empty()
                {
                    let owner = s(&parent["owner"], "login");
                    let name = s(parent, "name");
                    if !entry.visited.insert(format!("{owner}/{name}")) {
                        output.push((
                            entry.cwd,
                            error_payload(
                                &anyhow!("GitHub fork repository redirect cycle"),
                                Some("github"),
                            ),
                        ));
                        continue;
                    }
                    if s(&entry.lookup, "headRepositoryOwner").is_empty() {
                        entry.lookup["headRepositoryOwner"] = repo["owner"]["login"]
                            .as_str()
                            .unwrap_or(&entry.owner)
                            .into();
                    }
                    let redirect_key = format!(
                        "{}:{}:{}",
                        environment_hash(env),
                        entry.resolution.host,
                        entry.resolution.project_path
                    );
                    let redirect_cell = self.redirects.lock().await.cell(redirect_key, |_| None);
                    let _=redirect_cell.set(json!({"owner":owner,"name":name,"headRepositoryOwner":entry.lookup["headRepositoryOwner"]}));
                    entry.owner = owner.to_owned();
                    entry.name = name.to_owned();
                    redirects.push(entry);
                    continue;
                }
                let nodes = array(&repo["pullRequests"]["nodes"]);
                // `selectBatchPollNode`: with a fork-owner hint the candidate head owner must
                // match it exactly; without one only heads owned by this repository count,
                // compared case-insensitively against the canonical response owner.
                let hint = s(&entry.lookup, "headRepositoryOwner").to_owned();
                let candidate = if hint.is_empty() {
                    let self_owner = repo["owner"]["login"]
                        .as_str()
                        .unwrap_or(&entry.owner)
                        .to_lowercase();
                    let own: Vec<Value> = nodes
                        .iter()
                        .filter(|node| {
                            s(&node["headRepositoryOwner"], "login").to_lowercase() == self_owner
                        })
                        .cloned()
                        .collect();
                    github::choose(own, &entry.lookup, "")
                } else {
                    github::choose(nodes.to_vec(), &entry.lookup, &hint)
                };
                if let Some(node) = candidate {
                    let frozen =
                        if s(&node, "state") != "OPEN" && !s(&node, "headRefOid").is_empty() {
                            Some(format!(
                                "{}:{}:{}/{}#{}@{}",
                                environment_hash(env),
                                entry.resolution.host,
                                entry.owner,
                                entry.name,
                                node["number"],
                                s(&node, "headRefOid")
                            ))
                        } else {
                            None
                        };
                    let cached = if let Some(key) = &frozen {
                        self.frozen
                            .lock()
                            .await
                            .cell(key.clone(), |_| None)
                            .get()
                            .cloned()
                    } else {
                        None
                    };
                    if let Some(rollup) = cached {
                        self.finish_poll(entry, node, repo.clone(), rollup, output)
                            .await;
                    } else {
                        checks.push((entry, node, repo.clone(), frozen));
                    }
                } else if nodes.len() >= 10 || b(repo, "isFork") {
                    let context = Context {
                        engine: self,
                        cwd: &entry.cwd,
                        env,
                        cancel,
                        resolution: &entry.resolution,
                    };
                    let result = context
                        .github("getCurrentPullRequestStatus", &entry.target_lookup)
                        .await;
                    match result {
                        Ok(status) => self.store_poll(entry, status, output).await,
                        Err(e) => output.push((entry.cwd, error_payload(&e, Some("github")))),
                    }
                } else {
                    self.store_poll(entry, Value::Null, output).await;
                }
            }
            if !checks.is_empty() {
                let query = checks_query(&checks);
                match self
                    .poll_query(&checks[0].0, query, env, cancel, false)
                    .await
                {
                    Ok(response) => {
                        for (index, (entry, node, repo, frozen)) in checks.into_iter().enumerate() {
                            let pr = &response["data"][format!("t{index}")]["pullRequest"];
                            if !pr.is_object() {
                                let error = anyhow!(
                                    "GitHub batch PR checks response is missing {}/{}#{}",
                                    entry.owner,
                                    entry.name,
                                    node["number"]
                                );
                                output.push((entry.cwd, error_payload(&error, Some("github"))));
                                continue;
                            }
                            let rollup:Vec<_>=array(&pr["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["contexts"]["nodes"]).iter().map(|v| {
                            let mut value=v.clone();if s(v,"__typename")=="CheckRun" {if let Some(workflow)=v["checkSuite"]["workflowRun"]["workflow"]["name"].as_str(){value["workflowName"]=workflow.into();}}value
                        }).collect();
                            let rollup = Value::Array(rollup);
                            if let Some(key) = frozen {
                                if !github::checks(&rollup)
                                    .iter()
                                    .any(|v| s(v, "status") == "pending")
                                {
                                    let cell = self.frozen.lock().await.cell(key, |_| None);
                                    let _ = cell.set(rollup.clone());
                                }
                            }
                            self.finish_poll(entry, node, repo, rollup, output).await;
                        }
                    }
                    Err(error) => {
                        for (entry, _, _, _) in checks {
                            output.push((entry.cwd, error_payload(&error, Some("github"))));
                        }
                    }
                }
            }
            entries = redirects;
        }
    }
    async fn poll_query(
        &self,
        target: &Target,
        query: String,
        env: &BTreeMap<String, String>,
        cancel: &CancellationToken,
        enforce_reserve: bool,
    ) -> Result<Value> {
        let key = format!("{}:{}", environment_hash(env), target.resolution.host);
        let paused = self
            .poll_paused
            .lock()
            .await
            .get(&key)
            .copied()
            .unwrap_or(0);
        if paused > chrono::Utc::now().timestamp_millis() {
            bail!(
                "GitHub GraphQL polling paused until {}",
                chrono::DateTime::from_timestamp_millis(paused)
                    .map(|v| v.to_rfc3339())
                    .unwrap_or_else(|| paused.to_string())
            );
        }
        let context = Context {
            engine: self,
            cwd: &target.cwd,
            env,
            cancel,
            resolution: &target.resolution,
        };
        let value = match context
            .json(vec![
                "api".into(),
                "graphql".into(),
                "-f".into(),
                format!("query={query}"),
            ])
            .await
        {
            Ok(v) => v,
            Err(error) => {
                let text = error_text(&error).to_lowercase();
                if (text.contains("rate limit") || text.contains("rate_limit"))
                    && (text.contains("exceed") || text.contains("exhaust"))
                {
                    // A loaded reset time arms the host pause and fails the batch; when it
                    // cannot be loaded, salvageable partial data is still used.
                    if let Ok(rate) = context.api("rate_limit".into()).await {
                        if let Some(reset) = rate["resources"]["graphql"]["reset"].as_i64() {
                            self.pause_host(key, reset * 1000 + 1000).await;
                            return Err(error);
                        }
                    }
                }
                let partial = error
                    .downcast_ref::<ForgeError>()
                    .and_then(|e| serde_json::from_str::<Value>(&e.stdout).ok())
                    .filter(|v| v["data"].is_object());
                match partial {
                    Some(v) => v,
                    None => return Err(error),
                }
            }
        };
        let rate = &value["data"]["rateLimit"];
        if let (Some(limit), Some(remaining)) = (rate["limit"].as_f64(), rate["remaining"].as_f64())
        {
            if remaining <= limit * 0.4 {
                let reset = time(&rate["resetAt"]);
                if reset > 0 {
                    self.pause_host(key, reset + 1000).await;
                    if enforce_reserve {
                        bail!("GitHub GraphQL polling paused until {}", s(rate, "resetAt"));
                    }
                }
            }
        }
        Ok(value)
    }
    async fn pause_host(&self, key: String, until: i64) {
        let mut pauses = self.poll_paused.lock().await;
        pauses.retain(|_, v| *v > chrono::Utc::now().timestamp_millis());
        if pauses.len() >= 512 {
            if let Some(oldest) = pauses
                .iter()
                .min_by_key(|(_, v)| *v)
                .map(|(k, _)| k.clone())
            {
                pauses.remove(&oldest);
            }
        }
        let value = pauses.entry(key).or_default();
        *value = (*value).max(until);
    }
    async fn finish_poll(
        &self,
        entry: Target,
        mut node: Value,
        repo: Value,
        rollup: Value,
        output: &mut Vec<(PathBuf, Value)>,
    ) {
        node["statusCheckRollup"] = rollup;
        let mut status = github::status(&node, s(&entry.lookup, "headRef"));
        if status.is_object() {
            let mut facts = json!({"forge":"github","mergeStateStatus":node["mergeStateStatus"],"autoMergeRequest":null,"repository":{"autoMergeAllowed":b(&repo,"autoMergeAllowed"),"mergeCommitAllowed":b(&repo,"mergeCommitAllowed"),"squashMergeAllowed":b(&repo,"squashMergeAllowed"),"rebaseMergeAllowed":b(&repo,"rebaseMergeAllowed"),"viewerDefaultMergeMethod":repo["viewerDefaultMergeMethod"]}});
            for field in [
                "viewerCanEnableAutoMerge",
                "viewerCanDisableAutoMerge",
                "viewerCanMergeAsAdmin",
                "viewerCanUpdateBranch",
                "isMergeQueueEnabled",
                "isInMergeQueue",
            ] {
                facts[field] = b(&node, field).into();
            }
            let auto = &node["autoMergeRequest"];
            if !auto.is_null() {
                facts["autoMergeRequest"] = json!({"enabledAt":auto["enabledAt"],"mergeMethod":auto["mergeMethod"],"enabledBy":auto["enabledBy"]["login"]});
            }
            status["forgeSpecific"] = facts;
        }
        self.store_poll(entry, status, output).await;
    }
    async fn store_poll(
        &self,
        entry: Target,
        mut status: Value,
        output: &mut Vec<(PathBuf, Value)>,
    ) {
        let mut reads = self.reads.lock().await;
        if let Some(current) = reads.entries.get(&entry.key) {
            if Arc::ptr_eq(&current.cell, &entry.cell) {
                let cell = Arc::new(OnceCell::new());
                let _ = cell.set(status.clone());
                let clock = reads.clock;
                reads.entries.insert(
                    entry.key,
                    Cached {
                        born: Instant::now(),
                        cell,
                        used: clock,
                    },
                );
            } else if let Some(newer) = current.cell.get() {
                status = newer.clone();
            }
        }
        drop(reads);
        if status.is_object() {
            status["forge"] = "github".into();
            if status["projectPath"].is_null()
                && !s(&status, "repoOwner").is_empty()
                && !s(&status, "repoName").is_empty()
            {
                status["projectPath"] =
                    format!("{}/{}", s(&status, "repoOwner"), s(&status, "repoName")).into();
            }
            if status["forgeSpecific"].is_object() {
                let mut facts = status["forgeSpecific"].clone();
                facts.as_object_mut().unwrap().remove("forge");
                status["github"] = facts;
            }
        }
        output.push((entry.cwd,json!({"status":status,"githubFeaturesEnabled":true,"authState":"authenticated","error":null,"forge":"github"})));
    }
}
fn error_payload(error: &anyhow::Error, forge: Option<&str>) -> Value {
    let state = auth_state(error);
    let mut out = json!({"status":null,"githubFeaturesEnabled":state=="error","authState":state,"error":{"code":"UNKNOWN","message":error.to_string()}});
    if let Some(forge) = forge {
        out["forge"] = forge.into();
    }
    out
}
fn status_query(entries: &[Target]) -> String {
    let mut query =
        String::from("query PaseoBatchPullRequestStatus { rateLimit { limit remaining resetAt } ");
    for (index, entry) in entries.iter().enumerate() {
        query.push_str(&format!("t{index}: repository(owner: {}, name: {}) {{ owner {{ login }} isFork parent {{ owner {{ login }} name }} autoMergeAllowed mergeCommitAllowed squashMergeAllowed rebaseMergeAllowed viewerDefaultMergeMethod pullRequests(headRefName: {}, first: 10, orderBy: {{field: CREATED_AT, direction: DESC}}) {{ nodes {{ ...PaseoPollPullRequest }} }} }}",json!(entry.owner),json!(entry.name),entry.lookup["headRef"]));
    }
    query.push_str(" } fragment PaseoPollPullRequest on PullRequest { number url title state isDraft baseRefName headRefName headRefOid mergedAt mergeable reviewDecision headRepositoryOwner { login } mergeStateStatus autoMergeRequest { enabledAt mergeMethod enabledBy { login } } viewerCanEnableAutoMerge viewerCanDisableAutoMerge viewerCanMergeAsAdmin viewerCanUpdateBranch isMergeQueueEnabled isInMergeQueue }");
    query
}
fn checks_query(entries: &[(Target, Value, Value, Option<String>)]) -> String {
    let mut query =
        String::from("query PaseoBatchPullRequestChecks { rateLimit { limit remaining resetAt } ");
    for (index, (entry, node, _, _)) in entries.iter().enumerate() {
        query.push_str(&format!("t{index}: repository(owner: {}, name: {}) {{ pullRequest(number: {}) {{ commits(last: 1) {{ nodes {{ commit {{ statusCheckRollup {{ contexts(first: 100) {{ nodes {{ __typename ... on CheckRun {{ databaseId name status conclusion detailsUrl startedAt completedAt checkSuite {{ workflowRun {{ databaseId workflow {{ name }} }} }} }} ... on StatusContext {{ context state targetUrl createdAt }} }} }} }} }} }} }} }} }}",json!(entry.owner),json!(entry.name),node["number"]));
    }
    query.push_str(" }");
    query
}
