use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::Result;
use chrono::{DateTime, NaiveDate, SecondsFormat};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::providers::api::ProviderSpec;

use super::transport::{Connection, MuseError};

const EFFORTS: &[&str] = &["minimal", "low", "medium", "high", "xhigh", "max", "ultra"];

struct CachedCatalog {
    key: String,
    expires: Instant,
    value: Value,
}

struct ActiveHost {
    cancel: CancellationToken,
    completion: watch::Receiver<Option<std::result::Result<(), String>>>,
}

#[derive(Default)]
struct CatalogState {
    closed: bool,
    sequence: u64,
    cache: VecDeque<CachedCatalog>,
    active: BTreeMap<u64, ActiveHost>,
}

pub struct Catalog {
    state: Arc<Mutex<CatalogState>>,
}

impl Catalog {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(CatalogState::default())),
        }
    }

    pub async fn read(&self, spec: &ProviderSpec, cancel: CancellationToken) -> Result<Value> {
        let key = launch_key(spec);
        {
            let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            if state.closed {
                return Err(MuseError::new("closed", "Muse catalogue is closed").into());
            }
            if let Some(cached) = state
                .cache
                .iter()
                .find(|entry| entry.key == key && entry.expires > Instant::now())
            {
                return Ok(cached.value.clone());
            }
        }
        let mut lease = self.begin(cancel)?;
        let (host, _notifications) = Connection::spawn(
            spec,
            BTreeMap::new(),
            None,
            &json!({"timeoutMs": 5000}),
            lease.cancel.clone(),
        )
        .await?;
        lease.host = Some(host.clone());
        let result = host
            .request("model/list", json!({}))
            .await
            .and_then(present);
        if let Ok(catalog) = &result {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            if !state.closed {
                // JavaScript Map preserves insertion order on replacement, and
                // its source implementation evicts before every full-cache set.
                if state.cache.len() >= 8 {
                    state.cache.pop_front();
                }
                let expires = Instant::now() + Duration::from_secs(60);
                if let Some(entry) = state.cache.iter_mut().find(|entry| entry.key == key) {
                    entry.expires = expires;
                    entry.value = catalog.clone();
                } else {
                    state.cache.push_back(CachedCatalog {
                        key,
                        expires,
                        value: catalog.clone(),
                    });
                }
            }
        }
        let closed = host.close().await;
        let result = result?;
        closed?;
        Ok(result)
    }

    pub async fn list(
        &self,
        spec: &ProviderSpec,
        options: &Value,
        cancel: CancellationToken,
    ) -> Result<Vec<Value>> {
        let mut lease = self.begin(cancel)?;
        let (host, _notifications) = Connection::spawn(
            spec,
            BTreeMap::new(),
            None,
            &json!({}),
            lease.cancel.clone(),
        )
        .await?;
        lease.host = Some(host.clone());
        let result = list(&host, options).await;
        let closed = host.close().await;
        let result = result?;
        closed?;
        Ok(result)
    }

    pub async fn close(&self) -> Result<()> {
        let hosts: Vec<_> = {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            state.closed = true;
            state.cache.clear();
            state
                .active
                .values()
                .map(|host| (host.cancel.clone(), host.completion.clone()))
                .collect()
        };
        for (cancel, _) in &hosts {
            cancel.cancel();
        }
        let mut failure = None;
        for (_, mut completion) in hosts {
            loop {
                if let Some(result) = completion.borrow_and_update().clone() {
                    if let Err(error) = result {
                        failure.get_or_insert(error);
                    }
                    break;
                }
                if completion.changed().await.is_err() {
                    failure.get_or_insert_with(|| "Muse catalogue host cleanup stopped".into());
                    break;
                }
            }
        }
        match failure {
            Some(error) => Err(MuseError::new("closed", error).into()),
            None => Ok(()),
        }
    }

    fn begin(&self, cancel: CancellationToken) -> Result<HostLease> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.closed {
            return Err(MuseError::new("closed", "Muse catalogue is closed").into());
        }
        let cancel = cancel.child_token();
        let (completion, receiver) = watch::channel(None);
        state.sequence += 1;
        let id = state.sequence;
        state.active.insert(
            id,
            ActiveHost {
                cancel: cancel.clone(),
                completion: receiver,
            },
        );
        Ok(HostLease {
            state: self.state.clone(),
            id,
            cancel,
            host: None,
            completion,
            runtime: tokio::runtime::Handle::current(),
        })
    }
}

impl Drop for Catalog {
    fn drop(&mut self) {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        for host in state.active.values() {
            host.cancel.cancel();
        }
    }
}

struct HostLease {
    state: Arc<Mutex<CatalogState>>,
    id: u64,
    cancel: CancellationToken,
    host: Option<Arc<Connection>>,
    completion: watch::Sender<Option<std::result::Result<(), String>>>,
    runtime: tokio::runtime::Handle,
}

