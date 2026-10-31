use anyhow::{Result, anyhow, ensure};
use chrono::{DateTime, SecondsFormat, Utc};
use reqwest::Method;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::Duration,
};

use super::common::{
    UsageAccount, UsageRuntime, UsageScope, api_number, api_string, read_json, tone, unavailable,
    window,
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
struct Credentials {
    token: String,
    expires_at: Option<f64>,
}

async fn read_credentials(rt: &UsageRuntime, input: &Input) -> Option<Credentials> {
    if matches!(input.store, Store::Env) {
        return rt
            .env
            .get(&input.locator)
            .filter(|v| !v.is_empty())
            .map(|token| Credentials {
                token: token.clone(),
                expires_at: None,
            });
    }
    let value = read_json(Path::new(&input.locator)).await.ok()??;
    let object = value.as_object()?;
    for key in ["access_token", "refresh_token", "scope", "token_type"] {
        if object
            .get(key)
            .is_some_and(|v| !v.is_null() && !v.is_string())
        {
            return None;
        }
    }
    for key in ["expires_at", "expires_in"] {
        if object
            .get(key)
            .is_some_and(|v| !v.is_null() && api_number(v).is_none())
        {
            return None;
        }
    }
    let token = object
        .get("access_token")?
        .as_str()
        .filter(|v| !v.is_empty())?
        .to_owned();
    let expires_at = object
        .get("expires_at")
        .filter(|v| !v.is_null())
        .and_then(api_number);
    Some(Credentials { token, expires_at })
}

pub(super) async fn discover(rt: &UsageRuntime, scope: &UsageScope) -> Result<Vec<UsageAccount>> {
    if scope.provider.is_some() {
        return Ok(Vec::new());
    }
    let code_home = rt
        .env
        .get("KIMI_CODE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| rt.home.join(".kimi-code"));
    let candidates = [
        json!({"store":"env", "locator":"KIMI_TOKEN"}),
        json!({"store":"env", "locator":"KIMI_API_KEY"}),
        json!({"store":"file", "locator":code_home.join("credentials/kimi-code.json")}),
        json!({"store":"file", "locator":rt.home.join(".kimi/credentials/kimi-code.json")}),
    ];
    for input in candidates {
        if read_credentials(rt, &Input::deserialize(&input)?)
            .await
            .is_some()
        {
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

fn string(fields: &Value, key: &str) -> Option<String> {
    fields.get(key).filter(|v| !v.is_null()).map(api_string)
}

fn has_usage(fields: &Value) -> bool {
    fields.is_object()
        && ["limit", "used", "remaining"]
            .iter()
            .any(|key| fields.get(key).is_some_and(|v| !v.is_null()))
}

fn field_number(fields: &Value, key: &str) -> Option<f64> {
    // Kimi first coerces these fields to strings, then applies Number(). In particular,
    // booleans are not numeric quota values even though Number(true) itself is one.
    string(fields, key).and_then(|v| api_number(&Value::String(v)))
}

fn explicit_label(fields: &[&Value]) -> Option<String> {
    fields.iter().find_map(|fields| {
        ["name", "title", "scope"]
            .iter()
            .find_map(|key| string(fields, key))
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
    })
}

fn duration(fields: &Value) -> Option<f64> {
    fields
        .get("duration")
        .and_then(api_number)
        .filter(|v| *v > 0.0)
}

fn duration_label(duration: Option<f64>, unit: Option<&str>) -> Option<String> {
    let duration = duration?;
    let unit = unit?.to_ascii_uppercase();
    let unit = unit.strip_prefix("TIME_UNIT_").unwrap_or(&unit);
    let (duration, unit) = match unit {
        "MINUTE" if duration % 60.0 == 0.0 => (duration / 60.0, "hour"),
        "MINUTE" => (duration, "minute"),
        "HOUR" => (duration, "hour"),
        "DAY" => (duration, "day"),
        "WEEK" => (duration, "week"),
        "SECOND" => (duration, "second"),
        _ => return None,
    };
    Some(format!("{duration}-{unit} limit"))
}

fn id_part(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut separator = false;
    for c in value.chars() {
        if c.is_ascii_alphanumeric() {
            if separator && !result.is_empty() {
                result.push('_');
            }
            result.push(c.to_ascii_lowercase());
            separator = false;
        } else {
            separator = true;
        }
    }
    result
}

fn push_window(
    windows: &mut Vec<Value>,
    seen: &mut HashSet<String>,
    base: String,
    label: &str,
    fields: &Value,
) {
    let mut id = base.clone();
    let mut suffix = 2;
    while seen.contains(&id) {
        id = format!("{base}_{suffix}");
        suffix += 1;
    }
    seen.insert(id.clone());
    let pct = field_number(fields, "limit")
        .filter(|limit| *limit > 0.0)
        .and_then(|limit| {
            field_number(fields, "used")
                .or_else(|| field_number(fields, "remaining").map(|remaining| limit - remaining))
                .map(|used| (used / limit * 100.0).clamp(0.0, 100.0))
        });
    let reset = ["resetTime", "resetAt", "reset_time", "reset_at"]
        .iter()
        .find_map(|key| string(fields, key));
    windows.push(window(
        &id,
        label,
        None,
        pct,
        reset.as_deref(),
        false,
        Some(tone(pct)),
    ));
}

fn project(payload: &Value) -> Result<Value> {
    ensure!(
        payload.is_object(),
        "Invalid Kimi usage response: expected object"
    );
    let mut windows = Vec::new();
    let mut seen = HashSet::new();
    let usage = &payload["usage"];
    if has_usage(usage) {
        let label = explicit_label(&[usage])
            .or_else(|| duration_label(duration(usage), string(usage, "timeUnit").as_deref()))
            .unwrap_or_else(|| "Usage limit".into());
        push_window(
            &mut windows,
            &mut seen,
            "coding_usage".into(),
            &label,
            usage,
        );
    }
    if let Some(limits) = payload["limits"].as_array() {
        for (index, metadata) in limits.iter().enumerate() {
            if !metadata.is_object() {
                continue;
            }
            let detail = &metadata["detail"];
            let fields = if has_usage(detail) { detail } else { metadata };
            if !has_usage(fields) {
                continue;
            }
            let window_metadata = &metadata["window"];
            let label = explicit_label(&[metadata, fields]);
            // Select the first non-null descriptor before coercion: an invalid descriptor
            // is not permission to infer a different duration from a later field.
            let raw_duration = [window_metadata, metadata, fields]
                .iter()
                .find_map(|v| v.get("duration").filter(|v| !v.is_null()));
            let duration = raw_duration.and_then(api_number).filter(|v| *v > 0.0);
            let unit = [window_metadata, metadata, fields]
                .iter()
                .find_map(|v| string(v, "timeUnit"));
            let generated = duration_label(duration, unit.as_deref());
            let (id, label) = if let (Some(duration), Some(unit)) =
                (duration, unit.as_deref().filter(|v| !v.is_empty()))
            {
                (
                    format!(
                        "coding_limit_{}_{}",
                        id_part(&duration.to_string()),
                        id_part(unit)
                    ),
                    label
                        .or(generated)
                        .unwrap_or_else(|| format!("Limit {}", index + 1)),
                )
            } else if let Some(label) = label {
                (format!("coding_limit_{}", id_part(&label)), label)
            } else {
                (
                    format!("coding_limit_{}", index + 1),
                    generated.unwrap_or_else(|| format!("Limit {}", index + 1)),
                )
            };
            push_window(&mut windows, &mut seen, id, &label, fields);
        }
    }
    Ok(json!({"status":"available", "windows":windows, "balances":[], "details":[]}))
}

pub(super) async fn fetch(rt: &UsageRuntime, input: &Value) -> Result<Value> {
    let input = Input::deserialize(input)?;
    ensure!(
        !input.locator.is_empty(),
        "Kimi usage locator must not be empty"
    );
    let credentials = read_credentials(rt, &input)
        .await
        .ok_or_else(|| anyhow!("Kimi login store no longer exists"))?;
    if let Some(expires_at) = credentials
        .expires_at
        .filter(|v| *v * 1000.0 <= Utc::now().timestamp_millis() as f64)
    {
        let date = DateTime::<Utc>::from_timestamp_millis((expires_at * 1000.0) as i64)
            .ok_or_else(|| anyhow!("Invalid time value"))?;
        return Ok(unavailable(
            json!({"kind":"expired", "expiresAt":date.to_rfc3339_opts(SecondsFormat::Millis, true)}),
        ));
    }
    let response = rt
        .send(
            rt.request(Method::GET, "https://api.kimi.com/coding/v1/usages")
                .bearer_auth(&credentials.token)
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
        "Kimi usage API returned {}",
        status.as_u16()
    );
    let payload: Value = tokio::select! {
        _ = rt.cancel.cancelled() => return Err(anyhow!("Usage request cancelled")),
        result = response.json() => result?,
    };
    project(&payload)
}
