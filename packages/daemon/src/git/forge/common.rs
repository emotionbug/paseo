use super::*;
pub(super) fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or("")
}
pub(super) fn b(v: &Value, key: &str) -> bool {
    v[key].as_bool().unwrap_or(false)
}
pub(super) fn array(v: &Value) -> &[Value] {
    v.as_array().map(Vec::as_slice).unwrap_or(&[])
}
pub(super) fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| (*v).to_owned()).collect()
}
pub(super) fn number_value(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_str()?.parse().ok())
}
pub(super) fn number(v: &Value, key: &str) -> Result<u64> {
    number_value(&v[key])
        .filter(|v| *v > 0)
        .ok_or_else(|| anyhow!("{key} must be a positive integer"))
}
pub(super) fn required<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("{key} is required"))
}
pub(super) fn time(v: &Value) -> i64 {
    v.as_str()
        .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok())
        .map_or(0, |v| v.timestamp_millis())
}
pub(super) fn encode(v: &str) -> String {
    percent_encoding::utf8_percent_encode(v, percent_encoding::NON_ALPHANUMERIC).to_string()
}
pub(super) fn boolish(v: &Value) -> bool {
    v.as_bool().unwrap_or_else(|| {
        v.as_str()
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("true"))
    })
}
pub(super) fn labels(v: &Value) -> Vec<String> {
    if let Some(text) = v.as_str() {
        return text
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
    }
    array(v)
        .iter()
        .filter_map(|v| v.as_str().or_else(|| v["name"].as_str()))
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}
pub(super) fn unavailable(state: &str) -> Value {
    json!({"items":[],"featuresEnabled":false,"authState":state,"githubFeaturesEnabled":false})
}
/// `normalizeForgeSearchKinds`: legacy GitHub aliases map to the neutral kinds.
pub(super) fn search_kinds(input: &Value) -> Vec<&str> {
    input["kinds"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(|s| match s {
                    "github-issue" => "issue",
                    "github-pr" | "pr" => "change_request",
                    _ => s,
                })
                .collect()
        })
        .unwrap_or_else(|| vec!["issue", "change_request"])
}
/// Each adapter's `isAuthFailureText`: gh matches phrases anywhere; glab and tea
/// match whole words only.
pub(super) fn auth_failure(binary: &str, text: &str) -> bool {
    use std::sync::LazyLock;
    static GLAB: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(
            r"(?i)\b(401|unauthorized|not logged in|authentication failed|no token|invalid token)\b",
        )
        .expect("valid glab auth regex")
    });
    static TEA: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(
            r"(?i)\b(401|unauthorized|not logged in|authentication failed|no token|invalid token|no logins?)\b",
        )
        .expect("valid tea auth regex")
    });
    match binary {
        "gh" => {
            let text = text.to_lowercase();
            [
                "gh auth login",
                "not logged into any github hosts",
                "authentication failed",
                "authentication required",
                "bad credentials",
                "http 401",
            ]
            .iter()
            .any(|phrase| text.contains(phrase))
        }
        "tea" => TEA.is_match(text),
        _ => GLAB.is_match(text),
    }
}
pub(super) fn error_text(error: &anyhow::Error) -> String {
    error
        .downcast_ref::<ForgeError>()
        .filter(|e| !e.stderr.is_empty())
        .map(|e| e.stderr.clone())
        .unwrap_or_else(|| error.to_string())
}
/// Each adapter's `map*TimelineError`: command failures are classified from their
/// stderr, auth failures are forbidden, and anything else is unknown.
pub(super) fn timeline_error(error: &anyhow::Error, binary: &str) -> Value {
    let Some(forge) = error.downcast_ref::<ForgeError>() else {
        return json!({"kind":"unknown","message":error.to_string()});
    };
    let detail = if forge.stderr.is_empty() {
        forge.message.clone()
    } else {
        forge.stderr.clone()
    };
    let (kind, message) = match forge.auth_state {
        // tea folds the missing CLI into the forbidden bucket with the class message.
        "cli_missing" | "unauthenticated" if binary == "tea" => {
            ("forbidden", forge.message.clone())
        }
        "unauthenticated" => ("forbidden", detail),
        "error" => (classify_timeline_failure(binary, &forge.stderr), detail),
        _ => ("unknown", forge.message.clone()),
    };
    json!({"kind":kind,"message":message})
}
fn classify_timeline_failure(binary: &str, stderr: &str) -> &'static str {
    let text = stderr.to_lowercase();
    if binary == "gh" {
        if [
            "could not resolve to a pullrequest",
            "pull request not found",
            "pullrequest not found",
        ]
        .iter()
        .any(|v| text.contains(v))
        {
            return "not_found";
        }
        if [
            "forbidden",
            "resource not accessible",
            "permission",
            "access denied",
            "requires authentication",
            "http 403",
        ]
        .iter()
        .any(|v| text.contains(v))
        {
            return "forbidden";
        }
        return "unknown";
    }
    if text.contains("404") || text.contains("not found") {
        "not_found"
    } else if text.contains("403") || text.contains("forbidden") || auth_failure(binary, stderr) {
        "forbidden"
    } else {
        "unknown"
    }
}
pub(super) fn timeline_result(
    input: &Value,
    mut items: Vec<Value>,
    truncated: bool,
    error: Option<Value>,
) -> Value {
    items.sort_by(|a, b| {
        a["createdAt"]
            .as_i64()
            .unwrap_or(0)
            .cmp(&b["createdAt"].as_i64().unwrap_or(0))
            .then_with(|| s(a, "id").cmp(s(b, "id")))
    });
    json!({"prNumber":input["prNumber"],"repoOwner":input["repoOwner"],"repoName":input["repoName"],"items":items,"truncated":truncated,"error":error})
}
pub(super) fn checks_status(checks: &[Value]) -> &'static str {
    if checks.is_empty() {
        "none"
    } else if checks.iter().any(|v| s(v, "status") == "failure") {
        "failure"
    } else if checks.iter().any(|v| s(v, "status") == "pending") {
        "pending"
    } else {
        "success"
    }
}
pub(super) fn repo_identity(value: &str) -> Option<(String, String)> {
    let url = url::Url::parse(value).ok()?;
    let mut parts = url.path_segments()?.filter(|v| !v.is_empty());
    Some((parts.next()?.to_owned(), parts.next()?.to_owned()))
}
pub(super) fn insert_identity(out: &mut Value, value: &str, project: bool) {
    if let Some((owner, name)) = repo_identity(value) {
        if project {
            out["projectPath"] = format!("{owner}/{name}").into();
        }
        out["repoOwner"] = owner.into();
        out["repoName"] = name.into();
    }
}
/// `extractMergeRequestUrl`/`parseIidFromUrl` (GitLab) and
/// `extractPullRequestUrl`/`parseIndexFromUrl` (Gitea): the first matching URL in
/// the CLI output and the number parsed from it.
pub(super) fn created_url(text: &str, gitlab: bool) -> Option<(String, Option<u64>)> {
    use std::sync::LazyLock;
    static GITLAB_URL: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"https?://\S+/-/merge_requests/\d+").expect("valid MR URL regex")
    });
    static GITLAB_NUMBER: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"/merge_requests/(\d+)").expect("valid MR iid regex")
    });
    static GITEA_URL: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"https?://\S+/pulls/\d+").expect("valid PR URL regex")
    });
    static GITEA_NUMBER: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"/pulls/(\d+)").expect("valid PR index regex"));
    let (url, number) = if gitlab {
        (&*GITLAB_URL, &*GITLAB_NUMBER)
    } else {
        (&*GITEA_URL, &*GITEA_NUMBER)
    };
    let url = url.find(text)?.as_str().to_owned();
    let number = number
        .captures(&url)
        .and_then(|c| c.get(1)?.as_str().parse().ok());
    Some((url, number))
}
pub(super) fn active_pipeline(raw: &str) -> bool {
    matches!(
        raw,
        "created"
            | "waiting_for_resource"
            | "preparing"
            | "pending"
            | "running"
            | "canceling"
            | "scheduled"
    )
}
pub(super) fn normalized_pipeline(raw: &str) -> &'static str {
    match raw {
        "success" | "passed" => "success",
        "failed" => "failed",
        "running" => "running",
        "created" => "created",
        "canceled" | "cancelled" => "canceled",
        "skipped" => "skipped",
        "manual" => "manual",
        v if active_pipeline(v) => "pending",
        _ => "unknown",
    }
}
pub(super) fn merge_method(input: &Value) -> Result<&str> {
    match s(input, "mergeMethod") {
        v @ ("merge" | "squash" | "rebase") => Ok(v),
        _ => bail!("mergeMethod must be merge, squash, or rebase"),
    }
}
pub(super) fn tail_log(log: &str) -> (String, bool) {
    let lines: Vec<_> = log.split('\n').collect();
    let mut out = lines[lines.len().saturating_sub(200)..].join("\n");
    let mut truncated = lines.len() > 200;
    if out.len() > 16 * 1024 {
        let mut start = out.len() - 16 * 1024;
        while !out.is_char_boundary(start) {
            start += 1;
        }
        out = out[start..].to_owned();
        truncated = true;
    }
    (out, truncated)
}