impl Drop for HostLease {
    fn drop(&mut self) {
        self.cancel.cancel();
        let state = self.state.clone();
        let id = self.id;
        let host = self.host.take();
        let completion = self.completion.clone();
        self.runtime.spawn(async move {
            let result = match host {
                Some(host) => host.close().await.map_err(|error| error.to_string()),
                None => Ok(()),
            };
            completion.send_replace(Some(result));
            state
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .active
                .remove(&id);
        });
    }
}

fn launch_key(spec: &ProviderSpec) -> String {
    let environment: Vec<_> = spec
        .env
        .iter()
        .filter(|(key, _)| {
            key.starts_with("META_")
                || key.starts_with("MUSE_")
                || key.starts_with("XDG_")
                || matches!(
                    key.as_str(),
                    "HOME" | "PATH" | "HTTP_PROXY" | "HTTPS_PROXY" | "NO_PROXY"
                )
        })
        .collect();
    let (command, args) = match spec.command.split_first() {
        Some((command, args)) => (command.as_str(), args),
        None => ("", &[][..]),
    };
    let identity = json!([command, args, environment]);
    hex::encode(Sha256::digest(identity.to_string().as_bytes()))
}

pub fn modes() -> Vec<Value> {
    vec![
        json!({"id": "onRequest", "label": "Default", "icon": "ShieldCheck", "colorTier": "safe"}),
        json!({"id": "promptUnmatched", "label": "Ask", "icon": "ShieldAlert", "colorTier": "moderate"}),
        json!({"id": "denyUnmatched", "label": "Strict", "icon": "ShieldCheck", "colorTier": "safe"}),
        json!({"id": "allowAll", "label": "Full access", "icon": "ShieldOff", "colorTier": "dangerous", "isUnattended": true}),
    ]
}

pub fn present(mut response: Value) -> Result<Value> {
    let models = response
        .get_mut("models")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| invalid("model/list.models must be an array"))?;
    // Only consumed fields gate discovery. In particular, Muse's descriptive
    // reasoningEffortVariants metadata is deliberately not a protocol gate.
    for model in models.iter() {
        string(model, "modelId")?;
        string(model, "providerId")?;
        string(model, "displayLabel")?;
        nullable(model, "contextLimit", Value::is_number)?;
        if !model.get("isDefault").is_some_and(Value::is_boolean) {
            return Err(invalid("model/list.models[].isDefault must be a boolean").into());
        }
        if let Some(effort) = model.get("defaultReasoningEffort") {
            if !effort.is_null() && !is_effort(effort) {
                return Err(invalid("model/list.models[].defaultReasoningEffort must be a Muse reasoning effort or null").into());
            }
        }
        if let Some(variants) = model.get("variants") {
            if variants != "unknown"
                && !variants
                    .as_array()
                    .is_some_and(|variants| variants.iter().all(is_effort))
            {
                return Err(invalid("model/list.models[].variants must be an array of Muse reasoning efforts or unknown").into());
            }
        }
    }
    let mut presented = Vec::with_capacity(models.len());
    let mut default_model = None;
    for model in std::mem::take(models) {
        let mut model = match model {
            Value::Object(model) => model,
            _ => unreachable!("validated catalog model"),
        };
        let id = model.remove("modelId").expect("validated model ID");
        let is_default = model.remove("isDefault").expect("validated default flag");
        if default_model.is_none() && is_default == true {
            default_model = Some(id.clone());
        }
        let default_effort = model
            .remove("defaultReasoningEffort")
            .filter(|effort| !effort.is_null());
        let variants = match model.remove("variants") {
            Some(Value::Array(variants)) if !variants.is_empty() => variants,
            _ => EFFORTS.iter().map(|effort| Value::from(*effort)).collect(),
        };
        let thinking_options: Vec<_> = variants
            .into_iter()
            .map(|effort| {
                let is_default = default_effort.as_ref() == Some(&effort);
                let mut option = json!({"label": effort, "isDefault": is_default});
                option["id"] = effort;
                option
            })
            .collect();
        let mut presented_model = json!({"provider": "muse", "metadata": {}});
        presented_model["id"] = id;
        presented_model["label"] = model.remove("displayLabel").expect("validated model label");
        presented_model["isDefault"] = is_default;
        presented_model["thinkingOptions"] = Value::Array(thinking_options);
        presented_model["metadata"]["providerId"] = model
            .remove("providerId")
            .expect("validated model provider");
        if let Some(context) = model
            .remove("contextLimit")
            .filter(|context| !context.is_null())
        {
            presented_model["contextWindowMaxTokens"] = context;
        }
        if let Some(effort) = default_effort {
            presented_model["defaultThinkingOptionId"] = effort;
        }
        presented.push(presented_model);
    }
    let mut catalog = json!({"defaultModeId": "onRequest"});
    catalog["models"] = Value::Array(presented);
    catalog["modes"] = Value::Array(modes());
    if let Some(model) = default_model {
        catalog["defaultModel"] = model;
    }
    Ok(catalog)
}

