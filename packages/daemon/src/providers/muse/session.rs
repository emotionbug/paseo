use super::{
    catalog,
    projection::{Projection, Questions},
    transport::{Connection, command_id},
};
use crate::providers::api::{AgentSession, EventSink, LaunchContext, ProviderSpec, RewindTarget};
use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{Notify, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

pub(super) struct Session {
    provider: String,
    native: String,
    host: Arc<Connection>,
    caps: Value,
    env: BTreeMap<String, String>,
    state: Mutex<State>,
    history: Mutex<Vec<Value>>,
    sink: EventSink,
    actions: mpsc::UnboundedSender<Action>,
    stop: CancellationToken,
    ended: Notify,
}
struct State {
    config: Value,
    catalog: Value,
    commands: Vec<Value>,
    first_turn: bool,
    owned: HashSet<String>,
    turns: Vec<(String, bool)>,
    approvals: HashMap<String, Value>,
    requests: HashMap<String, Value>,
    responses: HashMap<String, Value>,
    questions: Vec<Value>,
    cursor: String,
    failure: Option<String>,
    closed: bool,
}
enum Action {
    Correlate(String, String, oneshot::Sender<()>),
    Answer(String, Value, oneshot::Sender<Result<bool>>),
    Activity,
    Close(oneshot::Sender<Result<()>>),
}

impl Session {
    pub(super) async fn open(
        spec: &ProviderSpec,
        persistence: Option<Value>,
        config: Value,
        context: LaunchContext,
        sink: EventSink,
    ) -> Result<Arc<Self>> {
        let cwd = config["cwd"]
            .as_str()
            .ok_or_else(|| anyhow!("Muse requires cwd to open a session"))?
            .to_owned();
        let mut env = spec.env.clone();
        env.extend(context.env.clone());
        let empty = json!({});
        let options = config
            .get("providerOptions")
            .unwrap_or(if spec.options.is_null() {
                &empty
            } else {
                &spec.options
            });
        let (host, rx) = Connection::spawn(
            spec,
            env.clone(),
            Some(cwd.clone().into()),
            &json!({"providerOptions":options}),
            context.cancel.clone(),
        )
        .await?;
        let result = Self::initialize(
            spec,
            persistence,
            config,
            &context,
            env,
            sink,
            host.clone(),
            rx,
            cwd,
        )
        .await;
        if result.is_err() {
            let _ = host.close().await;
        }
        result
    }
    async fn initialize(
        spec: &ProviderSpec,
        persistence: Option<Value>,
        mut config: Value,
        context: &LaunchContext,
        env: BTreeMap<String, String>,
        sink: EventSink,
        host: Arc<Connection>,
        rx: mpsc::UnboundedReceiver<Value>,
        cwd: String,
    ) -> Result<Arc<Self>> {
        let mut mcp = serde_json::Map::new();
        let mut servers = config
            .get("mcpServers")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        if let Some(runtime) = context.mcp_servers.as_object() {
            servers.extend(runtime.clone());
        }
        for (id, server) in servers {
            mcp.insert(id,if server["type"]=="stdio" || server.get("command").is_some() {
            json!({"transport":"stdio","command":server["command"],"args":server.get("args").cloned().unwrap_or(json!([])),"env":server.get("env").cloned().unwrap_or(json!({})),"framing":"lineDelimitedJson"})
        }else{json!({"transport":"streamableHttp","url":server["url"],"headers":server.get("headers").cloned().unwrap_or(json!({}))})});
        }
        let resumed = persistence.is_some();
        let response = if let Some(saved) = persistence {
            if config["model"].is_null() {
                config["model"] = saved["data"]["model"].clone();
            }
            if config["thinkingOptionId"].is_null() {
                config["thinkingOptionId"] = saved["data"]["thinkingOption"].clone();
            }
            host.command(
                "session/resume",
                json!({"sessionId":saved["data"]["sessionId"],"config":{"mcpServers":mcp}}),
            )
            .await?
        } else {
            let mode = config["modeId"].as_str().unwrap_or("onRequest");
            validate_mode(mode)?;
            let mut params =
                json!({"workspaceRoot":cwd,"approvalMode":mode,"config":{"mcpServers":mcp}});
            if let Some(model) = config["model"].as_str() {
                params["modelId"] = json!(model);
            }
            host.command("session/start", params).await?
        };
        for field in ["modelId", "providerId"] {
            if !response["session"]
                .get(field)
                .is_some_and(|v| v.is_null() || v.is_string())
            {
                bail!("Invalid Muse session {field}");
            }
        }
        if let Some(pending) = response.get("pendingRequests") {
            for item in pending
                .as_array()
                .ok_or_else(|| anyhow!("Invalid Muse pending requests"))?
            {
                if !item["kind"].is_string() || !item["viewCursor"].is_string() {
                    bail!("Invalid Muse pending request");
                }
            }
        }
        let cursor = response["viewCursor"]
            .as_str()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                super::transport::MuseError::new(
                    "emptyViewCursor",
                    "Muse returned an empty resume viewCursor; this session cannot be watched.",
                )
            })?
            .to_owned();
        let native = response["session"]["sessionId"]
            .as_str()
            .ok_or_else(|| anyhow!("Invalid Muse session response"))?
            .to_owned();
        if let Some(model) = response["session"]["modelId"].as_str() {
            config["model"] = json!(model);
        }
        config["nativeProviderId"] = response["session"]["providerId"].clone();
        let mode = response
            .pointer("/session/approvalMode/mode")
            .and_then(Value::as_str)
            .or_else(|| config["modeId"].as_str())
            .unwrap_or("onRequest")
            .to_owned();
        validate_mode(&mode)?;
        config["modeId"] = json!(mode);
        let (tx, actions) = mpsc::unbounded_channel();
        let session = Arc::new(Self {
            provider: spec.id.clone(),
            native: native.clone(),
            host: host.clone(),
            caps: super::capabilities(),
            env,
            state: Mutex::new(State {
                config,
                catalog: json!({}),
                commands: vec![],
                first_turn: !resumed,
                owned: HashSet::new(),
                turns: vec![],
                approvals: HashMap::new(),
                requests: HashMap::new(),
                responses: HashMap::new(),
                questions: vec![],
                cursor,
                failure: None,
                closed: false,
            }),
            history: Mutex::new(vec![]),
            sink,
            actions: tx,
            stop: context.cancel.child_token(),
            ended: Notify::new(),
        });
        let mut projection = Projection::new(host.clone(), native.clone(), cwd);
        projection.set_provider(&spec.id);
        let mut questions = Questions::new(host, native);
        questions.set_provider(&spec.id);
        if resumed && response.pointer("/history/mode").and_then(Value::as_str) == Some("inline") {
            let items = response
                .pointer("/history/items")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow!("Muse inline history has no items"))?;
            for item in items {
                session
                    .publish(projection.fold(item.clone()).await?, false)
                    .await?;
            }
        }
        if response["pendingRequests"].as_array().is_some_and(|v| {
            v.iter()
                .any(|p| p["kind"] == "approval" || p["kind"] == "userInput")
        }) {
            let pending = session
                .host
                .request("approval/listPending", json!({"sessionId":session.native}))
                .await?;
            for approval in pending["approvals"]
                .as_array()
                .ok_or_else(|| anyhow!("Invalid Muse pending approvals"))?
            {
                session.permission(approval.clone(), false).await?;
            }
            if let Some(inputs) = pending["userInputs"].as_array() {
                for input in inputs {
                    session.publish(questions.requested(input)?, false).await?;
                }
            }
            session.state.lock().unwrap().questions = questions.pending();
        }
        session.refresh_catalog().await?;
        session.refresh_commands().await?;
        session.publish_config(false).await?;
        let actor = session.clone();
        tokio::spawn(async move {
            actor.run(rx, actions, projection, questions).await;
        });
        Ok(session)
    }
    async fn publish_config(&self, live: bool) -> Result<()> {
        let runtime = self.runtime_info().await?;
        self.publish(vec![
            json!({"type":"mode_changed","currentModeId":runtime["modeId"],"availableModes":catalog::modes()}),
            json!({"type":"model_changed","runtimeInfo":runtime}),
            json!({"type":"thinking_option_changed","thinkingOptionId":runtime["thinkingOptionId"]})
        ],live).await
    }
    pub(super) fn history(&self) -> Vec<Value> {
        self.history.lock().unwrap().clone()
    }
    pub(super) fn matches_env(&self, env: &BTreeMap<String, String>) -> bool {
        !self.state.lock().unwrap().closed
            && super::usage_account_key(&self.env) == super::usage_account_key(env)
    }
    pub(super) async fn read_usage(&self) -> Result<Value> {
        super::validate_usage(self.host.request("usage/read", json!({})).await?)
    }
    async fn publish(&self, events: Vec<Value>, live: bool) -> Result<()> {
        for mut event in events {
            event["provider"] = json!(self.provider);
            for key in ["turnId", "timestamp", "code", "diagnostic"] {
                if event[key].is_null() {
                    event.as_object_mut().unwrap().remove(key);
                }
            }
            if let Some(usage) = event.get_mut("usage").and_then(Value::as_object_mut) {
                usage.retain(|_, value| !value.is_null());
            }
            if event["type"] == "permission_requested" {
                event["request"]["provider"] = json!(self.provider);
            }
            self.history.lock().unwrap().push(event.clone());
            if live {
                tokio::select! {result=self.sink.emit(event)=>result?,_=self.stop.cancelled()=>return Err(anyhow!("Muse session is closed"))};
            }
        }
        Ok(())
    }
    fn active(&self) -> Option<String> {
        self.state
            .lock()
            .unwrap()
            .turns
            .iter()
            .rev()
            .find(|(_, active)| *active)
            .map(|(id, _)| id.clone())
    }
    fn ensure_open(&self) -> Result<()> {
        let state = self.state.lock().unwrap();
        if state.closed {
            bail!("Muse session is closed");
        }
        if let Some(error) = &state.failure {
            bail!("{error}");
        }
        Ok(())
    }
    async fn refresh_catalog(&self) -> Result<()> {
        let catalog = catalog::present(
            self.host
                .request("model/list", json!({"sessionId":self.native}))
                .await?,
        )?;
        let mut state = self.state.lock().unwrap();
        if state.config["model"].is_null() {
            state.config["model"] = catalog["defaultModel"].clone();
        }
        if state.config["thinkingOptionId"].is_null() {
            if let Some(model) = catalog["models"]
                .as_array()
                .and_then(|a| a.iter().find(|m| m["id"] == state.config["model"]))
            {
                state.config["thinkingOptionId"] = model["defaultThinkingOptionId"].clone();
            }
        }
        state.catalog = catalog;
        Ok(())
    }
    async fn refresh_commands(&self) -> Result<()> {
        let response = self
            .host
            .request("skill/list", json!({"sessionId":self.native}))
            .await?;
        let skills = response["skills"]
            .as_array()
            .ok_or_else(|| anyhow!("Invalid Muse skill list"))?;
        let mut commands = vec![
            json!({"name":"compact","description":"Compact conversation context","argumentHint":""}),
        ];
        for skill in skills {
            let name = skill["selector"]
                .as_str()
                .ok_or_else(|| anyhow!("Invalid Muse skill selector"))?;
            let description = skill["description"]
                .as_str()
                .ok_or_else(|| anyhow!("Invalid Muse skill description"))?;
            if name != "compact" {
                commands.push(json!({"name":name,"description":description,"argumentHint":skill["argumentHint"].as_str().unwrap_or("")}));
            }
        }
        self.state.lock().unwrap().commands = commands;
        Ok(())
    }
    async fn permission(&self, approval: Value, live: bool) -> Result<()> {
        let id = approval["approvalId"]
            .as_str()
            .ok_or_else(|| anyhow!("Invalid Muse approval ID"))?
            .to_owned();
        let choices = approval["availableChoices"]
            .as_array()
            .ok_or_else(|| anyhow!("Invalid Muse approval choices"))?;
        let subject = &approval["subject"];
        let current = subject["stages"].as_array().and_then(|stages| {
            stages.iter().find(|stage| {
                stage.pointer("/requirementId/sourceIndex")
                    == approval.pointer("/currentRequirementId/sourceIndex")
            })
        });
        if current.is_some_and(|s| {
            s.pointer("/resolution/kind").and_then(Value::as_str) != Some("unresolved")
        }) {
            return Ok(());
        }
        self.state
            .lock()
            .unwrap()
            .approvals
            .insert(id.clone(), approval.clone());
        let allow_all = { self.state.lock().unwrap().config["modeId"] == "allowAll" };
        if allow_all {
            let choice = choices
                .iter()
                .find(|c| c["decision"] == "approved" && c["scope"] == "once")
                .ok_or_else(|| anyhow!("Muse escalation has no allow-once action"))?;
            return self
                .decide(
                    &approval,
                    choice["choiceId"]
                        .as_str()
                        .ok_or_else(|| anyhow!("Invalid Muse approval choice"))?,
                )
                .await;
        }
        let title = current
            .map(|s| format!("Stage {} of {}", s["position"], s["totalStages"]))
            .unwrap_or_else(|| approval["toolName"].as_str().unwrap_or("Tool").into());
        let detail = match subject["kind"].as_str() {
            Some("shell") => {
                json!({"type":"shell","command":current.and_then(|s|s["argv"].as_array()).map(|args|args.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" ")).unwrap_or_else(||subject["command"].as_str().unwrap_or("").into()),"cwd":subject["workspaceRoot"]})
            }
            Some("fileAccess") => json!({"type":"edit","filePath":subject["path"]}),
            _ => {
                json!({"type":"plain_text","label":subject["kind"],"text":(["host","path","command"].into_iter().find_map(|key|subject[key].as_str().filter(|s|!s.is_empty())).unwrap_or(""))})
            }
        };
        let actions:Vec<_>=choices.iter().map(|c|json!({"id":c["choiceId"],"label":c["label"],"behavior":if c["decision"].as_str().unwrap_or("").starts_with("approved"){"allow"}else{"deny"}})).collect();
        let request = json!({"id":id,"provider":self.provider,"name":approval["toolName"].as_str().unwrap_or("Tool"),"kind":"tool","title":title,"detail":detail,"actions":actions});
        self.state
            .lock()
            .unwrap()
            .requests
            .insert(id, request.clone());
        self.publish(
            vec![json!({"type":"permission_requested","request":request,"turnId":self.active()})],
            live,
        )
        .await
    }
    async fn decide(&self, approval: &Value, choice: &str) -> Result<()> {
        let mut params =
            json!({"sessionId":self.native,"approvalId":approval["approvalId"],"choiceId":choice});
        if let Some(requirement) = approval.get("currentRequirementId") {
            params["requirementId"] = requirement.clone();
        }
        self.host.command("approval/decide", params).await?;
        Ok(())
    }
    async fn fail(&self, error: anyhow::Error) {
        let value = super::transport::actionable_error(&error, &self.env);
        let message = value["message"]
            .as_str()
            .unwrap_or("Muse runtime failed")
            .to_owned();
        {
            let mut state = self.state.lock().unwrap();
            if state.closed || state.failure.is_some() {
                return;
            }
            state.failure = Some(message.clone());
        }
        self.ended.notify_waiters();
        let _=self.publish(vec![json!({"type":"turn_failed","error":message,"code":value["code"],"diagnostic":value["diagnostic"],"turnId":self.active()})],true).await;
    }
    async fn run(
        self: Arc<Self>,
        mut rx: mpsc::UnboundedReceiver<Value>,
        mut actions: mpsc::UnboundedReceiver<Action>,
        mut projection: Projection,
        mut questions: Questions,
    ) {
        loop {
            let result = tokio::select! {
                biased;
                _=self.stop.cancelled()=>break,
                action=actions.recv()=>match action {
                    Some(Action::Correlate(command,client,reply))=>{projection.correlate(command,client);let _=reply.send(());Ok(())},
                    Some(Action::Answer(id,response,reply))=>{let result=questions.answer(&id,&response).await;self.state.lock().unwrap().questions=questions.pending();let _=reply.send(result);Ok(())},
                    Some(Action::Activity)=>Ok(()),
                    Some(Action::Close(reply))=>{
                        let mut result=Ok(());
                        while let Ok(event)=rx.try_recv(){if event["method"]!="$error" {if let Err(error)=self.notify(event,&mut projection,&mut questions).await{result=Err(error);break;}}}
                        if result.is_ok(){result=self.publish(projection.close_children(),true).await;}
                        let _=reply.send(result);break;
                    },
                    None=>break,
                },
                event=rx.recv()=>match event {Some(event)=>self.notify(event,&mut projection,&mut questions).await,None=>Err(anyhow!("Muse MSP notification stream closed"))},
                _=tokio::time::sleep(Duration::from_secs(120)),if self.active().is_some()=>{
                    let cursor=self.state.lock().unwrap().cursor.clone();
                    let result=self.backfill(cursor,&mut projection,&mut questions).await;
                    match result {Ok(())=>match projection.refresh_children().await {Ok(events)=>self.publish(events,true).await,Err(e)=>Err(e)},Err(e)=>Err(e)}
                }
            };
            if let Err(error) = result {
                self.fail(error).await;
                break;
            }
        }
        let _ = self.publish(projection.close_children(), false).await;
        self.stop.cancel();
        self.ended.notify_waiters();
        let _ = self.host.close().await;
    }
    async fn backfill(
        &self,
        mut cursor: String,
        projection: &mut Projection,
        questions: &mut Questions,
    ) -> Result<()> {
        loop {
            let page=self.host.request("view/page",json!({"sessionId":self.native,"cursor":cursor,"direction":"forward","limit":1000})).await?;
            let events = page["events"]
                .as_array()
                .ok_or_else(|| anyhow!("Invalid Muse view page"))?;
            for event in events {
                self.notify(event.clone(), projection, questions).await?;
            }
            if self.stop.is_cancelled() {
                return Ok(());
            }
            match page.get("nextCursor") {
                Some(Value::Null) => break,
                Some(Value::String(next)) => cursor = next.clone(),
                _ => bail!("Invalid Muse view cursor"),
            }
        }
        Ok(())
    }
    async fn notify(
        &self,
        event: Value,
        projection: &mut Projection,
        questions: &mut Questions,
    ) -> Result<()> {
        let method = event["method"]
            .as_str()
            .ok_or_else(|| anyhow!("Invalid Muse notification"))?;
        let params = &event["params"];
        if method == "$error" {
            return Err(self
                .host
                .failure()
                .unwrap_or_else(|| {
                    super::transport::MuseError::new(
                        "runtime",
                        params["message"].as_str().unwrap_or("Muse host exited"),
                    )
                })
                .into());
        }
        if !params.is_object() {
            bail!("Invalid Muse notification parameters");
        }
        for field in ["sessionId", "viewCursor"] {
            if params.get(field).is_some_and(|v| !v.is_string()) {
                bail!("Invalid Muse notification {field}");
            }
        }
        if let Some(id) = params["sessionId"].as_str().filter(|id| *id != self.native) {
            return self
                .publish(projection.child_notify(id, &event).await?, true)
                .await;
        }
        projection.set_turn_id(self.active());
        questions.set_turn_id(self.active());
        match method {
            "view/gap"=>{let after=params["after"].as_str().ok_or_else(||anyhow!("Invalid Muse gap cursor"))?;Box::pin(self.backfill(after.into(),projection,questions)).await?;},
            "item/started"|"item/updated"|"item/completed"=>self.publish(projection.fold(params["item"].clone()).await?,true).await?,
            "item/delta"=>self.publish(projection.delta(params)?,true).await?,
            "session/todoListChanged"=>self.publish(vec![json!({"type":"timeline","item":super::projection::todo(params)?,"turnId":self.active()})],true).await?,
            "skill/changed"=>self.refresh_commands().await?,
            "userInput/requested"=>{self.publish(questions.requested(params)?,true).await?;self.state.lock().unwrap().questions=questions.pending();},
            "userInput/settled"=>{self.publish(questions.resolved(params)?,true).await?;self.state.lock().unwrap().questions=questions.pending();},
            "approval/requested"|"approval/updated"=>self.permission(params.clone(),true).await?,
            "approval/resolved"=>{
                let id=params["approvalId"].as_str().ok_or_else(||anyhow!("Invalid Muse approval resolution"))?;
                let response={let mut state=self.state.lock().unwrap();state.approvals.remove(id);state.requests.remove(id);state.responses.remove(id).unwrap_or(json!({"behavior":"deny","message":"Resolved by provider"}))};
                self.publish(vec![json!({"type":"permission_resolved","requestId":id,"resolution":response,"turnId":self.active()})],true).await?;
            },
            "turn/started"=>{
                let id=params["turnId"].as_str().ok_or_else(||anyhow!("Invalid Muse turn ID"))?;
                let own={let mut state=self.state.lock().unwrap();let owned=params["commandId"].as_str().is_some_and(|id|state.owned.contains(id));
                    if owned && !state.turns.iter().any(|(turn,active)|turn==id && !active) {if !state.turns.iter().any(|(turn,_)|turn==id){state.turns.push((id.into(),true));}true}else{false}};
                if own {self.publish(vec![json!({"type":"turn_started","turnId":id})],true).await?;}
            },
            "turn/completed"=>{
                let id=params["turnId"].as_str().ok_or_else(||anyhow!("Invalid Muse turn ID"))?;
                let owned={let mut state=self.state.lock().unwrap();if let Some((_,active))=state.turns.iter_mut().find(|(turn,_)|turn==id){let was=*active;*active=false;was}else{false}};
                if owned {
                    let output=if params["terminal"]=="cancelled" {json!({"type":"turn_canceled","turnId":id,"reason":"Canceled by provider"})}
                    else if params["terminal"]=="failed" || params.get("error").is_some_and(|e|!e.is_null()) {let error=super::transport::MuseError::new(params["error"]["kind"].as_str().unwrap_or("turnFailed"),params["error"]["message"].as_str().unwrap_or("Muse turn failed"));let error=super::transport::actionable_error(&error.into(),&self.env);json!({"type":"turn_failed","turnId":id,"error":error["message"],"code":error["code"],"diagnostic":error["diagnostic"]})}
                    else {json!({"type":"turn_completed","turnId":id})};
                    self.publish(vec![output],true).await?;self.ended.notify_waiters();
                }
            },
            "session/tokenUsage"=>{
                for field in ["promptTokens","outputTokens"] {if !params["cumulative"][field].is_number(){bail!("Invalid Muse token usage");}}
                if params.get("usage").is_some_and(|v|!v.is_object()||v.get("cachedTokens").is_some_and(|v|!v.is_number())){bail!("Invalid Muse cached token usage");}
                self.publish(vec![json!({"type":"usage_updated","usage":{"inputTokens":params["cumulative"]["promptTokens"],"outputTokens":params["cumulative"]["outputTokens"],"cachedInputTokens":params["usage"]["cachedTokens"]},"turnId":self.active()})],true).await?;
            },
            "session/contextUsage"=>{
                if !params["usedTokens"].is_number()||params.get("windowTokens").is_some_and(|v|!v.is_number()){bail!("Invalid Muse context usage");}
                self.publish(vec![json!({"type":"usage_updated","usage":{"contextWindowUsedTokens":params["usedTokens"],"contextWindowMaxTokens":params["windowTokens"]},"turnId":self.active()})],true).await?;
            },
            _=>{}
        }
        if let Some(cursor) = params["viewCursor"].as_str() {
            self.state.lock().unwrap().cursor = cursor.into();
        }
        Ok(())
    }
}

