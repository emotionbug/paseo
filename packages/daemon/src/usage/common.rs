use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, serde::Deserialize)]
pub(super) struct UsageAccount {
    pub key: String,
    pub label: Option<String>,
    pub harness: Option<String>,
    pub input: Value,
}

#[derive(Clone)]
pub(super) struct UsageScope {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub env: BTreeMap<String, String>,
}

#[derive(Clone)]
pub(super) struct UsageRuntime {
    pub home: PathBuf,
    pub env: BTreeMap<String, String>,
    pub http: reqwest::Client,
    pub cancel: CancellationToken,
    /// Explicit endpoint substitution permits isolated HTTP fixtures without live accounts.
    pub endpoints: BTreeMap<String, String>,
    pub muse_providers: Vec<std::sync::Arc<crate::providers::muse::MuseProvider>>,
    pub plugins: Option<std::sync::Arc<crate::plugins::PluginService>>,
}

impl UsageRuntime {
    pub fn request(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        let replacement = self
            .endpoints
            .iter()
            .filter(|(prefix, _)| url.starts_with(prefix.as_str()))
            .max_by_key(|(prefix, _)| prefix.len())
            .map(|(prefix, target)| format!("{}{}", target, &url[prefix.len()..]));
        self.http
            .request(method, replacement.as_deref().unwrap_or(url))
            .timeout(Duration::from_secs(15))
    }

    pub async fn send(&self, request: reqwest::RequestBuilder) -> Result<reqwest::Response> {
        tokio::select! {
            _ = self.cancel.cancelled() => Err(anyhow!("Usage fetch cancelled")),
            response = request.send() => response.map_err(|error| anyhow!(error.without_url())),
        }
    }
}

pub(super) fn hash_account_key(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

pub(super) async fn read_json(path: &Path) -> Result<Option<Value>> {
    match tokio::fs::read(path).await {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn tone(pct: Option<f64>) -> &'static str {
    match pct {
        Some(pct) if pct > 90.0 => "danger",
        Some(pct) if pct >= 70.0 => "warning",
        Some(_) => "ok",
        None => "default",
    }
}

pub(super) fn used_pct(used: Option<f64>, limit: Option<f64>) -> Option<f64> {
    used.zip(limit)
        .and_then(|(used, limit)| (limit > 0.0).then_some(used / limit * 100.0))
}

pub(super) fn unavailable(problem: Value) -> Value {
    json!({"status":"unavailable", "problem":problem})
}

pub(super) fn window(
    id: &str,
    label: &str,
    short: Option<&str>,
    pct: Option<f64>,
    reset: Option<&str>,
    summary: bool,
    tone: Option<&str>,
) -> Value {
    let mut value = json!({"id":id,"label":label,"usedPct":pct,
        "remainingPct":pct.map(|pct| (100.0-pct).max(0.0)),"resetsAt":reset});
    if let Some(short) = short {
        value["shortLabel"] = json!(short);
    }
    if summary {
        value["summary"] = json!(true);
    }
    if let Some(tone) = tone {
        value["tone"] = json!(tone);
    }
    value
}

pub(super) fn duration_window(
    seconds: Option<f64>,
    scope: Option<(&str, &str)>,
    unknown: (&str, &str, &str),
    pct: Option<f64>,
    reset: Option<&str>,
    summary: bool,
    tone: Option<&str>,
) -> Value {
    let (id, label, short) = match seconds.filter(|seconds| seconds.is_finite() && *seconds > 0.0) {
        Some(604800.0) => ("weekly".into(), "Weekly".into(), "wk".into()),
        Some(seconds) => {
            let id = if seconds == 18000.0 {
                "five_hour".into()
            } else {
                format!("{seconds}s")
            };
            let (size, unit, abbreviation) = [
                (86400.0, "day", "d"),
                (3600.0, "hour", "h"),
                (60.0, "minute", "m"),
                (1.0, "second", "s"),
            ]
            .into_iter()
            .find(|(size, _, _)| seconds % size == 0.0)
            .unwrap_or((1.0, "second", "s"));
            let amount = seconds / size;
            (
                id,
                format!("{amount}-{unit}"),
                format!("{amount}{abbreviation}"),
            )
        }
        None => (unknown.0.into(), unknown.1.into(), unknown.2.into()),
    };
    let (id, label, short) = if let Some((scope_id, scope_label)) = scope {
        (
            format!("{scope_id}:{id}"),
            format!("{scope_label} · {label}"),
            if short.is_empty() {
                scope_label.to_owned()
            } else {
                format!("{scope_label} {short}")
            },
        )
    } else {
        (id, label, short)
    };
    window(&id, &label, Some(&short), pct, reset, summary, tone)
}

/// Coercion used by the original z.coerce.number().finite() API schemas.
pub(super) fn api_number(value: &Value) -> Option<f64> {
    let number = match value {
        Value::Null => 0.0,
        Value::Bool(value) => {
            if *value {
                1.0
            } else {
                0.0
            }
        }
        Value::Number(value) => value.as_f64()?,
        Value::String(value) => {
            let value = value.trim();
            if value.is_empty() {
                0.0
            } else if let Some(hex) = value
                .strip_prefix("0x")
                .or_else(|| value.strip_prefix("0X"))
            {
                u64::from_str_radix(hex, 16).ok()? as f64
            } else if let Some(binary) = value
                .strip_prefix("0b")
                .or_else(|| value.strip_prefix("0B"))
            {
                u64::from_str_radix(binary, 2).ok()? as f64
            } else if let Some(octal) = value
                .strip_prefix("0o")
                .or_else(|| value.strip_prefix("0O"))
            {
                u64::from_str_radix(octal, 8).ok()? as f64
            } else {
                value.parse().ok()?
            }
        }
        Value::Array(_) => return api_number(&Value::String(api_string(value))),
        Value::Object(_) => return None,
    };
    number.is_finite().then_some(number)
}

pub(super) fn api_string(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => {
            let number = value.as_f64().unwrap_or_default();
            if number == 0.0 {
                return "0".into();
            }
            if (1e-6..1e21).contains(&number.abs()) {
                return number.to_string();
            }
            let scientific = format!("{number:e}");
            if let Some((mantissa, exponent)) = scientific.split_once('e') {
                let exponent: i32 = exponent.parse().unwrap_or(0);
                format!(
                    "{mantissa}e{}{exponent}",
                    if exponent >= 0 { "+" } else { "" }
                )
            } else {
                scientific
            }
        }
        Value::String(value) => value.clone(),
        Value::Array(values) => values
            .iter()
            .map(|value| {
                if value.is_null() {
                    String::new()
                } else {
                    api_string(value)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}