pub async fn list(connection: &Connection, options: &Value) -> Result<Vec<Value>> {
    let limit = match options.get("limit") {
        None => None,
        Some(value) => Some(
            value
                .as_u64()
                .filter(|limit| *limit > 0)
                .ok_or_else(|| invalid("session list limit must be a positive integer"))?,
        ),
    };
    let query = match options.get("query") {
        None => None,
        Some(value) => Some(
            value
                .as_str()
                .ok_or_else(|| invalid("session list query must be a string"))?
                .to_lowercase(),
        ),
    };
    if options.get("cwd").is_some_and(|cwd| !cwd.is_string()) {
        return Err(invalid("session list cwd must be a string").into());
    }
    let mut summaries = Vec::new();
    let mut cursor = Value::Null;
    loop {
        let mut params = json!({"limit": limit.unwrap_or(50).min(200), "cursor": cursor});
        if let Some(cwd) = options.get("cwd") {
            params["workspaceRoot"] = cwd.clone();
        }
        let mut response = connection.request("session/list", params).await?;
        validate_page(&response)?;
        cursor = response
            .get_mut("nextCursor")
            .expect("validated next cursor")
            .take();
        let sessions = response
            .get_mut("sessions")
            .and_then(Value::as_array_mut)
            .expect("validated sessions");
        for session in std::mem::take(sessions) {
            if session["workspaceRoot"].is_null() {
                continue;
            }
            if let Some(query) = &query {
                if !session
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_lowercase()
                    .contains(query)
                {
                    continue;
                }
            }
            let mut session = match session {
                Value::Object(session) => session,
                _ => unreachable!("validated session"),
            };
            let mut data =
                json!({"sessionId": session.remove("sessionId").expect("validated session ID")});
            if let Some(model) = session.remove("modelId").filter(|model| !model.is_null()) {
                data["model"] = model;
            }
            let persistence = json!({"version": 1, "data": data});
            let activity = activity_date(
                session["updatedAt"]
                    .as_str()
                    .expect("validated activity date"),
            );
            summaries.push(json!({
                "providerHandleId": format!("plugin:{persistence}"),
                "cwd": session.remove("workspaceRoot").expect("validated workspace"),
                "title": session.remove("title").unwrap_or(Value::Null),
                "firstPromptPreview": null,
                "lastPromptPreview": null,
                "lastActivityAt": activity,
            }));
            if limit.is_some_and(|limit| summaries.len() as u64 >= limit) {
                return Ok(summaries);
            }
        }
        if cursor.is_null() {
            return Ok(summaries);
        }
    }
}

fn validate_page(page: &Value) -> Result<()> {
    nullable(page, "nextCursor", Value::is_string)?;
    let sessions = page
        .get("sessions")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("session/list.sessions must be an array"))?;
    for session in sessions {
        string(session, "sessionId")?;
        nullable(session, "workspaceRoot", Value::is_string)?;
        if session.get("title").is_some_and(|title| !title.is_string()) {
            return Err(invalid("session/list.sessions[].title must be a string").into());
        }
        string(session, "updatedAt")?;
        nullable(session, "modelId", Value::is_string)?;
    }
    Ok(())
}

fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(format!("{key} must be a string")).into())
}

fn nullable(value: &Value, key: &str, predicate: fn(&Value) -> bool) -> Result<()> {
    if value
        .get(key)
        .is_some_and(|value| value.is_null() || predicate(value))
    {
        Ok(())
    } else {
        Err(invalid(format!("{key} is missing or has an invalid type")).into())
    }
}

fn is_effort(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|effort| effort == "none" || EFFORTS.contains(&effort))
}

fn invalid(message: impl Into<String>) -> MuseError {
    MuseError::new(
        "invalidFrame",
        format!("Invalid Muse response: {}", message.into()),
    )
}

fn activity_date(value: &str) -> String {
    let value = value.trim();
    DateTime::parse_from_rfc3339(value)
        .or_else(|_| DateTime::parse_from_rfc2822(value))
        .ok()
        .map(|date| date.to_utc())
        .or_else(|| {
            NaiveDate::parse_from_str(value, "%Y-%m-%d")
                .ok()?
                .and_hms_opt(0, 0, 0)
                .map(|date| date.and_utc())
        })
        .map(|date| date.to_rfc3339_opts(SecondsFormat::Millis, true))
        .unwrap_or_else(|| "1970-01-01T00:00:00.000Z".into())
}
