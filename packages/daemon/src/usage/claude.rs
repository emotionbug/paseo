//! Native, read-only port of plugins/claude-usage-source.
pub(super) mod stores;
use super::common::{
    UsageAccount, UsageRuntime, UsageScope, hash_account_key, tone, unavailable, window,
};
use anyhow::{Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::Path,
    sync::{Arc, LazyLock, Mutex},
    time::Duration,
};
use stores::Route;
use unicode_normalization::UnicodeNormalization;

const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";
const BETA: &str = "oauth-2025-04-20";
struct Credentials {
    token: String,
    expires: Option<f64>,
    subscription: Option<String>,
    tier: Option<String>,
}
fn credentials(value: &Value) -> Option<Credentials> {
    let oauth = value.get("claudeAiOauth")?;
    if !oauth.is_object() {
        return None;
    }
    let token = stores::optional_string(oauth, "accessToken").ok()??;
    if token.is_empty() {
        return None;
    }
    stores::optional_string(oauth, "refreshToken").ok()?;
    let expires = match oauth.get("expiresAt") {
        None => None,
        Some(v) => Some(v.as_f64().filter(|v| v.is_finite())?),
    };
    Some(Credentials {
        token: token.into(),
        expires,
        subscription: stores::optional_string(oauth, "subscriptionType")
            .ok()?
            .map(str::to_owned),
        tier: stores::optional_string(oauth, "rateLimitTier")
            .ok()?
            .map(str::to_owned),
    })
}
fn keychain_account(user: &str) -> String {
    if !user.is_empty()
        && user
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
    {
        user.into()
    } else {
        "claude-code-user".into()
    }
}
fn username(env: &BTreeMap<String, String>) -> String {
    // Passing USER explicitly preserves even the empty-string fallback behavior.
    if let Some(user) = env.get("USER") {
        return keychain_account(user);
    }
    #[cfg(unix)]
    if let Ok(Some(user)) = nix::unistd::User::from_uid(nix::unistd::getuid()) {
        return keychain_account(&user.name);
    }
    #[cfg(windows)]
    if let Some(user) = env.get("USERNAME") {
        return keychain_account(user);
    }
    keychain_account("")
}
fn keychain_route(env: &BTreeMap<String, String>) -> Route {
    let suffix = stores::nonempty(env, "CLAUDE_CONFIG_DIR")
        .map(|s| format!("-{}", &hash_account_key(&s.nfc().collect::<String>())[..8]))
        .unwrap_or_default();
    Route::Keychain {
        service: Some(format!("{KEYCHAIN_SERVICE}{suffix}")),
        account: Some(username(env)),
    }
}
async fn read_keychain(rt: &UsageRuntime, service: &str, account: &str) -> Option<Credentials> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    for args in [
        vec!["find-generic-password", "-a", account, "-w", "-s", service],
        vec!["find-generic-password", "-w", "-s", service],
    ] {
        let output = crate::process::run(
            crate::process::CommandSpec {
                program: "security".into(),
                args: args.into_iter().map(str::to_owned).collect(),
                cwd: None,
                env: rt.env.clone(),
                clear_env: true,
            },
            1024 * 1024,
            Duration::from_secs(2),
            rt.cancel.clone(),
        )
        .await;
        let Ok(output) = output else { continue };
        if output.status != 0 {
            continue;
        }
        if let Ok(value) = serde_json::from_slice::<Value>(&output.stdout) {
            if let Some(record) = credentials(&value) {
                return Some(record);
            }
        }
    }
    None
}
async fn read_credentials(rt: &UsageRuntime, route: &Route) -> Option<Credentials> {
    match route {
        Route::Claude { path } => credentials(&stores::read_json(path).await?),
        Route::Keychain { service, account } => {
            let default_account;
            let account = match account.as_deref() {
                Some(account) => account,
                None => {
                    default_account = username(&rt.env);
                    &default_account
                }
            };
            read_keychain(rt, service.as_deref().unwrap_or(KEYCHAIN_SERVICE), account).await
        }
        _ => {
            let auth = stores::read_harness(route, "anthropic").await?;
            Some(Credentials {
                token: auth.access,
                expires: auth.expires,
                subscription: None,
                tier: None,
            })
        }
    }
}
async fn claude_route(rt: &UsageRuntime, home: &Path, env: &BTreeMap<String, String>) -> Route {
    let route = keychain_route(env);
    if read_credentials(rt, &route).await.is_some() {
        return route;
    }
    // The source uses ?? rather than || here, so an explicitly empty directory is meaningful.
    let directory = env
        .get("CLAUDE_CONFIG_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| stores::join(home, ".claude"));
    Route::Claude {
        path: stores::join(&directory, ".credentials.json")
            .to_string_lossy()
            .into_owned(),
    }
}
fn enabled(value: Option<&String>) -> bool {
    value.is_some_and(|s| {
        s == "1" || s.eq_ignore_ascii_case("true") || s.eq_ignore_ascii_case("yes")
    })
}
pub(super) async fn discover(rt: &UsageRuntime, scope: &UsageScope) -> Result<Vec<UsageAccount>> {
    let (env, home) = stores::scope_env(rt, scope);
    let pi = || Route::Pi {
        path: stores::pi_path(&home, env).to_string_lossy().into_owned(),
    };
    let routes = if let Some(provider) = scope.provider.as_deref() {
        if provider == "claude" {
            let foreign = stores::nonempty(env, "ANTHROPIC_BASE_URL")
                .is_some_and(|s| s.trim_end_matches('/') != "https://api.anthropic.com");
            if enabled(env.get("CLAUDE_CODE_USE_BEDROCK"))
                || enabled(env.get("CLAUDE_CODE_USE_VERTEX"))
                || foreign
            {
                return Ok(Vec::new());
            }
            vec![claude_route(rt, &home, env).await]
        } else if scope
            .model
            .as_deref()
            .is_some_and(|s| s.starts_with("anthropic/"))
        {
            match provider {
                "pi" => vec![pi()],
                "omp" => stores::discover_omp(&home, env, "anthropic").await?,
                _ => Vec::new(),
            }
        } else {
            Vec::new()
        }
    } else {
        let mut routes = vec![claude_route(rt, &home, env).await, pi()];
        routes.extend(stores::discover_omp(&home, env, "anthropic").await?);
        routes
    };
    let mut accounts = Vec::new();
    for route in routes {
        if rt.cancel.is_cancelled() {
            bail!("Usage discovery cancelled");
        }
        let Some(credentials) = read_credentials(rt, &route).await else {
            continue;
        };
        let mut account = UsageAccount {
            key: route.fallback_key(),
            label: None,
            harness: Some(route.harness().into()),
            input: route.input(),
        };
        if !credentials
            .expires
            .is_some_and(|expires| expires <= now() as f64)
        {
            if let Ok(Identity::Account { key, label }) = read_profile(rt, &credentials.token).await
            {
                account.key = key;
                account.label = label;
            }
        }
        if rt.cancel.is_cancelled() {
            bail!("Usage discovery cancelled");
        }
        accounts.push(account);
    }
    Ok(accounts)
}

#[derive(Clone)]
enum Identity {
    Account { key: String, label: Option<String> },
    Rejected(u16),
}
type ProfileResult = Arc<tokio::sync::Mutex<Option<std::result::Result<Identity, String>>>>;
struct ProfileEntry {
    hash: String,
    at: i64,
    result: ProfileResult,
}
fn profile_cache() -> &'static Mutex<VecDeque<ProfileEntry>> {
    static CACHE: LazyLock<Mutex<VecDeque<ProfileEntry>>> =
        LazyLock::new(|| Mutex::new(VecDeque::new()));
    &CACHE
}
fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
fn profile_entry(hash: &str, create: bool) -> Option<ProfileResult> {
    let mut cache = profile_cache().lock().unwrap_or_else(|e| e.into_inner());
    let time = now();
    cache.retain(|entry| time - entry.at < 300_000);
    if let Some(entry) = cache.iter().find(|entry| entry.hash == hash) {
        return Some(entry.result.clone());
    }
    if !create {
        return None;
    }
    while cache.len() >= 128 {
        cache.pop_front();
    }
    let result = Arc::new(tokio::sync::Mutex::new(None));
    cache.push_back(ProfileEntry {
        hash: hash.into(),
        at: time,
        result: result.clone(),
    });
    Some(result)
}
fn api_request(rt: &UsageRuntime, path: &str, token: &str) -> reqwest::RequestBuilder {
    rt.request(
        reqwest::Method::GET,
        &format!("https://api.anthropic.com/api/oauth/{path}"),
    )
    .timeout(Duration::from_secs(15))
    .bearer_auth(token)
    .header("Accept", "application/json")
    .header("anthropic-beta", BETA)
}
async fn read_profile(rt: &UsageRuntime, token: &str) -> Result<Identity> {
    let hash = hash_account_key(token);
    let entry = profile_entry(&hash, true).expect("new profile entry");
    let mut result = tokio::select! {
        biased;
        _ = rt.cancel.cancelled() => bail!("Usage discovery cancelled"),
        result = entry.lock() => result,
    };
    if let Some(identity) = result.as_ref() {
        return identity.clone().map_err(anyhow::Error::msg);
    }
    let request: Result<Identity> = async {
        let response = rt.send(api_request(rt, "profile", token)).await?;
        let status = response.status();
        if matches!(status.as_u16(), 401 | 403) {
            return Ok(Identity::Rejected(status.as_u16()));
        }
        ensure!(
            status.is_success(),
            "Claude profile API returned {}",
            status.as_u16()
        );
        let value: Value = serde_json::from_str(&stores::response_text(rt, response).await?)?;
        let account = value
            .get("account")
            .filter(|v| v.is_object())
            .ok_or_else(|| anyhow::anyhow!("Invalid Claude profile account"))?;
        let organization = value
            .get("organization")
            .filter(|v| v.is_object())
            .ok_or_else(|| anyhow::anyhow!("Invalid Claude profile organization"))?;
        let account_id = account
            .get("uuid")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Missing Claude account UUID"))?;
        let organization_id = organization
            .get("uuid")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Missing Claude organization UUID"))?;
        let label = stores::optional_string(account, "email")?
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        Ok(Identity::Account {
            key: format!("{account_id}.{organization_id}"),
            label,
        })
    }
    .await;
    match request {
        Ok(identity) => {
            *result = Some(Ok(identity.clone()));
            Ok(identity)
        }
        Err(error) => {
            *result = Some(Err(error.to_string()));
            profile_cache()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .retain(|cached| !Arc::ptr_eq(&cached.result, &entry));
            Err(error)
        }
    }
}
fn rate_limits() -> &'static Mutex<BTreeMap<String, f64>> {
    static LIMITS: LazyLock<Mutex<BTreeMap<String, f64>>> =
        LazyLock::new(|| Mutex::new(BTreeMap::new()));
    &LIMITS
}
fn retry_after(header: Option<&str>, now: f64) -> Option<f64> {
    let header = header.filter(|s| !s.is_empty())?;
    if let Ok(seconds) = stores::number(&json!(header)) {
        return Some((seconds * 1000.0).max(0.0));
    }
    chrono::DateTime::parse_from_rfc2822(header)
        .or_else(|_| chrono::DateTime::parse_from_rfc3339(header))
        .ok()
        .map(|at| (at.timestamp_millis() as f64 - now).max(0.0))
}
fn limited_error(wait: Option<f64>) -> anyhow::Error {
    let when = wait
        .map(|ms| {
            let minutes = (ms / 60_000.0).ceil().max(1.0);
            if minutes < 60.0 {
                format!("in {minutes:.0}m")
            } else {
                format!("in {:.0}h", (minutes / 60.0).ceil())
            }
        })
        .unwrap_or_else(|| "later".into());
    anyhow::anyhow!("Rate limited by Claude. Try again {when}.")
}
pub(super) async fn fetch(rt: &UsageRuntime, input: &Value) -> Result<Value> {
    let route = Route::parse(input, true)?;
    let credentials = read_credentials(rt, &route)
        .await
        .ok_or_else(|| anyhow::anyhow!("Claude login store no longer exists"))?;
    let refreshed_by = if matches!(&route, Route::Claude { .. } | Route::Keychain { .. }) {
        "claude"
    } else {
        route.store()
    };
    if let Some(expires) = credentials
        .expires
        .filter(|expires| *expires <= now() as f64)
    {
        return Ok(unavailable(
            json!({"kind":"expired", "expiresAt":stores::iso_ms(expires)?, "refreshedBy":refreshed_by}),
        ));
    }
    let hash = hash_account_key(&credentials.token);
    if let Some(profile) = profile_entry(&hash, false) {
        let identity = tokio::select! {
            biased;
            _ = rt.cancel.cancelled() => bail!("Usage request cancelled"),
            identity = profile.lock() => identity,
        };
        match identity.as_ref() {
            Some(Ok(Identity::Rejected(status))) => {
                return Ok(unavailable(
                    json!({"kind":"rejected", "status":status, "refreshedBy":refreshed_by}),
                ));
            }
            Some(Err(error)) => bail!("{error}"),
            _ => {}
        }
    }
    let time = now() as f64;
    {
        let mut limits = rate_limits().lock().unwrap_or_else(|e| e.into_inner());
        limits.retain(|_, until| *until > time);
        if let Some(until) = limits.get(&hash) {
            return Err(limited_error(Some(*until - time)));
        }
    }
    let response = rt
        .send(api_request(rt, "usage", &credentials.token))
        .await?;
    let status = response.status();
    if matches!(status.as_u16(), 401 | 403) {
        return Ok(unavailable(
            json!({"kind":"rejected", "status":status.as_u16(), "refreshedBy":refreshed_by}),
        ));
    }
    if status.as_u16() == 429 {
        let wait = retry_after(
            response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok()),
            time,
        );
        if let Some(wait) = wait {
            rate_limits()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(hash, time + wait);
        }
        return Err(limited_error(wait));
    }
    ensure!(
        status.is_success(),
        "Claude usage API returned {}",
        status.as_u16()
    );
    let value = serde_json::from_str(&stores::response_text(rt, response).await?)?;
    project(
        &value,
        credentials.subscription.as_deref(),
        credentials.tier.as_deref(),
    )
}

