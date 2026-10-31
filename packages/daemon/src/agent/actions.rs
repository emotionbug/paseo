use super::*;

impl AgentManager {
    pub async fn send(&self, actor: &Value, id: &str, mut input: Value) -> Result<Value> {
        self.authorize(actor, id).await?;
        if actor["kind"] != "agent" && text(&input, "sourceAgentId").is_none() {
            return self.send_immediate(actor, id, input).await;
        }
        let caller = if actor["kind"] == "agent" {
            required(actor, "agentId")?
        } else {
            required(&input, "sourceAgentId")?
        }
        .to_owned();
        let caller_snapshot =
            if actor["kind"] == "agent" || input.get("sourceAgentServerId").is_none() {
                self.agent_snapshot(&caller).await?
            } else {
                None
            };
        let prompt = build_prompt(&input)?;
        let message_id = text(&input, "messageId")
            .or_else(|| text(&input, "clientMessageId"))
            .map(str::to_owned)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let mut source = json!({"kind":"agent-message","agentId":caller});
        if actor["kind"] != "agent" {
            if let Some(server) = input.get("sourceAgentServerId") {
                source["serverId"] = server.clone();
            }
        }
        if let Some(title) = caller_snapshot
            .as_ref()
            .and_then(|s| s.get("title"))
            .filter(|v| !v.is_null())
        {
            source["title"] = title.clone();
        }
        input["prompt"] = prompt.clone();
        input["source"] = source.clone();
        input["messageId"] = json!(message_id);
        if input.get("activeTurnBehavior").is_none() {
            input["activeTurnBehavior"] = json!("steer");
        }
        let registration = if actor["kind"] == "agent" && input["notifyOnFinish"] != false {
            Some(self.notifications.register(json!({"callerAgentId":caller,"childAgentId":id,"requireParentOwnership":false}),true).await?)
        } else {
            None
        };
        let mut queue = input.clone();
        queue["recipientId"] = json!(id);
        queue["clearPendingPermissions"] = json!(true);
        let request = receipt_request(&input, &prompt, Some(&source));
        let app = self.app.clone();
        let recipient = id.to_owned();
        let output_id = message_id.clone();
        let (admitted, admission) = oneshot::channel();
        tokio::spawn(async move {
            let id = recipient;
            let Some(state) = app.upgrade() else {
                let _ = admitted.send(Err(anyhow!("Agent manager is shutting down")));
                return Ok::<_, anyhow::Error>(());
            };
            let manager = state.agents.clone();
            drop(state);
            let admitted = Mutex::new(Some(admitted));
            let result = manager
                .receipts()?
                .send(
                    &id,
                    &message_id,
                    &request,
                    || async { Ok(()) },
                    |_| async {
                        let receipt = manager.message_delivery.enqueue(queue).await?;
                        if let Some(sender) = admitted.lock().await.take() {
                            let _ = sender.send(Ok(()));
                        }
                        receipt.dispatched(CancellationToken::new()).await?;
                        Ok(())
                    },
                )
                .await;
            if let Some(registration) = registration {
                if result.is_ok() {
                    registration.accept().await?;
                } else {
                    registration.reject().await?;
                }
            }
            if let Some(sender) = admitted.lock().await.take() {
                let _ = sender.send(
                    result
                        .as_ref()
                        .map(|_| ())
                        .map_err(|error| anyhow!(error.to_string())),
                );
            }
            if let Err(error) = result {
                tracing::error!(agent_id=%id,%error,"Queued agent message dispatch failed");
            }
            Ok::<_, anyhow::Error>(())
        });
        admission
            .await
            .map_err(|_| anyhow!("Agent message admission closed"))??;
        let status = match self.agents.read().await.get(id).cloned() {
            Some(cell) => cell.data.lock().await.snapshot["status"].clone(),
            None => json!("idle"),
        };
        Ok(json!({"deliveryStatus":"queued","status":status,"messageId":output_id}))
    }
    pub async fn send_immediate(&self, actor: &Value, id: &str, input: Value) -> Result<Value> {
        let manager = self.app()?.agents.clone();
        let actor = actor.clone();
        let id = id.to_owned();
        // Keep native admission owned after the requesting connection disappears.
        tokio::spawn(async move { manager.send_immediate_owned(&actor, &id, input).await })
            .await
            .context("Agent send operation terminated")?
    }
    async fn send_immediate_owned(&self, actor: &Value, id: &str, input: Value) -> Result<Value> {
        self.authorize(actor, id).await?;
        let prompt = build_prompt(&input)?;
        let source = if actor["kind"] == "agent" {
            let caller = required(actor, "agentId")?;
            let snapshot = self.agent_snapshot(caller).await?;
            let mut source = json!({"kind":"agent-message","agentId":caller});
            if let Some(title) = snapshot
                .as_ref()
                .and_then(|v| v.get("title"))
                .filter(|v| v.is_string())
            {
                source["title"] = title.clone();
            }
            Some(source)
        } else if actor["kind"] == "system" {
            input.get("source").cloned()
        } else {
            None
        };
        let prepared = input["preparedAgentMessage"] == true
            && actor["kind"] == "system"
            && actor["source"] == "agent-message-delivery";
        let message_id = text(&input, "messageId")
            .or_else(|| text(&input, "clientMessageId"))
            .map(str::to_owned);
        let request = receipt_request(&input, &prompt, source.as_ref());
        let unarchive = input["unarchive"] != false;
        let mut run_input = input;
        let (prompt, client_id) = if prepared {
            (prompt, message_id.clone())
        } else {
            notifications::prepare_agent_message(prompt, source.as_ref(), message_id.as_deref())?
        };
        run_input["prompt"] = prompt;
        if let Some(client_id) = client_id {
            run_input["clientMessageId"] = json!(client_id);
        }
        let snapshot = self
            .agent_snapshot(id)
            .await?
            .ok_or_else(|| anyhow!("Agent '{id}' not found"))?;
        if snapshot["archivedAt"].is_string() && !unarchive {
            return Ok(json!({"disposition":"turn_started"}));
        }
        let result = Mutex::new(None);
        if let Some(message_id) = message_id
            .as_deref()
            .filter(|_| !prepared && snapshot["internal"] != true)
        {
            self.receipts()?
                .send(
                    id,
                    message_id,
                    &request,
                    || async { self.prepare_send(id, &run_input).await },
                    |cell| {
                        let run_input = &run_input;
                        let result = &result;
                        async move {
                            let accepted = self.dispatch_prompt(id, &cell, run_input).await?;
                            *result.lock().await = Some(accepted);
                            Ok(())
                        }
                    },
                )
                .await?;
            Ok(result.into_inner().unwrap_or(
                json!({"disposition":"turn_started","deduplicated":true,"messageId":message_id}),
            ))
        } else {
            let cell = self.prepare_send(id, &run_input).await?;
            self.dispatch_prompt(id, &cell, &run_input).await
        }
    }
    async fn prepare_send(&self, id: &str, input: &Value) -> Result<Arc<AgentCell>> {
        let snapshot = self
            .agent_snapshot(id)
            .await?
            .ok_or_else(|| anyhow!("Agent '{id}' not found"))?;
        if snapshot["archivedAt"].is_string() {
            self.unarchive(id, &json!({})).await?;
        }
        let cell = self.ensure_loaded(id).await?;
        let snapshot = cell.data.lock().await.snapshot.clone();
        if snapshot["title"].is_null() && snapshot["lastUserMessageAt"].is_null() {
            let prompt = match notifications::agent_message_command(&input["prompt"]) {
                Some(prompt) => prompt,
                None => crate::providers::prompt::text(&input["prompt"])?,
            };
            if let Some(title) = creation::title(None, &prompt) {
                self.update_snapshot(&cell, |snapshot| {
                    snapshot["title"] = json!(title);
                    Ok(())
                })
                .await?;
            }
        }
        Ok(cell)
    }
    async fn dispatch_prompt(
        &self,
        id: &str,
        cell: &Arc<AgentCell>,
        input: &Value,
    ) -> Result<Value> {
        let mutation = cell.mutation.lock().await;
        let result = self.dispatch_prompt_unlocked(id, cell, input).await;
        drop(mutation);
        if result.as_ref().is_err_and(|error| {
            error
                .downcast_ref::<crate::plugins::StaleProviderSessionError>()
                .is_some()
        }) {
            self.reload(id, json!({}), false).await?;
            let replacement = self.cell(id).await?;
            let _mutation = replacement.mutation.lock().await;
            return self.dispatch_prompt_unlocked(id, &replacement, input).await;
        }
        result
    }
    async fn dispatch_prompt_unlocked(
        &self,
        id: &str,
        cell: &Arc<AgentCell>,
        input: &Value,
    ) -> Result<Value> {
        let session = cell
            .data
            .lock()
            .await
            .session
            .clone()
            .ok_or_else(|| anyhow!("Agent has no managed session"))?;
        let prompt = &input["prompt"];
        let mut options = input.get("runOptions").cloned().unwrap_or(json!({}));
        for key in ["clientMessageId", "outputSchema", "spokenInput"] {
            if let Some(value) = input.get(key) {
                options[key] = value.clone();
            }
        }
        let client_id = text(&options, "clientMessageId").map(str::to_owned);
        let before = cell.data.lock().await.snapshot.clone();
        if input["requireIdle"] == true && is_busy(&before) {
            bail!("Agent {id} already has an active run");
        }
        if let Some(mode) = text(input, "sessionMode") {
            self.change_mode(cell, session.as_ref(), mode).await?;
        }
        let command = notifications::agent_message_command(&prompt).map(Value::String);
        self.drain_events(cell).await?;
        cell.data.lock().await.staged_events = Some(Vec::new());
        let out_of_band = session
            .try_out_of_band(command.as_ref().unwrap_or(&prompt))
            .await;
        let submitted = if matches!(out_of_band, Ok(true)) {
            self.record_submitted(id, cell, prompt, client_id.as_deref(), None, false)
                .await
        } else {
            Ok(())
        };
        let released = self.release_event_stage(cell).await;
        submitted?;
        released?;
        if out_of_band? {
            return Ok(json!({"disposition":"out_of_band"}));
        }
        if is_busy(&before) && input["activeTurnBehavior"] == "steer" {
            let expected = text(&before, "activeTurnId").map(str::to_owned);
            let mut steer_options = options.clone();
            steer_options["expectedTurnId"] = json!(expected);
            steer_options["clearPendingPermissions"] =
                json!(input["clearPendingPermissions"] == true);
            self.drain_events(cell).await?;
            cell.data.lock().await.staged_events = Some(Vec::new());
            let steered = session
                .steer_active_turn(prompt.clone(), steer_options)
                .await;
            if let Err(error) = steered {
                self.release_event_stage(cell).await?;
                return Err(error);
            }
            let steered = steered?;
            if steered["status"] == "accepted" || steered["status"] == "steered" {
                let submitted = self
                    .record_submitted(
                        id,
                        cell,
                        prompt,
                        client_id.as_deref(),
                        expected.as_deref(),
                        false,
                    )
                    .await;
                let released = self.release_event_stage(cell).await;
                submitted?;
                released?;
                return Ok(json!({"disposition":"steered"}));
            }
            self.release_event_stage(cell).await?;
            if !matches!(
                text(&steered, "status"),
                Some("unavailable" | "inactive" | "not_running")
            ) {
                bail!("Provider did not accept steering: {steered}");
            }
            let current = cell.data.lock().await.snapshot.clone();
            if is_busy(&current) && text(&current, "activeTurnId") != expected.as_deref() {
                bail!("Active turn changed before steering could be delivered");
            }
        }
        let busy = { is_busy(&cell.data.lock().await.snapshot) };
        if busy {
            // replaceAgentRun: the replacing prompt holds the agent busy while the
            // interrupted turn settles, so clients never observe an idle frame.
            cell.data.lock().await.pending_replacement = true;
            let replaced = async {
                self.update_snapshot(cell, |snapshot| {
                    snapshot["lifecycle"] = json!("running");
                    Ok(())
                })
                .await?;
                self.interrupt_cell(id, cell).await
            }
            .await;
            let refused = replaced
                .as_ref()
                .is_ok_and(|result| result["status"] == "refused");
            if refused || replaced.is_err() {
                cell.data.lock().await.pending_replacement = false;
            }
            if refused {
                bail!(
                    "Cannot replace agent {id} because its active run cancellation was not acknowledged"
                );
            }
            replaced?;
        }
        if let Err(error) = self.drain_events(cell).await {
            cell.data.lock().await.pending_replacement = false;
            return Err(error);
        }
        cell.data.lock().await.staged_events = Some(Vec::new());
        let pending_id = uuid::Uuid::new_v4().to_string();
        let preparing = self
            .update_snapshot(cell, |snapshot| {
                snapshot["lifecycle"] = json!("running");
                snapshot["activeForegroundTurnId"] = json!(pending_id);
                object(snapshot).remove("lastError");
                Ok(())
            })
            .await;
        if let Err(error) = preparing {
            cell.data.lock().await.pending_replacement = false;
            let _ = self.release_event_stage(cell).await;
            return Err(error);
        }
        let accepted = session.start_turn(prompt.clone(), options).await;
        cell.data.lock().await.pending_replacement = false;
        match accepted {
            Ok(accepted) => {
                let admitted = async {
                    let turn_id = required(&accepted, "turnId")?.to_owned();
                    self.update_snapshot(cell, |snapshot| {
                        snapshot["lifecycle"] = json!("running");
                        snapshot["activeForegroundTurnId"] = json!(turn_id);
                        snapshot["activeTurnId"] = json!(turn_id);
                        snapshot["activeTurn"] = json!({"turnId":turn_id,"startedAt":now()});
                        Ok(())
                    })
                    .await?;
                    // streamAgent publishes the accepted turn start before the canonical prompt;
                    // the provider's duplicate start for this turn is suppressed on ingestion.
                    self.publish_stream(
                        id,
                        cell,
                        json!({"type":"turn_started","provider":session.provider(),"turnId":turn_id}),
                        None,
                    )
                    .await?;
                    self.record_submitted(
                        id,
                        cell,
                        prompt,
                        client_id.as_deref(),
                        Some(&turn_id),
                        false,
                    )
                    .await?;
                    Ok::<_, anyhow::Error>(turn_id)
                }
                .await;
                let released = self.release_event_stage(cell).await;
                let turn_id = admitted?;
                released?;
                Ok(json!({"disposition":"turn_started","turnId":turn_id}))
            }
            Err(error) => {
                self.release_event_stage(cell).await?;
                if error
                    .downcast_ref::<crate::plugins::StaleProviderSessionError>()
                    .is_some()
                {
                    // startPendingForegroundTurn: a stale session is reopened and retried by the
                    // caller, so the failed start is neither recorded nor surfaced as attention.
                    self.write_snapshot(cell, true, false, |snapshot| {
                        snapshot["lifecycle"] = json!("idle");
                        snapshot["activeForegroundTurnId"] = Value::Null;
                        snapshot["activeTurnId"] = Value::Null;
                        snapshot["activeTurn"] = Value::Null;
                        Ok(())
                    })
                    .await?;
                    return Err(error);
                }
                // A terminal rejection belongs after the submitted prompt even though no
                // provider turn exists.
                self.record_submitted(id, cell, prompt, client_id.as_deref(), None, true)
                    .await?;
                self.apply_event(
                    id,
                    cell,
                    json!({"type":"turn_failed","provider":session.provider(),"error":error.to_string()}),
                    false,
                )
                .await?;
                Err(error)
            }
        }
    }
    /// agent-manager.ts recordSubmittedPrompt: human attempts stay in history on
    /// rejection; delivery notices require acceptance.
    async fn record_submitted(
        &self,
        id: &str,
        cell: &Arc<AgentCell>,
        prompt: &Value,
        client_id: Option<&str>,
        turn: Option<&str>,
        rejected: bool,
    ) -> Result<()> {
        let Some(client_id) = client_id else {
            return Ok(());
        };
        if cell
            .timeline
            .lock()
            .map_err(|_| anyhow!("Timeline lock poisoned"))?
            .submitted_user_message(client_id)
            .is_some()
        {
            return Ok(());
        }
        let submitted = json!({"type":"user_message","text":crate::providers::prompt::text(prompt)?,"clientMessageId":client_id,"messageId":client_id});
        let provider = cell.data.lock().await.snapshot["provider"].clone();
        for item in notifications::project_agent_messages(&submitted) {
            let user_message = item["type"] == "user_message";
            if rejected && !user_message {
                continue;
            }
            let event = json!({"type":"timeline","provider":provider,"item":item,"turnId":turn});
            let row = self.append_to_cell(id, cell, item, &event).await?;
            self.update_snapshot(cell, |snapshot| {
                if user_message {
                    snapshot["lastUserMessageAt"] = json!(now());
                }
                Ok(())
            })
            .await?;
            self.publish_stream(id, cell, event, Some(row)).await?;
        }
        Ok(())
    }
    pub(super) async fn update_snapshot<F>(&self, cell: &Arc<AgentCell>, update: F) -> Result<Value>
    where
        F: FnOnce(&mut Value) -> Result<()>,
    {
        self.write_snapshot(cell, true, true, update).await
    }
    /// Commits a live snapshot. `touch` mirrors touchUpdatedAt; `check_attention` mirrors
    /// emitState's checkAndSetAttention edge detection.
    pub(super) async fn write_snapshot<F>(
        &self,
        cell: &Arc<AgentCell>,
        touch: bool,
        check_attention: bool,
        update: F,
    ) -> Result<Value>
    where
        F: FnOnce(&mut Value) -> Result<()>,
    {
        let (snapshot, attention) = {
            let _commit = cell.commit.lock().await;
            let mut snapshot = cell.data.lock().await.snapshot.clone();
            let previous = snapshot["lifecycle"].as_str().map(str::to_owned);
            update(&mut snapshot)?;
            if touch {
                snapshot["updatedAt"] = json!(next_updated_at(&snapshot["updatedAt"]));
            }
            snapshot["status"] = snapshot["lifecycle"].clone();
            let attention = if check_attention {
                runtime::transition_attention(previous.as_deref(), &mut snapshot)
            } else {
                None
            };
            let ready = cell.data.lock().await.ready;
            if ready {
                self.persist_snapshot(&snapshot).await?;
            }
            cell.data.lock().await.snapshot = snapshot.clone();
            let _ = self
                .events
                .send(json!({"type":"agent_state","agent":snapshot}));
            (snapshot, attention.filter(|_| ready))
        };
        if let Some(reason) = attention {
            let id = required(&snapshot, "id")?.to_owned();
            self.publish_stream(
                &id,
                cell,
                json!({"type":"attention_required","provider":snapshot["provider"],"reason":reason,"timestamp":snapshot["attentionTimestamp"]}),
                None,
            )
            .await?;
        }
        Ok(snapshot)
    }
    async fn change_mode(
        &self,
        cell: &Arc<AgentCell>,
        session: &dyn AgentSession,
        mode: &str,
    ) -> Result<(Value, Option<Value>)> {
        let notice = session.set_mode(mode).await?;
        self.drain_events(cell).await?;
        let observed = session
            .current_mode()
            .await?
            .unwrap_or_else(|| mode.to_owned());
        let info = session.runtime_info().await?;
        let snapshot = self
            .update_snapshot(cell, |snapshot| {
                snapshot["currentModeId"] = json!(observed);
                snapshot["config"]["modeId"] = json!(observed);
                snapshot["runtimeInfo"] = info;
                if let Some(handle) = session.describe_persistence() {
                    snapshot["persistence"] = with_cwd(handle, &snapshot["cwd"]);
                }
                Ok(())
            })
            .await?;
        Ok((snapshot, notice))
    }
    async fn change_model(
        &self,
        cell: &Arc<AgentCell>,
        session: &dyn AgentSession,
        model: Option<&str>,
    ) -> Result<Value> {
        let model = model.filter(|value| !value.trim().is_empty());
        session.set_model(model).await?;
        self.drain_events(cell).await?;
        self.update_snapshot(cell, |snapshot| {
            if let Some(model) = model {
                snapshot["config"]["model"] = json!(model);
            } else {
                object(&mut snapshot["config"]).remove("model");
            }
            if snapshot["runtimeInfo"].is_object() {
                snapshot["runtimeInfo"]["model"] = json!(model);
            }
            if let Some(handle) = session.describe_persistence() {
                snapshot["persistence"] = with_cwd(handle, &snapshot["cwd"]);
            }
            Ok(())
        })
        .await
    }
    async fn change_thinking(
        &self,
        cell: &Arc<AgentCell>,
        session: &dyn AgentSession,
        thinking: Option<&str>,
    ) -> Result<(Value, Option<Value>)> {
        let thinking = thinking.filter(|value| !value.trim().is_empty());
        let notice = session.set_thinking_option(thinking).await?;
        self.drain_events(cell).await?;
        let info = session.runtime_info().await?;
        let effective = info
            .get("thinkingOptionId")
            .cloned()
            .unwrap_or(json!(thinking));
        let snapshot = self
            .update_snapshot(cell, |snapshot| {
                if effective.is_null() {
                    object(&mut snapshot["config"]).remove("thinkingOptionId");
                } else {
                    snapshot["config"]["thinkingOptionId"] = effective.clone();
                }
                if snapshot["runtimeInfo"].is_object() {
                    snapshot["runtimeInfo"]["thinkingOptionId"] = effective;
                }
                Ok(())
            })
            .await?;
        Ok((snapshot, notice))
    }
    async fn change_feature(
        &self,
        cell: &Arc<AgentCell>,
        session: &dyn AgentSession,
        feature: &str,
        value: Value,
    ) -> Result<Value> {
        session.set_feature(feature, value.clone()).await?;
        self.drain_events(cell).await?;
        self.update_snapshot(cell, |snapshot| {
            snapshot["config"]["featureValues"][feature] = value;
            snapshot["features"] = json!(session.features().as_ref());
            Ok(())
        })
        .await
    }
    async fn interrupt_cell(&self, id: &str, cell: &Arc<AgentCell>) -> Result<Value> {
        let snapshot = cell.data.lock().await.snapshot.clone();
        if !is_busy(&snapshot) {
            return Ok(json!({"status":"not_running"}));
        }
        let session = cell
            .data
            .lock()
            .await
            .session
            .clone()
            .ok_or_else(|| anyhow!("Agent has no session"))?;
        let mut events = self.subscribe_events();
        let acknowledged = matches!(
            tokio::time::timeout(Duration::from_secs(2), session.interrupt()).await,
            Ok(Ok(()))
        );
        self.drain_events(cell).await?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        let mut settled = false;
        loop {
            if run_settled(&*cell.data.lock().await) {
                settled = true;
                break;
            }
            tokio::select! {_=tokio::time::sleep_until(deadline)=>break,event=events.recv()=>match event{Ok(_)|Err(broadcast::error::RecvError::Lagged(_))=>{},Err(_)=>break}}
        }
        if !acknowledged {
            return Ok(json!({"status":if settled {"settled"} else {"refused"}}));
        }
        if !settled {
            self.apply_event(id,cell,json!({"type":"turn_canceled","provider":snapshot["provider"],"reason":"interrupted","turnId":snapshot["activeTurnId"]}),false).await?;
        }
        // cancelAgentRunNow: requests still pending after an acknowledged cancel are denied.
        let (provider, pending) = {
            let data = cell.data.lock().await;
            (
                data.snapshot["provider"].clone(),
                data.snapshot["pendingPermissions"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default(),
            )
        };
        for request in pending {
            if let Some(request_id) = text(&request, "id") {
                self.apply_event(id,cell,json!({"type":"permission_resolved","provider":provider,"requestId":request_id,"resolution":{"behavior":"deny","message":"Interrupted"}}),false).await?;
            }
        }
        Ok(json!({"status":"settled"}))
    }
    pub async fn wait(
        &self,
        actor: &Value,
        id: &str,
        options: Value,
        cancel: CancellationToken,
    ) -> Result<Value> {
        self.authorize(actor, id).await?;
        if self
            .agent_snapshot(id)
            .await?
            .is_some_and(|snapshot| !snapshot["archivedAt"].is_string())
        {
            self.ensure_loaded(id).await?;
        }
        self.message_delivery
            .wait_for_dispatch(id, cancel.clone())
            .await?;
        let mut events = self.subscribe_events();
        let timeout = options["timeoutMs"].as_u64().map(Duration::from_millis);
        let started = tokio::time::Instant::now();
        loop {
            if cancel.is_cancelled() {
                bail!("wait_for_agent aborted");
            }
            let snapshot = self
                .agent_snapshot(id)
                .await?
                .ok_or_else(|| anyhow!("Agent {id} not found"))?;
            let permission = snapshot["pendingPermissions"]
                .as_array()
                .and_then(|v| v.first())
                .cloned()
                .unwrap_or(Value::Null);
            let start_only = options["phase"] == "start";
            if (!start_only && (!is_busy(&snapshot) || !permission.is_null()))
                || (start_only
                    && (snapshot["activeTurn"].is_object() || snapshot["status"] == "error"))
            {
                return Ok(
                    json!({"status":snapshot["status"],"permission":permission,"lastMessage":self.last_assistant_message(id).await?}),
                );
            }
            let remaining = timeout.map(|limit| limit.saturating_sub(started.elapsed()));
            tokio::select! {_=cancel.cancelled()=>bail!("wait_for_agent aborted"),_=async{match remaining{Some(duration)=>tokio::time::sleep(duration).await,None=>std::future::pending::<()>().await}}=>bail!("wait_for_agent timed out"),event=events.recv()=>match event{Ok(_)=>{},Err(broadcast::error::RecvError::Lagged(_))=>{},Err(_)=>bail!("Agent observation closed")}}
        }
    }
    pub async fn schedule_run(&self, id: &str, prompt: Value) -> Result<Value> {
        let actor = json!({"kind":"system","source":"schedule"});
        let cell = self.prepare_send(id, &json!({"prompt":prompt})).await?;
        self.dispatch_prompt(
            id,
            &cell,
            &json!({"prompt":prompt,"requireIdle":true,"activeTurnBehavior":"interrupt"}),
        )
        .await?;
        let mut result = self
            .wait(&actor, id, json!({}), self.app()?.cancel.child_token())
            .await?;
        result["finalText"] = result["lastMessage"].clone();
        result["timeline"] = json!(self.timeline_items(id).await?);
        result["canceled"] =
            json!(cell.data.lock().await.snapshot["lastTurnOutcome"] == "turn_canceled");
        Ok(result)
    }
    pub async fn pending_permissions(&self) -> Result<Vec<Value>> {
        let cells = self
            .agents
            .read()
            .await
            .iter()
            .map(|(id, cell)| (id.clone(), cell.clone()))
            .collect::<Vec<_>>();
        let mut out = Vec::new();
        for (id, cell) in cells {
            let data = cell.data.lock().await;
            if !data.ready || data.session.is_none() {
                continue;
            }
            if let Some(requests) = data.snapshot["pendingPermissions"].as_array() {
                for request in requests {
                    out.push(
                        json!({"agentId":id,"status":data.snapshot["status"],"request":request}),
                    );
                }
            }
        }
        Ok(out)
    }
    async fn resume_handle(
        &self,
        actor: &Value,
        requested_id: &str,
        input: &Value,
    ) -> Result<Value> {
        let handle = &input["handle"];
        let provider = required(handle, "provider")?;
        let native_id = required(handle, "sessionId")?;
        let key = format!("resume\0{provider}\0{native_id}");
        let lane = self.lane(&key).await;
        let _lane = lane.lock().await;
        let records = self
            .storage()?
            .list_by_provider_session(provider, native_id)
            .await?;
        let original = if !requested_id.is_empty() {
            self.agent_snapshot(requested_id).await?
        } else {
            records.into_iter().next()
        };
        let id = original
            .as_ref()
            .and_then(|record| text(record, "id"))
            .map(str::to_owned)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        if original.is_some() {
            self.authorize(actor, &id).await?;
            self.unarchive(&id, &json!({})).await?;
        } else {
            let mut config = handle.get("metadata").cloned().unwrap_or(json!({}));
            merge(&mut config, &input["overrides"]);
            config["provider"] = json!(provider);
            required(&config, "cwd")?;
            let timestamp = now();
            let mut record = json!({"id":id,"provider":provider,"cwd":config["cwd"],"config":config,"persistence":handle,"createdAt":timestamp,"updatedAt":timestamp,"lastStatus":"closed","labels":{},"title":null,"archivedAt":null});
            if let Some(workspace) = input.get("workspaceId") {
                record["workspaceId"] = workspace.clone();
            }
            if actor["kind"] == "daemon" {
                record["owner"] = actor.clone();
            }
            self.storage()?.upsert(record).await?;
        }
        let result = async {
            let snapshot = self
                .resume(
                    &id,
                    input.get("overrides").cloned().unwrap_or(json!({})),
                    false,
                )
                .await?;
            let cell = self.cell(&id).await?;
            self.hydrate_history(&id, &cell, false).await?;
            Ok::<_, anyhow::Error>(
                json!({"snapshot":snapshot,"timelineSize":self.timeline_items(&id).await?.len()}),
            )
        }
        .await;
        if result.is_err() {
            if let Ok(cell) = self.cell(&id).await {
                self.discard_failed_registration(&id, &cell).await;
            }
            if let Some(original) = original {
                self.storage()?.upsert(original).await?;
            } else {
                self.storage()?.remove(&id).await?;
            }
        }
        result
    }
    pub async fn control(
        &self,
        actor: &Value,
        id: &str,
        action: &str,
        input: Value,
    ) -> Result<Value> {
        if action == "commands" {
            return Ok(json!(
                self.commands(
                    (!id.is_empty()).then_some(id),
                    input.get("draftConfig").unwrap_or(&input)
                )
                .await?
            ));
        }
        if action == "resume" && input["handle"].is_object() {
            return self.resume_handle(actor, id, &input).await;
        }
        if action == "delete" && self.agent_snapshot(id).await?.is_none() {
            return Ok(json!({"deleted":false}));
        }
        self.authorize(actor, id).await?;
        if matches!(action, "reload" | "refresh" | "resume") {
            if let Ok(cell) = self.cell(id).await {
                let result = self.interrupt_cell(id, &cell).await?;
                if result["status"] == "refused" {
                    bail!(
                        "Cannot reload agent {id} because its active run cancellation was not acknowledged"
                    );
                }
            }
            let overrides = input.get("overrides").cloned().unwrap_or(json!({}));
            let snapshot = if action == "resume" {
                self.resume(id, overrides, input["rehydrateFromDisk"] == true)
                    .await?
            } else {
                self.reload(
                    id,
                    overrides,
                    input["rehydrateFromDisk"] == true || action == "refresh",
                )
                .await?
            };
            return Ok(
                json!({"snapshot":snapshot,"timelineSize":self.timeline_items(id).await?.len()}),
            );
        }
        if action == "archive" {
            return Box::pin(self.archive(actor, id)).await;
        }
        if action == "unarchive" {
            return self.unarchive(id, &input).await;
        }
        if action == "delete" || action == "close" {
            let lane = self.lane(id).await;
            let _lane = lane.lock().await;
            if action == "delete" {
                // Fence storage first so the close's final persist cannot recreate the record.
                self.storage()?.begin_delete(id).await?;
            }
            if let Ok(cell) = self.cell(id).await {
                if let Err(error) = self.close_cell(id, &cell).await {
                    if action != "delete" {
                        return Err(error);
                    }
                    tracing::warn!(agent_id = id, %error, "Failed to close agent {id} during delete");
                }
            }
            if action == "delete" {
                self.storage()?.remove(id).await?;
                let app = self.app()?;
                for path in [
                    self.timeline_path(id)?,
                    app.home.join("agent-subagents").join(format!("{id}.json")),
                ] {
                    match tokio::fs::remove_file(path).await {
                        Ok(()) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => return Err(e.into()),
                    }
                }
                self.subagent_store
                    .lock()
                    .map_err(|_| anyhow!("Subagent lock poisoned"))?
                    .delete_parent(id);
                let _ = self
                    .events
                    .send(json!({"type":"agent_deleted","agentId":id}));
                app.automation.complete_for_agent(id).await?;
            }
            return Ok(json!({"closed":true,"deleted":action=="delete"}));
        }
        if matches!(
            action,
            "update" | "set_labels" | "clear_attention" | "mark_unread" | "detach"
        ) {
            let lane = self.lane(id).await;
            let _lane = lane.lock().await;
            // updateAgentCommand: an empty name is ignored and empty labels are absent.
            let title = text(&input, "name")
                .or_else(|| text(&input, "title"))
                .map(str::trim)
                .filter(|title| !title.is_empty())
                .map(str::to_owned);
            let labels = input
                .get("labels")
                .and_then(Value::as_object)
                .filter(|labels| !labels.is_empty())
                .cloned();
            if action == "update" && title.is_none() && labels.is_none() {
                bail!(NOTHING_TO_UPDATE);
            }
            let live = self.cell(id).await.ok();
            let stored = match &live {
                Some(_) => None,
                None => Some(
                    self.storage()?
                        .get(id)
                        .await?
                        .ok_or_else(|| anyhow!("Agent not found: {id}"))?,
                ),
            };
            let current = match &live {
                Some(cell) => cell.data.lock().await.snapshot.clone(),
                None => self
                    .agent_snapshot(id)
                    .await?
                    .ok_or_else(|| anyhow!("Agent not found: {id}"))?,
            };
            let has_parent = current["labels"]["paseo.parent-agent-id"]
                .as_str()
                .is_some_and(|parent| !parent.trim().is_empty());
            // clearAgentAttention only acts on unread agents, and detaching an agent without
            // a parent changes nothing.
            if (action == "clear_attention" && current["requiresAttention"] != true)
                || (action == "detach" && !has_parent)
            {
                return Ok(current);
            }
            if action == "mark_unread" {
                // markAgentUnread: only a finished, read agent without pending input.
                let finished = match &stored {
                    Some(record) => {
                        matches!(text(record, "lastStatus"), Some("idle" | "closed"))
                            && record["internal"] != true
                            && !record["archivedAt"].is_string()
                    }
                    None => {
                        current["lifecycle"] == "idle"
                            && current["pendingPermissions"]
                                .as_array()
                                .is_none_or(Vec::is_empty)
                    }
                };
                if !finished || current["requiresAttention"] == true {
                    bail!("Agent is no longer finished and read: {id}");
                }
            }
            // Live attention changes keep the activity stamp; stored-record writes
            // (workspace clear, markAgentUnread on a stored agent) stamp it.
            let touch = !matches!(action, "clear_attention" | "mark_unread") || stored.is_some();
            let update = |snapshot: &mut Value| -> Result<()> {
                if matches!(action, "update" | "set_labels") {
                    if let Some(title) = &title {
                        snapshot["title"] = json!(title);
                        snapshot["config"]["title"] = json!(title);
                    }
                    if let Some(labels) = &labels {
                        for (key, value) in labels {
                            if value.is_null() {
                                object(&mut snapshot["labels"]).remove(key);
                            } else {
                                object(&mut snapshot["labels"]).insert(key.clone(), value.clone());
                            }
                        }
                    }
                } else if action == "detach" {
                    let labels = object(&mut snapshot["labels"]);
                    labels.retain(|key, _| {
                        key != "paseo.parent-agent-id" && !key.starts_with("paseo.open-agent-tab.")
                    });
                } else {
                    let unread = action == "mark_unread";
                    let timestamp = if unread {
                        json!(now())
                    } else {
                        Value::Null
                    };
                    snapshot["requiresAttention"] = json!(unread);
                    snapshot["attentionReason"] = if unread {
                        json!("finished")
                    } else {
                        Value::Null
                    };
                    snapshot["attentionTimestamp"] = timestamp;
                    snapshot["attention"] = json!({"requiresAttention":unread,"attentionReason":snapshot["attentionReason"],"attentionTimestamp":snapshot["attentionTimestamp"]});
                }
                Ok(())
            };
            if let Some(cell) = &live {
                return self.write_snapshot(cell, touch, false, update).await;
            }
            let mut snapshot = current;
            update(&mut snapshot)?;
            if touch {
                snapshot["updatedAt"] = json!(next_updated_at(&snapshot["updatedAt"]));
                if action == "mark_unread" {
                    snapshot["attentionTimestamp"] = snapshot["updatedAt"].clone();
                    snapshot["attention"]["attentionTimestamp"] = snapshot["updatedAt"].clone();
                }
            }
            self.persist_snapshot(&snapshot).await?;
            let _ = self
                .events
                .send(json!({"type":"agent_state","agent":snapshot}));
            return Ok(snapshot);
        }
        let cell = self.ensure_loaded(id).await?;
        let _mutation = cell.mutation.lock().await;
        let session = cell
            .data
            .lock()
            .await
            .session
            .clone()
            .ok_or_else(|| anyhow!("Agent has no session"))?;
        match action {
            "cancel" | "interrupt" => self.interrupt_cell(id, &cell).await,
            "pending_permissions" => {
                Ok(cell.data.lock().await.snapshot["pendingPermissions"].clone())
            }
            "respond_permission" | "permission" => {
                let request_id = required(&input, "requestId")?;
                let response = input
                    .get("response")
                    .cloned()
                    .ok_or_else(|| anyhow!("Missing permission response"))?;
                let pending = cell.data.lock().await.snapshot["pendingPermissions"]
                    .as_array()
                    .is_some_and(|p| p.iter().any(|p| text(p, "id") == Some(request_id)));
                if !pending {
                    bail!("Permission request '{request_id}' not found");
                }
                let result = session
                    .respond_to_permission(request_id, response.clone())
                    .await?;
                self.drain_events(&cell).await?;
                let pending = cell.data.lock().await.snapshot["pendingPermissions"]
                    .as_array()
                    .is_some_and(|requests| {
                        requests
                            .iter()
                            .any(|request| text(request, "id") == Some(request_id))
                    });
                if pending {
                    let provider = session.provider().to_owned();
                    self.apply_event(id,&cell,json!({"type":"permission_resolved","provider":provider,"requestId":request_id,"resolution":response}),false).await?;
                }
                if let Some(prompt) = result
                    .as_ref()
                    .and_then(|result| result.get("followUpPrompt"))
                {
                    self.dispatch_prompt_unlocked(id, &cell, &json!({"prompt":prompt}))
                        .await?;
                }
                Ok(result.unwrap_or(Value::Null))
            }
            "set_mode" => {
                let (snapshot, notice) = self
                    .change_mode(&cell, session.as_ref(), required(&input, "modeId")?)
                    .await?;
                Ok(json!({"agent":snapshot,"notice":notice}))
            }
            "set_model" => {
                let snapshot = self
                    .change_model(
                        &cell,
                        session.as_ref(),
                        text(&input, "model").or_else(|| text(&input, "modelId")),
                    )
                    .await?;
                Ok(json!({"agent":snapshot}))
            }
            "set_thinking" => {
                let (snapshot, notice) = self
                    .change_thinking(&cell, session.as_ref(), text(&input, "thinkingOptionId"))
                    .await?;
                Ok(json!({"agent":snapshot,"notice":notice}))
            }
            "set_feature" => {
                let snapshot = self
                    .change_feature(
                        &cell,
                        session.as_ref(),
                        required(&input, "featureId")?,
                        input
                            .get("value")
                            .cloned()
                            .ok_or_else(|| anyhow!("Missing feature value"))?,
                    )
                    .await?;
                Ok(json!({"agent":snapshot}))
            }
            "config_apply" => {
                let configuration = input.get("config").unwrap_or(&input);
                let mut notice = None;
                if let Some(model) = configuration
                    .get("modelId")
                    .or_else(|| configuration.get("model"))
                {
                    self.change_model(&cell, session.as_ref(), model.as_str())
                        .await?;
                }
                if let Some(mode) = text(configuration, "modeId") {
                    notice = self.change_mode(&cell, session.as_ref(), mode).await?.1;
                }
                if let Some(thinking) = configuration.get("thinkingOptionId") {
                    let current = self
                        .change_thinking(&cell, session.as_ref(), thinking.as_str())
                        .await?
                        .1;
                    if notice.is_none() {
                        notice = current;
                    }
                }
                if let Some(features) = configuration
                    .get("featureValues")
                    .and_then(Value::as_object)
                {
                    for (feature, value) in features {
                        self.change_feature(&cell, session.as_ref(), feature, value.clone())
                            .await?;
                    }
                }
                Ok(json!({"notice":notice}))
            }
            "rewind" => {
                let mode = text(&input, "mode")
                    .or_else(|| text(&input, "target"))
                    .unwrap_or("conversation");
                let target = match mode {
                    "conversation" => RewindTarget::Conversation,
                    "files" => RewindTarget::Files,
                    "both" => RewindTarget::Both,
                    _ => bail!("Invalid rewind target"),
                };
                let requested = required(&input, "messageId")?;
                // agent-manager.ts rewind: a daemon-submitted prompt rewinds through its
                // provider id; any other id is the provider's own message id.
                let submitted = cell
                    .timeline
                    .lock()
                    .map_err(|_| anyhow!("Timeline lock poisoned"))?
                    .rows()
                    .into_iter()
                    .find(|row| {
                        row["item"]["type"] == "user_message"
                            && text(&row["item"], "messageId") == Some(requested)
                            && text(&row["item"], "clientMessageId") == Some(requested)
                    });
                let provider_id = match &submitted {
                    Some(row) => text(row, "providerMessageId")
                        .ok_or_else(|| {
                            anyhow!(
                                "Cannot rewind before the provider acknowledges the submitted prompt"
                            )
                        })?
                        .to_owned(),
                    None => requested.to_owned(),
                };
                let settled = self.interrupt_cell(id, &cell).await?;
                if settled["status"] == "refused" {
                    bail!(
                        "Cannot rewind agent {id} because its active run cancellation was not acknowledged"
                    );
                }
                session.rewind(target, &provider_id).await?;
                if mode != "files" {
                    self.hydrate_history_for(id, &cell, true, text(&input, "_initiatingSourceId"))
                        .await?;
                }
                Ok(json!({"agent":cell.data.lock().await.snapshot}))
            }
            other => bail!("Unknown agent action '{other}'"),
        }
    }
    pub(super) async fn unarchive(&self, id: &str, updates: &Value) -> Result<Value> {
        let lane = self.lane(id).await;
        let _lane = lane.lock().await;
        let mut snapshot = self
            .agent_snapshot(id)
            .await?
            .ok_or_else(|| anyhow!("Agent not found"))?;
        if !snapshot["archivedAt"].is_string() {
            return Ok(json!({"unarchived":false}));
        }
        if let Some(handle) = snapshot.get("persistence").filter(|v| v.is_object()) {
            self.app()?
                .providers
                .get(required(&snapshot, "provider")?)?
                .unarchive_native_session(handle)
                .await?;
        }
        snapshot["archivedAt"] = Value::Null;
        snapshot["updatedAt"] = json!(next_updated_at(&snapshot["updatedAt"]));
        if let Some(workspace) = updates.get("workspaceId") {
            snapshot["workspaceId"] = workspace.clone();
        }
        if let Some(labels) = updates.get("labels").and_then(Value::as_object) {
            for (key, value) in labels {
                if value.is_null() {
                    object(&mut snapshot["labels"]).remove(key);
                } else {
                    snapshot["labels"][key] = value.clone();
                }
            }
        }
        self.persist_snapshot(&snapshot).await?;
        if let Ok(cell) = self.cell(id).await {
            cell.data.lock().await.snapshot = snapshot.clone();
            if snapshot["_historyOnly"] == true {
                self.close_cell(id, &cell).await?;
            }
        }
        let _ = self
            .events
            .send(json!({"type":"agent_state","agent":snapshot}));
        Ok(json!({"unarchived":true}))
    }
    async fn archive(&self, actor: &Value, id: &str) -> Result<Value> {
        let lane = self.lane(id).await;
        let _lane = lane.lock().await;
        let mut snapshot = self
            .agent_snapshot(id)
            .await?
            .ok_or_else(|| anyhow!("Agent not found"))?;
        let newly_archived = !snapshot["archivedAt"].is_string();
        let archived_at = text(&snapshot, "archivedAt")
            .map(str::to_owned)
            .unwrap_or_else(now);
        snapshot["archivedAt"] = json!(archived_at);
        snapshot["requiresAttention"] = json!(false);
        snapshot["attentionReason"] = Value::Null;
        snapshot["attentionTimestamp"] = Value::Null;
        snapshot["attention"] = json!({"requiresAttention":false});
        if let Ok(cell) = self.cell(id).await {
            self.update_snapshot(&cell, |s| {
                s["archivedAt"] = json!(archived_at);
                s["requiresAttention"] = json!(false);
                s["attentionReason"] = Value::Null;
                s["attentionTimestamp"] = Value::Null;
                s["attention"] = json!({"requiresAttention":false});
                Ok(())
            })
            .await?;
            self.close_cell(id, &cell).await?;
        } else {
            // buildArchivedAgentRecord: a stored-only archive keeps its activity stamp and
            // never records an in-flight status.
            if matches!(
                text(&snapshot, "lifecycle"),
                Some("running" | "initializing")
            ) {
                snapshot["lifecycle"] = json!("idle");
                snapshot["status"] = json!("idle");
            }
            self.persist_snapshot(&snapshot).await?;
            let _ = self
                .events
                .send(json!({"type":"agent_state","agent":snapshot}));
        }
        if let Some(handle) = snapshot.get("persistence").filter(|v| v.is_object()) {
            if let Ok(factory) = self.app()?.providers.get(required(&snapshot, "provider")?) {
                if let Err(error) = factory.archive_native_session(handle).await {
                    tracing::warn!(agent_id=id,%error,"Failed to archive native session");
                }
            }
        }
        self.app()?.automation.complete_for_agent(id).await?;
        if newly_archived && snapshot["internal"] != true {
            let _ = self
                .app()?
                .plugins
                .emit(
                    "agent.archived",
                    json!({"agent":runtime::hook_agent(&snapshot),"archivedAt":archived_at}),
                )
                .await;
        }
        for child in self.list_snapshots().await? {
            if child["archivedAt"].is_string() || child["labels"]["paseo.parent-agent-id"] != id {
                continue;
            }
            let child_id = required(&child, "id")?;
            let detach = child["workspaceId"].is_string()
                && snapshot["workspaceId"].is_string()
                && child["workspaceId"] != snapshot["workspaceId"]
                || child["labels"].as_object().is_some_and(|labels| {
                    labels.iter().any(|(key, value)| {
                        key.starts_with("paseo.open-agent-tab.") && value == "true"
                    })
                });
            if detach {
                Box::pin(self.control(
                    &json!({"kind":"system","source":"archive-cascade"}),
                    child_id,
                    "detach",
                    json!({}),
                ))
                .await?;
            } else {
                Box::pin(self.archive(actor, child_id)).await?;
            }
        }
        Ok(json!({"archivedAt":archived_at}))
    }
}
/// lifecycle-command.ts updateAgentCommand rejection.
pub(super) const NOTHING_TO_UPDATE: &str = "Nothing to update (provide name and/or labels)";
/// A cancel settles once the run is no longer busy; a pending replacement keeps the
/// agent busy, so its interrupted turn settles when no turn remains active.
fn run_settled(data: &AgentData) -> bool {
    !is_busy(&data.snapshot)
        || (data.pending_replacement
            && data.snapshot["activeTurnId"].is_null()
            && data.snapshot["activeForegroundTurnId"].is_null())
}
fn build_prompt(input: &Value) -> Result<Value> {
    if let Some(prompt) = input.get("prompt") {
        if prompt.is_string() || prompt.is_array() {
            return Ok(prompt.clone());
        }
        bail!("Prompt must be text or content blocks");
    }
    Ok(crate::providers::prompt::build(
        text(input, "text").unwrap_or(""),
        input["images"].as_array().map(Vec::as_slice).unwrap_or(&[]),
        input["attachments"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[]),
    ))
}
fn receipt_request(input: &Value, prompt: &Value, source: Option<&Value>) -> Value {
    let mut request = json!({"prompt":prompt,"activeTurnBehavior":text(input,"activeTurnBehavior").unwrap_or(if source.is_some(){"steer"}else{"interrupt"})});
    if let Some(source) = source {
        if let Some(id) = source.get("agentId") {
            request["sourceAgentId"] = id.clone();
        }
        if let Some(server) = source.get("serverId") {
            request["sourceAgentServerId"] = server.clone();
        }
    }
    if let Some(mode) = input.get("sessionMode") {
        request["sessionMode"] = mode.clone();
    }
    request
}
