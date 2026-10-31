//! Native MSP adapter. Protocol authority: plugins/muse-provider/server (MSP 1.3+).
mod catalog;
mod projection;
mod session;
mod transport;

use super::api::{
    AgentProvider, AgentSession, EventSink, ImportedSession, LaunchContext, ProviderSpec,
};
use crate::process::{self, CommandSpec};
use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use serde_json::{Value, json};
use session::Session;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use tokio_util::sync::CancellationToken;
use transport::Connection;

pub struct MuseProvider {
    spec: ProviderSpec,
    capabilities: Value,
    sessions: Mutex<Vec<Weak<Session>>>,
    diagnostic: Mutex<String>,
    usage_environments: Mutex<BTreeMap<String, BTreeMap<String, String>>>,
    catalog: catalog::Catalog,
}

pub(super) fn capabilities() -> Value {
    json!({"supportsStreaming":true,"supportsSessionPersistence":true,"supportsSessionListing":true,
        "supportsDynamicModes":true,"supportsMcpServers":true,"supportsNativePaseoTools":false,
        "supportsReasoningStream":true,"supportsToolInvocations":true,
        "supportsRewindConversation":false,"supportsRewindFiles":false,"supportsRewindBoth":false})
}

impl MuseProvider {
    pub fn new(spec: ProviderSpec) -> Self {
        Self {
            spec,
            capabilities: capabilities(),
            sessions: Mutex::new(Vec::new()),
            usage_environments: Mutex::new(BTreeMap::new()),
            catalog: catalog::Catalog::new(),
            diagnostic: Mutex::new("Muse availability has not been probed".into()),
        }
    }
    async fn open(
        &self,
        persistence: Option<Value>,
        config: Value,
        context: LaunchContext,
        sink: EventSink,
    ) -> Result<Arc<Session>> {
        self.remember(&self.spec.env);
        let mut env = self.spec.env.clone();
        env.extend(context.env.clone());
        let session = Session::open(&self.spec, persistence, config, context, sink).await?;
        self.remember(&env);
        let mut sessions = self.sessions.lock().unwrap();
        sessions.retain(|s| s.strong_count() > 0);
        sessions.push(Arc::downgrade(&session));
        Ok(session)
    }
    fn remember(&self, env: &BTreeMap<String, String>) {
        self.usage_environments
            .lock()
            .unwrap()
            .insert(usage_account_key(env), env.clone());
    }
    pub fn known_usage_environments(&self) -> Vec<BTreeMap<String, String>> {
        self.usage_environments
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect()
    }
    /// Reads the actual MSP quota response; usage-source presentation is daemon-owned.
    pub async fn read_usage(
        &self,
        env: BTreeMap<String, String>,
        cancel: CancellationToken,
    ) -> Result<Value> {
        let sessions: Vec<_> = self
            .sessions
            .lock()
            .unwrap()
            .iter()
            .filter_map(Weak::upgrade)
            .filter(|s| s.matches_env(&env))
            .collect();
        if !sessions.is_empty() {
            let mut newest: Option<Value> = None;
            let observations = futures_util::future::try_join_all(
                sessions.iter().map(|session| session.read_usage()),
            )
            .await?;
            for mut response in observations {
                if let Some(usage) = response
                    .as_object_mut()
                    .and_then(|object| object.remove("usage"))
                {
                    if newest.as_ref().is_none_or(|old| {
                        usage["observedAtMs"].as_f64().unwrap_or(0.)
                            > old["observedAtMs"].as_f64().unwrap_or(0.)
                    }) {
                        newest = Some(usage);
                    }
                }
            }
            return Ok(newest
                .map(|usage| json!({"usage":usage}))
                .unwrap_or_else(|| json!({})));
        }
        let (host, _notifications) =
            Connection::spawn(&self.spec, env, None, &json!({}), cancel).await?;
        let result = host.request("usage/read", json!({})).await;
        let closed = host.close().await;
        let result = result?;
        closed?;
        validate_usage(result)
    }
    async fn status(&self, cancel: CancellationToken) -> Result<()> {
        let (program, prefix) = self
            .spec
            .command
            .split_first()
            .ok_or_else(|| anyhow!("Install Muse Code and ensure `muse` is on PATH."))?;
        self.remember(&self.spec.env);
        let mut args = prefix.to_vec();
        args.push("--version".into());
        let display = format!("{program} {}", args.join(" "));
        let output = process::run(
            CommandSpec {
                program: program.clone(),
                args,
                cwd: None,
                env: self.spec.env.clone(),
                clear_env: false,
            },
            8192,
            Duration::from_secs(3),
            cancel.clone(),
        )
        .await
        .map_err(|error| {
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
            {
                anyhow!("spawn {program} ENOENT")
            } else {
                error
            }
        })?;
        if output.status != 0 {
            bail!("Command failed: {display}\n{}", output.stderr);
        }
        let text = String::from_utf8_lossy(&output.stdout);
        static VERSION: std::sync::LazyLock<regex::Regex> =
            std::sync::LazyLock::new(|| regex::Regex::new(r"\b(\d+)\.(\d+)\.(\d+)\b").unwrap());
        let version = VERSION
            .captures(&text)
            .ok_or_else(|| anyhow!("Muse returned an unrecognized version."))?;
        let major = version[1].parse::<f64>()?;
        let minor = version[2].parse::<f64>()?;
        if major < 1. || (major == 1. && minor < 3.) {
            bail!("Update Muse Code: found {}, need ≥1.3.0", &version[0]);
        }
        let (host, _notifications) = Connection::spawn(
            &self.spec,
            self.spec.env.clone(),
            None,
            &json!({"timeoutMs":3000,"startupTimeoutMs":20000}),
            cancel,
        )
        .await?;
        let account = host.request("account/read", json!({})).await;
        let closed = host.close().await;
        let account = account?;
        closed?;
        match account["state"].as_str() {
            Some("loggedOut") => bail!("Run `muse login` or set META_API_KEY"),
            Some(_) => Ok(()),
            None => bail!("Invalid Muse account response"),
        }
    }
}