struct ScopedLimit {
    dimension: &'static str,
    id: Option<String>,
    name: String,
    used: Option<f64>,
    reset: Option<String>,
}
fn normalize(name: &str) -> String {
    let mut normalized = String::new();
    let mut separator = false;
    for c in name.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            if separator && !normalized.is_empty() {
                normalized.push('_');
            }
            normalized.push(c);
            separator = false;
        } else {
            separator = true;
        }
    }
    normalized
}
fn same_limit(a: &ScopedLimit, b: &ScopedLimit) -> bool {
    a.dimension == b.dimension
        && match (&a.id, &b.id) {
            (Some(a), Some(b)) => a == b,
            _ => normalize(&a.name) == normalize(&b.name),
        }
}
fn parse_scoped(entry: &Value) -> Result<Option<ScopedLimit>> {
    ensure!(entry.is_object(), "Invalid Claude scoped limit");
    let kind = entry
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("Missing Claude limit kind"))?;
    let used = stores::optional_number(entry, "percent", true)?;
    let reset = stores::nullable_string(entry, "resets_at")?.map(str::to_owned);
    let scope = stores::nullable_object(entry, "scope")?;
    let mut chosen = None;
    if let Some(scope) = scope {
        for dimension in ["model", "surface"] {
            if let Some(value) = stores::nullable_object(scope, dimension)? {
                let id = stores::nullable_string(value, "id")?
                    .map(str::trim)
                    .filter(|s| !s.is_empty());
                let name = stores::nullable_string(value, "display_name")?
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .or(id);
                if chosen.is_none() {
                    if let Some(name) = name {
                        chosen = Some(ScopedLimit {
                            dimension,
                            id: id.map(str::to_owned),
                            name: name.into(),
                            used,
                            reset: reset.clone(),
                        });
                    }
                }
            }
        }
    }
    Ok(if kind == "weekly_scoped" {
        chosen
    } else {
        None
    })
}
fn project(value: &Value, subscription: Option<&str>, tier: Option<&str>) -> Result<Value> {
    ensure!(value.is_object(), "Invalid Claude usage response");
    let mut windows = Vec::new();
    let mut scoped = Vec::new();
    for (field, id, label, short) in [
        ("five_hour", "five_hour", "Session", "5h"),
        ("seven_day", "weekly", "Weekly", "wk"),
        ("seven_day_opus", "", "Opus", ""),
        ("seven_day_omelette", "", "Omelette", ""),
    ] {
        let Some(data) = stores::nullable_object(value, field)? else {
            continue;
        };
        let used = Some(stores::number(
            data.get("utilization")
                .ok_or_else(|| anyhow::anyhow!("Missing Claude utilization"))?,
        )?);
        let reset = stores::nullable_string(data, "resets_at")?;
        if id.is_empty() {
            scoped.push(ScopedLimit {
                dimension: "model",
                id: None,
                name: label.into(),
                used,
                reset: reset.map(str::to_owned),
            });
        } else {
            windows.push(window(
                id,
                label,
                Some(short),
                used,
                reset,
                true,
                Some(tone(used)),
            ));
        }
    }
    if let Some(limits) = value.get("limits").filter(|v| !v.is_null()) {
        let limits = limits
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("Invalid Claude limits array"))?;
        for entry in limits {
            match parse_scoped(entry) {
                Ok(Some(mut limit)) => {
                    if let Some(index) = scoped
                        .iter()
                        .position(|candidate| same_limit(candidate, &limit))
                    {
                        limit.used = limit.used.or(scoped[index].used);
                        limit.reset = limit.reset.or_else(|| scoped[index].reset.take());
                        scoped[index] = limit;
                    } else {
                        scoped.push(limit);
                    }
                }
                Ok(None) => {
                    if entry.get("kind").and_then(Value::as_str) == Some("weekly_scoped") {
                        tracing::warn!(
                            "Skipping scoped Claude usage limit with no resolvable scope name"
                        );
                    }
                }
                Err(_) => tracing::warn!("Skipping unparseable Claude usage limit entry"),
            }
        }
    }
    let mut taken = BTreeSet::new();
    for limit in scoped {
        let candidate = format!(
            "weekly_{}_{}",
            limit.dimension,
            limit.id.unwrap_or_else(|| normalize(&limit.name))
        );
        let mut id = candidate.clone();
        let mut suffix = 2;
        while taken.contains(&id) {
            id = format!("{candidate}_{suffix}");
            suffix += 1;
        }
        taken.insert(id.clone());
        windows.push(window(
            &id,
            &format!("Weekly · {}", limit.name),
            Some(&format!("wk {}", limit.name)),
            limit.used,
            limit.reset.as_deref(),
            false,
            Some(tone(limit.used)),
        ));
    }
    let mut details = Vec::new();
    if let Some(extra) = stores::nullable_object(value, "extra_usage")? {
        if let Some(enabled) = extra.get("is_enabled") {
            let enabled = enabled
                .as_bool()
                .ok_or_else(|| anyhow::anyhow!("Invalid Claude extra usage flag"))?;
            details.push(json!({"id":"extra_usage", "label":"Extra usage", "value":if enabled {"Enabled"} else {"Disabled"}}));
        }
    }
    if windows.is_empty() {
        tracing::warn!("Claude usage response parsed but produced no windows");
    }
    let mut report =
        json!({"status":"available", "windows":windows, "balances":[], "details":details});
    if let Some(subscription) = subscription.filter(|s| !s.is_empty()) {
        let mut chars = subscription.chars();
        let mut label = chars
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_default();
        label.push_str(chars.as_str());
        if let Some(tier) = tier
            .and_then(|s| s.rsplit('_').next())
            .filter(|s| !s.is_empty())
        {
            label.push(' ');
            label.push_str(tier);
        }
        report["planLabel"] = json!(label);
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scoped_limits_reconcile_per_field_and_dimension() {
        let report = project(
            &json!({"seven_day_omelette":{"utilization":12,"resets_at":"reset"}, "limits":[
            {"kind":"weekly_scoped","percent":null,"scope":{"model":{"display_name":"Omelette"}}},
            {"kind":"weekly_scoped","percent":0,"scope":{"surface":{"display_name":"Omelette"}}},
            {"percent":"invalid"}]}),
            Some("pro"),
            Some("default_1x"),
        )
        .unwrap();
        assert_eq!(report["planLabel"], "Pro 1x");
        assert_eq!(report["windows"].as_array().unwrap().len(), 2);
        assert_eq!(report["windows"][0]["id"], "weekly_model_omelette");
        assert_eq!(report["windows"][0]["usedPct"], 12.0);
        assert_eq!(report["windows"][0]["resetsAt"], "reset");
        assert_eq!(report["windows"][1]["id"], "weekly_surface_omelette");
        assert_eq!(report["windows"][1]["usedPct"], 0.0);
    }
    #[test]
    fn explicit_ids_and_colliding_name_fallbacks_remain_distinct() {
        let report = project(
            &json!({"limits":[
                {"kind":"weekly_scoped","scope":{"model":{"id":"fable-pro","display_name":"One"}}},
                {"kind":"weekly_scoped","scope":{"model":{"id":"fable_pro","display_name":"Two"}}},
                {"kind":"weekly_scoped","scope":{"model":{"display_name":"Fable Pro"}}}
            ]}),
            None,
            None,
        )
        .unwrap();
        let ids: Vec<_> = report["windows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["id"].as_str().unwrap())
            .collect();
        assert_eq!(
            ids,
            [
                "weekly_model_fable-pro",
                "weekly_model_fable_pro",
                "weekly_model_fable_pro_2"
            ]
        );
    }
    #[test]
    fn keychain_service_normalizes_unicode_and_sanitizes_account() {
        let a = keychain_route(&BTreeMap::from([
            ("CLAUDE_CONFIG_DIR".into(), "/cafe\u{301}".into()),
            ("USER".into(), "a@b".into()),
        ]));
        let b = keychain_route(&BTreeMap::from([
            ("CLAUDE_CONFIG_DIR".into(), "/café".into()),
            ("USER".into(), "a@b".into()),
        ]));
        assert_eq!(a.input(), b.input());
        assert_eq!(a.input()["route"]["account"], "claude-code-user");
    }
    #[test]
    fn retry_after_preserves_vendor_backoff() {
        assert_eq!(retry_after(Some("2240"), 0.0), Some(2_240_000.0));
        assert_eq!(
            limited_error(Some(2_240_000.0)).to_string(),
            "Rate limited by Claude. Try again in 38m."
        );
        assert_eq!(
            retry_after(Some("Thu, 01 Jan 1970 00:01:00 GMT"), 30_000.0),
            Some(30_000.0)
        );
        assert_eq!(retry_after(Some("nonsense"), 0.0), None);
    }
}
