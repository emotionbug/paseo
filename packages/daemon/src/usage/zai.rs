use anyhow::{Result, anyhow, ensure};
use reqwest::Method;
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;

use super::common::{UsageAccount, UsageRuntime, UsageScope, api_string, unavailable};

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
}

pub(super) async fn discover(rt: &UsageRuntime, scope: &UsageScope) -> Result<Vec<UsageAccount>> {
    if scope.provider.is_some() {
        return Ok(Vec::new());
    }
    for locator in ["ZAI_API_KEY", "GLM_API_KEY"] {
        if rt.env.get(locator).is_some_and(|v| !v.is_empty()) {
            return Ok(vec![UsageAccount {
                key: "default".into(),
                label: None,
                harness: None,
                input: json!({"store":"env", "locator":locator}),
            }]);
        }
    }
    Ok(Vec::new())
}

fn string(value: &Value, key: &str) -> Option<String> {
    value.get(key).filter(|v| !v.is_null()).map(api_string)
}

fn project(payload: &Value) -> Result<Value> {
    ensure!(
        payload.is_object(),
        "Invalid Z.ai usage response: expected object"
    );
    let subscriptions = match payload.get("data") {
        None => None,
        Some(data) => Some(
            data.as_array()
                .ok_or_else(|| anyhow!("Invalid Z.ai usage response: data must be an array"))?,
        ),
    };
    if let Some(subscriptions) = subscriptions {
        ensure!(
            subscriptions.iter().all(Value::is_object),
            "Invalid Z.ai usage response: subscription must be an object"
        );
    }
    let Some(subscription) = subscriptions.and_then(|v| v.first()) else {
        return Ok(unavailable(
            json!({"kind":"no_quota", "detail":"No active coding plan"}),
        ));
    };
    let mut details = Vec::new();
    for (key, id, label) in [
        ("status", "status", "Status"),
        ("valid", "valid", "Valid"),
        ("purchaseTime", "purchase_time", "Purchased"),
    ] {
        if let Some(value) = string(subscription, key).filter(|v| !v.is_empty()) {
            details.push(json!({"id":id, "label":label, "value":value}));
        }
    }
    let mut report = json!({"status":"available", "windows":[], "balances":[], "details":details});
    if let Some(label) = string(subscription, "productName").filter(|v| !v.is_empty()) {
        report["planLabel"] = Value::String(label);
    }
    Ok(report)
}

pub(super) async fn fetch(rt: &UsageRuntime, input: &Value) -> Result<Value> {
    let input = Input::deserialize(input)?;
    let Store::Env = input.store;
    ensure!(
        !input.locator.is_empty(),
        "Z.ai usage locator must not be empty"
    );
    let token = rt
        .env
        .get(&input.locator)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| anyhow!("Z.ai login store no longer exists"))?;
    let response = rt
        .send(
            rt.request(Method::GET, "https://api.z.ai/api/biz/subscription/list")
                .bearer_auth(token)
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
        "Z.ai usage API returned {}",
        status.as_u16()
    );
    let payload: Value = tokio::select! {
        _ = rt.cancel.cancelled() => return Err(anyhow!("Usage request cancelled")),
        result = response.json() => result?,
    };
    project(&payload)
}
