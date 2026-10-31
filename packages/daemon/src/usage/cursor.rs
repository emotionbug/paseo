use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Result, anyhow, bail};
use base64::{
    Engine,
    engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD},
};
use chrono::{DateTime, NaiveDate, NaiveDateTime, SecondsFormat, Utc};
use rusqlite::{Connection, OpenFlags, OptionalExtension, types::ValueRef};
use serde_json::{Map, Value, json};

use super::common::{
    UsageAccount, UsageRuntime, UsageScope, api_number, hash_account_key, read_json, tone,
    unavailable, used_pct, window,
};

const ENDPOINT: &str = "https://api2.cursor.sh/aiserver.v1.DashboardService/GetCurrentPeriodUsage";

fn input(store: &str, locator: &str) -> Value {
    // The fallback account hash uses JavaScript's store-then-locator insertion order.
    let mut value = Map::new();
    value.insert("store".into(), Value::String(store.into()));
    value.insert("locator".into(), Value::String(locator.into()));
    Value::Object(value)
}

fn fields(value: &Value) -> Result<(&str, &str)> {
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("Invalid Cursor usage input"))?;
    let store = object.get("store").and_then(Value::as_str);
    let locator = object
        .get("locator")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    match (store, locator) {
        (Some(store @ ("env" | "file" | "sqlite")), Some(locator)) if object.len() == 2 => {
            Ok((store, locator))
        }
        _ => bail!("Invalid Cursor usage input"),
    }
}

fn sqlite_token(path: &Path) -> Option<String> {
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    let read = |key: &str| -> rusqlite::Result<Option<String>> {
        db.query_row("SELECT value FROM ItemTable WHERE key = ?", [key], |row| {
            Ok(match row.get_ref(0)? {
                ValueRef::Text(bytes) | ValueRef::Blob(bytes) => {
                    Some(String::from_utf8_lossy(bytes).into_owned())
                }
                _ => None,
            })
        })
        .optional()
        .map(Option::flatten)
    };
    if let Some(token) = read("cursorAuth/accessToken")
        .ok()?
        .filter(|s| !s.trim().is_empty())
    {
        return Some(token.trim().to_owned());
    }
    let legacy: Value = serde_json::from_str(&read("cursorAuthStatus").ok()??).ok()?;
    legacy
        .get("accessToken")?
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

async fn read_token(rt: &UsageRuntime, value: &Value) -> Result<Option<String>> {
    let (store, locator) = fields(value)?;
    match store {
        "env" => Ok(rt.env.get(locator).filter(|s| !s.is_empty()).cloned()),
        "sqlite" => {
            let path = PathBuf::from(locator);
            let task = tokio::task::spawn_blocking(move || sqlite_token(&path));
            tokio::select! {
                _ = rt.cancel.cancelled() => bail!("Cursor usage cancelled"),
                result = task => Ok(result?),
            }
        }
        _ => Ok(read_json(Path::new(locator))
            .await
            .ok()
            .flatten()
            .and_then(|value| {
                value
                    .get("accessToken")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
            })),
    }
}

fn account(token: &str, input: Value) -> UsageAccount {
    let mut parts = token.split('.');
    let subject = match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(_), Some(payload), Some(_), None) => URL_SAFE_NO_PAD
            .decode(payload)
            .or_else(|_| URL_SAFE.decode(payload))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|value| {
                value
                    .get("sub")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
            }),
        _ => None,
    };
    let key = match subject {
        Some(subject) => format!("account-{}", hash_account_key(&subject)),
        None => format!("store-{}", hash_account_key(&input.to_string())),
    };
    UsageAccount {
        key,
        label: None,
        harness: None,
        input,
    }
}

fn nonempty<'a>(env: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
    env.get(name).map(String::as_str).filter(|s| !s.is_empty())
}

