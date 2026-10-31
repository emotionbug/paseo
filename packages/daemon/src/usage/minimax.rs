use anyhow::{Result, anyhow, ensure};
use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, SecondsFormat, TimeZone, Utc};
use reqwest::Method;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{path::Path, time::Duration};

use super::common::{
    UsageAccount, UsageRuntime, UsageScope, api_number, api_string, read_json, unavailable, window,
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
    Credentials,
    Config,
}
struct Auth {
    token: String,
    base_url: String,
    expires_at: Option<String>,
}

fn string(parent: &Value, key: &str) -> Option<String> {
    parent.get(key).filter(|v| !v.is_null()).map(api_string)
}

fn base_url(explicit: Option<String>, region: Option<&str>) -> String {
    explicit
        .filter(|v| v.starts_with("http"))
        .unwrap_or_else(|| {
            if region == Some("cn") {
                "https://api.minimaxi.com"
            } else {
                "https://api.minimax.io"
            }
            .into()
        })
}

fn credentials_valid(value: &Value) -> bool {
    value.is_object()
        && ["access_token", "refresh_token"]
            .iter()
            .all(|key| value.get(key).is_none_or(Value::is_string))
}

async fn read_auth(rt: &UsageRuntime, input: &Input) -> Option<Auth> {
    if matches!(input.store, Store::Env) {
        let token = rt
            .env
            .get(&input.locator)
            .filter(|v| !v.is_empty())?
            .clone();
        return Some(Auth {
            token,
            base_url: base_url(rt.env.get("MINIMAX_BASE_URL").cloned(), None),
            expires_at: None,
        });
    }
    let value = read_json(Path::new(&input.locator)).await.ok()??;
    if matches!(input.store, Store::Credentials) {
        if !credentials_valid(&value) {
            return None;
        }
        return Some(Auth {
            token: value
                .get("access_token")?
                .as_str()
                .filter(|v| !v.is_empty())?
                .to_owned(),
            base_url: base_url(string(&value, "resource_url"), None),
            expires_at: string(&value, "expires_at"),
        });
    }
    if !value.is_object()
        || ["api_key", "region"]
            .iter()
            .any(|key| value.get(key).is_some_and(|v| !v.is_string()))
    {
        return None;
    }
    if value.get("oauth").is_some_and(|v| !credentials_valid(v)) {
        return None;
    }
    let region = value.get("region").and_then(Value::as_str);
    if let Some(token) = value
        .get("api_key")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
    {
        return Some(Auth {
            token: token.to_owned(),
            base_url: base_url(string(&value, "base_url"), region),
            expires_at: None,
        });
    }
    let oauth = value.get("oauth")?;
    Some(Auth {
        token: oauth
            .get("access_token")?
            .as_str()
            .filter(|v| !v.is_empty())?
            .to_owned(),
        base_url: base_url(
            string(oauth, "resource_url").or_else(|| string(&value, "base_url")),
            region,
        ),
        expires_at: string(oauth, "expires_at"),
    })
}

pub(super) async fn discover(rt: &UsageRuntime, scope: &UsageScope) -> Result<Vec<UsageAccount>> {
    if scope.provider.is_some() {
        return Ok(Vec::new());
    }
    let candidates = [
        json!({"store":"env", "locator":"MINIMAX_API_KEY"}),
        json!({"store":"credentials", "locator":rt.home.join(".mmx/credentials.json")}),
        json!({"store":"config", "locator":rt.home.join(".mmx/config.json")}),
    ];
    let mut accounts = Vec::new();
    // Preserve all discovered stores, including the source's duplicate default keys.
    for input in candidates {
        if read_auth(rt, &Input::deserialize(&input)?).await.is_some() {
            accounts.push(UsageAccount {
                key: "default".into(),
                label: None,
                harness: None,
                input,
            });
        }
    }
    Ok(accounts)
}

fn number(parent: &Value, key: &str) -> Result<Option<f64>> {
    parent
        .get(key)
        .map(|v| {
            api_number(v)
                .ok_or_else(|| anyhow!("Invalid MiniMax usage response: {key} must be finite"))
        })
        .transpose()
}

fn epoch_iso(value: Option<f64>) -> Result<Option<String>> {
    value
        .filter(|v| *v > 0.0)
        .map(|value| {
            DateTime::<Utc>::from_timestamp_millis(value as i64)
                .map(|date| date.to_rfc3339_opts(SecondsFormat::Millis, true))
                .ok_or_else(|| anyhow!("Invalid time value"))
        })
        .transpose()
}

fn expiry(value: &str) -> Option<DateTime<Utc>> {
    if let Ok(date) = DateTime::parse_from_rfc3339(value) {
        return Some(date.with_timezone(&Utc));
    }
    if let Ok(date) = DateTime::parse_from_rfc2822(value) {
        return Some(date.with_timezone(&Utc));
    }
    for format in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f"] {
        if let Ok(date) = NaiveDateTime::parse_from_str(value, format) {
            return Local
                .from_local_datetime(&date)
                .earliest()
                .map(|date| date.with_timezone(&Utc));
        }
    }
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .ok()?
        .and_hms_opt(0, 0, 0)
        .map(|date| date.and_utc())
}

