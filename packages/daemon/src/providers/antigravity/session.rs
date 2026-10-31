use std::{
    collections::HashMap,
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, BufReader, Lines},
    sync::{mpsc, oneshot},
};
use tokio_util::sync::CancellationToken;

use super::{
    catalog,
    wire::{self, Error, Frame, Status, Turn},
};
use crate::{
    process::ManagedProcess,
    providers::{api::*, prompt},
};

type Reply = oneshot::Sender<Result<Value>>;
enum Command {
    Prompt(Value, Value, Reply),
    Configure(&'static str, Value, Reply),
    Interrupt(Reply),
    Close(Reply),
}
struct Snapshot {
    conversation: Option<String>,
    config: Value,
    history: Vec<Value>,
}
pub(super) struct Session {
    provider: String,
    id: String,
    persist: bool,
    capabilities: Value,
    initial: Vec<Value>,
    snapshot: Arc<Mutex<Snapshot>>,
    commands: mpsc::Sender<Command>,
    cancel: CancellationToken,
    done: CancellationToken,
    closing: std::sync::atomic::AtomicBool,
}
impl Session {
    pub async fn open(
        spec: ProviderSpec,
        mut config: Value,
        context: LaunchContext,
        options: Value,
        conversation: Option<String>,
        catalog: Arc<RwLock<Value>>,
        sink: EventSink,
    ) -> Result<Arc<Self>> {
        config["cwd"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("Antigravity requires cwd"))?;
        if config["modeId"].is_null() {
            config["modeId"] = json!("full-access");
        }
        catalog::validate(&config, &catalog.read().unwrap_or_else(|e| e.into_inner()))?;
        let persist = options["persistSession"] != false;
        let snapshot = Arc::new(Mutex::new(Snapshot {
            conversation: conversation.clone(),
            config: config.clone(),
            history: Vec::new(),
        }));
        let (commands, receiver) = mpsc::channel(32);
        let cancel = context.cancel.child_token();
        let done = CancellationToken::new();
        let mut runtime = Runtime {
            spec: spec.clone(),
            config,
            context,
            catalog,
            snapshot: snapshot.clone(),
            sink,
            driver: None,
            turn: None,
            conversation: conversation.clone(),
            first_message: conversation.is_none(),
            denials: HashMap::new(),
            images: None,
            persist,
            cancel: cancel.clone(),
        };
        runtime.ensure_driver().await?;
        let notice = notice(
            &spec.id,
            None,
            "Antigravity is running with full access",
            "Antigravity's CLI cannot ask for permission when another app drives it, so Paseo starts it with --dangerously-skip-permissions. Every tool call, including shell commands, runs without asking.",
        );
        let initial = vec![json!({"item":notice["item"],"timestamp":notice["timestamp"]})];
        let config_events = runtime.config_events();
        {
            let mut snapshot = snapshot.lock().unwrap_or_else(|e| e.into_inner());
            snapshot.history.push(notice);
            snapshot.history.extend(config_events);
        }
        let session = Arc::new(Self {
            provider: spec.id,
            id: runtime.context.agent_id.clone(),
            persist,
            capabilities: catalog::capabilities(),
            initial,
            snapshot,
            commands,
            cancel,
            done: done.clone(),
            closing: std::sync::atomic::AtomicBool::new(false),
        });
        tokio::spawn(async move {
            runtime.run(receiver).await;
            done.cancel();
        });
        Ok(session)
    }
    async fn request(&self, command: impl FnOnce(Reply) -> Command) -> Result<Value> {
        let (send, receive) = oneshot::channel();
        tokio::select! {
            _ = self.done.cancelled() => return Err(anyhow!("Antigravity session is closed")),
            result = self.commands.send(command(send)) => result.map_err(|_| anyhow!("Antigravity session is closed"))?,
        }
        receive
            .await
            .map_err(|_| anyhow!("Antigravity session is closed"))?
    }
    fn info(&self) -> Value {
        let snapshot = self.snapshot.lock().unwrap_or_else(|e| e.into_inner());
        runtime_info(
            &self.provider,
            &self.id,
            self.persist,
            snapshot.conversation.as_deref(),
            &snapshot.config,
        )
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
#[async_trait]
impl AgentSession for Session {
    fn provider(&self) -> &str {
        &self.provider
    }
    fn native_id(&self) -> Option<String> {
        self.describe_persistence()
            .and_then(|v| v["sessionId"].as_str().map(str::to_owned))
    }
    fn capabilities(&self) -> &Value {
        &self.capabilities
    }
    fn features(&self) -> Arc<[Value]> {
        Arc::from([])
    }
    fn initial_timeline(&self) -> &[Value] {
        &self.initial
    }
    fn describe_persistence(&self) -> Option<Value> {
        if !self.persist {
            return None;
        }
        self.snapshot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .conversation
            .as_deref()
            .map(|id| catalog::persistence(&self.provider, id))
    }
    fn pending_permissions(&self) -> Vec<Value> {
        Vec::new()
    }
    fn usage_session(&self) -> Option<Value> {
        None
    }
    async fn start_turn(&self, prompt: Value, options: Value) -> Result<Value> {
        self.request(|reply| Command::Prompt(prompt, options, reply))
            .await
    }
    async fn steer_active_turn(&self, _prompt: Value, _options: Value) -> Result<Value> {
        Ok(json!({"status":"unavailable"}))
    }
    async fn stream_history(&self, sink: EventSink) -> Result<()> {
        let events = self
            .snapshot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .history
            .clone();
        for event in events {
            sink.emit(event).await?;
        }
        Ok(())
    }
    async fn runtime_info(&self) -> Result<Value> {
        Ok(self.info())
    }
    async fn available_modes(&self) -> Result<Vec<Value>> {
        Ok(catalog::modes())
    }
    async fn current_mode(&self) -> Result<Option<String>> {
        Ok(Some("full-access".into()))
    }
    async fn set_mode(&self, mode: &str) -> Result<Option<Value>> {
        self.request(|reply| Command::Configure("modeId", json!(mode), reply))
            .await?;
        Ok(None)
    }
    async fn respond_to_permission(
        &self,
        _request_id: &str,
        _response: Value,
    ) -> Result<Option<Value>> {
        Err(anyhow!(
            "Antigravity does not support session.permission; the CLI runs with full access"
        ))
    }
    async fn interrupt(&self) -> Result<()> {
        self.request(Command::Interrupt).await?;
        Ok(())
    }
    async fn close(&self) -> Result<()> {
        if self.done.is_cancelled() {
            return Ok(());
        }
        let _cancel_on_drop = self.cancel.drop_guard_ref();
        if self.closing.swap(true, std::sync::atomic::Ordering::AcqRel) {
            self.done.cancelled().await;
            return Ok(());
        }
        let result = self.request(Command::Close).await;
        self.cancel.cancel();
        self.done.cancelled().await;
        result.map(|_| ())
    }
    async fn list_commands(&self) -> Result<Vec<Value>> {
        Ok(Vec::new())
    }
    async fn set_model(&self, model: Option<&str>) -> Result<()> {
        self.request(|reply| Command::Configure("model", json!(model), reply))
            .await?;
        Ok(())
    }
    async fn set_thinking_option(&self, option: Option<&str>) -> Result<Option<Value>> {
        self.request(|reply| Command::Configure("thinkingOptionId", json!(option), reply))
            .await?;
        Ok(None)
    }
    async fn set_feature(&self, _feature: &str, _value: Value) -> Result<()> {
        Err(anyhow!("Antigravity has no configurable settings"))
    }
    async fn rewind(&self, _target: RewindTarget, _message_id: &str) -> Result<()> {
        Err(anyhow!("Antigravity does not support session.revert"))
    }
    async fn try_out_of_band(&self, _prompt: &Value) -> Result<bool> {
        Ok(false)
    }
}

struct Driver {
    process: Arc<ManagedProcess>,
    lines: Lines<BufReader<tokio::process::ChildStdout>>,
    selection: Value,
}
impl Driver {
    async fn stop(self, interrupt: bool) -> Result<()> {
        if interrupt {
            self.process.interrupt(Duration::from_secs(1)).await
        } else {
            #[cfg(windows)]
            {
                self.process.terminate(Duration::ZERO).await
            }
            #[cfg(not(windows))]
            {
                self.process.close_stdin().await?;
                if let Ok(result) =
                    tokio::time::timeout(Duration::from_secs(1), self.process.wait()).await
                {
                    // The supervisor also owns cleanup of descendants after leader exit.
                    return result.map(|_| ());
                }
                self.process.terminate(Duration::from_secs(1)).await
            }
        }
    }
}
struct Runtime {
    spec: ProviderSpec,
    config: Value,
    context: LaunchContext,
    catalog: Arc<RwLock<Value>>,
    snapshot: Arc<Mutex<Snapshot>>,
    sink: EventSink,
    driver: Option<Driver>,
    turn: Option<Turn>,
    conversation: Option<String>,
    first_message: bool,
    denials: HashMap<(String, String), usize>,
    images: Option<tempfile::TempDir>,
    persist: bool,
    cancel: CancellationToken,
}
impl Runtime {
    async fn run(&mut self, mut commands: mpsc::Receiver<Command>) {
        loop {
            enum Next {
                Cancel,
                Command(Option<Command>),
                Line(std::io::Result<Option<String>>),
            }
            let next = tokio::select! {
                biased;
                _ = self.cancel.cancelled() => Next::Cancel,
                _ = self.sink.cancellation().cancelled_owned() => Next::Cancel,
                command = commands.recv() => Next::Command(command),
                line = async {
                    match self.driver.as_mut() {
                        Some(driver) => driver.lines.next_line().await,
                        None => std::future::pending().await,
                    }
                } => Next::Line(line),
            };
            match next {
                Next::Cancel | Next::Command(None) => break,
                Next::Command(Some(Command::Close(reply))) => {
                    let result = self.stop(true).await.map(|_| Value::Null);
                    if let Some(images) = self.images.take() {
                        if let Err(error) = images.close() {
                            let _ = reply.send(Err(error.into()));
                            break;
                        }
                    }
                    let _ = reply.send(result);
                    break;
                }
                Next::Command(Some(Command::Interrupt(reply))) => {
                    let result = if self.turn.is_some() {
                        self.stop(false).await
                    } else {
                        Ok(())
                    };
                    let _ = reply.send(result.map(|_| Value::Null));
                }
                Next::Command(Some(Command::Configure(key, value, reply))) => {
                    let result = self.configure(key, value).await;
                    let _ = reply.send(result.map(|_| Value::Null));
                }
                Next::Command(Some(Command::Prompt(prompt, options, reply))) => {
                    let result = self.start(prompt, options).await;
                    let _ = reply.send(result);
                }
                Next::Line(Ok(Some(line))) if line.trim().is_empty() => {}
                Next::Line(Ok(Some(line))) => match wire::decode(&line) {
                    Ok(frame) => {
                        if self.accept(frame).await.is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        if self.failure(error).await.is_err() {
                            break;
                        }
                    }
                },
                Next::Line(Ok(None)) => {
                    let message = if let Some(driver) = &self.driver {
                        let status = driver.process.wait().await;
                        let stderr = driver.process.stderr_tail();
                        if stderr.trim().is_empty() {
                            format!(
                                "Antigravity exited ({})",
                                status
                                    .map(|s| s.to_string())
                                    .unwrap_or_else(|e| e.to_string())
                            )
                        } else {
                            stderr.trim().to_owned()
                        }
                    } else {
                        "Antigravity exited".into()
                    };
                    if self
                        .failure(Error::new("PROCESS_EXIT", wire::diagnostic(&message)).into())
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Next::Line(Err(error)) => {
                    if self
                        .failure(
                            Error::new(
                                "INVALID_FRAME",
                                format!("Invalid Antigravity output: {error}"),
                            )
                            .into(),
                        )
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
        if let Some(driver) = self.driver.take() {
            let _ = driver.stop(false).await;
        }
        self.images.take();
    }
    async fn ensure_driver(&mut self) -> Result<()> {
        let selection = json!([self.config["model"], self.config["modeId"]]);
        if self
            .driver
            .as_ref()
            .is_some_and(|d| d.selection != selection)
        {
            if let Some(driver) = self.driver.take() {
                driver.stop(false).await?;
            }
        }
        if self.driver.is_some() {
            return Ok(());
        }
        if self.cancel.is_cancelled() {
            return Err(anyhow!("Antigravity startup canceled"));
        }
        self.denials.clear();
        let cwd = self.config["cwd"]
            .as_str()
            .ok_or_else(|| anyhow!("Antigravity requires cwd"))?;
        let mut command = catalog::command(&self.spec, Some(cwd))?;
        command.env.extend(self.context.env.clone());
        command.args.extend(
            [
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--add-dir",
                cwd,
                "--disable-slash-commands",
                "--dangerously-skip-permissions",
            ]
            .map(str::to_owned),
        );
        if let Some(model) = self.config["model"].as_str().filter(|s| !s.is_empty()) {
            command.args.extend(["--model".into(), model.into()]);
        }
        if let Some(id) = &self.conversation {
            command.args.extend(["--conversation".into(), id.clone()]);
        }
        let (process, stdout) = ManagedProcess::spawn(command)
            .await
            .map_err(|e| Error::new("SPAWN_ERROR", e.to_string()))?;
        let mut driver = Driver {
            process,
            lines: BufReader::new(stdout).lines(),
            selection,
        };
        let ready = async {
            loop {
                let Some(line) = driver.lines.next_line().await? else {
                    let status = driver.process.wait().await?;
                    let stderr = driver.process.stderr_tail();
                    let message = if stderr.trim().is_empty() {
                        format!("Antigravity exited ({status})")
                    } else {
                        stderr.trim().to_owned()
                    };
                    return Err(Error::new("PROCESS_EXIT", wire::diagnostic(&message)).into());
                };
                if line.trim().is_empty() {
                    continue;
                }
                match wire::decode(&line)? {
                    Frame::Init {
                        conversation_id,
                        ..
                    } => return Ok(conversation_id),
                    Frame::Result { result } => {
                        return Err(
                            Error::new("STARTUP_ERROR", wire::diagnostic(&result.error)).into()
                        );
                    }
                    _ => {}
                }
            }
        };
        let result: Result<String> = tokio::select! {
            _ = self.cancel.cancelled() => Err(anyhow!("Antigravity startup canceled")),
            _ = self.sink.cancellation().cancelled_owned() => Err(anyhow!("Antigravity startup canceled")),
            result = tokio::time::timeout(Duration::from_secs(30), ready) => match result {
                Ok(result) => result,
                Err(_) => Err(Error::new("STARTUP_TIMEOUT", "Antigravity did not initialize within 30 seconds").into()),
            }
        };
        let id = match result {
            Ok(id)
                if self
                    .conversation
                    .as_ref()
                    .is_none_or(|expected| *expected == id) =>
            {
                id
            }
            Ok(_) => {
                driver.stop(true).await?;
                return Err(Error::new(
                    "INVALID_RESUME",
                    "Antigravity restored a different conversation",
                )
                .into());
            }
            Err(error) => {
                driver.stop(true).await?;
                return Err(error);
            }
        };
        self.conversation = Some(id.clone());
        self.snapshot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .conversation = Some(id);
        self.driver = Some(driver);
        Ok(())
    }
    async fn publish(&self, event: Value) -> Result<()> {
        self.snapshot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .history
            .push(event.clone());
        self.sink.emit(event).await
    }
    async fn publish_all(&self, events: Vec<Value>) -> Result<()> {
        for event in events {
            self.publish(event).await?;
        }
        Ok(())
    }
    fn config_events(&self) -> Vec<Value> {
        vec![
            json!({"type":"mode_changed","provider":self.spec.id,"currentModeId":"full-access","availableModes":catalog::modes()}),
            json!({"type":"model_changed","provider":self.spec.id,"runtimeInfo":runtime_info(&self.spec.id, &self.context.agent_id, self.persist, self.conversation.as_deref(), &self.config)}),
            json!({"type":"thinking_option_changed","provider":self.spec.id,"thinkingOptionId":self.config["thinkingOptionId"]}),
        ]
    }
    async fn configure(&mut self, key: &str, value: Value) -> Result<()> {
        let mut config = self.config.clone();
        config[key] = value;
        catalog::validate(
            &config,
            &self.catalog.read().unwrap_or_else(|e| e.into_inner()),
        )?;
        self.config = config;
        self.snapshot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .config = self.config.clone();
        self.publish_all(self.config_events()).await
    }
    async fn start(&mut self, input: Value, options: Value) -> Result<Value> {
        if self.turn.is_some() {
            return Err(Error::new(
                "TURN_ACTIVE",
                "Antigravity already has an active turn; wait for it to finish",
            )
            .into());
        }
        if options.get("outputSchema").is_some() {
            return Err(anyhow!("Antigravity supports messages only"));
        }
        let (text, native) = self.encode_prompt(&input)?;
        self.ensure_driver().await?;
        let turn = Turn::new();
        let id = turn.id.clone();
        let client_id = options["clientMessageId"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let user = turn.timeline(&self.spec.id, json!({"type":"user_message","text":text,"messageId":client_id,"clientMessageId":client_id}));
        self.turn = Some(turn);
        self.publish(json!({"type":"turn_started","provider":self.spec.id,"turnId":id}))
            .await?;
        self.publish(user).await?;
        let prefix: Vec<_> = ["systemPrompt", "daemonAppendSystemPrompt"]
            .iter()
            .filter_map(|k| self.config[*k].as_str())
            .filter(|s| !s.trim().is_empty())
            .collect();
        let native = if self.first_message && !prefix.is_empty() {
            format!("{}\n\n{native}", prefix.join("\n\n"))
        } else {
            native
        };
        let result = self
            .driver
            .as_ref()
            .ok_or_else(|| anyhow!("Antigravity session is not ready"))?
            .process
            .write_json(&json!({"event":"user","message":{"content":native}}))
            .await;
        match result {
            Ok(()) => self.first_message = false,
            Err(error) => {
                self.failure(Error::new("STDIN_ERROR", error.to_string()).into())
                    .await?
            }
        }
        Ok(json!({"turnId":id}))
    }
    fn encode_prompt(&mut self, input: &Value) -> Result<(String, String)> {
        if input.is_string() {
            let text = prompt::text(input)?;
            return Ok((text.clone(), text));
        }
        let parts = input
            .as_array()
            .ok_or_else(|| anyhow!("Antigravity supports messages only"))?;
        // Validate every part before creating any private files, as the original does.
        for part in parts {
            match part["type"].as_str() {
                Some("text") if part["text"].is_string() => {}
                Some("image") if part["data"].is_string() => {
                    validate_image_mime(part["mimeType"].as_str().unwrap_or(""))?;
                }
                _ => return Err(anyhow!("Antigravity supports text and image content only")),
            }
        }
        let text = parts
            .iter()
            .filter(|p| p["type"] == "text")
            .map(|p| prompt::text(&p["text"]))
            .collect::<Result<Vec<_>>>()?
            .join("\n");
        let mut native = Vec::new();
        for part in parts {
            if part["type"] == "text" {
                native.push(part["text"].as_str().unwrap_or("").to_owned());
            } else {
                let mime = part["mimeType"].as_str().unwrap_or("");
                if self.images.is_none() {
                    self.images = Some(
                        tempfile::Builder::new()
                            .prefix("paseo-antigravity-images-")
                            .tempdir()?,
                    );
                }
                let directory = self
                    .images
                    .as_ref()
                    .ok_or_else(|| anyhow!("Missing Antigravity image directory"))?;
                let path = prompt::materialize_unique_image_in(
                    part["data"].as_str().unwrap_or(""),
                    Some(mime),
                    directory.path(),
                )?;
                native.push(format!("Attached image: {}", path.display()));
            }
        }
        Ok((text, native.join("\n")))
    }
    async fn accept(&mut self, frame: Frame) -> Result<()> {
        let Some(turn) = self.turn.as_mut() else {
            return Ok(());
        };
        match frame {
            Frame::Step { step_update } => {
                let events = turn.step(step_update, &self.spec.id);
                self.publish_all(events).await
            }
            Frame::Result { result } => {
                let turn = self
                    .turn
                    .take()
                    .ok_or_else(|| anyhow!("Missing Antigravity turn"))?;
                if let Some(title) = wire::denial_title(&result.denied_actions, &mut self.denials) {
                    self.publish(notice(
                        &self.spec.id,
                        Some(&turn.id),
                        &title,
                        "Antigravity's own policy denied it.",
                    ))
                    .await?;
                }
                if result.status == Status::ERROR {
                    let canceled = result.error == "interrupted";
                    self.publish_all(
                        turn.finish(&self.spec.id, if canceled { "canceled" } else { "failed" }),
                    )
                    .await?;
                    self.publish(if canceled { json!({"type":"turn_canceled","provider":self.spec.id,"turnId":turn.id,"reason":wire::diagnostic(&result.error)}) }
                        else { json!({"type":"turn_failed","provider":self.spec.id,"turnId":turn.id,"error":wire::diagnostic(&result.error)}) }).await?;
                    if let Some(driver) = self.driver.take() {
                        driver.stop(true).await?;
                    }
                    Ok(())
                } else {
                    self.publish_all(turn.finish(&self.spec.id, "completed"))
                        .await?;
                    self.publish(
                        json!({"type":"turn_completed","provider":self.spec.id,"turnId":turn.id}),
                    )
                    .await
                }
            }
            Frame::Init { .. } => Ok(()),
        }
    }
    async fn failure(&mut self, error: anyhow::Error) -> Result<()> {
        let turn = self.turn.take();
        if let Some(turn) = &turn {
            self.publish_all(turn.finish(&self.spec.id, "failed"))
                .await?;
        }
        let mut event = json!({"type":"turn_failed","provider":self.spec.id,"error":error.to_string(),
            "code":error.downcast_ref::<Error>().map(|e| e.code).unwrap_or("ANTIGRAVITY_ERROR")});
        if let Some(turn) = turn {
            event["turnId"] = json!(turn.id);
        }
        self.publish(event).await?;
        if let Some(driver) = self.driver.take() {
            driver.stop(true).await?;
        }
        Ok(())
    }
    async fn stop(&mut self, close: bool) -> Result<()> {
        if let Some(driver) = self.driver.take() {
            driver.stop(!close).await?;
        }
        if let Some(turn) = self.turn.take() {
            self.publish_all(turn.finish(&self.spec.id, "canceled"))
                .await?;
            self.publish(json!({"type":"turn_canceled","provider":self.spec.id,"turnId":turn.id,"reason":"Canceled by provider"})).await?;
        }
        Ok(())
    }
}
fn runtime_info(
    provider: &str,
    id: &str,
    persist: bool,
    conversation: Option<&str>,
    config: &Value,
) -> Value {
    let id = if persist {
        conversation
            .map(|c| {
                catalog::persistence(provider, c)["sessionId"]
                    .as_str()
                    .unwrap_or(id)
                    .to_owned()
            })
            .unwrap_or_else(|| id.to_owned())
    } else {
        id.to_owned()
    };
    json!({"provider":provider,"sessionId":id,"model":config["model"],"modeId":"full-access","thinkingOptionId":config["thinkingOptionId"]})
}
fn notice(provider: &str, turn: Option<&str>, title: &str, description: &str) -> Value {
    let mut value = json!({"type":"timeline","provider":provider,"timestamp":chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "item":{"type":"notification","level":"warning","message":format!("{title}\n{description}")}});
    if let Some(turn) = turn {
        value["turnId"] = json!(turn);
    }
    value
}
fn validate_image_mime(mime: &str) -> Result<()> {
    let valid = mime.split_once('/').is_some_and(|(kind, subtype)| {
        kind.eq_ignore_ascii_case("image")
            && subtype
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
            && subtype
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".+-".contains(&b))
    });
    if !valid {
        return Err(anyhow!("Expected an image MIME type, received {mime}"));
    }
    Ok(())
}