pub(super) fn validate_effort(option: &str) -> Result<()> {
    if ![
        "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
    ]
    .contains(&option)
    {
        bail!("Invalid Muse reasoning effort: {option}");
    }
    Ok(())
}
fn validate_mode(mode: &str) -> Result<()> {
    if !["onRequest", "promptUnmatched", "denyUnmatched", "allowAll"].contains(&mode) {
        bail!("Invalid Muse approval mode: {mode}");
    }
    Ok(())
}

impl Session {
    async fn prompt(&self, prompt: Value, options: Value, steer: bool) -> Result<Value> {
        self.ensure_open()?;
        if options.get("outputSchema").is_some() {
            bail!("Muse does not support prompt.output_schema");
        }
        let command = if !steer {
            prompt
                .as_str()
                .filter(|s| s.starts_with('/'))
                .and_then(|text| {
                    let (name, args) = text[1..].split_once(' ').unwrap_or((&text[1..], ""));
                    self.state
                        .lock()
                        .unwrap()
                        .commands
                        .iter()
                        .any(|c| c["name"] == name)
                        .then(|| (name.to_owned(), args.to_owned()))
                })
        } else {
            None
        };
        let client = options["clientMessageId"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(command_id);
        if command.as_ref().is_some_and(|(name, _)| name == "compact") {
            self.host
                .command("session/compact", json!({"sessionId":self.native}))
                .await?;
            let turn = format!("command-{client}");
            self.publish(
                vec![
                    json!({"type":"turn_started","turnId":turn}),
                    json!({"type":"turn_completed","turnId":turn}),
                ],
                true,
            )
            .await?;
            return Ok(json!({"turnId":turn,"disposition":"completed"}));
        }
        let config = { self.state.lock().unwrap().config.clone() };
        let first = { self.state.lock().unwrap().first_turn };
        let mut input = Vec::new();
        let mut display = Vec::new();
        if first {
            let system = ["systemPrompt", "daemonAppendSystemPrompt"]
                .into_iter()
                .filter_map(|key| config[key].as_str())
                .filter(|text| !text.trim().is_empty())
                .collect::<Vec<_>>()
                .join("\n\n");
            if !system.is_empty() {
                input.push(json!({"type":"text","text":format!("<system_instructions>\n{system}\n</system_instructions>")}));
            }
        }
        if let Some((name, args)) = command {
            input.push(json!({"type":"skill","selector":name,"arguments":args}));
            display.push(format!(
                "/{name}{}",
                if args.is_empty() {
                    String::new()
                } else {
                    format!(" {args}")
                }
            ));
        } else {
            let parts = match prompt {
                Value::String(text) => vec![json!({"type":"text","text":text})],
                Value::Array(parts) => parts,
                _ => bail!("Invalid Muse prompt content"),
            };
            for part in parts {
                match part["type"].as_str() {
                    Some("text") => {
                        if !part["text"].is_string() {
                            bail!("Invalid Muse text block");
                        }
                        let text = crate::providers::prompt::text(&part["text"])?;
                        input.push(json!({"type":"text","text":text}));
                        display.push(text);
                    }
                    Some("image") => {
                        let data = part["data"]
                            .as_str()
                            .ok_or_else(|| anyhow!("Invalid Muse image data"))?;
                        let mime = part["mimeType"]
                            .as_str()
                            .ok_or_else(|| anyhow!("Invalid Muse image media type"))?;
                        input.push(json!({"type":"image","base64Data":data,"mediaType":mime}));
                    }
                    Some(kind) => bail!("Muse does not support {kind} attachments"),
                    None => bail!("Invalid Muse prompt attachment"),
                }
            }
        }
        let id = command_id();
        self.state.lock().unwrap().owned.insert(id.clone());
        let (tx, rx) = oneshot::channel();
        self.actions
            .send(Action::Correlate(id.clone(), client, tx))
            .map_err(|_| anyhow!("Muse session is closed"))?;
        rx.await.map_err(|_| anyhow!("Muse session is closed"))?;
        let mut params = json!({"commandId":id,"sessionId":self.native,"input":input,"displayText":display.join("\n"),"ifBusy":if steer{"steer"}else{"queue"}});
        if let Some(effort) = config["thinkingOptionId"].as_str() {
            validate_effort(effort)?;
            params["reasoningEffort"] = json!(effort);
        }
        let response = self.host.command("turn/start", params).await?;
        let turn = response["turnId"]
            .as_str()
            .ok_or_else(|| anyhow!("Invalid Muse turn response"))?
            .to_owned();
        let disposition = response["disposition"]
            .as_str()
            .ok_or_else(|| anyhow!("Invalid Muse prompt disposition"))?;
        {
            let mut state = self.state.lock().unwrap();
            state.first_turn = false;
            if disposition == "started" && !state.turns.iter().any(|(id, _)| id == &turn) {
                state.turns.push((turn.clone(), true));
            }
        }
        let _ = self.actions.send(Action::Activity);
        Ok(json!({"turnId":turn,"disposition":disposition}))
    }
    fn saved(&self) -> Value {
        let state = self.state.lock().unwrap();
        let mut data = json!({"sessionId":self.native});
        if let Some(model) = state.config["model"].as_str() {
            data["model"] = json!(model);
        }
        if let Some(effort) = state.config["thinkingOptionId"].as_str() {
            data["thinkingOption"] = json!(effort);
        }
        json!({"version":1,"data":data})
    }
}

#[async_trait]
impl AgentSession for Session {
    fn provider(&self) -> &str {
        &self.provider
    }
    fn native_id(&self) -> Option<String> {
        Some(format!("plugin:{}", self.saved()))
    }
    fn capabilities(&self) -> &Value {
        &self.caps
    }
    fn features(&self) -> Arc<[Value]> {
        Arc::from([])
    }
    fn initial_timeline(&self) -> &[Value] {
        &[]
    }
    fn describe_persistence(&self) -> Option<Value> {
        let saved = self.saved();
        Some(
            json!({"provider":self.provider,"sessionId":format!("plugin:{saved}"),"metadata":{"pluginProviderPersistence":saved}}),
        )
    }
    fn pending_permissions(&self) -> Vec<Value> {
        let state = self.state.lock().unwrap();
        state
            .requests
            .values()
            .cloned()
            .chain(state.questions.iter().cloned())
            .collect()
    }
    fn usage_session(&self) -> Option<Value> {
        let model = self.state.lock().unwrap().config["model"].clone();
        Some(
            json!({"provider":self.provider,"model":model,"env":self.env,"sessionKey":self.native_id()}),
        )
    }
    async fn start_turn(&self, prompt: Value, options: Value) -> Result<Value> {
        let result = self.prompt(prompt, options, false).await?;
        Ok(json!({"turnId":result["turnId"]}))
    }
    async fn steer_active_turn(&self, prompt: Value, options: Value) -> Result<Value> {
        let expected = options["expectedTurnId"].clone();
        let result = self.prompt(prompt, options, true).await?;
        Ok(
            json!({"status":if result["disposition"]=="steered" && result["turnId"]==expected{"accepted"}else{"unavailable"}}),
        )
    }
    async fn stream_history(&self, sink: EventSink) -> Result<()> {
        for event in self.history() {
            sink.emit(event).await?;
        }
        Ok(())
    }
    async fn runtime_info(&self) -> Result<Value> {
        let config = self.state.lock().unwrap().config.clone();
        Ok(
            json!({"provider":self.provider,"sessionId":self.native_id(),"model":config["model"],"thinkingOptionId":config["thinkingOptionId"],"modeId":config["modeId"]}),
        )
    }
    async fn available_modes(&self) -> Result<Vec<Value>> {
        Ok(catalog::modes())
    }
    async fn current_mode(&self) -> Result<Option<String>> {
        Ok(self.state.lock().unwrap().config["modeId"]
            .as_str()
            .map(str::to_owned))
    }
    async fn set_mode(&self, mode: &str) -> Result<Option<Value>> {
        self.ensure_open()?;
        validate_mode(mode)?;
        self.host
            .command(
                "session/setApprovalMode",
                json!({"sessionId":self.native,"mode":mode}),
            )
            .await?;
        self.state.lock().unwrap().config["modeId"] = json!(mode);
        self.publish_config(true).await?;
        Ok(None)
    }
    async fn respond_to_permission(&self, id: &str, response: Value) -> Result<Option<Value>> {
        self.ensure_open()?;
        let (tx, rx) = oneshot::channel();
        self.actions
            .send(Action::Answer(id.into(), response.clone(), tx))
            .map_err(|_| anyhow!("Muse session is closed"))?;
        if rx.await.map_err(|_| anyhow!("Muse session is closed"))?? {
            return Ok(None);
        }
        let approval = self
            .state
            .lock()
            .unwrap()
            .approvals
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow!("This Muse approval is no longer pending"))?;
        let choices = approval["availableChoices"]
            .as_array()
            .ok_or_else(|| anyhow!("Invalid Muse approval choices"))?;
        let choice = if let Some(selected) = response["selectedActionId"].as_str() {
            choices.iter().find(|c| c["choiceId"] == selected)
        } else {
            choices
                .iter()
                .find(|c| {
                    c["scope"] == "once"
                        && c["decision"]
                            == if response["behavior"] == "allow" {
                                "approved"
                            } else {
                                "denied"
                            }
                })
                .or_else(|| {
                    (response["behavior"] == "deny")
                        .then(|| {
                            choices
                                .iter()
                                .find(|c| c["scope"] == "once" && c["decision"] == "abort")
                        })
                        .flatten()
                })
        }
        .ok_or_else(|| anyhow!("Choose one of Muse's available approval actions"))?;
        let choice = choice["choiceId"]
            .as_str()
            .ok_or_else(|| anyhow!("Invalid Muse approval choice"))?;
        self.state
            .lock()
            .unwrap()
            .responses
            .insert(id.into(), response);
        if let Err(error) = self.decide(&approval, choice).await {
            self.state.lock().unwrap().responses.remove(id);
            return Err(error);
        }
        Ok(None)
    }
    async fn interrupt(&self) -> Result<()> {
        self.ensure_open()?;
        let Some(turn) = self.active() else {
            return Ok(());
        };
        let completion = async {
            loop {
                let notified = self.ended.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                self.ensure_open()?;
                if !self
                    .state
                    .lock()
                    .unwrap()
                    .turns
                    .iter()
                    .any(|(id, active)| id == &turn && *active)
                {
                    return Ok::<(), anyhow::Error>(());
                }
                notified.await;
            }
        };
        let command = self.host.command(
            "turn/interrupt",
            json!({"sessionId":self.native,"turnId":turn}),
        );
        match tokio::time::timeout(Duration::from_secs(10), async {
            tokio::try_join!(command, completion)?;
            Ok::<(), anyhow::Error>(())
        })
        .await
        {
            Ok(result) => result,
            Err(_) => {
                let message = "Muse did not confirm cancellation within 10000ms. Reopen the session before sending another prompt.";
                self.fail(anyhow!(message)).await;
                self.stop.cancel();
                self.host.close().await?;
                bail!(message)
            }
        }
    }
    async fn close(&self) -> Result<()> {
        {
            let mut state = self.state.lock().unwrap();
            state.closed = true;
        }
        self.ended.notify_waiters();
        let result = self.host.close().await;
        let (tx, rx) = oneshot::channel();
        if self.actions.send(Action::Close(tx)).is_ok() {
            let drained = rx.await;
            result?;
            if let Ok(result) = drained {
                result?;
            }
        } else {
            result?;
        }
        self.stop.cancel();
        Ok(())
    }
    async fn list_commands(&self) -> Result<Vec<Value>> {
        Ok(self.state.lock().unwrap().commands.clone())
    }
    async fn set_model(&self, model: Option<&str>) -> Result<()> {
        self.ensure_open()?;
        let model = model
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("Choose a Muse model"))?;
        let provider = self.state.lock().unwrap().config["nativeProviderId"].clone();
        let mut selector = json!({"modelId":model});
        if !provider.is_null() {
            selector["providerId"] = provider;
        }
        self.host
            .command(
                "session/setModel",
                json!({"sessionId":self.native,"model":selector}),
            )
            .await?;
        self.state.lock().unwrap().config["model"] = json!(model);
        self.publish_config(true).await
    }
    async fn set_thinking_option(&self, option: Option<&str>) -> Result<Option<Value>> {
        self.ensure_open()?;
        if let Some(option) = option {
            validate_effort(option)?;
        }
        self.state.lock().unwrap().config["thinkingOptionId"] = json!(option);
        self.publish_config(true).await?;
        Ok(None)
    }
    async fn set_feature(&self, feature: &str, _value: Value) -> Result<()> {
        bail!("Muse does not advertise setting '{feature}'")
    }
    async fn rewind(&self, _target: RewindTarget, _message_id: &str) -> Result<()> {
        bail!("Muse does not support session rewind")
    }
    async fn try_out_of_band(&self, _prompt: &Value) -> Result<bool> {
        Ok(false)
    }
}
