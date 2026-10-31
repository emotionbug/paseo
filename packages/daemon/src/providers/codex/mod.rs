mod commands;
mod events;
mod input;
mod permissions;
mod projection;
mod session;
#[cfg(test)]
mod tests;
mod transport;

use crate::providers::registry::catalog::refresh_activity;
use crate::{
    process::{self, CommandSpec},
    providers::api::*,
};
use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use serde_json::{Value, json};
use session::CodexSession;
use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use tokio_util::sync::CancellationToken;
use transport::Transport;

pub struct CodexProvider {
    spec: ProviderSpec,
    caps: Value,
    sessions: Mutex<Vec<Weak<CodexSession>>>,
}
pub(super) fn capabilities() -> Value {
    json!({"supportsStreaming":true,"supportsSessionPersistence":true,"supportsSessionListing":true,"supportsDynamicModes":false,"supportsMcpServers":true,"supportsReasoningStream":true,"supportsToolInvocations":true,"supportsRewindConversation":true,"supportsRewindFiles":false,"supportsRewindBoth":false})
}
pub(super) fn modes(auto_review: bool) -> Vec<Value> {
    let mut values = vec![
        json!({"id":"auto","label":"Default Permissions","description":"Edit files and run commands with Codex's default approval flow."}),
    ];
    if auto_review {
        values.push(json!({"id":"auto-review","label":"Auto-review","description":"Route eligible on-request approvals through the auto-reviewer subagent."}));
    }
    values.push(json!({"id":"full-access","label":"Full Access","description":"Edit files, run commands, and access the network without additional prompts."}));
    values
}
pub(super) fn version(text: &str) -> Option<(u64, u64, u64)> {
    text.split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .find_map(|part| {
            let p: Vec<_> = part.split('.').collect();
            if p.len() != 3 {
                return None;
            }
            Some((p[0].parse().ok()?, p[1].parse().ok()?, p[2].parse().ok()?))
        })
}
pub(super) fn command(
    spec: &ProviderSpec,
    extra: &[&str],
    env: &BTreeMap<String, String>,
    cwd: Option<PathBuf>,
) -> Result<CommandSpec> {
    let program = spec
        .command
        .first()
        .ok_or_else(|| anyhow!("No Codex executable configured"))?
        .clone();
    let mut args = spec.command[1..].to_vec();
    args.extend(extra.iter().map(|s| s.to_string()));
    let mut environment = spec.env.clone();
    environment.extend(env.clone());
    Ok(CommandSpec {
        program,
        args,
        cwd,
        env: environment,
        clear_env: false,
    })
}
pub(super) async fn probe(spec: &ProviderSpec, cancel: CancellationToken) -> Result<String> {
    let output = process::run(
        command(spec, &["--version"], &BTreeMap::new(), None)?,
        1024 * 1024,
        Duration::from_secs(5),
        cancel,
    )
    .await?;
    if output.status != 0 {
        bail!(
            "Codex version probe exited {}: {}",
            output.status,
            output.stderr
        )
    }
    Ok(String::from_utf8(output.stdout)?.trim().into())
}
pub(super) fn home(spec: &ProviderSpec, env: &BTreeMap<String, String>) -> PathBuf {
    env.get("CODEX_HOME")
        .or_else(|| spec.env.get("CODEX_HOME"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("CODEX_HOME").map(PathBuf::from))
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".codex")
        })
}
pub(super) fn custom_config(spec: &ProviderSpec) -> Option<Value> {
    if spec.id == "codex" {
        return None;
    }
    let base = spec
        .env
        .get("OPENAI_BASE_URL")?
        .trim()
        .trim_end_matches('/');
    if base.is_empty() {
        return None;
    }
    let base = if base.ends_with("/v1") {
        base.to_owned()
    } else {
        format!("{base}/v1")
    };
    let mut provider = json!({"name":spec.defaults["label"].as_str().unwrap_or(&spec.id),"base_url":base,"wire_api":"responses"});
    if spec
        .env
        .get("OPENAI_API_KEY")
        .is_some_and(|s| !s.trim().is_empty())
    {
        provider["env_key"] = json!("OPENAI_API_KEY");
        provider["requires_openai_auth"] = json!(false);
    }
    Some(json!({"model_provider":spec.id,"model_providers":{(spec.id.clone()):provider}}))
}
pub(super) async fn metadata(
    client: &Transport,
    cwd: Value,
) -> Result<(Vec<Value>, Value, Vec<Value>, Option<Vec<Value>>)> {
    let model_reply =
        refresh_activity("model/list", client.request("model/list", json!({}))).await?;
    let models = model_reply["data"]
        .as_array()
        .ok_or_else(|| anyhow!("Codex model/list returned no model array"))?
        .clone();
    let config = refresh_activity("config/read", async {
        let mut config = client
            .request("config/read", json!({"cwd":cwd}))
            .await
            .unwrap_or_else(|_| json!({}))["config"]
            .clone();
        if !config.is_object() {
            config = json!({});
        }
        if let Ok(saved) = client.request("getUserSavedConfig", json!({})).await {
            for (source, dest) in [
                ("model", "model"),
                ("modelReasoningEffort", "model_reasoning_effort"),
            ] {
                if saved["config"][source]
                    .as_str()
                    .is_some_and(|s| !s.trim().is_empty())
                {
                    config[dest] = saved["config"][source].clone();
                }
            }
        }
        Ok(config)
    })
    .await?;
    let collaboration = client
        .request("collaborationMode/list", json!({}))
        .await
        .ok()
        .and_then(|v| v["data"].as_array().cloned())
        .unwrap_or_default();
    let skills = client
        .request("skills/list", json!({"cwds":[cwd]}))
        .await
        .ok()
        .map(|v| {
            v["data"]
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|v| v["skills"].as_array().into_iter().flatten())
                .filter(|s| s["enabled"] != false && s["name"].is_string() && s["path"].is_string())
                .cloned()
                .collect()
        });
    Ok((models, config, collaboration, skills))
}
impl CodexProvider {
    pub fn new(spec: ProviderSpec) -> Self {
        Self {
            spec,
            caps: capabilities(),
            sessions: Mutex::new(Vec::new()),
        }
    }
    async fn client(&self, cancel: CancellationToken) -> Result<Arc<Transport>> {
        let (client, mut rx) = refresh_activity("app-server.start", async {
            Transport::spawn(
                command(&self.spec, &["app-server"], &BTreeMap::new(), None)?,
                cancel.clone(),
            )
            .await
        })
        .await?;
        let weak = Arc::downgrade(&client);
        tokio::spawn(async move {
            while let Some(frame) = rx.recv().await {
                if let Some(c) = weak.upgrade() {
                    if frame.get("id").is_some() {
                        tokio::select! {
                            _=c.cancel.cancelled()=>break,
                            _=c.reject(frame["id"].clone(),"No interactive session is attached")=>{},
                        }
                    }
                } else {
                    break;
                }
            }
        });
        let initialized = tokio::select! {
            biased;
            _=cancel.cancelled()=>Err(anyhow!("Codex app-server canceled")),
            result=client.initialize()=>result,
        };
        if let Err(e) = initialized {
            let _ = client.close().await;
            return Err(e);
        }
        Ok(client)
    }
    async fn open(
        &self,
        config: Value,
        context: LaunchContext,
        options: Value,
        persistence: Option<Value>,
        sink: EventSink,
    ) -> Result<Arc<dyn AgentSession>> {
        let session = CodexSession::open(
            self.spec.clone(),
            config,
            context,
            options,
            persistence,
            sink,
        )
        .await?;
        self.sessions.lock().unwrap().push(Arc::downgrade(&session));
        Ok(session)
    }
    async fn archive(&self, persistence: &Value, restore: bool) -> Result<()> {
        let id = persistence["nativeHandle"]
            .as_str()
            .or(persistence["sessionId"].as_str())
            .ok_or_else(|| anyhow!("Missing Codex thread ID"))?;
        let c = self.client(CancellationToken::new()).await?;
        let result = c
            .request(
                if restore {
                    "thread/unarchive"
                } else {
                    "thread/archive"
                },
                json!({"threadId":id}),
            )
            .await;
        let result = match result {
            Err(error)
                if restore
                    && error
                        .to_string()
                        .contains(&format!("no archived rollout found for thread id {id}")) =>
            {
                match c.request("thread/read", json!({"threadId":id})).await {
                    Ok(_) => Ok(()),
                    Err(_) => Err(error),
                }
            }
            result => result.map(|_| ()),
        };
        let close = c.close().await;
        result?;
        close
    }
}
#[async_trait]
impl AgentProvider for CodexProvider {
    fn id(&self) -> &str {
        &self.spec.id
    }
    fn capabilities(&self) -> &Value {
        &self.caps
    }
    async fn create_session(
        &self,
        config: Value,
        context: LaunchContext,
        options: Value,
        sink: EventSink,
    ) -> Result<Arc<dyn AgentSession>> {
        self.open(config, context, options, None, sink).await
    }
    async fn resume_session(
        &self,
        persistence: Value,
        config: Value,
        context: LaunchContext,
        options: Value,
        sink: EventSink,
    ) -> Result<Arc<dyn AgentSession>> {
        let mut merged = persistence["metadata"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        if let Some(c) = config.as_object() {
            merged.extend(c.clone());
        }
        self.open(
            Value::Object(merged),
            context,
            options,
            Some(persistence),
            sink,
        )
        .await
    }
    async fn catalog_cache_key(
        &self,
        _options: Value,
        _cancel: CancellationToken,
    ) -> Result<Option<String>> {
        Ok(Some("host".to_owned()))
    }
    async fn fetch_catalog(&self, _options: Value, cancel: CancellationToken) -> Result<Value> {
        let (version_result, metadata_result) = tokio::join!(
            refresh_activity("version", probe(&self.spec, cancel.clone())),
            async {
                let c = self.client(cancel.clone()).await?;
                let result = tokio::select! {
                    biased;
                    _=cancel.cancelled()=>Err(anyhow!("Codex catalog refresh canceled")),
                    result=metadata(&c,Value::Null)=>result,
                };
                let close = c.close().await;
                let result = result?;
                close?;
                Ok::<_, anyhow::Error>(result)
            }
        );
        if cancel.is_cancelled() {
            bail!("Codex catalog refresh canceled");
        }
        let auto = version_result
            .ok()
            .and_then(|v| version(&v))
            .is_some_and(|v| v >= (0, 115, 0));
        let (models, config, _, _) = metadata_result?;
        let configured = models.iter().any(|m| m["id"] == config["model"]);
        let models:Vec<Value>=models.iter().map(|m|{
            let effort=config["model_reasoning_effort"].as_str().or(m["defaultReasoningEffort"].as_str());
            let mut thinking:Vec<Value>=m["supportedReasoningEfforts"].as_array().into_iter().flatten().filter_map(|e|{let id=e["reasoningEffort"].as_str()?;Some(json!({"id":id,"label":id,"description":e["description"],"isDefault":Some(id)==effort}))}).collect();
            if let Some(e)=effort {if !thinking.iter().any(|t|t["id"]==e){thinking.push(json!({"id":e,"label":e,"isDefault":true}));}}
            json!({"provider":self.spec.id,"id":m["id"],"label":m["displayName"].as_str().unwrap_or("").replace("gpt","GPT"),"description":m["description"],"isDefault":if configured{m["id"]==config["model"]}else{m["isDefault"]==true},"thinkingOptions":thinking,"defaultThinkingOptionId":effort,"metadata":{"model":m["model"],"defaultReasoningEffort":m["defaultReasoningEffort"],"supportedReasoningEfforts":m["supportedReasoningEfforts"]}})
        }).collect();
        Ok(
            json!({"models":models,"modes":modes(auto),"defaultModeId":if auto{"auto-review"}else{"auto"}}),
        )
    }
    async fn list_features(&self, config: &Value) -> Result<Vec<Value>> {
        let c = self.client(CancellationToken::new()).await?;
        let result = metadata(&c, config["cwd"].clone()).await;
        let close = c.close().await;
        let (models, _, collab, _) = result?;
        close?;
        Ok(session::features(config, &models, &collab))
    }
    async fn list_commands(&self, config: &Value) -> Result<Vec<Value>> {
        let c = self.client(CancellationToken::new()).await?;
        let result = metadata(&c, config["cwd"].clone()).await;
        let close = c.close().await;
        let (_, _, _, skills) = result?;
        close?;
        let goals = probe(&self.spec, CancellationToken::new())
            .await
            .ok()
            .and_then(|v| version(&v))
            .is_some_and(|v| v >= (0, 128, 0));
        session::command_list(
            &home(&self.spec, &BTreeMap::new()),
            config["cwd"].as_str().unwrap_or("."),
            skills.as_deref(),
            goals,
        )
        .await
    }
    async fn list_importable_sessions(&self, options: Value) -> Result<Vec<Value>> {
        let c = self.client(CancellationToken::new()).await?;
        let result:Result<Vec<Value>>=async{let limit=options["limit"].as_u64().unwrap_or(20) as usize;let mut window=options["scanLimit"].as_u64().unwrap_or(limit as u64).min(500) as usize;if options["cwd"].is_string(){window=window.max(50);}
            let mut rows=Vec::new();let mut seen=HashSet::new();let mut cursor=Value::Null;
            while rows.len()<window {let mut p=json!({"limit":window-rows.len(),"sortKey":"updated_at"});if options["cwd"].is_string(){p["cwd"]=options["cwd"].clone();}if !cursor.is_null(){p["cursor"]=cursor;}if let Some(custom)=custom_config(&self.spec){p["modelProviders"]=json!([custom["model_provider"]]);}
                let reply=c.request("thread/list",p).await?;let before=rows.len();for row in reply["data"].as_array().into_iter().flatten(){if let Some(id)=row["id"].as_str(){if seen.insert(id.to_owned()){rows.push(row.clone());}}}cursor=reply["nextCursor"].clone();if cursor.is_null()||rows.len()==before{break}
            }
            let cwd=options["cwd"].as_str().map(|p|std::fs::canonicalize(p).unwrap_or_else(|_|PathBuf::from(p)));
            Ok(rows.into_iter().filter(|r|cwd.as_ref().is_none_or(|cwd|r["cwd"].as_str().is_some_and(|p|std::fs::canonicalize(p).unwrap_or_else(|_|PathBuf::from(p))==*cwd))).take(limit).map(|r|{let secs=r["updatedAt"].as_i64().or(r["createdAt"].as_i64()).unwrap_or(0);let timestamp=chrono::DateTime::from_timestamp(secs,0).map(|d|d.to_rfc3339());json!({"providerHandleId":r["id"],"cwd":r["cwd"],"title":r["name"].as_str().filter(|s|!s.trim().is_empty()).map(Value::from).unwrap_or_else(||r["preview"].clone()),"firstPromptPreview":r["preview"],"lastPromptPreview":r["preview"],"lastActivityAt":timestamp})}).collect())}.await;
        let close = c.close().await;
        let result = result?;
        close?;
        Ok(result)
    }
    async fn import_session(
        &self,
        input: Value,
        mut config: Value,
        mut stored_config: Value,
        context: LaunchContext,
        sink: EventSink,
    ) -> Result<ImportedSession> {
        let id = input["providerHandleId"]
            .as_str()
            .or(input["sessionId"].as_str())
            .ok_or_else(|| anyhow!("Missing Codex import thread ID"))?;
        config["provider"] = json!(self.spec.id);
        config["cwd"] = input["cwd"].clone();
        stored_config["provider"] = json!(self.spec.id);
        stored_config["cwd"] = input["cwd"].clone();
        let persistence = json!({"provider":self.spec.id,"sessionId":id,"nativeHandle":id,"metadata":stored_config});
        let session = CodexSession::open(
            self.spec.clone(),
            config,
            context,
            json!({}),
            Some(persistence.clone()),
            sink,
        )
        .await?;
        self.sessions.lock().unwrap().push(Arc::downgrade(&session));
        let mut timeline = Vec::new();
        let mut provider_subagent_events = Vec::new();
        let history = std::mem::take(&mut session.state.lock().unwrap().history);
        for event in history {
            if event["type"] == "provider_subagent" {
                provider_subagent_events.push(event);
            } else if event["type"] == "timeline" {
                let mut entry = json!({"item":event["item"]});
                if let Some(timestamp) = event.get("timestamp") {
                    entry["timestamp"] = timestamp.clone();
                }
                timeline.push(entry);
            }
        }
        Ok(ImportedSession {
            session,
            config: stored_config,
            persistence,
            timeline,
            provider_subagent_events,
        })
    }
    async fn is_available(&self, _options: Value, cancel: CancellationToken) -> Result<bool> {
        Ok(probe(&self.spec, cancel).await.is_ok())
    }
    async fn diagnostic(&self) -> Result<String> {
        match probe(&self.spec, CancellationToken::new()).await {
            Ok(v) => Ok(format!(
                "Codex\nCommand: {}\nVersion: {v}\nCODEX_HOME: {}",
                self.spec.command.join(" "),
                home(&self.spec, &BTreeMap::new()).display()
            )),
            Err(e) => Ok(format!("Codex unavailable: {e}")),
        }
    }
    async fn archive_native_session(&self, persistence: &Value) -> Result<()> {
        self.archive(persistence, false).await
    }
    async fn unarchive_native_session(&self, persistence: &Value) -> Result<()> {
        self.archive(persistence, true).await
    }
    async fn shutdown(&self) -> Result<()> {
        let sessions: Vec<_> = self
            .sessions
            .lock()
            .unwrap()
            .iter()
            .filter_map(Weak::upgrade)
            .collect();
        let mut first = None;
        for s in sessions {
            if let Err(e) = s.close().await {
                if first.is_none() {
                    first = Some(e);
                }
            }
        }
        if let Some(e) = first { Err(e) } else { Ok(()) }
    }
}