fn session_path(env: &BTreeMap<String, String>, home: &Path, platform: &str) -> Option<PathBuf> {
    if nonempty(env, "CURSOR_API_KEY").is_some() || nonempty(env, "CURSOR_AUTH_TOKEN").is_some() {
        return None;
    }
    let store = env.get("AGENT_CLI_CREDENTIAL_STORE").map(String::as_str);
    // The source cannot name in-memory credentials or the macOS Keychain login.
    if store == Some("memory") || (platform == "macos" && store != Some("file")) {
        return None;
    }
    let home = nonempty(env, "HOME")
        .or_else(|| nonempty(env, "USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| home.to_path_buf());
    Some(match platform {
        "windows" => nonempty(env, "APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData/Roaming"))
            .join("Cursor/auth.json"),
        "macos" => home.join(".cursor/auth.json"),
        _ => nonempty(env, "XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"))
            .join("cursor/auth.json"),
    })
}

pub(super) async fn discover(rt: &UsageRuntime, scope: &UsageScope) -> Result<Vec<UsageAccount>> {
    let mut candidates = Vec::new();
    if let Some(provider) = scope.provider.as_deref() {
        if provider != "cursor" {
            return Ok(Vec::new());
        }
        let Some(path) = session_path(&scope.env, &rt.home, std::env::consts::OS) else {
            return Ok(Vec::new());
        };
        candidates.push(input("file", &path.to_string_lossy()));
    } else {
        candidates.extend(["CURSOR_ACCESS_TOKEN", "CURSOR_TOKEN"].map(|name| input("env", name)));
        if let Some(appdata) = nonempty(&rt.env, "APPDATA") {
            candidates.push(input(
                "sqlite",
                &Path::new(appdata)
                    .join("Cursor/User/globalStorage/state.vscdb")
                    .to_string_lossy(),
            ));
        }
        for path in [
            rt.home
                .join("Library/Application Support/Cursor/User/globalStorage/state.vscdb"),
            rt.home
                .join(".config/Cursor/User/globalStorage/state.vscdb"),
        ] {
            candidates.push(input("sqlite", &path.to_string_lossy()));
        }
        candidates.push(input(
            "file",
            &rt.home.join(".config/cursor/auth.json").to_string_lossy(),
        ));
    }
    for input in candidates {
        if let Some(token) = read_token(rt, &input).await? {
            return Ok(vec![account(&token, input)]);
        }
    }
    Ok(Vec::new())
}

// z.coerce.number() follows JavaScript Number(), including booleans and arrays.
fn number(value: &Value) -> Result<Option<f64>> {
    if value.is_null() {
        return Ok(None);
    }
    api_number(value)
        .map(Some)
        .ok_or_else(|| anyhow!("Invalid Cursor usage numeric field"))
}

