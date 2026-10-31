use std::{
    path::{Path, PathBuf},
    sync::LazyLock,
    time::Duration,
};

use anyhow::{Result, anyhow, bail};
use regex::Regex;
use serde_json::{Map, Value, json};

use super::common::{UsageAccount, UsageRuntime, UsageScope, api_string, unavailable};

const ENDPOINT: &str = "https://api.github.com/copilot_internal/user";
static OAUTH_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"oauth_token:\s*["']?([a-zA-Z0-9_-]+)["']?"#)
        .expect("constant OAuth token pattern")
});

fn input(store: &str, locator: &str) -> Value {
    let mut object = Map::new();
    object.insert("store".into(), Value::String(store.into()));
    object.insert("locator".into(), Value::String(locator.into()));
    Value::Object(object)
}

fn fields(input: &Value) -> Result<(&str, &str)> {
    let object = input
        .as_object()
        .ok_or_else(|| anyhow!("Invalid Copilot usage input"))?;
    let store = object.get("store").and_then(Value::as_str);
    let locator = object
        .get("locator")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    match (store, locator) {
        (Some(store @ ("env" | "file")), Some(locator)) if object.len() == 2 => {
            Ok((store, locator))
        }
        _ => bail!("Invalid Copilot usage input"),
    }
}

fn oauth_token(raw: &str) -> Option<String> {
    // Preserve the source's first-match semantics, including quoted YAML values.
    OAUTH_TOKEN
        .captures(raw)?
        .get(1)
        .map(|m| m.as_str().to_owned())
}

async fn read_token(rt: &UsageRuntime, input: &Value) -> Result<Option<String>> {
    let (store, locator) = fields(input)?;
    if store == "env" {
        return Ok(rt.env.get(locator).filter(|s| !s.is_empty()).cloned());
    }
    let raw = tokio::select! {
        _ = rt.cancel.cancelled() => bail!("Copilot usage cancelled"),
        raw = tokio::fs::read(locator) => raw,
    };
    Ok(raw
        .ok()
        .and_then(|bytes| oauth_token(&String::from_utf8_lossy(&bytes))))
}

pub(super) async fn discover(rt: &UsageRuntime, scope: &UsageScope) -> Result<Vec<UsageAccount>> {
    // This plugin intentionally registers discovery only for global scope.
    if scope.provider.is_some() {
        return Ok(Vec::new());
    }
    let mut candidates: Vec<Value> = ["COPILOT_TOKEN", "GITHUB_TOKEN", "GITHUB_PAT"]
        .map(|name| input("env", name))
        .into();
    if let Some(appdata) = rt.env.get("APPDATA").filter(|s| !s.is_empty()) {
        candidates.push(input(
            "file",
            &Path::new(appdata)
                .join("GitHub CLI/hosts.yml")
                .to_string_lossy(),
        ));
    }
    let path: PathBuf = rt.home.join(".config/gh/hosts.yml");
    candidates.push(input("file", &path.to_string_lossy()));
    for input in candidates {
        if read_token(rt, &input).await?.is_some() {
            return Ok(vec![UsageAccount {
                key: "default".into(),
                label: None,
                harness: None,
                input,
            }]);
        }
    }
    Ok(Vec::new())
}

fn optional_string(value: &Value) -> Option<String> {
    (!value.is_null())
        .then(|| api_string(value))
        .filter(|s| !s.is_empty())
}

fn project(response: &Value) -> Result<Value> {
    if !response.is_object() {
        bail!("Invalid Copilot usage response");
    }
    let details = optional_string(&response["quota_reset_date"])
        .map(|reset| vec![json!({"id": "reset", "label": "Quota reset", "value": reset})])
        .unwrap_or_default();
    let mut report =
        json!({"status": "available", "windows": [], "balances": [], "details": details});
    if let Some(plan) = optional_string(&response["copilot_plan"]) {
        report["planLabel"] = Value::String(plan);
    }
    Ok(report)
}

pub(super) async fn fetch(rt: &UsageRuntime, input: &Value) -> Result<Value> {
    let token = read_token(rt, input)
        .await?
        .ok_or_else(|| anyhow!("Copilot login store no longer exists"))?;
    let request = rt
        .request(reqwest::Method::GET, ENDPOINT)
        .timeout(Duration::from_secs(15))
        .header("Authorization", format!("token {token}"))
        .header("Accept", "application/json")
        .header("Editor-Version", "vscode/1.96.2")
        .header("Editor-Plugin-Version", "copilot-chat/0.26.7")
        .header("User-Agent", "GitHubCopilotChat/0.26.7")
        .header("X-Github-Api-Version", "2025-04-01");
    let response = rt.send(request).await?;
    let status = response.status();
    if matches!(status.as_u16(), 401 | 403) {
        return Ok(unavailable(
            json!({"kind": "rejected", "status": status.as_u16()}),
        ));
    }
    if !status.is_success() {
        bail!("Copilot usage API returned {}", status.as_u16());
    }
    let body = tokio::select! {
        _ = rt.cancel.cancelled() => bail!("Copilot usage cancelled"),
        body = response.json::<Value>() => body?,
    };
    project(&body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projects_only_the_reported_plan_and_reset_detail() {
        assert_eq!(
            project(
                &json!({"copilot_plan": "business", "quota_reset_date": "2026-07-01T00:00:00Z",
            "quota_snapshots": {"premium_interactions": {"entitlement": 300, "remaining": 10}}})
            )
            .unwrap(),
            json!({"status": "available", "planLabel": "business", "windows": [], "balances": [],
                "details": [{"id": "reset", "label": "Quota reset", "value": "2026-07-01T00:00:00Z"}]})
        );
    }

    #[test]
    fn null_fields_are_absent_and_scalar_fields_coerce_like_source() {
        assert_eq!(
            project(&json!({"copilot_plan": null, "quota_reset_date": null})).unwrap(),
            json!({"status": "available", "windows": [], "balances": [], "details": []})
        );
        let report = project(&json!({"copilot_plan": false, "quota_reset_date": 123})).unwrap();
        assert_eq!(report["planLabel"], "false");
        assert_eq!(report["details"][0]["value"], "123");
        assert!(project(&Value::Null).is_err());
    }

    #[test]
    fn yaml_token_extraction_preserves_first_match_and_quotes() {
        assert_eq!(
            oauth_token(
                "github.com:\n  oauth_token: 'gho_fixture-123'\nother:\n  oauth_token: second"
            )
            .as_deref(),
            Some("gho_fixture-123")
        );
        assert_eq!(
            oauth_token("oauth_token: \"github_pat_fixture\"").as_deref(),
            Some("github_pat_fixture")
        );
        assert_eq!(oauth_token("github.com:\n  user: fixture"), None);
    }
}