pub fn default_checkout_refs(forge: &str, number: u64) -> Vec<Value> {
    if forge == "gitlab" {
        return Vec::new();
    }
    vec![
        json!({"remoteName":"origin","remoteRef":format!("refs/pull/{number}/head")}),
        json!({"remoteName":"upstream","remoteRef":format!("refs/pull/{number}/head")}),
    ]
}
pub fn supports_cross_repo_checkout_without_refs(forge: &str) -> bool {
    forge == "github"
}
pub fn build_pr_local_branch_name(forge: &str, head_ref: &str, target: &Value) -> Option<String> {
    if forge == "gitlab" {
        return None;
    }
    let fallback = format!("pr-{}", number_value(&target["number"])?);
    let head = if s(target, "headRefKind") == "pull-ref" {
        fallback.as_str()
    } else {
        head_ref
    };
    if !b(target, "isCrossRepository") {
        return Some(head.to_owned());
    }
    let owner = s(target, "headOwnerLogin").trim().to_lowercase();
    let safe = owner
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphanumeric)
        && owner
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
        && !owner.contains("..")
        && !owner.ends_with('.')
        && !owner.ends_with(".lock");
    Some(if safe {
        format!("{owner}/{head}")
    } else if head == fallback {
        fallback
    } else {
        format!("{fallback}/{head}")
    })
}