fn billing_timestamp(value: &Value) -> Option<String> {
    let raw = match value {
        Value::String(s) => s.trim().to_owned(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    if raw.is_empty() {
        return None;
    }
    let date = if let Some(value) = api_number(value) {
        let millis = if value.abs() < 10_000_000_000.0 {
            value * 1000.0
        } else {
            value
        };
        if !millis.is_finite() || millis.abs() > 8_640_000_000_000_000.0 {
            return None;
        }
        DateTime::<Utc>::from_timestamp_millis(millis.trunc() as i64)?
    } else if let Ok(date) = DateTime::parse_from_rfc3339(&raw) {
        date.with_timezone(&Utc)
    } else if let Ok(date) = DateTime::parse_from_rfc2822(&raw) {
        date.with_timezone(&Utc)
    } else if let Ok(date) = NaiveDate::parse_from_str(&raw, "%Y-%m-%d") {
        date.and_hms_opt(0, 0, 0)?.and_utc()
    } else {
        NaiveDateTime::parse_from_str(&raw, "%Y-%m-%dT%H:%M:%S%.f")
            .ok()?
            .and_utc()
    };
    Some(date.to_rfc3339_opts(SecondsFormat::Millis, true))
}

fn project(response: &Value) -> Result<Value> {
    if !response.is_object() {
        bail!("Invalid Cursor usage response");
    }
    let reset = billing_timestamp(&response["billingCycleEnd"]);
    let mut windows = Vec::new();
    let mut balances = Vec::new();
    if let Some(plan) = response.get("planUsage").filter(|v| !v.is_null()) {
        if !plan.is_object() {
            bail!("Invalid Cursor plan usage");
        }
        let total = number(&plan["totalSpend"])?;
        number(&plan["includedSpend"])?;
        number(&plan["bonusSpend"])?;
        let remaining = number(&plan["remaining"])?;
        let limit = number(&plan["limit"])?;
        for (id, label, field) in [
            ("plan_usage", "Total", "totalPercentUsed"),
            ("cursor_usage", "Cursor", "autoPercentUsed"),
            ("third_party_usage", "Third Party", "apiPercentUsed"),
        ] {
            if let Some(pct) = number(&plan[field])? {
                windows.push(window(
                    id,
                    label,
                    None,
                    Some(pct),
                    reset.as_deref(),
                    id == "plan_usage",
                    Some(tone(Some(pct))),
                ));
            }
        }
        if windows.is_empty() {
            let used = total.map(|n| n / 100.0);
            let limit = limit.map(|n| n / 100.0);
            balances.push(json!({
                "id": "plan_usage", "label": "Plan usage", "used": used,
                "remaining": remaining.map(|n| n / 100.0), "limit": limit,
                "unit": "usd", "resetsAt": reset, "tone": tone(used_pct(used, limit)),
            }));
        }
    }
    Ok(json!({"status": "available", "windows": windows, "balances": balances, "details": []}))
}

pub(super) async fn fetch(rt: &UsageRuntime, input: &Value) -> Result<Value> {
    let token = read_token(rt, input)
        .await?
        .ok_or_else(|| anyhow!("Cursor login store no longer exists"))?;
    let request = rt
        .request(reqwest::Method::POST, ENDPOINT)
        .timeout(Duration::from_secs(15))
        .bearer_auth(token)
        .header("Content-Type", "application/json")
        .header("Connect-Protocol-Version", "1")
        .json(&json!({}));
    let response = rt.send(request).await?;
    let status = response.status();
    if matches!(status.as_u16(), 401 | 403) {
        return Ok(unavailable(
            json!({"kind": "rejected", "status": status.as_u16()}),
        ));
    }
    if !status.is_success() {
        bail!("Cursor usage API returned {}", status.as_u16());
    }
    let body = tokio::select! {
        _ = rt.cancel.cancelled() => bail!("Cursor usage cancelled"),
        body = response.json::<Value>() => body?,
    };
    project(&body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reported_percentages_override_bonus_inclusive_dollars() {
        let report = project(&json!({"planUsage": {"totalSpend": 5717, "limit": 2000,
            "totalPercentUsed": 11.54949494949495, "autoPercentUsed": 9.157777777777778,
            "apiPercentUsed": 35.46666666666667}, "billingCycleEnd": "2026-10-25T07:10:08Z"}))
        .unwrap();
        assert_eq!(report["balances"], json!([]));
        assert_eq!(report["windows"][0]["usedPct"], 11.54949494949495);
        assert_eq!(report["windows"][0]["summary"], true);
        assert_eq!(report["windows"][1]["id"], "cursor_usage");
        assert_eq!(report["windows"][2]["resetsAt"], "2026-10-25T07:10:08.000Z");
    }

    #[test]
    fn zero_pool_and_dollar_only_reports() {
        let report =
            project(&json!({"planUsage": {"totalPercentUsed": "0", "autoPercentUsed": null}}))
                .unwrap();
        assert_eq!(report["windows"].as_array().unwrap().len(), 1);
        assert_eq!(report["windows"][0]["remainingPct"], 100.0);
        let report = project(&json!({"planUsage": {"totalSpend": "1500", "remaining": "2500", "limit": "4000"}, "billingCycleEnd": "not-a-date"})).unwrap();
        assert_eq!(report["balances"][0]["used"], 15.0);
        assert_eq!(report["balances"][0]["remaining"], 25.0);
        assert_eq!(report["balances"][0]["resetsAt"], Value::Null);
        assert!(project(&json!({"planUsage": {"bonusSpend": "invalid"}})).is_err());
    }

    #[test]
    fn timestamps_accept_seconds_milliseconds_and_iso() {
        let expected = "2026-10-25T07:10:08.000Z";
        let millis = DateTime::parse_from_rfc3339(expected)
            .unwrap()
            .timestamp_millis();
        assert_eq!(billing_timestamp(&json!(millis)).as_deref(), Some(expected));
        assert_eq!(
            billing_timestamp(&json!((millis / 1000).to_string())).as_deref(),
            Some(expected)
        );
        assert_eq!(
            billing_timestamp(&json!(expected)).as_deref(),
            Some(expected)
        );
        assert_eq!(billing_timestamp(&json!("")), None);
    }

    #[test]
    fn session_paths_honor_source_credential_precedence() {
        let home = Path::new("/fixture");
        let mut env = BTreeMap::from([
            ("HOME".into(), "/session".into()),
            ("XDG_CONFIG_HOME".into(), "/xdg".into()),
        ]);
        assert_eq!(
            session_path(&env, home, "linux"),
            Some(PathBuf::from("/xdg/cursor/auth.json"))
        );
        assert_eq!(session_path(&env, home, "macos"), None);
        env.insert("AGENT_CLI_CREDENTIAL_STORE".into(), "file".into());
        assert_eq!(
            session_path(&env, home, "macos"),
            Some(PathBuf::from("/session/.cursor/auth.json"))
        );
        env.insert("APPDATA".into(), "/roaming".into());
        assert_eq!(
            session_path(&env, home, "windows"),
            Some(PathBuf::from("/roaming/Cursor/auth.json"))
        );
        env.insert("CURSOR_AUTH_TOKEN".into(), "fixture".into());
        assert_eq!(session_path(&env, home, "linux"), None);
    }

    #[test]
    fn identity_joins_subjects_but_keeps_opaque_stores_separate() {
        let token = format!(
            "e30.{}.signature",
            URL_SAFE_NO_PAD.encode(br#"{"sub":"user-a"}"#)
        );
        assert_eq!(
            account(&token, input("env", "CURSOR_TOKEN")).key,
            account(&token, input("file", "/fixture/auth.json")).key
        );
        assert_ne!(
            account("opaque", input("env", "CURSOR_TOKEN")).key,
            account("opaque", input("file", "/fixture/auth.json")).key
        );
        assert_eq!(
            account("opaque", input("file", "/fixture/auth.json")).key,
            account("rotated", input("file", "/fixture/auth.json")).key
        );
    }

    #[test]
    fn sqlite_reads_modern_text_and_legacy_blob_without_creating_missing_stores() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.vscdb");
        assert_eq!(sqlite_token(&path), None);
        assert!(!path.exists());
        let db = Connection::open(&path).unwrap();
        db.execute_batch("CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value BLOB)")
            .unwrap();
        db.execute(
            "INSERT INTO ItemTable VALUES (?1, ?2)",
            rusqlite::params![
                "cursorAuthStatus",
                br#"{"accessToken":"legacy"}"#.as_slice()
            ],
        )
        .unwrap();
        assert_eq!(sqlite_token(&path).as_deref(), Some("legacy"));
        db.execute(
            "INSERT INTO ItemTable VALUES (?1, ?2)",
            ["cursorAuth/accessToken", " modern "],
        )
        .unwrap();
        assert_eq!(sqlite_token(&path).as_deref(), Some("modern"));
    }
}