fn project(payload: &Value) -> Result<Value> {
    ensure!(
        payload.is_object(),
        "Invalid MiniMax usage response: expected object"
    );
    let base = payload.get("base_resp").unwrap_or(&Value::Null);
    ensure!(
        base.is_null() || base.is_object(),
        "Invalid MiniMax usage response: base_resp must be an object"
    );
    let status = number(base, "status_code")?;
    let models = payload.get("model_remains").unwrap_or(&Value::Null);
    ensure!(
        models.is_null() || models.is_array(),
        "Invalid MiniMax usage response: model_remains must be an array"
    );
    if let Some(models) = models.as_array() {
        for model in models {
            ensure!(
                model.is_object(),
                "Invalid MiniMax usage response: model must be an object"
            );
            for key in [
                "start_time",
                "end_time",
                "remains_time",
                "current_interval_total_count",
                "current_interval_usage_count",
                "current_interval_remaining_percent",
                "current_weekly_total_count",
                "current_weekly_usage_count",
                "current_weekly_remaining_percent",
                "current_interval_status",
                "current_weekly_status",
                "weekly_start_time",
                "weekly_end_time",
                "weekly_remains_time",
                "weekly_boost_permille",
            ] {
                number(model, key)?;
            }
        }
    }
    if status.is_some_and(|status| status != 0.0) {
        return Ok(unavailable(
            json!({"kind":"no_quota", "detail":string(base, "status_msg").unwrap_or_else(|| "No active coding plan".into())}),
        ));
    }
    let mut windows = Vec::new();
    if let Some(models) = models.as_array() {
        for model in models {
            let name = string(model, "model_name").unwrap_or_else(|| "token-plan".into());
            for (id, label, total_key, used_key, remaining_key, reset_key, status_key) in [
                (
                    "interval",
                    "Interval",
                    "current_interval_total_count",
                    "current_interval_usage_count",
                    "current_interval_remaining_percent",
                    "end_time",
                    "current_interval_status",
                ),
                (
                    "weekly",
                    "Weekly",
                    "current_weekly_total_count",
                    "current_weekly_usage_count",
                    "current_weekly_remaining_percent",
                    "weekly_end_time",
                    "current_weekly_status",
                ),
            ] {
                let pct = number(model, remaining_key)?
                    .map(|remaining| (100.0 - remaining).clamp(0.0, 100.0))
                    .or_else(|| {
                        api_number(model.get(total_key)?)
                            .filter(|total| *total > 0.0)
                            .zip(api_number(model.get(used_key)?))
                            .map(|(total, used)| {
                                // The upstream computes remaining first, then subtracts it again.
                                ((total - (total - used)) / total * 100.0).clamp(0.0, 100.0)
                            })
                    });
                let Some(pct) = pct else {
                    continue;
                };
                let tone = match number(model, status_key)? {
                    Some(2.0) => "danger",
                    Some(3.0) => "default",
                    _ => "ok",
                };
                windows.push(window(
                    &format!("{id}_{name}"),
                    &format!("{name} · {label}"),
                    None,
                    Some(pct),
                    epoch_iso(number(model, reset_key)?)?.as_deref(),
                    false,
                    Some(tone),
                ));
            }
        }
    }
    if windows.is_empty() {
        return Ok(unavailable(
            json!({"kind":"no_quota", "detail":"No active coding plan"}),
        ));
    }
    Ok(json!({"status":"available", "windows":windows, "balances":[], "details":[]}))
}

pub(super) async fn fetch(rt: &UsageRuntime, input: &Value) -> Result<Value> {
    let input = Input::deserialize(input)?;
    ensure!(
        !input.locator.is_empty(),
        "MiniMax usage locator must not be empty"
    );
    let auth = read_auth(rt, &input)
        .await
        .ok_or_else(|| anyhow!("MiniMax login store no longer exists"))?;
    if let Some(date) = auth
        .expires_at
        .as_deref()
        .and_then(expiry)
        .filter(|date| *date <= Utc::now())
    {
        return Ok(unavailable(
            json!({"kind":"expired", "expiresAt":date.to_rfc3339_opts(SecondsFormat::Millis, true)}),
        ));
    }
    let response = rt
        .send(
            rt.request(
                Method::GET,
                &format!("{}/v1/token_plan/remains", auth.base_url),
            )
            .bearer_auth(&auth.token)
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
        "MiniMax usage API returned {}",
        status.as_u16()
    );
    let payload: Value = tokio::select! {
        _ = rt.cancel.cancelled() => return Err(anyhow!("Usage request cancelled")),
        result = response.json() => result?,
    };
    project(&payload)
}
