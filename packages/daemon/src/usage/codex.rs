//! Native, read-only port of plugins/codex-usage-source.
use super::{
    claude::stores::{self, Route},
    common::{UsageAccount, UsageRuntime, UsageScope, duration_window, tone, unavailable},
};
use anyhow::{Result, bail, ensure};
use base64::{
    Engine,
    engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD},
};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};

struct Auth {
    token: String,
    account_id: Option<String>,
    id_token: Option<String>,
    expires: Option<f64>,
}

pub(super) async fn discover(rt: &UsageRuntime, scope: &UsageScope) -> Result<Vec<UsageAccount>> {
    let (env, home) = stores::scope_env(rt, scope);
    let default_codex = stores::join(&home, ".codex/auth.json");
    let selected_codex = stores::nonempty(env, "CODEX_HOME")
        .map(|p| stores::join(Path::new(p), "auth.json"))
        .unwrap_or_else(|| default_codex.clone());
    let codex_route = |path: std::path::PathBuf| Route::Codex {
        path: path.to_string_lossy().into_owned(),
    };
    let pi = || Route::Pi {
        path: stores::pi_path(&home, env).to_string_lossy().into_owned(),
    };
    let opencode = || Route::Opencode {
        path: stores::join(
            &stores::nonempty(env, "XDG_DATA_HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| stores::join(&home, ".local/share")),
            "opencode/auth.json",
        )
        .to_string_lossy()
        .into_owned(),
    };
    let routes = if let Some(provider) = scope.provider.as_deref() {
        if provider == "codex" {
            if stores::nonempty(env, "OPENAI_BASE_URL").is_some() {
                return Ok(Vec::new());
            }
            vec![codex_route(selected_codex)]
        } else if scope
            .model
            .as_deref()
            .is_some_and(|s| s.starts_with("openai/"))
        {
            match provider {
                "pi" => vec![pi()],
                "omp" => stores::discover_omp(&home, env, "openai-codex").await?,
                "opencode" => vec![opencode()],
                _ => Vec::new(),
            }
        } else {
            Vec::new()
        }
    } else {
        let mut routes = vec![codex_route(selected_codex.clone())];
        if selected_codex != default_codex {
            routes.push(codex_route(default_codex));
        }
        routes.push(opencode());
        routes.push(pi());
        routes.extend(stores::discover_omp(&home, env, "openai-codex").await?);
        routes
    };
    let mut accounts = Vec::new();
    for route in routes {
        if rt.cancel.is_cancelled() {
            bail!("Usage discovery cancelled");
        }
        if let Some(auth) = read_auth(&route).await {
            let (key, label) = account_identity(&auth, &route);
            accounts.push(UsageAccount {
                key,
                label,
                harness: Some(route.harness().into()),
                input: route.input(),
            });
        }
    }
    Ok(accounts)
}

async fn read_auth(route: &Route) -> Option<Auth> {
    if let Route::Codex { path } = route {
        let value = stores::read_json(path).await?;
        let tokens = value.get("tokens")?;
        if !tokens.is_object() {
            return None;
        }
        let token = stores::optional_string(tokens, "access_token").ok()??;
        if token.is_empty() {
            return None;
        }
        Some(Auth {
            token: token.into(),
            account_id: stores::optional_string(tokens, "account_id")
                .ok()?
                .map(str::to_owned),
            id_token: stores::optional_string(tokens, "id_token")
                .ok()?
                .map(str::to_owned),
            expires: None,
        })
    } else {
        let auth = stores::read_harness(route, "openai-codex").await?;
        Some(Auth {
            token: auth.access,
            account_id: auth.account_id,
            id_token: None,
            expires: auth.expires,
        })
    }
}
fn claims(token: Option<&str>) -> Value {
    token
        .and_then(|s| s.split('.').nth(1))
        .and_then(|s| {
            URL_SAFE_NO_PAD
                .decode(s)
                .or_else(|_| URL_SAFE.decode(s))
                .ok()
        })
        .and_then(|s| serde_json::from_slice(&s).ok())
        .unwrap_or(Value::Null)
}
fn claim(value: &Value) -> Option<&str> {
    value.as_str().filter(|s| !s.is_empty())
}
fn account_identity(auth: &Auth, route: &Route) -> (String, Option<String>) {
    let access = claims(Some(&auth.token));
    let id = claims(auth.id_token.as_deref());
    let key = auth
        .account_id
        .as_deref()
        .or_else(|| claim(&access["https://api.openai.com/auth"]["chatgpt_account_id"]))
        .or_else(|| claim(&access["chatgpt_account_id"]))
        .or_else(|| claim(&id["https://api.openai.com/auth"]["chatgpt_account_id"]));
    let label = claim(&access["https://api.openai.com/profile"]["email"])
        .or_else(|| claim(&access["email"]))
        .or_else(|| claim(&id["https://api.openai.com/profile"]["email"]))
        .or_else(|| claim(&id["email"]));
    (
        key.map(str::to_owned)
            .unwrap_or_else(|| route.fallback_key()),
        label.map(str::to_owned),
    )
}

pub(super) async fn fetch(rt: &UsageRuntime, input: &Value) -> Result<Value> {
    let route = Route::parse(input, false)?;
    let auth = read_auth(&route)
        .await
        .ok_or_else(|| anyhow::anyhow!("Codex login store no longer exists"))?;
    if let Some(expires) = auth
        .expires
        .filter(|expires| *expires <= chrono::Utc::now().timestamp_millis() as f64)
    {
        return Ok(unavailable(
            json!({"kind":"expired", "expiresAt": stores::iso_ms(expires)?, "refreshedBy":route.store()}),
        ));
    }
    let mut request = rt
        .request(
            reqwest::Method::GET,
            "https://chatgpt.com/backend-api/wham/usage",
        )
        .timeout(Duration::from_secs(15))
        .bearer_auth(&auth.token)
        .header("Accept", "application/json")
        .header(
            "User-Agent",
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)",
        );
    if let Some(id) = auth.account_id.as_deref().filter(|s| !s.is_empty()) {
        request = request.header("ChatGPT-Account-Id", id);
    }
    let response = rt.send(request).await?;
    let status = response.status();
    if matches!(status.as_u16(), 401 | 403) {
        return Ok(unavailable(
            json!({"kind":"rejected", "status":status.as_u16(), "refreshedBy":route.store()}),
        ));
    }
    ensure!(
        status.is_success(),
        "Codex usage API returned {}",
        status.as_u16()
    );
    let text = stores::response_text(rt, response).await?;
    ensure!(
        !text.trim().starts_with('<'),
        "Codex usage API returned HTML"
    );
    project(&serde_json::from_str(&text)?)
}
fn windows(
    value: Option<&Value>,
    scope: Option<(&str, &str)>,
    summary: bool,
    out: &mut Vec<Value>,
) -> Result<()> {
    let Some(value) = value else {
        return Ok(());
    };
    for (slot, unknown) in [
        ("primary_window", ("unknown_primary", "Primary limit", "")),
        (
            "secondary_window",
            ("unknown_secondary", "Secondary limit", ""),
        ),
    ] {
        let Some(window) = stores::nullable_object(value, slot)? else {
            continue;
        };
        let used = stores::optional_number(window, "used_percent", false)?;
        let reset = stores::optional_number(window, "reset_at", false)?
            .map(|s| stores::iso_ms(s * 1000.0))
            .transpose()?;
        let seconds = stores::optional_number(window, "limit_window_seconds", true)?;
        out.push(duration_window(
            seconds,
            scope,
            unknown,
            used,
            reset.as_deref(),
            summary,
            Some(tone(used)),
        ));
    }
    Ok(())
}
// JavaScript encodeURIComponent preserves these punctuation characters as well as alphanumerics.
fn encode_identity(s: &str) -> String {
    const ENCODE: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'_')
        .remove(b'.')
        .remove(b'!')
        .remove(b'~')
        .remove(b'*')
        .remove(b'\'')
        .remove(b'(')
        .remove(b')');
    percent_encoding::utf8_percent_encode(s, ENCODE).to_string()
}
fn project(usage: &Value) -> Result<Value> {
    ensure!(usage.is_object(), "Invalid Codex usage response");
    let plan = stores::optional_string(usage, "plan_type")?;
    stores::optional_string(usage, "email")?;
    let mut output = Vec::new();
    windows(
        stores::nullable_object(usage, "rate_limit")?,
        None,
        true,
        &mut output,
    )?;
    if let Some(limits) = usage.get("additional_rate_limits").filter(|v| !v.is_null()) {
        for limit in limits
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("Invalid additional rate limits"))?
        {
            ensure!(limit.is_object(), "Invalid additional rate limit");
            let name = stores::optional_string(limit, "limit_name")?;
            let feature = stores::optional_string(limit, "metered_feature")?;
            let rate = stores::nullable_object(limit, "rate_limit")?;
            let identity = feature
                .filter(|s| !s.is_empty())
                .or_else(|| name.filter(|s| !s.is_empty()));
            if let Some(identity) = identity {
                let id = format!("limit:{}", encode_identity(identity));
                windows(
                    rate,
                    Some((&id, name.filter(|s| !s.is_empty()).unwrap_or(identity))),
                    false,
                    &mut output,
                )?;
            } else {
                // Schema validation still applies to unnamed limits even though they cannot be rendered.
                windows(rate, None, false, &mut Vec::new())?;
            }
        }
    }
    windows(
        stores::nullable_object(usage, "code_review_rate_limit")?,
        Some(("code_review", "Code review")),
        false,
        &mut output,
    )?;
    let balance = stores::nullable_object(usage, "credits")?
        .map(|v| stores::optional_number(v, "balance", false))
        .transpose()?
        .flatten();
    let balances: Vec<Value> = balance.into_iter().map(|b| json!({"id":"credits", "label":"Credits", "remaining":b, "unit":"credits", "tone":if b <= 0.0 {"danger"} else {"ok"}})).collect();
    let mut report =
        json!({"status":"available", "windows":output, "balances":balances, "details":[]});
    if let Some(plan) = plan {
        report["planLabel"] = json!(plan);
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn projection_uses_duration_not_slot_and_keeps_named_pools() {
        let report = project(&json!({"rate_limit":{"primary_window":{"used_percent":"96","limit_window_seconds":604800}},
            "additional_rate_limits":[{"metered_feature":"a/b", "limit_name":"Spark", "rate_limit":{"secondary_window":{"used_percent":0,"limit_window_seconds":18000}}}],
            "code_review_rate_limit":{"primary_window":{"used_percent":3}}, "credits":{"balance":"0"}})).unwrap();
        assert_eq!(report["windows"][0]["id"], "weekly");
        assert_eq!(report["windows"][0]["tone"], "danger");
        assert_eq!(report["windows"][1]["id"], "limit:a%2Fb:five_hour");
        assert_eq!(report["windows"][2]["id"], "code_review:unknown_primary");
        assert_eq!(report["balances"][0]["tone"], "danger");
    }
    #[test]
    fn identity_prefers_explicit_account_and_id_token_email() {
        let payload = URL_SAFE_NO_PAD.encode(br#"{"email":"owner@example.test","https://api.openai.com/auth":{"chatgpt_account_id":"claim-account"}}"#);
        let auth = Auth {
            token: "opaque".into(),
            account_id: Some("selected-account".into()),
            id_token: Some(format!("h.{payload}.s")),
            expires: None,
        };
        let route = Route::Codex {
            path: "/fixture/auth.json".into(),
        };
        assert_eq!(
            account_identity(&auth, &route),
            ("selected-account".into(), Some("owner@example.test".into()))
        );
        let opaque = Auth {
            account_id: None,
            id_token: None,
            ..auth
        };
        assert_eq!(account_identity(&opaque, &route).0, route.fallback_key());
    }
}