#[async_trait]
impl AgentProvider for MuseProvider {
    fn id(&self) -> &str {
        &self.spec.id
    }
    fn capabilities(&self) -> &Value {
        &self.capabilities
    }
    async fn create_session(
        &self,
        config: Value,
        context: LaunchContext,
        _options: Value,
        sink: EventSink,
    ) -> Result<Arc<dyn AgentSession>> {
        Ok(self.open(None, config, context, sink).await?)
    }
    async fn resume_session(
        &self,
        persistence: Value,
        config: Value,
        context: LaunchContext,
        _options: Value,
        sink: EventSink,
    ) -> Result<Arc<dyn AgentSession>> {
        Ok(self
            .open(
                Some(decode_persistence(&persistence)?),
                config,
                context,
                sink,
            )
            .await?)
    }
    async fn fetch_catalog(&self, _options: Value, cancel: CancellationToken) -> Result<Value> {
        self.remember(&self.spec.env);
        let mut catalog = self.catalog.read(&self.spec, cancel).await?;
        if let Some(models) = catalog["models"].as_array_mut() {
            for model in models {
                model["provider"] = json!(self.spec.id);
            }
        }
        Ok(catalog)
    }
    async fn list_features(&self, _config: &Value) -> Result<Vec<Value>> {
        Ok(vec![])
    }
    async fn list_commands(&self, _config: &Value) -> Result<Vec<Value>> {
        Ok(vec![])
    }
    async fn list_importable_sessions(&self, options: Value) -> Result<Vec<Value>> {
        self.remember(&self.spec.env);
        self.catalog
            .list(&self.spec, &options, CancellationToken::new())
            .await
    }
    async fn import_session(
        &self,
        input: Value,
        mut config: Value,
        _stored_config: Value,
        context: LaunchContext,
        sink: EventSink,
    ) -> Result<ImportedSession> {
        let handle = input["providerHandleId"]
            .as_str()
            .ok_or_else(|| anyhow!("Missing Muse import handle"))?;
        if let Some(cwd) = input.get("cwd") {
            config["cwd"] = cwd.clone();
        }
        config["provider"] = json!(self.spec.id);
        let saved = decode_persistence(&json!({"sessionId":handle}))?;
        let session = self
            .open(Some(saved), config.clone(), context, sink)
            .await?;
        let history = session.history();
        let timeline = history
            .iter()
            .filter(|e| e["type"] == "timeline")
            .map(|e| json!({"item":e["item"],"timestamp":e["timestamp"]}))
            .collect();
        let provider_subagent_events = history
            .into_iter()
            .filter(|e| e["type"] == "provider_subagent")
            .collect();
        let persistence = session
            .describe_persistence()
            .ok_or_else(|| anyhow!("Muse session did not persist import"))?;
        Ok(ImportedSession {
            session,
            config,
            persistence,
            timeline,
            provider_subagent_events,
        })
    }
    async fn is_available(&self, _options: Value, cancel: CancellationToken) -> Result<bool> {
        let result = self.status(cancel).await;
        *self.diagnostic.lock().unwrap() = match &result {
            Ok(()) => "Provider is available".into(),
            Err(e) => transport::actionable_error(e, &self.spec.env)["message"]
                .as_str()
                .unwrap_or("Muse is unavailable")
                .into(),
        };
        Ok(result.is_ok())
    }
    async fn diagnostic(&self) -> Result<String> {
        self.is_available(json!({}), CancellationToken::new())
            .await?;
        Ok(self.diagnostic.lock().unwrap().clone())
    }
    // Muse never negotiated archive/unarchive; original core intentionally leaves native storage untouched.
    async fn archive_native_session(&self, _persistence: &Value) -> Result<()> {
        Ok(())
    }
    async fn unarchive_native_session(&self, _persistence: &Value) -> Result<()> {
        Ok(())
    }
    async fn shutdown(&self) -> Result<()> {
        let sessions: Vec<_> = self
            .sessions
            .lock()
            .unwrap()
            .iter()
            .filter_map(Weak::upgrade)
            .collect();
        let mut failure = self.catalog.close().await.err();
        for session in sessions {
            if let Err(e) = session.close().await {
                failure = Some(e);
            }
        }
        match failure {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

fn decode_persistence(handle: &Value) -> Result<Value> {
    let saved = if let Some(saved) = handle.pointer("/metadata/pluginProviderPersistence") {
        saved.clone()
    } else if handle.get("version").is_some() {
        handle.clone()
    } else {
        let id = handle["sessionId"]
            .as_str()
            .ok_or_else(|| anyhow!("Invalid Muse persistence handle"))?;
        serde_json::from_str(
            id.strip_prefix("plugin:")
                .ok_or_else(|| anyhow!("Invalid plugin provider persistence handle"))?,
        )?
    };
    if saved["version"] != 1
        || saved
            .pointer("/data/sessionId")
            .and_then(Value::as_str)
            .is_none()
    {
        bail!("Unsupported Muse persistence version or invalid session ID");
    }
    if let Some(option) = saved
        .pointer("/data/thinkingOption")
        .and_then(Value::as_str)
    {
        session::validate_effort(option)?;
    }
    Ok(saved)
}

pub fn usage_account_key(env: &BTreeMap<String, String>) -> String {
    use sha2::{Digest, Sha256};
    use std::path::{Component, PathBuf};
    let home = env
        .get("HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .unwrap_or_default();
    let path = env
        .get("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"))
        .join("muse");
    let absolute = if path.is_absolute() {
        path
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let mut normalized = PathBuf::new();
    for part in absolute.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    hex::encode(Sha256::digest(normalized.to_string_lossy().as_bytes()))
}

fn validate_usage(response: Value) -> Result<Value> {
    if !response.is_object() {
        bail!("Invalid Muse usage response");
    }
    if let Some(usage) = response.get("usage") {
        if !usage["tier"].is_string() {
            bail!("Invalid Muse usage tier");
        }
        for path in [
            "/observedAtMs",
            "/window/resetsAtMs",
            "/window/usedPercent",
            "/window/windowDurationMins",
            "/weekly/resetsAtMs",
            "/weekly/usedPercent",
        ] {
            if !usage.pointer(path).is_some_and(Value::is_number) {
                bail!("Invalid Muse usage {path}");
            }
        }
    }
    Ok(response)
}
