use super::{
    commands,
    permissions::{self, PendingPermission},
    projection,
    transport::{RpcError, SessionTransport, Transport},
};
use crate::providers::api::*;
use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{Mutex as AsyncMutex, Notify};

pub(super) struct State {
    pub config: Value,
    pub thread: Option<String>,
    pub native_turn: Option<String>,
    pub foreground: Option<String>,
    pub client_message: Option<String>,
    pub next_turn: u64,
    pub starting: bool,
    pub cancel_start: bool,
    pub closed: bool,
    pub interactive: bool,
    pub pending: HashMap<String, PendingPermission>,
    pub usage: Option<Value>,
    pub history: Vec<Value>,
    pub async_questions: permissions::AsyncQuestions,
    pub features: Arc<[Value]>,
    pub text: HashMap<String, String>,
    pub outputs: HashMap<String, String>,
    pub completed: HashSet<String>,
    pub children: HashMap<String, Value>,
    pub orphan_events: HashMap<String, VecDeque<Value>>,
    pub child_tools: HashMap<String, Value>,
    pub child_items: HashMap<String, Vec<(String, Value)>>,
    pub terminal_commands: HashMap<String, String>,
    pub terminal_pending: HashMap<String, Vec<Value>>,
    pub terminal_seen: HashSet<String>,
    pub terminal_ordinal: u64,
    pub last_plan: Option<String>,
    pub assistant_boundary: bool,
    pub manual_compactions: usize,
    pub compactions: HashSet<String>,
    pub completed_compactions: i64,
    pub user_turns: Vec<(String, Option<String>)>,
    pub sandbox: Value,
}
struct Metadata {
    models: Vec<Value>,
    collaboration: Vec<Value>,
    resolved_config: Value,
}
pub(super) struct CodexSession {
    pub spec: ProviderSpec,
    pub client: Arc<SessionTransport>,
    pub sink: EventSink,
    pub state: Mutex<State>,
    self_weak: std::sync::Weak<Self>,
    reconnect_gate: AsyncMutex<()>,
    pub gate: AsyncMutex<()>,
    pub changed: Notify,
    pub skills: Mutex<Option<Vec<Value>>>,
    metadata: Mutex<Arc<Metadata>>,
    pub codex_home: PathBuf,
    pub inner: Value,
    pub auto_review: bool,
    pub goals: bool,
    pub rollback: bool,
    pub caps: Value,
    pub usage_key: String,
    pub environment: std::collections::BTreeMap<String, String>,
}
pub(super) fn tiers<'a>(config: &Value, models: &'a [Value]) -> &'a [Value] {
    let selected = config["model"]
        .as_str()
        .and_then(|id| models.iter().find(|m| m["id"] == id || m["model"] == id))
        .or_else(|| models.iter().find(|m| m["isDefault"] == true))
        .or(models.first());
    selected
        .and_then(|m| m["serviceTiers"].as_array())
        .map(Vec::as_slice)
        .unwrap_or(&[])
}
pub(super) fn features(config: &Value, models: &[Value], collaboration: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    let tiers = tiers(config, models);
    if !tiers.is_empty() {
        let mut options = vec![json!({"id":"default","label":"Normal","isDefault":true})];
        options.extend(
            tiers
                .iter()
                .map(|t| json!({"id":t["id"],"label":t["name"]})),
        );
        out.push(json!({"type":"select","id":"service_tier","label":"Speed","description":"Choose processing speed. Faster tiers increase usage.","tooltip":"Select speed","icon":"zap","desktopTrigger":"icon","value":config["featureValues"]["service_tier"].as_str().unwrap_or("default"),"options":options}));
    }
    if collaboration.iter().any(is_plan) {
        out.push(json!({"type":"toggle","id":"plan_mode","label":"Plan","description":"Switch Codex into planning-only collaboration mode","tooltip":"Toggle plan mode","icon":"list-todo","value":config["featureValues"]["plan_mode"]==true}));
    }
    out
}
fn is_plan(v: &Value) -> bool {
    let n = v["name"].as_str().unwrap_or("").to_lowercase();
    n.contains("plan") || n.contains("read")
}
fn normalize_config(config: &mut Value, models: &[Value], resolved: &Value) {
    if config["model"].as_str().is_none_or(|s| s.trim().is_empty()) {
        config["model"] = resolved["model"].clone();
    }
    if config["model"].is_null() {
        if let Some(model) = models
            .iter()
            .find(|m| m["isDefault"] == true)
            .or(models.first())
        {
            config["model"] = model["id"].clone();
        }
    }
    if config["thinkingOptionId"]
        .as_str()
        .is_none_or(|s| s.trim().is_empty())
    {
        config["thinkingOptionId"] = resolved["model_reasoning_effort"].clone();
    }
    if config["thinkingOptionId"].is_null() {
        if let Some(model) = models.iter().find(|m| m["id"] == config["model"]) {
            config["thinkingOptionId"] = model["defaultReasoningEffort"].clone();
        }
    }
    reconcile_tier(config, models);
}
fn reconcile_tier(config: &mut Value, models: &[Value]) {
    let advertised = tiers(config, models);
    let mut tier = config["featureValues"]["service_tier"]
        .as_str()
        .unwrap_or(if config["featureValues"]["fast_mode"] == true {
            "fast"
        } else {
            "default"
        })
        .to_owned();
    if tier == "fast" && !advertised.iter().any(|t| t["id"] == tier) {
        tier = advertised
            .iter()
            .find(|t| {
                t["name"]
                    .as_str()
                    .is_some_and(|s| s.eq_ignore_ascii_case("fast"))
            })
            .and_then(|t| t["id"].as_str())
            .unwrap_or("default")
            .into();
    }
    if tier != "default" && !advertised.iter().any(|t| t["id"] == tier) {
        tier = "default".into();
    }
    if !config["featureValues"].is_object() {
        config["featureValues"] = json!({});
    }
    config["featureValues"]["service_tier"] = json!(tier);
    config["featureValues"]
        .as_object_mut()
        .unwrap()
        .remove("fast_mode");
}
pub(super) async fn command_list(
    home: &Path,
    cwd: &str,
    skills: Option<&[Value]>,
    goals: bool,
) -> Result<Vec<Value>> {
    let mut list = vec![
        json!({"name":"compact","description":"Summarize conversation to prevent hitting the context limit","argumentHint":"","kind":"command"}),
    ];
    if goals {
        list.push(json!({"name":"goal","description":"Set, pause, resume, or clear the agent's goal","argumentHint":"[<objective>|pause|resume|clear]","kind":"command"}));
    }
    if let Some(skills) = skills {
        list.extend(skills.iter().map(|s|json!({"name":s["name"],"description":s["description"],"argumentHint":"","kind":"skill"})));
    } else {
        list.extend(commands::fallback_skills(home, Path::new(cwd)).await?);
    }
    list.extend(commands::custom_prompts(home).await?);
    list.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    Ok(list)
}
fn inner_config(spec: &ProviderSpec, config: &Value, context: &LaunchContext) -> Result<Value> {
    let mut inner = config["providerOptions"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    if let Some(custom) = super::custom_config(spec) {
        inner.extend(custom.as_object().unwrap().clone());
    }
    let mut servers = config["mcpServers"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    if let Some(runtime) = context.mcp_servers.as_object() {
        servers.extend(runtime.clone());
    }
    let mut converted = serde_json::Map::new();
    for (name, s) in servers {
        let v = match s["type"].as_str() {
            Some("stdio") => json!({"command":s["command"],"args":s["args"],"env":s["env"]}),
            Some("http" | "sse") => json!({"url":s["url"],"http_headers":s["headers"]}),
            other => bail!("Unsupported Codex MCP transport: {other:?}"),
        };
        converted.insert(name, v);
    }
    let mut grants: HashMap<String, Vec<Value>> = HashMap::new();
    for g in config["toolPolicy"]["preapproved"]
        .as_array()
        .into_iter()
        .flatten()
    {
        if let (Some(server), Some(tool)) = (g["server"].as_str(), g["tool"].as_str()) {
            grants.entry(server.into()).or_default().push(json!(tool));
        }
    }
    for (server, tools) in grants {
        let target = converted.entry(server).or_insert_with(|| json!({}));
        let approvals: serde_json::Map<String, Value> = tools
            .iter()
            .filter_map(|t| Some((t.as_str()?.into(), json!({"approval_mode":"approve"}))))
            .collect();
        target["enabled_tools"] = json!(tools);
        target["default_tools_approval_mode"] = json!("prompt");
        target["tools"] = Value::Object(approvals);
    }
    if !converted.is_empty() {
        inner.insert("mcp_servers".into(), Value::Object(converted));
    }
    Ok(Value::Object(inner))
}
impl CodexSession {
    pub async fn open(
        spec: ProviderSpec,
        mut config: Value,
        context: LaunchContext,
        options: Value,
        persistence: Option<Value>,
        sink: EventSink,
    ) -> Result<Arc<Self>> {
        if !config.is_object() {
            bail!("Codex session config must be an object");
        }
        let mut provider_options = spec.options.as_object().cloned().unwrap_or_default();
        if let Some(overrides) = config["providerOptions"].as_object() {
            provider_options.extend(overrides.clone());
        }
        config["providerOptions"] = Value::Object(provider_options);
        if let Some(mode) = config["modeId"].as_str() {
            validate_mode(mode)?;
        }
        let native = persistence
            .as_ref()
            .and_then(|p| p["nativeHandle"].as_str().or(p["sessionId"].as_str()))
            .map(str::to_owned);
        let history_only = options["purpose"] == "history";
        let v = super::probe(&spec, context.cancel.clone())
            .await
            .ok()
            .and_then(|s| super::version(&s));
        let goals = v.is_some_and(|v| v >= (0, 128, 0));
        let auto_review = v.is_some_and(|v| v >= (0, 115, 0));
        let args = if goals {
            vec!["app-server", "--enable", "goals"]
        } else {
            vec!["app-server"]
        };
        let inner = inner_config(&spec, &config, &context)?;
        let (client, receiver) = Transport::spawn(
            super::command(&spec, &args, &context.env, None)?,
            context.cancel.clone(),
        )
        .await?;
        let prepared = async {
            let initialized = client.initialize().await?;
            let rollback = !initialized["userAgent"]
                .as_str()
                .and_then(super::version)
                .is_some_and(|v| v >= (0, 156, 0));
            let (models, resolved, collaboration, skills) = if history_only {
                (Vec::new(), json!({}), Vec::new(), None)
            } else {
                super::metadata(&client, config["cwd"].clone()).await?
            };
            if !history_only {
                normalize_config(&mut config, &models, &resolved);
            }
            let sandbox = Value::Null;
            let history = if let Some(id) = &native {
                load_history(&client, id, config["cwd"].as_str(), &spec.id).await?
            } else {
                Vec::new()
            };
            Ok((
                rollback,
                models,
                resolved,
                collaboration,
                skills,
                sandbox,
                history,
            ))
        }
        .await;
        let (rollback, models, resolved_config, collaboration, skills, sandbox, history) =
            match prepared {
                Ok(v) => v,
                Err(e) => {
                    let _ = client.close().await;
                    return Err(e);
                }
            };
        let mut history = history;
        let async_questions = permissions::AsyncQuestions::new(&config["asyncQuestions"]);
        for entry in &mut history {
            if entry["item"]["name"] == "request_user_input_async" {
                if let Some(item) = entry["item"]["callId"]
                    .as_str()
                    .and_then(|id| async_questions.timeline(id))
                {
                    entry["item"] = item;
                }
            }
        }
        let initial_features: Arc<[Value]> = features(&config, &models, &collaboration).into();
        let user_turns = history
            .iter()
            .filter(|e| e["type"] == "timeline" && e["item"]["type"] == "user_message")
            .filter_map(|e| {
                Some((
                    e["item"]["messageId"].as_str()?.to_owned(),
                    e["turnId"].as_str().map(str::to_owned),
                ))
            })
            .collect();
        let mut children = HashMap::new();
        let mut child_items: HashMap<String, Vec<(String, Value)>> = HashMap::new();
        for (index, event) in history.iter().enumerate() {
            if event["type"] != "provider_subagent" {
                continue;
            }
            let update = &event["event"];
            let Some(id) = update["id"].as_str() else {
                continue;
            };
            if update["type"] == "upsert" {
                children.insert(id.to_owned(), update.clone());
            } else if update["type"] == "timeline" {
                let item = &update["item"];
                let key = item["messageId"]
                    .as_str()
                    .or(item["callId"].as_str())
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("history-{index}"));
                child_items
                    .entry(id.to_owned())
                    .or_default()
                    .push((key, item.clone()));
            }
        }
        let child_tools = children
            .iter()
            .filter_map(|(id, child)| {
                history.iter().find_map(|event| {
                    let item = if event["type"] == "provider_subagent" {
                        &event["event"]["item"]
                    } else {
                        &event["item"]
                    };
                    (item["type"] == "tool_call" && item["callId"] == child["toolCallId"])
                        .then(|| (id.clone(), item.clone()))
                })
            })
            .collect();
        let codex_home = super::home(&spec, &context.env);
        let mut environment = spec.env.clone();
        environment.extend(context.env);
        let session = Arc::new_cyclic(|weak| Self {
            spec,
            client: SessionTransport::new(client.clone(), context.cancel.clone()),
            sink,
            self_weak: weak.clone(),
            reconnect_gate: AsyncMutex::new(()),
            state: Mutex::new(State {
                config,
                thread: native,
                native_turn: None,
                foreground: None,
                client_message: None,
                next_turn: 0,
                starting: false,
                cancel_start: false,
                closed: false,
                interactive: !history_only,
                pending: HashMap::new(),
                async_questions,
                features: initial_features,
                usage: None,
                history,
                text: HashMap::new(),
                outputs: HashMap::new(),
                completed: HashSet::new(),
                children,
                orphan_events: HashMap::new(),
                last_plan: None,
                child_tools,
                child_items,
                terminal_commands: HashMap::new(),
                terminal_pending: HashMap::new(),
                terminal_seen: HashSet::new(),
                terminal_ordinal: 0,
                assistant_boundary: false,
                manual_compactions: 0,
                compactions: HashSet::new(),
                completed_compactions: 0,
                user_turns,
                sandbox,
            }),
            gate: AsyncMutex::new(()),
            changed: Notify::new(),
            metadata: Mutex::new(Arc::new(Metadata {
                models,
                collaboration,
                resolved_config,
            })),
            skills: Mutex::new(skills),
            codex_home,
            inner,
            auto_review,
            goals,
            rollback,
            caps: super::capabilities(),
            usage_key: uuid::Uuid::new_v4().to_string(),
            environment,
        });
        session.attach_reader(client.clone(), receiver);
        if history_only {
            client.close().await?;
        } else {
            let (thread, config) = {
                let state = session.state.lock().unwrap();
                (state.thread.clone(), state.config.clone())
            };
            if let Some(thread) = thread {
                match resume(&client, &thread, &config, &session.inner).await {
                    Ok(response) => {
                        session.state.lock().unwrap().sandbox = response["sandbox"].clone()
                    }
                    Err(error) => {
                        let _ = session.close().await;
                        return Err(error);
                    }
                }
            }
        }
        Ok(session)
    }
    fn attach_reader(
        &self,
        transport: Arc<Transport>,
        mut receiver: tokio::sync::mpsc::Receiver<Value>,
    ) {
        let weak = self.self_weak.clone();
        tokio::spawn(async move {
            while let Some(message) = receiver.recv().await {
                let Some(session) = weak.upgrade() else { break };
                if !session.client.is_current(&transport) {
                    break;
                }
                if let Err(error) = session.handle_frame(message).await {
                    let turn = {
                        let mut state = session.state.lock().unwrap();
                        state.native_turn = None;
                        state.foreground.take()
                    };
                    if let Some(turn) = turn {
                        let _=session.emit(json!({"type":"turn_failed","turnId":turn,"error":error.to_string()})).await;
                    }
                    let _ = transport.close().await;
                    break;
                }
            }
        });
    }
    async fn ensure_connected(&self) -> Result<()> {
        if self.state.lock().unwrap().closed {
            bail!("Codex session is closed")
        }
        if !self.client.is_closed() {
            return Ok(());
        }
        let _connection = self.reconnect_gate.lock().await;
        if !self.client.is_closed() {
            return Ok(());
        }
        let (config, thread, interactive) = {
            let state = self.state.lock().unwrap();
            if state.closed {
                bail!("Codex session is closed")
            }
            (
                state.config.clone(),
                state.thread.clone(),
                state.interactive,
            )
        };
        if !interactive {
            bail!("A history-only Codex session cannot reconnect for interaction")
        }
        self.handle_frame(json!({"method":"paseo/transportClosed","params":{
            "message":format!("Codex app-server disconnected: {}",self.client.current().process.stderr_tail())
        }})).await?;
        if self.client.cancel.is_cancelled() {
            bail!("Codex session is closed")
        }
        let args = if self.goals {
            vec!["app-server", "--enable", "goals"]
        } else {
            vec!["app-server"]
        };
        let (transport, receiver) = Transport::spawn(
            super::command(&self.spec, &args, &self.environment, None)?,
            self.client.cancel.clone(),
        )
        .await?;
        if self.state.lock().unwrap().closed || self.client.cancel.is_cancelled() {
            transport.close().await?;
            bail!("Codex session closed while reconnecting");
        }
        self.client.replace(transport.clone());
        self.attach_reader(transport.clone(), receiver);
        let result: Result<()> = async {
            transport.initialize().await?;
            let (models, resolved_config, collaboration, skills) =
                super::metadata(&transport, config["cwd"].clone()).await?;
            let metadata = Arc::new(Metadata {
                models,
                resolved_config,
                collaboration,
            });
            *self.metadata.lock().unwrap() = metadata.clone();
            *self.skills.lock().unwrap() = skills;
            {
                let mut state = self.state.lock().unwrap();
                normalize_config(
                    &mut state.config,
                    &metadata.models,
                    &metadata.resolved_config,
                );
                state.features =
                    features(&state.config, &metadata.models, &metadata.collaboration).into();
            }
            if let Some(thread) = thread {
                let response = resume(&transport, &thread, &config, &self.inner).await?;
                let history =
                    load_history(&transport, &thread, config["cwd"].as_str(), &self.spec.id)
                        .await?;
                let mut state = self.state.lock().unwrap();
                state.sandbox = response["sandbox"].clone();
                state.user_turns = history
                    .iter()
                    .filter(|event| {
                        event["type"] == "timeline" && event["item"]["type"] == "user_message"
                    })
                    .filter_map(|event| {
                        Some((
                            event["item"]["messageId"].as_str()?.to_owned(),
                            event["turnId"].as_str().map(str::to_owned),
                        ))
                    })
                    .collect();
                state.history = history;
            }
            Ok(())
        }
        .await;
        if let Err(error) = result {
            let _ = transport.close().await;
            return Err(error);
        }
        if self.state.lock().unwrap().closed {
            transport.close().await?;
            bail!("Codex session closed while reconnecting")
        }
        Ok(())
    }
    pub(super) async fn emit(&self, mut event: Value) -> Result<()> {
        event["provider"] = json!(self.spec.id);
        if event.get("turnId").is_none() {
            if let Some(turn) = self.state.lock().unwrap().foreground.clone() {
                event["turnId"] = json!(turn);
            }
        }
        self.sink.emit(event).await
    }
    async fn ensure_thread(&self) -> Result<String> {
        self.ensure_connected().await?;
        let _gate = self.gate.lock().await;
        let (config, thread, interactive, closed) = {
            let s = self.state.lock().unwrap();
            (s.config.clone(), s.thread.clone(), s.interactive, s.closed)
        };
        if closed {
            bail!("Codex session is closed")
        };
        if !interactive {
            bail!("A history-only Codex session cannot execute turns; resume it for interaction")
        };
        if let Some(id) = thread {
            return Ok(id);
        }
        if !config["model"].is_string() {
            bail!("No models available from Codex app-server")
        }
        let metadata = self.metadata.lock().unwrap().clone();
        let mut params = json!({"model":config["model"],"cwd":config["cwd"],"config":self.inner});
        apply_workflow(
            &mut params,
            &config,
            &metadata.resolved_config,
            &Value::Null,
            false,
        );
        instructions(&mut params, &config);
        let response = self.client.request("thread/start", params).await?;
        let id = response["thread"]["id"]
            .as_str()
            .ok_or_else(|| anyhow!("Codex thread/start did not return thread ID"))?
            .to_owned();
        {
            let mut s = self.state.lock().unwrap();
            s.thread = Some(id.clone());
            s.sandbox = response["sandbox"].clone();
            let mode = s.config["modeId"].as_str();
            let approval = s.config["providerOptions"]["approval_policy"]
                .as_str()
                .or(mode.map(|mode| {
                    if mode == "full-access" {
                        "never"
                    } else {
                        "on-request"
                    }
                }));
            let sandbox = s.config["providerOptions"]["sandbox_mode"]
                .as_str()
                .or(mode.map(|mode| match mode {
                    "full-access" => "danger-full-access",
                    "read-only" => "read-only",
                    _ => "workspace-write",
                }));
            if matches!(
                response["approvalsReviewer"].as_str(),
                Some("auto_review" | "guardian_subagent")
            ) && approval == Some("on-request")
                && sandbox == Some("workspace-write")
            {
                s.config["modeId"] = json!("auto-review");
            }
        }
        Ok(id)
    }
    async fn input(&self, prompt: Value) -> Result<Value> {
        let mut invocation = prompt;
        if let Some(text) = invocation.as_str().and_then(|p| p.trim().strip_prefix('/')) {
            let (name, args) = text
                .split_once(char::is_whitespace)
                .map(|(name, args)| (name, args.trim()))
                .unwrap_or((text, ""));
            if !name.is_empty() && !name.contains('/') {
                match self.list_commands().await {
                    Ok(commands) if commands.iter().any(|c| c["name"] == name) => {
                        invocation = json!({"commandName":name,"args":args});
                    }
                    Err(error) => tracing::warn!(%error,"Unable to resolve Codex slash command"),
                    _ => {}
                }
            }
        }
        let skills = self.skills.lock().unwrap().clone().unwrap_or_default();
        let expanded = commands::expand(&self.codex_home, &invocation, &skills).await?;
        super::input::turn_input(expanded).await
    }
    fn collaboration_mode(&self, config: &Value) -> Option<Value> {
        let metadata = self.metadata.lock().unwrap().clone();
        let plan = config["featureValues"]["plan_mode"] == true;
        let selected = if plan {
            metadata.collaboration.iter().find(|c| is_plan(c))
        } else {
            metadata.collaboration.iter().find(|c| !is_plan(c))
        }?;
        let mut settings = json!({});
        for key in ["model", "reasoning_effort", "developer_instructions"] {
            if !selected[key].is_null() {
                settings[key] = selected[key].clone();
            }
        }
        if !config["model"].is_null() {
            settings["model"] = config["model"].clone();
        }
        if !config["thinkingOptionId"].is_null() {
            settings["reasoning_effort"] = config["thinkingOptionId"].clone();
        }
        let text = [
            selected["developer_instructions"].as_str(),
            config["systemPrompt"].as_str(),
            config["daemonAppendSystemPrompt"].as_str(),
        ]
        .into_iter()
        .flatten()
        .filter(|s| !s.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
        if !text.is_empty() {
            settings["developer_instructions"] = json!(text);
        }
        Some(json!({"mode":selected["mode"].as_str().unwrap_or("code"),"settings":settings}))
    }
}
fn validate_mode(mode: &str) -> Result<()> {
    if !["auto", "auto-review", "full-access", "read-only"].contains(&mode) {
        bail!("Unknown Codex mode: {mode}")
    }
    Ok(())
}
fn instructions(params: &mut Value, config: &Value) {
    let text = [
        config["systemPrompt"].as_str(),
        config["daemonAppendSystemPrompt"].as_str(),
    ]
    .into_iter()
    .flatten()
    .filter(|s| !s.trim().is_empty())
    .collect::<Vec<_>>()
    .join("\n\n");
    if !text.is_empty() {
        params["developerInstructions"] = json!(text);
    }
}
fn apply_workflow(
    params: &mut Value,
    config: &Value,
    resolved: &Value,
    sandbox: &Value,
    turn: bool,
) {
    let mode = config["modeId"].as_str();
    let full = mode == Some("full-access");
    let native = if full {
        "danger-full-access"
    } else if mode == Some("read-only") {
        "read-only"
    } else {
        "workspace-write"
    };
    if mode.is_some() {
        if config["providerOptions"]["approval_policy"].is_null() {
            params["approvalPolicy"] = json!(if full { "never" } else { "on-request" });
        }
        params["approvalsReviewer"] = json!(if mode == Some("auto-review") {
            "auto_review"
        } else {
            "user"
        });
    }
    let sandbox_type = config["providerOptions"]["sandbox_mode"]
        .as_str()
        .or(mode.map(|_| native));
    if let Some(kind) = sandbox_type {
        if !turn {
            if config["providerOptions"]["sandbox_mode"].is_null() {
                params["sandbox"] = json!(kind);
            }
        } else {
            let kind = match kind {
                "read-only" => "readOnly",
                "danger-full-access" => "dangerFullAccess",
                _ => "workspaceWrite",
            };
            if sandbox["type"] == kind {
                params["sandboxPolicy"] = sandbox.clone();
            } else {
                let mut policy = json!({"type":kind});
                if kind == "workspaceWrite" {
                    for (snake, camel, fallback) in [
                        ("writable_roots", "writableRoots", json!([])),
                        ("network_access", "networkAccess", json!(false)),
                        ("exclude_slash_tmp", "excludeSlashTmp", json!(false)),
                        (
                            "exclude_tmpdir_env_var",
                            "excludeTmpdirEnvVar",
                            json!(false),
                        ),
                    ] {
                        policy[camel] = config["providerOptions"]["sandbox_workspace_write"]
                            .get(snake)
                            .or_else(|| resolved["sandbox_workspace_write"].get(snake))
                            .cloned()
                            .unwrap_or(fallback);
                    }
                }
                params["sandboxPolicy"] = policy;
            }
        }
    }
}
async fn resume(client: &Transport, id: &str, config: &Value, inner: &Value) -> Result<Value> {
    let loaded = client.request("thread/loaded/list", json!({})).await?;
    if loaded["data"]
        .as_array()
        .is_some_and(|ids| ids.iter().any(|v| v == id))
    {
        return Ok(json!({}));
    }
    let mut p = json!({"threadId":id,"config":inner});
    instructions(&mut p, config);
    match client.request("thread/resume", p.clone()).await {
        Ok(value) => Ok(value),
        Err(error)
            if error.to_string()
                == format!(
                    "session {id} is archived. Run `codex unarchive {id}` to unarchive it first."
                ) =>
        {
            match client
                .request("thread/unarchive", json!({"threadId":id}))
                .await
            {
                Ok(_) => {}
                Err(error)
                    if error
                        .to_string()
                        .contains(&format!("no archived rollout found for thread id {id}")) => {}
                Err(error) => return Err(error),
            }
            client.request("thread/resume", p).await
        }
        Err(error) => Err(error),
    }
}
async fn load_history(
    client: &Transport,
    id: &str,
    cwd: Option<&str>,
    provider: &str,
) -> Result<Vec<Value>> {
    let root = client
        .request("thread/read", json!({"threadId":id,"includeTurns":true}))
        .await?;
    if !root["thread"].is_object() {
        bail!("Codex thread/read returned no thread")
    }
    let mut materialize = |data: &str, mime: Option<&str>| {
        crate::providers::prompt::materialize_image(data, mime)
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
    };
    let mut history = projection::history_with_images(&root["thread"], cwd, &mut materialize);
    let mut queue = VecDeque::new();
    let mut visited = HashSet::from([id.to_owned()]);
    for turn in root["thread"]["turns"].as_array().into_iter().flatten() {
        for item in turn["items"].as_array().into_iter().flatten() {
            for child in projection::child_threads(item) {
                queue.push_back((child, item.clone(), None::<String>));
            }
        }
    }
    let mut children = Vec::new();
    while !queue.is_empty() && visited.len() < 100 {
        let (child, item, parent) = queue.pop_front().unwrap();
        if !visited.insert(child.clone()) {
            continue;
        }
        let Some(tool) = projection::item(&item, false, cwd) else {
            continue;
        };
        children.push(json!({"type":"provider_subagent","event":{
            "type":"upsert","id":child,
            "title":tool["detail"]["subAgentType"],
            "description":tool["detail"]["description"],
            "status":if tool["status"]=="canceled"{"canceled"}else{"completed"},
            "toolCallId":tool["callId"],"parentSubagentId":parent}}));
        match client
            .request("thread/read", json!({"threadId":child,"includeTurns":true}))
            .await
        {
            Ok(reply) => {
                for entry in
                    projection::history_with_images(&reply["thread"], cwd, &mut materialize)
                {
                    children.push(json!({"type":"provider_subagent","event":{
                        "type":"timeline","id":child,"item":entry["item"],"timestamp":entry["timestamp"]
                    }}));
                }
                for turn in reply["thread"]["turns"].as_array().into_iter().flatten() {
                    for item in turn["items"].as_array().into_iter().flatten() {
                        for id in projection::child_threads(item) {
                            queue.push_back((id, item.clone(), Some(child.clone())));
                        }
                    }
                }
            }
            Err(error) => {
                tracing::warn!(%error,thread_id=%child,"Unable to read Codex subagent history")
            }
        }
    }
    children.append(&mut history);
    for event in &mut children {
        event["provider"] = json!(provider);
    }
    Ok(children)
}
#[async_trait]
impl AgentSession for CodexSession {
    fn provider(&self) -> &str {
        &self.spec.id
    }
    fn native_id(&self) -> Option<String> {
        self.state.lock().unwrap().thread.clone()
    }
    fn capabilities(&self) -> &Value {
        &self.caps
    }
    fn features(&self) -> Arc<[Value]> {
        self.state.lock().unwrap().features.clone()
    }
    fn initial_timeline(&self) -> &[Value] {
        &[]
    }
    fn describe_persistence(&self) -> Option<Value> {
        let s = self.state.lock().unwrap();
        let id = s.thread.as_ref()?;
        let mut metadata = s.config.as_object()?.clone();
        metadata.remove("daemonAppendSystemPrompt");
        metadata.insert("asyncQuestions".into(), s.async_questions.serialize());
        metadata.insert("threadId".into(), json!(id));
        metadata.insert("provider".into(), json!(self.spec.id));
        Some(json!({"provider":self.spec.id,"sessionId":id,"nativeHandle":id,"metadata":metadata}))
    }
    fn pending_permissions(&self) -> Vec<Value> {
        let s = self.state.lock().unwrap();
        let mut result: Vec<Value> = s.pending.values().map(|p| p.request.clone()).collect();
        result.extend(s.async_questions.pending(&self.spec.id));
        result
    }
    fn usage_session(&self) -> Option<Value> {
        let s = self.state.lock().unwrap();
        if s.closed {
            return None;
        }
        Some(
            json!({"provider":self.spec.id,"model":s.config["model"],"env":self.environment,"sessionKey":self.usage_key}),
        )
    }
    async fn start_turn(&self, prompt: Value, options: Value) -> Result<Value> {
        {
            let mut s = self.state.lock().unwrap();
            if s.foreground.is_some() || s.starting {
                bail!("A foreground turn is already active")
            }
            s.starting = true;
            s.cancel_start = false;
        }
        let result=async{
            let plans:Vec<_>=self.state.lock().unwrap().pending.iter().filter(|(_,p)|p.method=="plan").map(|(id,_)|id.clone()).collect();for id in plans{self.respond_to_permission(&id,json!({"behavior":"deny","message":"Dismissed by a new prompt"})).await?;}
            let thread=self.ensure_thread().await?;let input=self.input(prompt).await?;
            self.state.lock().unwrap().client_message=options["clientMessageId"].as_str().map(str::to_owned);
            let metadata=self.metadata.lock().unwrap().clone();
            let(mut params,turn)={
                let mut s=self.state.lock().unwrap();
                if s.cancel_start{bail!("Codex turn start was interrupted before reaching Codex")}
                let turn=format!("codex-turn-{}",s.next_turn);s.next_turn+=1;
                s.foreground=Some(turn.clone());s.native_turn=None;
                let mut p=json!({"threadId":thread,"input":input,
                    "serviceTier":s.config["featureValues"]["service_tier"].as_str().unwrap_or("default"),
                    "config":self.inner});
                for(src,dst)in[("model","model"),("thinkingOptionId","effort"),("cwd","cwd")]{
                    if !s.config[src].is_null(){p[dst]=s.config[src].clone();}
                }
                apply_workflow(&mut p,&s.config,&metadata.resolved_config,&s.sandbox,true);
                instructions(&mut p,&s.config);
                if let Some(collab)=self.collaboration_mode(&s.config){p["collaborationMode"]=collab;}
                (p,turn)
            };
            if !options["outputSchema"].is_null(){params["outputSchema"]=super::input::output_schema(&options["outputSchema"])?;}
            self.client.request_timeout("turn/start",params,Duration::from_secs(90)).await?;Ok(json!({"turnId":turn}))
        }.await;
        {
            let mut s = self.state.lock().unwrap();
            s.starting = false;
            if result.is_err() {
                s.foreground = None;
                s.native_turn = None;
            }
        }
        self.changed.notify_waiters();
        result
    }
    async fn steer_active_turn(&self, prompt: Value, options: Value) -> Result<Value> {
        let (thread, native, foreground) = {
            let s = self.state.lock().unwrap();
            match (&s.thread, &s.native_turn, &s.foreground) {
                (Some(t), Some(n), Some(f)) if options["expectedTurnId"] == *f => {
                    (t.clone(), n.clone(), f.clone())
                }
                _ => return Ok(json!({"status":"unavailable"})),
            }
        };
        if let Some(text) = prompt.as_str() {
            if text.trim_start().starts_with('/') {
                let name = text
                    .trim_start()
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .trim_start_matches('/');
                if self
                    .list_commands()
                    .await?
                    .iter()
                    .any(|c| c["name"] == name)
                {
                    return Ok(json!({"status":"unavailable"}));
                }
            }
        }
        let input = super::input::turn_input(prompt).await?;
        {
            let s = self.state.lock().unwrap();
            if s.thread.as_ref() != Some(&thread)
                || s.native_turn.as_ref() != Some(&native)
                || s.foreground.as_ref() != Some(&foreground)
            {
                return Ok(json!({"status":"unavailable"}));
            }
        }
        let mut p = json!({"threadId":thread,"expectedTurnId":native,"input":input});
        if options["clientMessageId"].is_string() {
            p["clientUserMessageId"] = options["clientMessageId"].clone();
        }
        let result = self
            .client
            .request_timeout("turn/steer", p, Duration::from_secs(90))
            .await;
        let reply = match result {
            Ok(value) => value,
            Err(error)
                if error
                    .downcast_ref::<RpcError>()
                    .is_some_and(RpcError::definitive_steer_rejection) =>
            {
                return Ok(json!({"status":"unavailable"}));
            }
            Err(error) => return Err(error),
        };
        if reply["turnId"].as_str().or(reply["turn"]["id"].as_str()) != Some(&native) {
            bail!("Codex returned an invalid steer acknowledgement")
        }
        if options["clearPendingPermissions"] == true {
            let ids: Vec<_> = self.state.lock().unwrap().pending.keys().cloned().collect();
            for id in ids {
                self.respond_to_permission(&id,json!({"behavior":"deny","message":"The user answered with a message instead of approving. Their message follows."})).await?;
            }
        }
        Ok(json!({"status":"accepted"}))
    }
    async fn stream_history(&self, sink: EventSink) -> Result<()> {
        let history = std::mem::take(&mut self.state.lock().unwrap().history);
        for e in history {
            sink.emit(e).await?;
        }
        Ok(())
    }
    async fn runtime_info(&self) -> Result<Value> {
        let interactive = self.state.lock().unwrap().interactive;
        if interactive {
            self.ensure_thread().await?;
        }
        let s = self.state.lock().unwrap();
        let mut info = json!({"provider":self.spec.id,"sessionId":s.thread,"model":s.config["model"],
            "thinkingOptionId":s.config["thinkingOptionId"],
            "modeId":s.config["modeId"].as_str().unwrap_or("auto"),
            "features":s.features
        });
        if let Some(mode) = self.collaboration_mode(&s.config) {
            info["extra"] = json!({"collaborationMode":mode["mode"]});
        }
        Ok(info)
    }
    async fn available_modes(&self) -> Result<Vec<Value>> {
        Ok(super::modes(self.auto_review))
    }
    async fn current_mode(&self) -> Result<Option<String>> {
        Ok(Some(
            self.state.lock().unwrap().config["modeId"]
                .as_str()
                .unwrap_or("auto")
                .into(),
        ))
    }
    async fn set_mode(&self, mode: &str) -> Result<Option<Value>> {
        validate_mode(mode)?;
        let mut s = self.state.lock().unwrap();
        s.config["modeId"] = json!(mode);
        Ok(next_turn_notice(s.foreground.is_some(), "mode"))
    }
    async fn respond_to_permission(&self, id: &str, response: Value) -> Result<Option<Value>> {
        let asynchronous = {
            let s = self.state.lock().unwrap();
            if s.async_questions.has_pending(id) {
                Some((
                    s.async_questions.prepare_response(id, &response)?,
                    s.foreground.clone(),
                ))
            } else {
                None
            }
        };
        if let Some((prepared, foreground)) = asynchronous {
            let mut follow_up = prepared.prompt.clone();
            if let (Some(prompt), Some(turn)) = (&prepared.prompt, foreground) {
                let admission = self
                    .steer_active_turn(
                        json!(prompt),
                        json!({
                            "expectedTurnId":turn,"clientMessageId":uuid::Uuid::new_v4().to_string()
                        }),
                    )
                    .await?;
                if admission["status"] != "accepted" {
                    bail!("The active Codex turn changed. Retry sending your answer.");
                }
                follow_up = None;
            }
            let item = self
                .state
                .lock()
                .unwrap()
                .async_questions
                .complete(prepared)?;
            self.emit(json!({"type":"timeline","item":item})).await?;
            self.emit(json!({"type":"permission_resolved","requestId":id,"resolution":response}))
                .await?;
            return Ok(follow_up.map(|prompt| json!({"followUpPrompt":prompt})));
        }
        let pending = {
            let s = self.state.lock().unwrap();
            s.pending
                .get(id)
                .cloned()
                .ok_or_else(|| anyhow!("No pending Codex permission request '{id}'"))?
        };
        let result = permissions::reply(&pending, &response)?;
        if let Some(native) = pending.native_id.clone() {
            self.client.reply(native, result.clone()).await?;
        }
        self.state.lock().unwrap().pending.remove(id);
        if pending.method == "plan" {
            let allow = response["behavior"] == "allow";
            let mut event = json!({"type":"timeline","item":permissions::plan_timeline(
                &pending,Some(if allow{"approved"}else{"rejected"})
            )});
            if let Some(turn) = pending.params["turnId"].as_str() {
                event["turnId"] = json!(turn);
            }
            self.emit(event).await?;
            if allow {
                let mut s = self.state.lock().unwrap();
                s.config["featureValues"]["plan_mode"] = json!(false);
                let metadata = self.metadata.lock().unwrap().clone();
                s.features = features(&s.config, &metadata.models, &metadata.collaboration).into();
            }
        } else if matches!(
            pending.method.as_str(),
            "item/tool/requestUserInput" | "tool/requestUserInput"
        ) {
            self.emit(json!({"type":"timeline","item":permissions::question_timeline(&pending,Some(&response))?})).await?;
        } else if response["behavior"] == "deny" && pending.request["kind"] == "tool" {
            let name = match pending.request["name"].as_str() {
                Some("CodexBash") => "shell",
                Some("CodexFileChange") => "apply_patch",
                Some(name) => name,
                None => "tool",
            };
            self.emit(json!({"type":"timeline","item":{
                "type":"tool_call","callId":id,"name":name,"status":"failed",
                "error":{"message":response["message"].as_str().unwrap_or("Permission denied")},
                "detail":pending.request.get("detail").cloned().unwrap_or_else(||json!({
                    "type":"unknown","input":pending.request["input"],"output":null
                })),
                "metadata":{"permissionRequestId":id,"denied":true}
            }}))
            .await?;
        }
        self.emit(json!({"type":"permission_resolved","requestId":id,"resolution":response}))
            .await?;
        Ok((pending.method == "plan" && response["behavior"] == "allow").then_some(result))
    }
    async fn interrupt(&self) -> Result<()> {
        loop {
            let notified = self.changed.notified();
            let ready = {
                let mut s = self.state.lock().unwrap();
                if s.starting {
                    s.cancel_start = true;
                    None
                } else if s.foreground.is_none() && s.native_turn.is_none() {
                    return Ok(());
                } else {
                    match (&s.thread, &s.native_turn) {
                        (Some(t), Some(n)) => Some((t.clone(), n.clone(), s.foreground.clone())),
                        _ => None,
                    }
                }
            };
            if let Some((thread, native, foreground)) = ready {
                let result = self
                    .client
                    .request_timeout(
                        "turn/interrupt",
                        json!({"threadId":thread,"turnId":native}),
                        Duration::from_secs(2),
                    )
                    .await;
                match result {
                    Ok(_) => return Ok(()),
                    Err(error)
                        if error
                            .downcast_ref::<RpcError>()
                            .is_some_and(RpcError::already_idle_interrupt) =>
                    {
                        let mut s = self.state.lock().unwrap();
                        if s.foreground == foreground {
                            s.foreground = None;
                            s.native_turn = None;
                        }
                        self.changed.notify_waiters();
                        return Ok(());
                    }
                    Err(error) => return Err(error),
                }
            }
            tokio::select! {_=notified=>{},_=self.client.cancel.cancelled()=>bail!("Codex disconnected before active turn identification")}
        }
    }
    async fn close(&self) -> Result<()> {
        let pending = {
            let mut s = self.state.lock().unwrap();
            if s.closed {
                return Ok(());
            }
            s.closed = true;
            s.foreground = None;
            s.native_turn = None;
            std::mem::take(&mut s.pending)
        };
        self.changed.notify_waiters();
        for (_, pending) in pending {
            if pending.method == "plan" {
                let mut event = json!({"type":"timeline","item":permissions::plan_timeline(&pending,Some("canceled"))});
                if let Some(turn) = pending.params["turnId"].as_str() {
                    event["turnId"] = json!(turn);
                }
                let _ = self.emit(event).await;
            }
            if let Some(id) = pending.native_id.clone() {
                let _ = self.client.reply(id, permissions::deny(&pending)).await;
            }
        }
        self.client.close().await
    }
    async fn list_commands(&self) -> Result<Vec<Value>> {
        let (cwd, interactive) = {
            let state = self.state.lock().unwrap();
            (
                state.config["cwd"].as_str().unwrap_or(".").to_owned(),
                state.interactive,
            )
        };
        if interactive {
            self.ensure_connected().await?;
        }
        let native = self
            .client
            .request("skills/list", json!({"cwds":[cwd]}))
            .await;
        let skills = match native {
            Ok(value) => Some(
                value["data"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .flat_map(|value| value["skills"].as_array().into_iter().flatten())
                    .filter(|skill| skill["enabled"] != false)
                    .cloned()
                    .collect::<Vec<_>>(),
            ),
            Err(_) => None,
        };
        *self.skills.lock().unwrap() = skills.clone();
        command_list(&self.codex_home, &cwd, skills.as_deref(), self.goals).await
    }
    async fn set_model(&self, model: Option<&str>) -> Result<()> {
        let mut s = self.state.lock().unwrap();
        s.config["model"] = json!(model);
        let metadata = self.metadata.lock().unwrap().clone();
        normalize_config(&mut s.config, &metadata.models, &metadata.resolved_config);
        s.features = features(&s.config, &metadata.models, &metadata.collaboration).into();
        Ok(())
    }
    async fn set_thinking_option(&self, option: Option<&str>) -> Result<Option<Value>> {
        let mut s = self.state.lock().unwrap();
        s.config["thinkingOptionId"] = json!(option.map(str::trim).filter(|s| !s.is_empty()));
        Ok(next_turn_notice(s.foreground.is_some(), "thinking"))
    }
    async fn set_feature(&self, feature: &str, value: Value) -> Result<()> {
        let metadata = self.metadata.lock().unwrap().clone();
        let mut s = self.state.lock().unwrap();
        match feature {
            "plan_mode" => {
                if value == true && !metadata.collaboration.iter().any(is_plan) {
                    bail!("Codex planning collaboration mode is unavailable")
                }
                s.config["featureValues"]["plan_mode"] = json!(value == true);
            }
            "service_tier" | "fast_mode" => {
                let tiers = tiers(&s.config, &metadata.models);
                let tier = if feature == "fast_mode" {
                    if value == true {
                        tiers
                            .iter()
                            .find(|t| {
                                t["name"]
                                    .as_str()
                                    .is_some_and(|n| n.eq_ignore_ascii_case("fast"))
                            })
                            .and_then(|t| t["id"].as_str())
                            .ok_or_else(|| {
                                anyhow!("Codex fast mode is unavailable for the selected model")
                            })?
                    } else {
                        "default"
                    }
                } else {
                    value
                        .as_str()
                        .ok_or_else(|| anyhow!("Codex speed must be a string"))?
                };
                if tier != "default" && !tiers.iter().any(|t| t["id"] == tier) {
                    bail!("Codex speed '{tier}' is unavailable for the selected model")
                }
                s.config["featureValues"]["service_tier"] = json!(tier);
            }
            _ => bail!("Unknown Codex feature: {feature}"),
        }
        s.features = features(&s.config, &metadata.models, &metadata.collaboration).into();
        Ok(())
    }
    async fn rewind(&self, target: RewindTarget, message_id: &str) -> Result<()> {
        if target != RewindTarget::Conversation {
            bail!("Codex supports conversation rewind only; file changes are not reverted")
        }
        let thread = self.ensure_thread().await?;
        let (config, index, count, turn) = {
            let s = self.state.lock().unwrap();
            if s.foreground.is_some() {
                bail!("Cannot rewind an active Codex turn")
            }
            let (index, (_, turn)) = s
                .user_turns
                .iter()
                .enumerate()
                .find(|(_, v)| v.0 == message_id)
                .ok_or_else(|| anyhow!("Codex could not find user message '{message_id}'"))?;
            (s.config.clone(), index, s.user_turns.len(), turn.clone())
        };
        let mut params = json!({"threadId":thread,"cwd":config["cwd"],"model":config["model"],"serviceTier":config["featureValues"]["service_tier"],"config":self.inner,"excludeTurns":false,"persistExtendedHistory":true});
        let history_mode = if self.rollback {
            let meta = self
                .client
                .request(
                    "thread/read",
                    json!({"threadId":thread,"includeTurns":false}),
                )
                .await?;
            match meta["thread"]["historyMode"].as_str() {
                None | Some("legacy") => "legacy",
                Some("paginated") => "paginated",
                Some(other) => bail!("Unknown Codex history mode: {other}"),
            }
        } else {
            "paginated"
        };
        if history_mode == "paginated" {
            params["beforeTurnId"] = json!(
                turn.ok_or_else(|| anyhow!("Codex could not identify the target provider turn"))?
            );
        }
        let forked = self.client.request("thread/fork", params).await?;
        let mut id = forked["thread"]["id"]
            .as_str()
            .ok_or_else(|| anyhow!("Codex fork returned no thread ID"))?
            .to_owned();
        if history_mode == "legacy" {
            let rolled = self
                .client
                .request(
                    "thread/rollback",
                    json!({"threadId":id,"numTurns":count-index}),
                )
                .await?;
            id = rolled["thread"]["id"]
                .as_str()
                .ok_or_else(|| anyhow!("Codex rollback returned no thread ID"))?
                .into();
        }
        let history = load_history(
            &self.client.current(),
            &id,
            config["cwd"].as_str(),
            &self.spec.id,
        )
        .await?;
        let removed = {
            let mut s = self.state.lock().unwrap();
            s.thread = Some(id);
            s.user_turns = history
                .iter()
                .filter(|e| e["item"]["type"] == "user_message")
                .filter_map(|e| {
                    Some((
                        e["item"]["messageId"].as_str()?.into(),
                        e["turnId"].as_str().map(str::to_owned),
                    ))
                })
                .collect();
            let retained = history
                .iter()
                .filter_map(|e| e["item"]["callId"].as_str().map(str::to_owned))
                .collect();
            let removed = s.async_questions.retain(&retained);
            s.history = history;
            removed
        };
        for request_id in removed {
            self.emit(json!({"type":"permission_resolved","requestId":request_id,
                "resolution":{"behavior":"deny","message":"Removed by conversation rewind"}}))
                .await?;
        }
        Ok(())
    }
    async fn try_out_of_band(&self, prompt: &Value) -> Result<bool> {
        let Some(text) = prompt.as_str() else {
            return Ok(false);
        };
        let text = text.trim();
        let (command, args) = text
            .split_once(char::is_whitespace)
            .map(|(a, b)| (a, b.trim()))
            .unwrap_or((text, ""));
        if command != "/compact" && (command != "/goal" || !self.goals) {
            return Ok(false);
        }
        let thread = self.ensure_thread().await?;
        if command == "/compact" {
            self.state.lock().unwrap().manual_compactions += 1;
            if let Err(e) = self
                .client
                .request("thread/compact/start", json!({"threadId":thread}))
                .await
            {
                let mut s = self.state.lock().unwrap();
                s.manual_compactions = s.manual_compactions.saturating_sub(1);
                return Err(e);
            }
            return Ok(true);
        }
        let message = match args {
            "" => "Usage: /goal <objective>|pause|resume|clear".into(),
            "clear" => {
                self.client
                    .request("thread/goal/clear", json!({"threadId":thread}))
                    .await?;
                "Goal cleared.".into()
            }
            "pause" | "resume" => {
                self.client.request("thread/goal/set",json!({"threadId":thread,"status":if args=="pause"{"paused"}else{"active"}})).await?;
                if args == "pause" {
                    "Goal paused.".into()
                } else {
                    "Goal resumed.".into()
                }
            }
            objective => {
                self.client
                    .request(
                        "thread/goal/set",
                        json!({"threadId":thread,"objective":objective,"status":"active"}),
                    )
                    .await?;
                format!("Goal set: {objective}")
            }
        };
        self.emit(json!({"type":"timeline","item":{"type":"assistant_message","text":format!("{message}\n\n")}})).await?;
        Ok(true)
    }
}
fn next_turn_notice(active: bool, setting: &str) -> Option<Value> {
    active.then(|| {
        json!({"type":"warning","message":if setting=="mode"{
        "Permission mode applies next turn"
    }else{"Thinking level applies next turn"}})
    })
}
impl Drop for CodexSession {
    fn drop(&mut self) {
        self.client.cancel.cancel();
        self.client.current().cancel.cancel();
    }
}
