use super::{
    permissions, projection,
    session::{CodexSession, State},
};
use anyhow::{Result, anyhow, bail};
use base64::Engine;
use serde_json::{Value, json};
use std::collections::VecDeque;

impl CodexSession {
    pub(super) async fn handle_frame(&self, frame: Value) -> Result<()> {
        let mut queue = VecDeque::from([frame]);
        while let Some(frame) = queue.pop_front() {
            let method = frame["method"]
                .as_str()
                .ok_or_else(|| anyhow!("Codex notification is missing its method"))?;
            if let Some(id) = frame.get("id") {
                if !id.is_number() {
                    self.client
                        .reject(id.clone(), "Codex server request IDs must be numeric")
                        .await?;
                    continue;
                }
                let params = frame["params"].clone();
                match permissions::request(&self.spec.id, id.clone(), method, params) {
                    Ok(pending) => {
                        if let Some(reply) = permissions::automatic_reply(&pending) {
                            self.client.reply(id.clone(), reply).await?;
                            continue;
                        }
                        let question = matches!(
                            pending.method.as_str(),
                            "item/tool/requestUserInput" | "tool/requestUserInput"
                        );
                        let timeline = if question {
                            Some(permissions::question_timeline(&pending, None)?)
                        } else {
                            None
                        };
                        let request = pending.request.clone();
                        self.state
                            .lock()
                            .unwrap()
                            .pending
                            .insert(pending.id.clone(), pending);
                        if let Some(item) = timeline {
                            self.emit(json!({"type":"timeline","item":item})).await?;
                        }
                        self.emit(json!({"type":"permission_requested","request":request}))
                            .await?;
                    }
                    Err(error) => self.client.reject(id.clone(), &error.to_string()).await?,
                }
                continue;
            }
            if method == "serverRequest/resolved" {
                let native_id = {
                    let s = self.state.lock().unwrap();
                    s.pending
                        .values()
                        .find(|p| {
                            p.method == "mcpServer/elicitation/request"
                                && p.native_id.as_ref() == frame["params"].get("requestId")
                        })
                        .and_then(|p| p.native_id.clone())
                };
                if let Some(native_id) = native_id {
                    self.client
                        .reply(
                            native_id,
                            json!({"action":"cancel","content":null,"_meta":null}),
                        )
                        .await?;
                }
            }
            let source_id = frame["params"]["itemId"]
                .as_str()
                .or(frame["params"]["item"]["id"].as_str())
                .map(str::to_owned);
            let delta = method.ends_with("/delta") || method.ends_with("TextDelta");
            let (mut events, replay) = self.notification(frame)?;
            let updates = self.aggregate_children(&events, source_id.as_deref(), delta)?;
            events.extend(updates);
            self.changed.notify_waiters();
            for event in events {
                self.emit(event).await?;
            }
            queue.extend(replay);
        }
        Ok(())
    }
    fn notification(&self, frame: Value) -> Result<(Vec<Value>, Vec<Value>)> {
        let original = frame["method"].as_str().unwrap_or("");
        let mut method = original.to_owned();
        let mut params = frame["params"].clone();
        if method.starts_with("codex/event/") {
            let message = params["msg"].clone();
            let thread = params
                .get("threadId")
                .or(params.get("thread_id"))
                .or(message.get("threadId"))
                .or(message.get("thread_id"))
                .cloned();
            let turn = params
                .get("turnId")
                .or(params.get("turn_id"))
                .or(message.get("turnId"))
                .or(message.get("turn_id"))
                .cloned();
            params = message;
            if !params.is_object() {
                params = json!({});
            }
            if let Some(thread) = thread {
                params["threadId"] = thread;
            }
            if let Some(turn) = turn {
                params["turnId"] = turn;
            }
            match method.as_str() {
                "codex/event/task_complete" => {
                    method = "turn/completed".into();
                    params["turn"] = json!({"id":params["turnId"],"status":"completed"});
                }
                "codex/event/turn_aborted" => {
                    method = "turn/completed".into();
                    params["turn"] = json!({"id":params["turnId"],"status":"interrupted"});
                }
                "codex/event/item_started" | "codex/event/item_completed" => {
                    if projection::child_threads(&params["item"]).is_empty() {
                        return Ok((vec![], vec![]));
                    }
                    method = if method.ends_with("item_started") {
                        "item/started"
                    } else {
                        "item/completed"
                    }
                    .into();
                }
                "codex/event/exec_command_begin" | "codex/event/exec_command_end" => {
                    let done = method.ends_with("_end");
                    let exit = params
                        .get("exit_code")
                        .or(params.get("exitCode"))
                        .cloned()
                        .unwrap_or(Value::Null);
                    let failed =
                        params["success"] == false || exit.as_i64().is_some_and(|n| n != 0);
                    let output = [
                        "aggregated_output",
                        "aggregatedOutput",
                        "formatted_output",
                        "stdout",
                    ]
                    .iter()
                    .find_map(|key| params.get(*key).filter(|v| !v.is_null()))
                    .cloned()
                    .unwrap_or(Value::Null);
                    method = if done {
                        "item/completed"
                    } else {
                        "item/started"
                    }
                    .into();
                    params["item"] = json!({"type":"commandExecution","id":params["call_id"],
                        "command":params["command"],"cwd":params["cwd"],
                        "status":if done{if failed{"failed"}else{"completed"}}else{"inProgress"},
                        "exitCode":exit,"aggregatedOutput":output,"durationMs":params["duration_ms"],
                        "error":if failed{json!({"message":params["stderr"].as_str().unwrap_or("Command failed")})}else{Value::Null}});
                }
                "codex/event/exec_command_output_delta" => {
                    method = "item/commandExecution/outputDelta".into();
                    params["itemId"] = params["call_id"].clone();
                    let chunk = params["chunk"]
                        .as_str()
                        .or(params["delta"].as_str())
                        .unwrap_or("");
                    let text = base64::engine::general_purpose::STANDARD
                        .decode(chunk)
                        .ok()
                        .and_then(|b| String::from_utf8(b).ok())
                        .unwrap_or_else(|| chunk.into());
                    params["delta"] = json!(text);
                }
                "codex/event/patch_apply_begin" | "codex/event/patch_apply_end" => {
                    let done = method.ends_with("_end");
                    method = if done {
                        "item/completed"
                    } else {
                        "item/started"
                    }
                    .into();
                    params["item"] = json!({"type":"fileChange","id":params["call_id"],
                        "changes":params["changes"],
                        "status":if done{if params["success"]==false{"failed"}else{"completed"}}else{"inProgress"},
                        "output":params["stdout"],
                        "error":if params["success"]==false{
                            json!({"message":params["stderr"].as_str().unwrap_or("Patch failed")})
                        }else{Value::Null}});
                }
                "codex/event/turn_diff" => return Ok((vec![], vec![])),
                "codex/event/thread_rolled_back" => method = "thread/rolledBack".into(),
                "codex/event/terminal_interaction" => {
                    method = "item/commandExecution/terminalInteraction".into();
                    params["itemId"] = params["call_id"].clone();
                    params["processId"] = match &params["process_id"] {
                        Value::Number(number) => json!(number.to_string()),
                        value => value.clone(),
                    };
                }
                _ => {}
            }
        }
        let mut s = self.state.lock().unwrap();
        let mut events = Vec::new();
        let mut replay = Vec::new();
        let thread = params["threadId"].as_str().map(str::to_owned);
        let child = thread
            .as_ref()
            .filter(|id| s.thread.as_ref() != Some(*id))
            .cloned();
        if let Some(id) = &child {
            if !s.children.contains_key(id) {
                if !matches!(
                    method.as_str(),
                    "paseo/transportClosed" | "serverRequest/resolved" | "thread/started"
                ) {
                    if s.orphan_events.len() >= 32 && !s.orphan_events.contains_key(id) {
                        bail!("Codex announced more than 32 unresolved child threads")
                    }
                    let pending = s.orphan_events.entry(id.clone()).or_default();
                    if pending.len() >= 128 {
                        bail!("Codex child thread '{id}' exceeded 128 unassociated events")
                    }
                    pending.push_back(frame);
                    return Ok((events, replay));
                } else if method == "thread/started" {
                    return Ok((events, replay));
                }
            }
        }
        match method.as_str() {
            "paseo/transportClosed" => {
                let active = s.foreground.is_some() || s.native_turn.is_some();
                if !s.closed && active {
                    let mut event = json!({"type":"turn_failed","error":params["message"]});
                    if let Some(turn) = &s.foreground {
                        event["turnId"] = json!(turn);
                    }
                    events.push(event);
                }
                let discarded: Vec<_> = s
                    .pending
                    .iter()
                    .filter(|(_, pending)| active || pending.method != "plan")
                    .map(|(id, _)| id.clone())
                    .collect();
                for id in discarded {
                    if let Some(pending) = s.pending.remove(&id) {
                        if pending.method == "plan" {
                            let mut event = json!({"type":"timeline","item":permissions::plan_timeline(&pending,Some("canceled"))});
                            if let Some(turn) = pending.params["turnId"].as_str() {
                                event["turnId"] = json!(turn);
                            }
                            events.push(event);
                        }
                    }
                }
                s.foreground = None;
                s.native_turn = None;
                s.client_message = None;
            }
            "thread/started" => {
                if let Some(id) = params["thread"]["id"].as_str() {
                    if s.thread.is_none() {
                        s.thread = Some(id.into());
                    }
                    if s.thread.as_deref() == Some(id) {
                        events.push(json!({"type":"thread_started","sessionId":id}));
                    }
                }
            }
            "turn/started" => {
                if let Some(child) = &child {
                    events.push(subagent_status(&s, child, "running"));
                } else if let Some(id) = params["turn"]["id"].as_str() {
                    s.native_turn = Some(id.into());
                    s.text.clear();
                    s.outputs.clear();
                    s.completed.clear();
                    s.last_plan = None;
                    s.assistant_boundary = false;
                    s.completed_compactions = 0;
                    events.push(json!({"type":"turn_started"}));
                }
            }
            "turn/completed" => {
                let status = params["turn"]["status"].as_str().unwrap_or("completed");
                if let Some(child) = &child {
                    events.push(subagent_status(
                        &s,
                        child,
                        match status {
                            "failed" => "failed",
                            "interrupted" => "canceled",
                            _ => "completed",
                        },
                    ));
                } else {
                    let native = params["turn"]["id"].as_str();
                    if native.is_some()
                        && s.native_turn
                            .as_deref()
                            .is_some_and(|id| Some(id) != native)
                    {
                        return Ok((events, replay));
                    }
                    if s.native_turn.is_none() && s.foreground.is_none() {
                        return Ok((events, replay));
                    }
                    for _ in s.compactions.drain() {
                        events.push(json!({"type":"timeline","item":{"type":"compaction","status":"completed"}}));
                    }
                    if status == "completed" && s.config["featureValues"]["plan_mode"] == true {
                        if let Some(text) = s.last_plan.take() {
                            let previous: Vec<_> = s
                                .pending
                                .values()
                                .filter(|pending| pending.method == "plan")
                                .cloned()
                                .collect();
                            for pending in previous {
                                s.pending.remove(&pending.id);
                                let mut event = json!({"type":"timeline",
                                    "item":permissions::plan_timeline(&pending,Some("rejected"))});
                                if let Some(turn) = pending.params["turnId"].as_str() {
                                    event["turnId"] = json!(turn);
                                }
                                events.push(event);
                                events.push(json!({"type":"permission_resolved","requestId":pending.id,
                                    "resolution":{"behavior":"deny","message":"Superseded by a newer plan"}}));
                            }
                            let id = format!("permission-{}", uuid::Uuid::new_v4());
                            let pending = permissions::plan(
                                &self.spec.id,
                                id.clone(),
                                &text,
                                s.foreground.as_deref(),
                            );
                            events.push(json!({"type":"timeline","item":permissions::plan_timeline(&pending,None)}));
                            events.push(
                                json!({"type":"permission_requested","request":pending.request}),
                            );
                            s.pending.insert(id, pending);
                        }
                    }
                    if status == "interrupted" {
                        for request in s.async_questions.pending(&self.spec.id) {
                            if let Some(id) = request["id"].as_str() {
                                let resolution = json!({"behavior":"deny","message":"Interrupted"});
                                let prepared =
                                    s.async_questions.prepare_response(id, &resolution)?;
                                let item = s.async_questions.complete(prepared)?;
                                events.push(json!({"type":"timeline","item":item}));
                                events.push(json!({"type":"permission_resolved","requestId":id,"resolution":resolution}));
                            }
                        }
                    }
                    let mut event = match status {
                        "failed" => {
                            json!({"type":"turn_failed","error":params["turn"]["error"]["message"].as_str().unwrap_or("Codex turn failed")})
                        }
                        "interrupted" => json!({"type":"turn_canceled","reason":"interrupted"}),
                        _ => json!({"type":"turn_completed","usage":s.usage}),
                    };
                    if let Some(turn) = &s.foreground {
                        event["turnId"] = json!(turn);
                        for e in &mut events {
                            if e.get("turnId").is_none() {
                                e["turnId"] = json!(turn);
                            }
                        }
                    }
                    events.push(event);
                    s.foreground = None;
                    s.native_turn = None;
                    s.client_message = None;
                    s.last_plan = None;
                    s.text.clear();
                    s.outputs.clear();
                }
            }
            "item/agentMessage/delta"
            | "item/reasoning/summaryTextDelta"
            | "item/reasoning/textDelta" => {
                let id = params["itemId"].as_str().unwrap_or("");
                let delta = params["delta"].as_str().unwrap_or("");
                let key = format!("{}:{id}", thread.as_deref().unwrap_or(""));
                let first = !s.text.contains_key(&key);
                s.text.entry(key).or_default().push_str(delta);
                let reasoning = method.contains("reasoning");
                let text = if !reasoning && first && s.assistant_boundary && child.is_none() {
                    s.assistant_boundary = false;
                    format!("\n\n---\n\n{delta}")
                } else {
                    delta.into()
                };
                let item = if reasoning {
                    json!({"type":"reasoning","text":text})
                } else {
                    json!({"type":"assistant_message","messageId":id,"text":text})
                };
                events.push(timeline(child.as_deref(), item));
            }
            "item/commandExecution/outputDelta" | "item/fileChange/outputDelta" => {
                let id = params["itemId"].as_str().unwrap_or("");
                let key = format!("{}:{id}", thread.as_deref().unwrap_or(""));
                s.outputs
                    .entry(key)
                    .or_default()
                    .push_str(params["delta"].as_str().unwrap_or(""));
            }
            "item/started" | "item/completed" => {
                let mut raw = params["item"].clone();
                let id = raw["id"].as_str().unwrap_or("").to_owned();
                let kind = raw["type"].as_str().unwrap_or("").to_owned();
                let done = method == "item/completed";
                let key = format!("{}:{id}", thread.as_deref().unwrap_or(""));
                let dedup = format!("{}:{key}", if done { "end" } else { "start" });
                if original == "codex/event/exec_command_end" {
                    if !s.completed.insert(format!("exec:{key}")) {
                        return Ok((events, replay));
                    }
                    s.completed.insert(dedup);
                } else if !id.is_empty() && !s.completed.insert(dedup) {
                    return Ok((events, replay));
                }
                if !done && matches!(kind.as_str(), "agentMessage" | "reasoning") {
                    return Ok((events, replay));
                }
                if done && child.is_none() {
                    if let Some(request) = s.async_questions.receive(&self.spec.id, &raw) {
                        events.push(json!({"type":"permission_requested","request":request}));
                    }
                }
                if kind == "contextCompaction" || kind == "context_compaction" {
                    if done {
                        s.compactions.remove(&key);
                        if s.completed_compactions > 0 {
                            s.completed_compactions -= 1;
                            return Ok((events, replay));
                        }
                        s.completed_compactions -= 1;
                    } else {
                        s.compactions.insert(key);
                    }
                    let mut item =
                        json!({"type":"compaction","status":if done{"completed"}else{"loading"}});
                    if s.manual_compactions > 0 {
                        item["trigger"] = json!("manual");
                        if done {
                            s.manual_compactions -= 1;
                        }
                    }
                    events.push(timeline(child.as_deref(), item));
                    return Ok((events, replay));
                }
                if let Some(output) = s.outputs.remove(&key) {
                    if raw["aggregatedOutput"].is_null() {
                        raw["aggregatedOutput"] = json!(output);
                    }
                    if raw["output"].is_null() {
                        raw["output"] = raw["aggregatedOutput"].clone();
                    }
                }
                if !done {
                    raw["status"] = json!("inProgress");
                }
                if kind == "commandExecution" {
                    let process = raw["processId"].as_str().map(str::to_owned).or_else(|| {
                        static PROCESS: std::sync::LazyLock<regex::Regex> =
                            std::sync::LazyLock::new(|| {
                                regex::Regex::new(
                                    r"(?i)process running with session id\s+([A-Za-z0-9._:-]+)",
                                )
                                .unwrap()
                            });
                        PROCESS
                            .captures(raw["aggregatedOutput"].as_str().unwrap_or(""))
                            .and_then(|capture| capture.get(1).map(|id| id.as_str().to_owned()))
                    });
                    if let (Some(process), Some(command)) =
                        (process, projection::command(&raw["command"]))
                    {
                        s.terminal_commands.insert(process.clone(), command.clone());
                        if let Some(pending) = s.terminal_pending.remove(&process) {
                            for mut event in pending {
                                if event["type"] == "provider_subagent" {
                                    event["event"]["item"]["detail"]["label"] = json!(command);
                                } else {
                                    event["item"]["detail"]["label"] = json!(command);
                                }
                                events.push(event);
                            }
                        }
                    }
                }
                let cwd = s.config["cwd"].as_str();
                let mut materialize = |data: &str, mime: Option<&str>| {
                    crate::providers::prompt::materialize_image(data, mime)
                        .ok()
                        .map(|p| p.to_string_lossy().into_owned())
                };
                let entries =
                    projection::item_entries_with_images(&raw, true, cwd, &mut materialize);
                for mut item in entries {
                    if item["type"] == "user_message" {
                        if child.is_none() {
                            let message = item["messageId"].as_str().unwrap_or(&id).to_owned();
                            let native = params["turnId"]
                                .as_str()
                                .map(str::to_owned)
                                .or_else(|| s.native_turn.clone());
                            if let Some(existing) = s.user_turns.iter_mut().find(|v| v.0 == message)
                            {
                                if native.is_some() {
                                    existing.1 = native;
                                }
                                continue;
                            }
                            s.user_turns.push((message, native));
                            if item["clientMessageId"].is_null() {
                                if let Some(client_id) = s.client_message.take() {
                                    item["clientMessageId"] = json!(client_id);
                                }
                            } else {
                                s.client_message = None;
                            }
                        } else if !s.completed.insert(format!("user:{key}")) {
                            continue;
                        }
                    }
                    if matches!(
                        item["type"].as_str(),
                        Some("assistant_message" | "reasoning")
                    ) && done
                    {
                        if let Some(child) = &child {
                            let message = item["messageId"].as_str().unwrap_or(&id).to_owned();
                            upsert_child_item(
                                s.child_items.entry(child.clone()).or_default(),
                                message,
                                item.clone(),
                                false,
                            );
                        }
                        if let Some(streamed) = s.text.remove(&key) {
                            let text = item["text"].as_str().unwrap_or("");
                            if text == streamed {
                                if item["type"] == "assistant_message" && child.is_none() {
                                    s.assistant_boundary = true;
                                }
                                continue;
                            }
                            if let Some(suffix) = text.strip_prefix(&streamed) {
                                item["text"] = json!(suffix);
                            }
                        }
                    }
                    if item["type"] == "tool_call"
                        && item["detail"]["type"] == "plan"
                        && child.is_none()
                    {
                        s.last_plan = item["detail"]["text"].as_str().map(str::to_owned);
                        if s.config["featureValues"]["plan_mode"] == true {
                            continue;
                        }
                    }
                    for id in projection::child_threads(&raw) {
                        if kind == "subAgentActivity" {
                            if let Some(existing) = s.child_tools.get(&id) {
                                let mut original = existing.clone();
                                original["status"] = item["status"].clone();
                                original["detail"]["subAgentType"] =
                                    item["detail"]["subAgentType"].clone();
                                item = original;
                            }
                        }
                        let mut upsert = json!({"type":"upsert","id":id,
                            "title":item["detail"]["subAgentType"].as_str().unwrap_or("Codex subagent"),
                            "description":item["detail"]["description"],
                            "status":item["status"].as_str().unwrap_or("running"),
                            "toolCallId":item["callId"],"parentSubagentId":child});
                        if upsert["status"] == "completed" && kind.contains("Spawn") {
                            upsert["status"] = json!("running");
                        }
                        s.children.insert(id.clone(), upsert.clone());
                        s.child_tools.insert(id.clone(), item.clone());
                        events.push(json!({"type":"provider_subagent","event":upsert}));
                        if let Some(pending) = s.orphan_events.remove(&id) {
                            replay.extend(pending);
                        }
                    }
                    if item["type"] == "assistant_message" && child.is_none() {
                        if s.assistant_boundary && !s.text.contains_key(&key) {
                            if let Some(text) = item["text"].as_str() {
                                item["text"] = json!(format!("\n\n---\n\n{text}"));
                            }
                        }
                        s.assistant_boundary = done;
                    }
                    if item["name"] == "request_user_input_async" {
                        if let Some(reconciled) = s
                            .async_questions
                            .timeline(item["callId"].as_str().unwrap_or(""))
                        {
                            item = reconciled;
                        }
                    }
                    events.push(timeline(child.as_deref(), item));
                }
            }
            "turn/plan/updated" => {
                let plan = params["plan"].clone();
                if s.config["featureValues"]["plan_mode"] == true && child.is_none() {
                    s.last_plan = Some(
                        plan.as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|s| s["step"].as_str())
                            .map(|s| format!("- {s}"))
                            .collect::<Vec<_>>()
                            .join("\n"),
                    );
                } else {
                    events.push(timeline(child.as_deref(), projection::plan_todo(&plan)));
                }
            }
            "turn/diff/updated" => {}
            "thread/tokenUsage/updated" => {
                if child.is_none() {
                    s.usage = projection::usage(&params["tokenUsage"]);
                    if let Some(usage) = &s.usage {
                        events.push(json!({"type":"usage_updated","usage":usage}));
                    }
                }
            }
            "thread/compacted" => {
                if child.is_none() {
                    if s.completed_compactions < 0 {
                        s.completed_compactions += 1;
                    } else {
                        s.completed_compactions += 1;
                        if let Some(id) = s.compactions.iter().next().cloned() {
                            s.compactions.remove(&id);
                        }
                        let mut item = json!({"type":"compaction","status":"completed"});
                        if s.manual_compactions > 0 {
                            s.manual_compactions -= 1;
                            item["trigger"] = json!("manual");
                        }
                        events.push(timeline(None, item));
                    }
                }
            }
            "thread/rolledBack" => {
                let n = params["numTurns"]
                    .as_u64()
                    .or(params["num_turns"].as_u64())
                    .unwrap_or(0) as usize;
                let retain = s.user_turns.len().saturating_sub(n);
                s.user_turns.truncate(retain);
            }
            "serverRequest/resolved" => {
                let id = &params["requestId"];
                let key = s
                    .pending
                    .iter()
                    .find(|(_, p)| {
                        p.method == "mcpServer/elicitation/request"
                            && p.native_id.as_ref() == Some(id)
                    })
                    .map(|(id, _)| id.clone());
                if let Some(key) = key {
                    s.pending.remove(&key);
                    events.push(json!({"type":"permission_resolved","requestId":key,"resolution":{"behavior":"deny","interrupt":true}}));
                }
            }
            "item/commandExecution/terminalInteraction" => {
                let process = params["processId"].as_str();
                let stdin = params["stdin"].as_str();
                let key = format!("{}\0{}", process.unwrap_or(""), stdin.unwrap_or(""));
                if !s.terminal_seen.insert(key) {
                    return Ok((events, replay));
                }
                s.terminal_ordinal += 1;
                let base = process
                    .map(|id| format!("terminal-session-{id}"))
                    .unwrap_or_else(|| {
                        params["itemId"]
                            .as_str()
                            .unwrap_or("terminal-interaction")
                            .to_owned()
                    });
                let mut item = json!({"type":"tool_call",
                    "callId":format!("{base}-{}",s.terminal_ordinal),"name":"terminal",
                    "status":"completed","error":null,
                    "detail":{"type":"plain_text","icon":"square_terminal"}});
                if let Some(stdin) = stdin {
                    item["detail"]["text"] = json!(stdin);
                }
                if let Some(process) = process {
                    item["metadata"] = json!({"processId":process});
                    if let Some(command) = s.terminal_commands.get(process) {
                        item["detail"]["label"] = json!(command);
                    }
                }
                let event = timeline(child.as_deref(), item);
                if let Some(process) =
                    process.filter(|process| !s.terminal_commands.contains_key(*process))
                {
                    s.terminal_pending
                        .entry(process.to_owned())
                        .or_default()
                        .push(event.clone());
                }
                events.push(event);
            }
            _ => tracing::debug!(method=%method,"Unmapped Codex notification"),
        }
        Ok((events, replay))
    }
    fn aggregate_children(
        &self,
        events: &[Value],
        source_id: Option<&str>,
        delta: bool,
    ) -> Result<Vec<Value>> {
        let mut state = self.state.lock().unwrap();
        let mut changed = Vec::new();
        for event in events {
            if event["type"] != "provider_subagent" {
                continue;
            }
            let update = &event["event"];
            let Some(id) = update["id"].as_str() else {
                continue;
            };
            if update["type"] == "upsert" {
                state.children.insert(id.to_owned(), update.clone());
            } else if update["type"] == "timeline" {
                let item = update["item"].clone();
                let key = item["messageId"]
                    .as_str()
                    .or(item["callId"].as_str())
                    .or(source_id)
                    .unwrap_or_else(|| item["type"].as_str().unwrap_or("item"))
                    .to_owned();
                if delta
                    || !matches!(
                        item["type"].as_str(),
                        Some("assistant_message" | "reasoning")
                    )
                {
                    let items = state.child_items.entry(id.to_owned()).or_default();
                    upsert_child_item(items, key, item, delta);
                }
            } else {
                continue;
            }
            if !changed.iter().any(|v| v == id) {
                changed.push(id.to_owned());
            }
        }
        let mut updates = Vec::new();
        for id in changed {
            let mut current = Some(id);
            let mut visited = std::collections::HashSet::new();
            while let Some(id) = current.take() {
                if !visited.insert(id.clone()) {
                    break;
                }
                let Some(mut tool) = state.child_tools.get(&id).cloned() else {
                    break;
                };
                let descriptor = state.children.get(&id).cloned().unwrap_or(Value::Null);
                if !descriptor["status"].is_null() {
                    tool["status"] = descriptor["status"].clone();
                }
                if tool["status"] == "failed" && tool["error"].is_null() {
                    tool["error"] = json!({"message":"Sub-agent failed"});
                }
                let mut siblings: Vec<_> = state
                    .child_tools
                    .iter()
                    .filter(|(_, other)| other["callId"] == tool["callId"])
                    .map(|(id, _)| id.clone())
                    .collect();
                siblings.sort();
                let child_timeline: Vec<Value> = siblings
                    .iter()
                    .flat_map(|id| {
                        state
                            .child_items
                            .get(id)
                            .into_iter()
                            .flatten()
                            .map(|(_, item)| item.clone())
                    })
                    .collect();
                let entries = crate::activity::curate(
                    &child_timeline,
                    &json!({"labelAssistantMessages":true}),
                )?;
                tool["detail"]["log"] = json!(
                    entries
                        .iter()
                        .filter_map(|entry| entry["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                );
                state.child_tools.insert(id.clone(), tool.clone());
                let parent = descriptor["parentSubagentId"].as_str().map(str::to_owned);
                if let Some(parent) = parent {
                    let key = tool["callId"].as_str().unwrap_or(&id).to_owned();
                    upsert_child_item(
                        state.child_items.entry(parent.clone()).or_default(),
                        key,
                        tool,
                        false,
                    );
                    current = Some(parent);
                } else {
                    updates.push(json!({"type":"timeline","item":tool}));
                }
            }
        }
        Ok(updates)
    }
}
fn timeline(child: Option<&str>, item: Value) -> Value {
    if let Some(child) = child {
        json!({"type":"provider_subagent","event":{"type":"timeline","id":child,"item":item}})
    } else {
        json!({"type":"timeline","item":item})
    }
}
fn subagent_status(state: &State, id: &str, status: &str) -> Value {
    let mut event=state.children.get(id).cloned().unwrap_or_else(||json!({"type":"upsert","id":id,"title":"Codex subagent","description":null,"toolCallId":null,"parentSubagentId":null}));
    event["status"] = json!(status);
    json!({"type":"provider_subagent","event":event})
}

fn upsert_child_item(items: &mut Vec<(String, Value)>, key: String, mut item: Value, delta: bool) {
    if let Some((_, existing)) = items.iter_mut().find(|(id, _)| id == &key) {
        if delta
            && matches!(
                item["type"].as_str(),
                Some("assistant_message" | "reasoning")
            )
        {
            let mut text = existing["text"].as_str().unwrap_or("").to_owned();
            text.push_str(item["text"].as_str().unwrap_or(""));
            item["text"] = json!(text);
        }
        *existing = item;
    } else {
        items.push((key, item));
    }
}
