use anyhow::{Result, anyhow, ensure};
use reqwest::Method;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{path::Path, time::Duration};

use super::common::{
    UsageAccount, UsageRuntime, UsageScope, api_number, api_string, read_json, tone, unavailable,
    used_pct, window,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    store: Store,
    locator: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Store {
    Env,
    File,
}

async fn read_token(rt: &UsageRuntime, input: &Input) -> Option<String> {
    match input.store {
        Store::Env => rt
            .env
            .get(&input.locator)
            .filter(|v| !v.is_empty())
            .cloned(),
        Store::File => {
            let auth = read_json(Path::new(&input.locator)).await.ok()??;
            let record = auth.as_object()?;
            if let Some(token) = record
                .get("access_token")
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
            {
                return Some(token.to_owned());
            }
            let preferred = record
                .keys()
                .any(|key| key.starts_with("https://auth.x.ai::"));
            record
                .iter()
                .filter(|(key, _)| !preferred || key.starts_with("https://auth.x.ai::"))
                .find_map(|(_, value)| {
                    value
                        .get("key")
                        .and_then(Value::as_str)
                        .filter(|v| !v.is_empty())
                        .map(str::to_owned)
                })
        }
    }
}

pub(super) async fn discover(rt: &UsageRuntime, scope: &UsageScope) -> Result<Vec<UsageAccount>> {
    if scope.provider.is_some() {
        return Ok(Vec::new());
    }
    let candidates = [
        json!({"store":"env", "locator":"GROK_API_KEY"}),
        json!({"store":"env", "locator":"GROK_TOKEN"}),
        json!({"store":"file", "locator":rt.home.join(".grok/auth.json")}),
    ];
    for input in candidates {
        if read_token(rt, &Input::deserialize(&input)?).await.is_some() {
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

fn optional_object<'a>(parent: &'a Value, key: &str) -> Result<&'a Value> {
    let value = parent.get(key).unwrap_or(&Value::Null);
    ensure!(
        value.is_null() || value.is_object(),
        "Invalid Grok usage response: {key} must be an object"
    );
    Ok(value)
}

fn number(parent: &Value, key: &str) -> Result<Option<f64>> {
    parent
        .get(key)
        .map(|value| {
            api_number(value)
                .ok_or_else(|| anyhow!("Invalid Grok usage response: {key} must be finite"))
        })
        .transpose()
}

fn string(parent: &Value, key: &str) -> Option<String> {
    parent.get(key).filter(|v| !v.is_null()).map(api_string)
}

fn project(payload: &Value) -> Result<Value> {
    ensure!(
        payload.is_object(),
        "Invalid Grok usage response: expected object"
    );
    let config = optional_object(payload, "config")?;
    let usage = optional_object(payload, "usage")?;
    let limit = number(optional_object(config, "monthlyLimit")?, "val")?;
    let configured_used = number(optional_object(config, "used")?, "val")?;
    let legacy_used = number(usage, "creditUsage")?;
    let used = configured_used.or(legacy_used);
    let pct = number(config, "creditUsagePercent")?;
    let period = optional_object(config, "currentPeriod")?;
    let mut balances = Vec::new();
    if limit.is_some() || used.is_some() {
        balances.push(json!({
            "id": if limit.is_some() { "monthly_credits" } else { "credits" },
            "label": if limit.is_some() { "Monthly credits" } else { "Credits" },
            "used": used, "remaining": limit.zip(used).map(|(limit, used)| (limit-used).max(0.0)),
            "limit": limit, "unit": "credits", "tone": tone(used_pct(used, limit)),
        }));
    }
    let mut windows = Vec::new();
    if let Some(pct) = pct {
        let kind = string(period, "type");
        let unknown = kind.as_deref().filter(|s| !s.is_empty());
        let unknown_id = format!("period:{}", unknown.unwrap_or("unknown"));
        let (id, label, short) = match kind.as_deref() {
            Some("USAGE_PERIOD_TYPE_WEEKLY") => ("weekly", "Weekly", "wk"),
            Some("USAGE_PERIOD_TYPE_MONTHLY") => ("monthly", "Monthly", "mo"),
            _ => (unknown_id.as_str(), unknown.unwrap_or("Current period"), ""),
        };
        windows.push(window(
            id,
            label,
            Some(short),
            Some(pct),
            string(period, "end").as_deref(),
            false,
            Some(tone(Some(pct))),
        ));
    }
    Ok(json!({"status":"available", "windows":windows, "balances":balances, "details":[]}))
}

pub(super) async fn fetch(rt: &UsageRuntime, input: &Value) -> Result<Value> {
    let input = Input::deserialize(input)?;
    ensure!(
        !input.locator.is_empty(),
        "Grok usage locator must not be empty"
    );
    let token = read_token(rt, &input)
        .await
        .ok_or_else(|| anyhow!("Grok login store no longer exists"))?;
    let response = rt
        .send(
            rt.request(
                Method::GET,
                "https://cli-chat-proxy.grok.com/v1/billing?format=credits",
            )
            .bearer_auth(token)
            .header("X-XAI-Token-Auth", "xai-grok-cli")
            .header("Accept", "application/json")
            .timeout(Duration::from_secs(15)),
        )
        .await?;
    let status = response.status();
    if matches!(status.as_u16(), 401 | 403) {
        return Ok(unavailable(
            json!({"kind":"rejected", "status":status.as_u16()}),
        ));
    }
    ensure!(
        status.is_success(),
        "Grok usage API returned {}",
        status.as_u16()
    );
    let payload: Value = tokio::select! {
        _ = rt.cancel.cancelled() => return Err(anyhow!("Usage request cancelled")),
        result = response.json() => result?,
    };
    project(&payload)
}
