use anyhow::{Result, anyhow, ensure};
use chrono::NaiveDate;
use reqwest::Method;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::LazyLock,
    time::Duration,
};

use super::common::{
    UsageAccount, UsageRuntime, UsageScope, hash_account_key, read_json, tone, unavailable, window,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    path: String,
}

async fn read_key(path: &Path) -> Option<String> {
    let auth = read_json(path).await.ok()??;
    let record = auth.as_object()?.get("opencode-go")?.as_object()?;
    if record.get("type")?.as_str()? != "api" {
        return None;
    }
    record
        .get("key")?
        .as_str()
        .filter(|key| !key.is_empty())
        .map(str::to_owned)
}

pub(super) async fn discover(rt: &UsageRuntime, scope: &UsageScope) -> Result<Vec<UsageAccount>> {
    if scope.provider.is_some() {
        return Ok(Vec::new());
    }
    let data_home = rt
        .env
        .get("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            rt.env
                .get("HOME")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| rt.home.clone())
                .join(".local/share")
        });
    let path = data_home.join("opencode/auth.json");
    if read_key(&path).await.is_none() {
        return Ok(Vec::new());
    }
    Ok(vec![UsageAccount {
        key: hash_account_key(&path.to_string_lossy()),
        label: None,
        harness: None,
        input: json!({"path":path}),
    }])
}

fn valid_datetime(value: &str) -> bool {
    // z.iso.datetime() accepts UTC only, with optional seconds and arbitrary fractional precision.
    static DATETIME: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(
        r"^([0-9]{4})-([0-9]{2})-([0-9]{2})T([01][0-9]|2[0-3]):([0-5][0-9])(?::([0-5][0-9])(?:\.[0-9]+)?)?Z$"
    ).expect("constant ISO datetime pattern")
    });
    let Some(parts) = DATETIME.captures(value) else {
        return false;
    };
    let year = parts[1].parse::<i32>().ok();
    let month = parts[2].parse::<u32>().ok();
    let day = parts[3].parse::<u32>().ok();
    match (year, month, day) {
        (Some(year), Some(month), Some(day)) => NaiveDate::from_ymd_opt(year, month, day).is_some(),
        _ => false,
    }
}

fn project(payload: &Value) -> Result<Value> {
    ensure!(
        payload.is_object() && payload.get("usage").is_some_and(Value::is_object),
        "Invalid OpenCode Go usage response: expected usage object"
    );
    let mut windows = Vec::with_capacity(3);
    for (id, label, short) in [
        ("rolling", "Rolling", ""),
        ("weekly", "Weekly", "wk"),
        ("monthly", "Monthly", "mo"),
    ] {
        let value = &payload["usage"][id];
        ensure!(
            value.is_object(),
            "Invalid OpenCode Go usage response: missing {id} window"
        );
        ensure!(
            matches!(
                value.get("status").and_then(Value::as_str),
                Some("ok" | "rate-limited")
            ),
            "Invalid OpenCode Go usage response: invalid {id} status"
        );
        let percent = value
            .get("percent")
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite())
            .ok_or_else(|| anyhow!("Invalid OpenCode Go usage response: invalid {id} percent"))?;
        let reset = value
            .get("resetsAt")
            .and_then(Value::as_str)
            .filter(|v| valid_datetime(v))
            .ok_or_else(|| anyhow!("Invalid OpenCode Go usage response: invalid {id} resetsAt"))?;
        windows.push(window(
            id,
            label,
            Some(short),
            Some(percent),
            Some(reset),
            false,
            Some(tone(Some(percent))),
        ));
    }
    // This source supplies windows only, not synthetic empty balances/details.
    Ok(json!({"status":"available", "planLabel":"Go", "windows":windows}))
}

pub(super) async fn fetch(rt: &UsageRuntime, input: &Value) -> Result<Value> {
    let input = Input::deserialize(input)?;
    ensure!(
        !input.path.is_empty(),
        "OpenCode Go usage path must not be empty"
    );
    let key = read_key(Path::new(&input.path))
        .await
        .ok_or_else(|| anyhow!("OpenCode Go login store no longer exists"))?;
    let response = rt
        .send(
            rt.request(Method::GET, "https://opencode.ai/zen/go/v1/usage")
                .bearer_auth(key)
                .header("Accept", "application/json")
                .timeout(Duration::from_secs(15)),
        )
        .await?;
    let status = response.status();
    if matches!(status.as_u16(), 401 | 403) {
        return Ok(unavailable(
            json!({"kind":"rejected", "status":status.as_u16(), "refreshedBy":"opencode"}),
        ));
    }
    ensure!(
        status.is_success(),
        "OpenCode Go usage API returned {}",
        status.as_u16()
    );
    let payload: Value = tokio::select! {
        _ = rt.cancel.cancelled() => return Err(anyhow!("Usage request cancelled")),
        result = response.json() => result?,
    };
    project(&payload)
}
