use super::common::{
    UsageAccount, UsageRuntime, UsageScope, duration_window, tone, unavailable, window,
};
use crate::providers::muse::usage_account_key;
use anyhow::{Result, anyhow, bail};
use chrono::{SecondsFormat, Utc};
use serde_json::{Value, json};

pub(super) async fn discover(rt: &UsageRuntime, scope: &UsageScope) -> Result<Vec<UsageAccount>> {
    if scope
        .provider
        .as_deref()
        .is_some_and(|provider| provider != "muse")
    {
        return Ok(Vec::new());
    }
    let selected = scope
        .provider
        .as_ref()
        .map(|_| usage_account_key(&scope.env));
    let mut seen = std::collections::HashSet::new();
    let mut accounts = Vec::new();
    for provider in &rt.muse_providers {
        for env in provider.known_usage_environments() {
            let account = usage_account_key(&env);
            if selected
                .as_ref()
                .is_some_and(|selected| selected != &account)
                || !seen.insert(account.clone())
            {
                continue;
            }
            accounts.push(UsageAccount {
                key: account.clone(),
                label: Some("Muse Code".into()),
                harness: None,
                input: json!({"account":account}),
            });
        }
    }
    Ok(accounts)
}

pub(super) async fn fetch(rt: &UsageRuntime, input: &Value) -> Result<Value> {
    let object = input
        .as_object()
        .ok_or_else(|| anyhow!("Invalid Muse usage input"))?;
    let account = object
        .get("account")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Invalid Muse usage account"))?;
    if object.len() != 1 {
        bail!("Invalid Muse usage input");
    }
    for provider in rt.muse_providers.iter().rev() {
        if let Some(env) = provider
            .known_usage_environments()
            .into_iter()
            .find(|env| usage_account_key(env) == account)
        {
            let response = provider.read_usage(env, rt.cancel.clone()).await?;
            return project(&response);
        }
    }
    bail!("Muse login no longer exists")
}

fn number(value: &Value, name: &str) -> Result<f64> {
    value
        .as_f64()
        .filter(|number| number.is_finite())
        .ok_or_else(|| anyhow!("Invalid Muse usage field: {name}"))
}
fn iso(value: &Value, name: &str) -> Result<String> {
    let millis = number(value, name)?;
    if millis.abs() > 8_640_000_000_000_000.0 {
        bail!("Invalid Muse usage timestamp: {name}");
    }
    chrono::DateTime::<Utc>::from_timestamp_millis(millis as i64)
        .map(|date| date.to_rfc3339_opts(SecondsFormat::Millis, true))
        .ok_or_else(|| anyhow!("Invalid Muse usage timestamp: {name}"))
}

// plugins/muse-provider/server/usage.ts and wire.ts: no reported quota is an
// unavailable result, not a zero-utilization quota or a reason to read another account.
fn project(response: &Value) -> Result<Value> {
    let response = response
        .as_object()
        .ok_or_else(|| anyhow!("Invalid Muse usage response"))?;
    let Some(usage) = response.get("usage") else {
        return Ok(unavailable(
            json!({"kind":"no_quota","detail":"No usage quota reported"}),
        ));
    };
    let tier = usage
        .get("tier")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Invalid Muse usage tier"))?;
    let primary = &usage["window"];
    let weekly = &usage["weekly"];
    let duration = number(&primary["windowDurationMins"], "window.windowDurationMins")? * 60.0;
    let primary_pct = number(&primary["usedPercent"], "window.usedPercent")?;
    let weekly_pct = number(&weekly["usedPercent"], "weekly.usedPercent")?;
    let primary_reset = iso(&primary["resetsAtMs"], "window.resetsAtMs")?;
    let weekly_reset = iso(&weekly["resetsAtMs"], "weekly.resetsAtMs")?;
    let observed = iso(&usage["observedAtMs"], "observedAtMs")?;
    Ok(json!({"status":"available","planLabel":tier,"windows":[
        duration_window(Some(duration),None,("window","Current window",""),Some(primary_pct),Some(&primary_reset),true,Some(tone(Some(primary_pct)))),
        window("weekly","Weekly",Some("wk"),Some(weekly_pct),Some(&weekly_reset),true,Some(tone(Some(weekly_pct))))
    ],"details":[{"id":"observed","label":"Observed","value":observed}]}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_quota_is_unavailable_and_null_quota_is_invalid() {
        assert_eq!(project(&json!({})).unwrap()["problem"]["kind"], "no_quota");
        assert!(project(&json!({"usage":null})).is_err());
    }
    #[test]
    fn msp_quota_preserves_duration_and_observation() {
        let report = project(&json!({"usage":{"observedAtMs":0,"tier":"Pro",
            "window":{"windowDurationMins":300,"usedPercent":12,"resetsAtMs":1},
            "weekly":{"usedPercent":91,"resetsAtMs":2}}}))
        .unwrap();
        assert_eq!(report["windows"][0]["id"], "five_hour");
        assert_eq!(report["windows"][1]["tone"], "danger");
        assert_eq!(report["details"][0]["value"], "1970-01-01T00:00:00.000Z");
    }
}