pub(super) fn read_key(
    cwd: &Path,
    env: &BTreeMap<String, String>,
    operation: &str,
    input: &Value,
) -> String {
    let fields: &[&str] = match operation {
        "listPullRequests" | "listIssues" | "searchRepositories" => &["query", "limit"],
        "getPullRequest" | "getPullRequestHeadRef" | "getPullRequestCheckoutTarget" => &["number"],
        "getCurrentPullRequestStatus" => &["headRef", "headSha", "headRepositoryOwner"],
        "getPullRequestTimeline" => &["prNumber", "repoOwner", "repoName"],
        "getCheckDetails" => &[
            "repoOwner",
            "repoName",
            "checkRunId",
            "workflowRunId",
            "changeRequestNumber",
        ],
        "searchIssuesAndPrs" => &["query", "limit", "kinds"],
        _ => &[],
    };
    let mut args = BTreeMap::new();
    for field in fields {
        if let Some(value) = input.get(*field) {
            args.insert(*field, value.clone());
        }
    }
    if matches!(
        operation,
        "listPullRequests" | "listIssues" | "searchRepositories"
    ) {
        args.entry("query").or_insert_with(|| json!(""));
        args.entry("limit").or_insert_with(|| json!(20));
    }
    format!(
        "{}{}:{}",
        cache_prefix(cwd, env),
        operation,
        serde_json::to_string(&args).expect("JSON arguments serialize")
    )
}
