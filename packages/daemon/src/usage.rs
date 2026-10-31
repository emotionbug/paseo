//! Native built-in counterparts of plugins/*-usage-source and UsageSourceRegistry.
mod claude;
mod codex;
mod common;
mod copilot;
mod cursor;
mod grok;
mod kimi;
mod minimax;
mod muse;
mod opencode_go;
mod report;
mod zai;
use report::validate as validate_report;
#[cfg(test)]
mod tests;

use crate::providers::api::ProviderSpec;
use anyhow::{Result, anyhow, bail};
use chrono::{SecondsFormat, Utc};
use common::{UsageAccount, UsageRuntime, UsageScope, hash_account_key};
use futures_util::{
    StreamExt,
    stream::{FuturesOrdered, FuturesUnordered},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

/// `sessions` contains `{agentId, provider, model?, env, sessionKey}`; an existing
/// agent without a usage session is represented by `{agentId}`.
pub struct UsageContext {
    pub env: BTreeMap<String, String>,
    pub user_home: Option<PathBuf>,
    pub provider_specs: Vec<ProviderSpec>,
    pub sessions: Vec<Value>,
    pub muse_providers: Vec<Arc<crate::providers::muse::MuseProvider>>,
    pub plugins: Option<Arc<crate::plugins::PluginService>>,
}

macro_rules! sources {
    ($( $variant:ident => ($module:ident, $id:literal, $label:literal, $icon:literal) ),+ $(,)?) => {
        #[derive(Clone, PartialEq, Eq, Hash)]
        enum Source {
            $( $variant ),+,
            Plugin { id:String, label:String, icon:Option<String>, plugin_id:String, generation:String },
        }
        impl Source {
            const ALL: &'static [Self] = &[$( Self::$variant ),+];
            fn id(&self) -> &str { match self { $(Self::$variant => $id),+, Self::Plugin {id,..} => id } }
            fn label(&self) -> &str { match self { $(Self::$variant => $label),+, Self::Plugin {label,..} => label } }
            fn icon(&self) -> Option<&str> { match self { $(Self::$variant => Some(include_str!($icon))),+, Self::Plugin {icon,..} => icon.as_deref() } }
            fn generation(&self) -> Option<&str> { match self { Self::Plugin {generation,..} => Some(generation), _ => None } }
            fn current(&self, rt:&UsageRuntime) -> bool {
                match self {
                    Self::Plugin {id,plugin_id,generation,..} => rt.plugins.as_ref().is_some_and(|plugins|
                        plugins.registered_usage_sources().iter().any(|source|
                            source["id"] == *id && source["pluginId"] == *plugin_id && source["generation"] == *generation)),
                    _ => true,
                }
            }
            async fn discover(&self, rt: &UsageRuntime, scope: &UsageScope) -> Result<Vec<UsageAccount>> {
                match self {
                    $(Self::$variant => $module::discover(rt,scope).await),+,
                    Self::Plugin {id,..} => {
                        if !self.current(rt) { bail!("Usage source is no longer registered"); }
                        // Original AgentUsageSession.provider is the harness family ("codex"),
                        // not a custom provider ID that extends it.
                        let mut scope_value = json!({"kind":"global"});
                        if let Some(provider) = &scope.provider {
                            scope_value = json!({"kind":"session","provider":provider,"env":scope.env});
                            if let Some(model) = &scope.model { scope_value["model"] = json!(model); }
                        }
                        let result = rt.plugins.as_ref().ok_or_else(||anyhow!("Plugin runtime is unavailable"))?
                            .discover_usage(id,scope_value,rt.cancel.clone()).await?;
                        for account in result.as_array().ok_or_else(||anyhow!("Invalid usage accounts"))? {
                            for field in ["label","harness"] {
                                if account.get(field).is_some_and(|value|!value.is_string()) {
                                    bail!("Invalid usage account field: {field}");
                                }
                            }
                        }
                        Ok(serde_json::from_value(result)?)
                    }
                }
            }
            async fn fetch(&self, rt: &UsageRuntime, input: &Value) -> Result<Value> {
                match self {
                    $(Self::$variant => $module::fetch(rt,input).await),+,
                    Self::Plugin {id,..} => {
                        if !self.current(rt) { bail!("Usage source is no longer registered"); }
                        rt.plugins.as_ref().ok_or_else(||anyhow!("Plugin runtime is unavailable"))?
                            .fetch_usage(id,input.clone(),rt.cancel.clone()).await
                    }
                }
            }
        }
    }
}
sources! {
    Claude => (claude,"claude","Claude","../../../plugins/claude-usage-source/icon.svg"),
    Codex => (codex,"codex","Codex","../../../plugins/codex-usage-source/icon.svg"),
    Copilot => (copilot,"copilot","GitHub Copilot","../../../plugins/copilot-usage-source/icon.svg"),
    Cursor => (cursor,"cursor","Cursor","../../../plugins/cursor-usage-source/icon.svg"),
    Grok => (grok,"grok","Grok","../../../plugins/grok-usage-source/icon.svg"),
    Kimi => (kimi,"kimi","Kimi","../../../plugins/kimi-usage-source/icon.svg"),
    MiniMax => (minimax,"minimax","MiniMax","../../../plugins/minimax-usage-source/icon.svg"),
    Muse => (muse,"muse","Muse Code","../../../plugins/muse-provider/icon.svg"),
    OpenCodeGo => (opencode_go,"opencode-go","OpenCode Go","../../../plugins/opencode-go-usage-source/icon.svg"),
    Zai => (zai,"zai","Z.ai","../../../plugins/zai-usage-source/icon.svg"),
}

fn registered_sources(rt: &UsageRuntime) -> Result<Vec<Source>> {
    let mut sources = Source::ALL.to_vec();
    if let Some(plugins) = &rt.plugins {
        for row in plugins.registered_usage_sources() {
            let field = |key: &str| {
                row.get(key)
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
                    .ok_or_else(|| anyhow!("Invalid plugin usage source {key}"))
            };
            let id = field("id")?;
            if sources.iter().any(|source| source.id() == id) {
                bail!("Duplicate usage source: {id}");
            }
            sources.push(Source::Plugin {
                id,
                label: field("label")?,
                icon: row.get("icon").and_then(Value::as_str).map(str::to_owned),
                plugin_id: field("pluginId")?,
                generation: field("generation")?,
            });
        }
    }
    Ok(sources)
}

#[derive(Clone)]
struct Login {
    input: Value,
    harness: String,
}
#[derive(Clone)]
struct KnownReport {
    source: Source,
    label: Option<String>,
    logins: Vec<Login>,
}
type Reports = Vec<(String, KnownReport)>;
struct AgentReports {
    signature: Value,
    reports: Reports,
}
struct CacheEntry {
    at: Instant,
    entry: Value,
}
#[derive(Default)]
struct State {
    defaults: Reports,
    by_agent: Vec<(String, AgentReports)>,
    sessions: HashMap<String, Value>,
    cache: HashMap<String, CacheEntry>,
    successful: HashMap<String, Value>,
    pending: HashMap<String, watch::Sender<Option<Value>>>,
}
struct Inner {
    home: PathBuf,
    http: reqwest::Client,
    endpoints: BTreeMap<String, String>,
    state: Mutex<State>,
}

#[derive(Clone)]
pub struct UsageService {
    inner: Arc<Inner>,
}
impl UsageService {
    pub fn new(home: PathBuf) -> Self {
        Self::with_http_client(home, reqwest::Client::new(), BTreeMap::new())
    }

    /// Endpoint-prefix substitution and client injection support offline HTTP fixtures.
    pub fn with_http_client(
        home: PathBuf,
        http: reqwest::Client,
        endpoints: BTreeMap<String, String>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                home,
                http,
                endpoints,
                state: Mutex::new(State::default()),
            }),
        }
    }

    fn runtime(&self, context: &UsageContext, cancel: CancellationToken) -> UsageRuntime {
        UsageRuntime {
            home: context
                .user_home
                .clone()
                .unwrap_or_else(|| self.inner.home.clone()),
            env: context.env.clone(),
            http: self.inner.http.clone(),
            cancel,
            endpoints: self.inner.endpoints.clone(),
            muse_providers: context.muse_providers.clone(),
            plugins: context.plugins.clone(),
        }
    }

    /// Returns the report entries, not a wire envelope. The wire adapter emits one
    /// `usage.list_reports.update` per entry followed by its terminal response.
    pub async fn list_reports(
        &self,
        request: &Value,
        context: &UsageContext,
        cancel: CancellationToken,
    ) -> Result<Value> {
        self.list_reports_inner(request, context, cancel, None)
            .await
    }

    /// Streams report entries as each account finishes; sender values are bare entries.
    pub async fn list_reports_streaming(
        &self,
        request: &Value,
        context: &UsageContext,
        cancel: CancellationToken,
        updates: mpsc::Sender<Value>,
    ) -> Result<Value> {
        self.list_reports_inner(request, context, cancel, Some(updates))
            .await
    }

    async fn list_reports_inner(
        &self,
        request: &Value,
        context: &UsageContext,
        cancel: CancellationToken,
        updates: Option<mpsc::Sender<Value>>,
    ) -> Result<Value> {
        let request = request.get("payload").unwrap_or(request);
        let agent_id = request
            .get("agentId")
            .map(|id| id.as_str().ok_or_else(|| anyhow!("Invalid agentId")))
            .transpose()?;
        let report_ids = request
            .get("reportIds")
            .map(|ids| ids.as_array().ok_or_else(|| anyhow!("Invalid reportIds")))
            .transpose()?
            .map(|ids| {
                ids.iter()
                    .map(|id| {
                        id.as_str()
                            .map(str::to_owned)
                            .ok_or_else(|| anyhow!("Invalid reportIds"))
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?;
        if agent_id.is_some() && report_ids.is_some() {
            bail!("agentId and reportIds cannot be combined");
        }
        let force = request
            .get("forceRefresh")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let rt = self.runtime(context, cancel.clone());
        let sources = registered_sources(&rt)?;
        {
            let mut state = self.inner.state.lock().unwrap();
            state
                .defaults
                .retain(|(_, report)| sources.contains(&report.source));
            state.by_agent.retain(|(_, mapping)| {
                mapping
                    .reports
                    .iter()
                    .all(|(_, report)| sources.contains(&report.source))
            });
            state.sessions = context
                .sessions
                .iter()
                .filter_map(|session| {
                    session
                        .get("agentId")
                        .and_then(Value::as_str)
                        .map(|id| (id.to_owned(), session_signature(session)))
                })
                .collect();
            let live: HashSet<_> = state.sessions.keys().cloned().collect();
            state.by_agent.retain(|(id, _)| live.contains(id));
            prune_cache(&mut state);
        }
        let reports = if let Some(agent_id) = agent_id {
            let session = context
                .sessions
                .iter()
                .find(|session| session.get("agentId").and_then(Value::as_str) == Some(agent_id))
                .ok_or_else(|| anyhow!("Unknown agent: {agent_id}"))?;
            if session.get("provider").and_then(Value::as_str).is_none() {
                let mut state = self.inner.state.lock().unwrap();
                state.by_agent.retain(|(id, _)| id != agent_id);
                prune_cache(&mut state);
                return Ok(json!([]));
            }
            let signature = session_signature(session);
            let cached = {
                let state = self.inner.state.lock().unwrap();
                state
                    .by_agent
                    .iter()
                    .find(|(id, mapping)| id == agent_id && mapping.signature == signature)
                    .map(|(_, mapping)| mapping.reports.clone())
            };
            if let Some(cached) = cached {
                cached
            } else {
                let provider = session["provider"].as_str().unwrap();
                let family = context
                    .provider_specs
                    .iter()
                    .find(|spec| spec.id == provider)
                    .map(|spec| spec.family.as_str())
                    .unwrap_or(provider);
                let scope = UsageScope {
                    provider: Some(family.to_owned()),
                    model: session
                        .get("model")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    env: session
                        .get("env")
                        .and_then(Value::as_object)
                        .map(|env| {
                            env.iter()
                                .filter_map(|(key, value)| {
                                    value.as_str().map(|value| (key.clone(), value.to_owned()))
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                };
                let reports = tokio::select! {
                    _ = cancel.cancelled() => bail!("Usage request cancelled"),
                    reports = discover(&rt, &scope, &[], &sources) => reports,
                };
                let mut state = self.inner.state.lock().unwrap();
                // Original publishes unless the agent was resumed under another sessionKey.
                if state.sessions.get(agent_id).map(|current| &current[2]) != Some(&signature[2]) {
                    return Ok(json!([]));
                }
                if let Some((_, mapping)) = state.by_agent.iter_mut().find(|(id, _)| id == agent_id)
                {
                    *mapping = AgentReports {
                        signature,
                        reports: reports.clone(),
                    };
                } else {
                    state.by_agent.push((
                        agent_id.to_owned(),
                        AgentReports {
                            signature,
                            reports: reports.clone(),
                        },
                    ));
                }
                prune_cache(&mut state);
                reports
            }
        } else if let Some(ids) = report_ids {
            let known = merged_reports(&self.inner.state.lock().unwrap());
            let mut seen = HashSet::new();
            ids.into_iter()
                .filter(|id| seen.insert(id.clone()))
                .filter_map(|id| known.iter().find(|(known_id, _)| *known_id == id).cloned())
                .collect()
        } else {
            let previous = self.inner.state.lock().unwrap().defaults.clone();
            let scope = UsageScope {
                provider: None,
                model: None,
                env: context.env.clone(),
            };
            let defaults = tokio::select! {
                _ = cancel.cancelled() => bail!("Usage request cancelled"),
                defaults = discover(&rt,&scope,&previous,&sources) => defaults,
            };
            let mut state = self.inner.state.lock().unwrap();
            state.defaults = defaults;
            prune_cache(&mut state);
            merged_reports(&state)
        };
        let mut pending = FuturesUnordered::new();
        for (index, (id, known)) in reports.into_iter().enumerate() {
            let rt = rt.clone();
            let cancel = cancel.clone();
            pending.push(async move {
                self.fetch_id(id, known, force, rt, cancel)
                    .await
                    .map(|entry| (index, entry))
            });
        }
        let mut entries = Vec::new();
        while let Some(result) = pending.next().await {
            let (index, entry) = result?;
            if let Some(updates) = &updates {
                tokio::select! {
                    _ = cancel.cancelled() => bail!("Usage request cancelled"),
                    result = updates.send(entry.clone()) => result.map_err(|_|anyhow!("Usage update consumer closed"))?,
                }
            }
            entries.push((index, entry));
        }
        entries.sort_by_key(|(index, _)| *index);
        Ok(Value::Array(
            entries.into_iter().map(|(_, entry)| entry).collect(),
        ))
    }

    async fn fetch_id(
        &self,
        id: String,
        known: KnownReport,
        force: bool,
        mut rt: UsageRuntime,
        cancel: CancellationToken,
    ) -> Result<Value> {
        let key = cache_identity(&id, &known);
        let (mut receiver, owner) = {
            let mut state = self.inner.state.lock().unwrap();
            if !force {
                if let Some(cached) = state
                    .cache
                    .get(&key)
                    .filter(|cached| cached.at.elapsed() < Duration::from_secs(300))
                {
                    return Ok(cached.entry.clone());
                }
            }
            if let Some(pending) = state.pending.get(&key) {
                (pending.subscribe(), false)
            } else {
                let (sender, receiver) = watch::channel(None);
                state.pending.insert(key.clone(), sender);
                (receiver, true)
            }
        };
        if owner {
            // A cancelled subscriber must not cancel another subscriber's shared fetch.
            rt.cancel = CancellationToken::new();
            let inner = self.inner.clone();
            tokio::spawn(async move {
                let fetched_at = now_iso();
                let outcome = fetch_with_fallback(&rt, &known).await;
                let completed_at = now_iso();
                let mut account = json!({});
                if let Some(label) = &known.label {
                    account["label"] = json!(label);
                }
                let mut entry = json!({"id":id,"sourceId":known.source.id(),"sourceLabel":known.source.label(),
                    "account":account,"fetchedAt":fetched_at,"report":outcome.0});
                if let Some(icon) = known.source.icon() {
                    entry["icon"] = json!(icon);
                }
                if let Some(errors) = outcome.1 {
                    entry["loginErrors"] = Value::Array(errors);
                }
                let mut state = inner.state.lock().unwrap();
                let available = entry["report"]["status"] == "available";
                if available {
                    entry["successfulAt"] = json!(completed_at);
                } else if let Some(previous) = state.successful.get(&key) {
                    entry["lastSuccessful"] = previous.clone();
                }
                if known.source.current(&rt) && active_cache_keys(&state).contains(&key) {
                    if available {
                        state.successful.insert(
                            key.clone(),
                            json!({"fetchedAt":completed_at,"report":entry["report"]}),
                        );
                    }
                    state
                        .cache
                        .retain(|_, cached| cached.at.elapsed() < Duration::from_secs(300));
                    state.cache.insert(
                        key.clone(),
                        CacheEntry {
                            at: Instant::now(),
                            entry: entry.clone(),
                        },
                    );
                }
                if let Some(sender) = state.pending.remove(&key) {
                    sender.send_replace(Some(entry));
                }
            });
        }
        loop {
            if let Some(entry) = receiver.borrow().clone() {
                return Ok(entry);
            }
            tokio::select! {
                _ = cancel.cancelled() => bail!("Usage request cancelled"),
                result = receiver.changed() => result.map_err(|_|anyhow!("Usage fetch task ended without a report"))?,
            }
        }
    }

    pub async fn list_legacy(
        &self,
        context: &UsageContext,
        cancel: CancellationToken,
    ) -> Result<Value> {
        let entries = self.list_reports(&json!({}), context, cancel).await?;
        let entries = entries
            .as_array()
            .ok_or_else(|| anyhow!("Invalid usage report list"))?;
        let fetched_at = entries
            .iter()
            .filter_map(|entry| entry["fetchedAt"].as_str())
            .min()
            .map(str::to_owned)
            .unwrap_or_else(now_iso);
        let providers: Vec<_> = entries.iter().map(|entry| {
            let report = &entry["report"];
            let available = report["status"] == "available";
            let label = entry["sourceLabel"].as_str().unwrap_or_default();
            let name = entry["account"]["label"].as_str().filter(|label| !label.is_empty())
                .map(|account|format!("{label} ({account})")).unwrap_or_else(||label.to_owned());
            json!({"providerId":entry["sourceId"],"displayName":name,"status":report["status"],
                "planLabel":if available { report.get("planLabel").cloned().unwrap_or(Value::Null) } else { Value::Null },
                "windows":if available { report["windows"].clone() } else { json!([]) },
                "balances":if available { report.get("balances").cloned().unwrap_or_else(||json!([])) } else { json!([]) },
                "details":if available { report.get("details").cloned().unwrap_or_else(||json!([])) } else { json!([]) },
                "error":legacy_error(report)})
        }).collect();
        Ok(json!({"fetchedAt":fetched_at,"providers":providers}))
    }
}

fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}
fn session_signature(session: &Value) -> Value {
    json!([
        session.get("provider"),
        session.get("model"),
        session.get("sessionKey")
    ])
}
fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let sorted: BTreeMap<_, _> = object
                .iter()
                .map(|(key, value)| (key.clone(), canonical(value)))
                .collect();
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(values) => Value::Array(values.iter().map(canonical).collect()),
        value => value.clone(),
    }
}
fn login_key(login: &Login) -> String {
    canonical(&json!({"input":login.input,"harness":login.harness})).to_string()
}
fn cache_identity(id: &str, known: &KnownReport) -> String {
    let identity = json!([
        known.label,
        known.logins.iter().map(login_key).collect::<Vec<_>>()
    ]);
    let hash = hash_account_key(&identity.to_string());
    match known.source.generation() {
        Some(generation) => format!("{id}:{generation}:{hash}"),
        None => format!("{id}:{hash}"),
    }
}
fn merged_reports(state: &State) -> Reports {
    let mut merged: Reports = Vec::new();
    for (id, report) in state.defaults.iter().chain(
        state
            .by_agent
            .iter()
            .flat_map(|(_, mapping)| mapping.reports.iter()),
    ) {
        if let Some((_, known)) = merged.iter_mut().find(|(known_id, _)| known_id == id) {
            for login in &report.logins {
                let key = login_key(login);
                if !known
                    .logins
                    .iter()
                    .any(|existing| login_key(existing) == key)
                {
                    known.logins.push(login.clone());
                }
            }
        } else {
            merged.push((id.clone(), report.clone()));
        }
    }
    merged
}
fn active_cache_keys(state: &State) -> HashSet<String> {
    merged_reports(state)
        .iter()
        .chain(state.defaults.iter())
        .chain(
            state
                .by_agent
                .iter()
                .flat_map(|(_, mapping)| mapping.reports.iter()),
        )
        .map(|(id, known)| cache_identity(id, known))
        .collect()
}
fn prune_cache(state: &mut State) {
    let active = active_cache_keys(state);
    state.cache.retain(|key, _| active.contains(key));
    state.successful.retain(|key, _| active.contains(key));
}
async fn discover(
    rt: &UsageRuntime,
    scope: &UsageScope,
    previous: &[(String, KnownReport)],
    sources: &[Source],
) -> Reports {
    let results = futures_util::future::join_all(sources.iter().map(|source| async move {
        match source.discover(rt, scope).await.and_then(validate_accounts) {
            Ok(accounts) => {
                let mut reports: Reports = Vec::new();
                for account in accounts {
                    let id = format!("{}:{}", source.id(), account.key);
                    let login = Login {
                        input: account.input,
                        harness: account.harness.unwrap_or_else(|| source.label().to_owned()),
                    };
                    if let Some((_, known)) =
                        reports.iter_mut().find(|(known_id, _)| *known_id == id)
                    {
                        let key = login_key(&login);
                        if !known
                            .logins
                            .iter()
                            .any(|existing| login_key(existing) == key)
                        {
                            known.logins.push(login);
                        }
                    } else {
                        reports.push((
                            id,
                            KnownReport {
                                source: source.clone(),
                                label: account.label,
                                logins: vec![login],
                            },
                        ));
                    }
                }
                reports
            }
            Err(_) => {
                // Never include store contents, token-bearing request URLs, or raw errors in logs.
                tracing::warn!(source_id = source.id(), "Usage source discovery failed");
                previous
                    .iter()
                    .filter(|(_, known)| known.source == *source)
                    .cloned()
                    .collect()
            }
        }
    }))
    .await;
    results.into_iter().flatten().collect()
}
fn validate_accounts(accounts: Vec<UsageAccount>) -> Result<Vec<UsageAccount>> {
    for account in &accounts {
        if account.key.is_empty()
            || account.key.len() > 128
            || !account
                .key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
            || account.harness.as_ref().is_some_and(String::is_empty)
        {
            bail!("Invalid usage account");
        }
    }
    Ok(accounts)
}
async fn fetch_with_fallback(
    rt: &UsageRuntime,
    known: &KnownReport,
) -> (Value, Option<Vec<Value>>) {
    let mut attempts = FuturesOrdered::new();
    for login in &known.logins {
        attempts.push_back(async move {
            let report = match tokio::time::timeout(
                Duration::from_secs(20),
                known.source.fetch(rt, &login.input),
            )
            .await
            {
                Ok(Ok(report)) => validate_report(report)
                    .unwrap_or_else(|error| json!({"status":"error","error":error.to_string()})),
                Ok(Err(error)) => json!({"status":"error","error":error.to_string()}),
                Err(_) => json!({"status":"error","error":"Usage fetch timed out"}),
            };
            (login.harness.clone(), report)
        });
    }
    let mut errors = Vec::new();
    while let Some((harness, report)) = attempts.next().await {
        if report["status"] == "available" {
            return (report, None);
        }
        errors.push(json!({"harness":harness,"report":report}));
    }
    let report = errors
        .last()
        .map(|error| error["report"].clone())
        .unwrap_or_else(|| json!({"status":"error","error":"Usage account has no login"}));
    (report, Some(errors))
}
fn legacy_error(report: &Value) -> Option<String> {
    match report["status"].as_str()? {
        "error" => report["error"].as_str().map(str::to_owned),
        "unavailable" => {
            let problem = &report["problem"];
            let remedy = problem["refreshedBy"]
                .as_str()
                .filter(|by| !by.is_empty())
                .map(|by| format!("Run {by} to refresh it."))
                .unwrap_or_else(|| "Sign in again.".into());
            match problem["kind"].as_str()? {
                "no_quota" => problem["detail"].as_str().map(str::to_owned),
                "rejected" => Some(format!(
                    "Login rejected (HTTP {}). {remedy}",
                    problem["status"]
                )),
                "expired" => {
                    let elapsed = problem["expiresAt"]
                        .as_str()
                        .and_then(|date| chrono::DateTime::parse_from_rfc3339(date).ok())
                        .map(|date| {
                            (Utc::now().timestamp_millis() - date.timestamp_millis()).max(0)
                        })
                        .unwrap_or(0);
                    let ago = if elapsed >= 86_400_000 {
                        format!("{}d ago", elapsed / 86_400_000)
                    } else if elapsed >= 3_600_000 {
                        format!("{}h ago", elapsed / 3_600_000)
                    } else if elapsed >= 60_000 {
                        format!("{}m ago", elapsed / 60_000)
                    } else {
                        "just now".into()
                    };
                    Some(format!("Login expired {ago}. {remedy}"))
                }
                _ => None,
            }
        }
        _ => None,
    }
}
