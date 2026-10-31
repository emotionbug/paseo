use std::{
    collections::{HashMap, HashSet},
    io::Read,
};

use anyhow::{Result, bail};
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Map, Value, json};
use sha1::{Digest, Sha1};

/// Session-scoped projection. Task routing intentionally survives foreground turns.
pub struct Projection {
    provider: String,
    turn: Option<String>,
    autonomous: bool,
    replaying: bool,
    session: Option<String>,
    model: Option<String>,
    lanes: HashMap<String, Lane>,
    tools: HashMap<String, Tool>,
    tasks: HashMap<String, Child>,
    aliases: HashMap<String, String>,
    task_ids: HashMap<String, String>,
    tool_owners: HashMap<String, String>,
    task_owners: HashMap<String, String>,
    announces_tasks: bool,
    usage: Usage,
    compacting: bool,
    compaction_open: bool,
    assistant_text: bool,
    users: HashSet<String>,
    handbacks: HashSet<String>,
    emitted_handbacks: HashSet<String>,
    counter: u64,
    next_turn_ordinal: u64,
}

#[derive(Default)]
struct Lane {
    active: Option<String>,
    messages: HashMap<String, Message>,
    finalized: HashSet<String>,
    blocks: HashMap<u64, String>,
    todos: Map<String, Value>,
    applied_tasks: HashSet<String>,
}
#[derive(Default)]
struct Message {
    text: String,
    reasoning: String,
}
#[derive(Clone)]
struct Tool {
    name: String,
    input: Value,
    partial: String,
    owner: Option<String>,
    turn: Option<String>,
    started: bool,
}
#[derive(Default)]
struct Child {
    title: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    tokens: Option<f64>,
    subtitle: String,
    status: String,
    background: bool,
    workflow: bool,
    turn: Option<String>,
    last_result: Option<String>,
    description: Option<String>,
    parent: Option<String>,
    actions: Map<String, Value>,
    settled_card: Option<Value>,
}
#[derive(Default)]
struct Usage {
    max: Option<f64>,
    input: Option<[f64; 3]>,
    output: f64,
    compacted: Option<f64>,
    completed: u64,
}

impl Projection {
    pub fn new(provider: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            turn: None,
            autonomous: false,
            replaying: false,
            session: None,
            model: None,
            lanes: HashMap::new(),
            tools: HashMap::new(),
            tasks: HashMap::new(),
            aliases: HashMap::new(),
            task_ids: HashMap::new(),
            tool_owners: HashMap::new(),
            task_owners: HashMap::new(),
            announces_tasks: false,
            usage: Usage::default(),
            compacting: false,
            compaction_open: false,
            assistant_text: false,
            users: HashSet::new(),
            handbacks: HashSet::new(),
            emitted_handbacks: HashSet::new(),
            counter: 0,
            next_turn_ordinal: 1,
        }
    }

    pub fn begin_turn(&mut self, turn_id: impl Into<String>) -> Vec<Value> {
        let mut events = Vec::new();
        if self.autonomous {
            events.push(self.event(json!({"type":"turn_completed"}), None));
        }
        self.autonomous = false;
        self.turn = Some(turn_id.into());
        self.assistant_text = false;
        self.usage.input = None;
        self.usage.output = 0.0;
        self.usage.compacted = None;
        self.lanes.entry(String::new()).or_default().active = None;
        events.push(self.event(json!({"type":"turn_started"}), None));
        events
    }

    pub fn end_turn(&mut self) {
        self.turn = None;
        self.autonomous = false;
    }
    pub fn active_turn_id(&self) -> Option<&str> {
        self.turn.as_deref()
    }
    pub fn is_autonomous(&self) -> bool {
        self.autonomous
    }
    pub fn is_compacting(&self) -> bool {
        self.compacting
    }
    pub fn next_foreground_turn_id(&mut self) -> String {
        self.next_turn_id("foreground")
    }
    fn next_turn_id(&mut self, owner: &str) -> String {
        let id = format!("{owner}-turn-{}", self.next_turn_ordinal);
        self.next_turn_ordinal += 1;
        id
    }
    pub fn set_context_window(&mut self, tokens: Option<u64>) {
        self.usage.max = tokens.map(|v| v as f64);
    }
    pub fn reset_conversation(&mut self) {
        let provider = std::mem::take(&mut self.provider);
        let ordinal = self.next_turn_ordinal;
        let counter = self.counter;
        let model = self.model.take();
        let mut usage = std::mem::take(&mut self.usage);
        usage.input = None;
        usage.output = 0.0;
        usage.compacted = None;
        *self = Self::new(provider);
        self.next_turn_ordinal = ordinal;
        self.counter = counter;
        self.model = model;
        self.usage = usage;
    }

    fn event(&self, mut event: Value, owner: Option<&str>) -> Value {
        event["provider"] = json!(self.provider);
        let turn = owner
            .and_then(|id| self.tasks.get(id))
            .and_then(|child| child.turn.as_ref())
            .or(self.turn.as_ref());
        if let Some(turn) = turn {
            event["turnId"] = json!(turn);
        }
        event
    }
    fn timeline(&mut self, item: Value, owner: Option<&str>) -> Value {
        if owner.is_none() && s(&item, "type") == Some("assistant_message") {
            self.assistant_text = true;
        }
        match owner {
            Some(id) => self.event(
                json!({"type":"provider_subagent","event":{"type":"timeline","id":id,"item":item}}),
                owner,
            ),
            None => self.event(json!({"type":"timeline","item":item}), None),
        }
    }
    fn child_event(&self, event: Value) -> Value {
        let owner = s(&event, "id");
        self.event(json!({"type":"provider_subagent","event":event}), owner)
    }

    /// Accepts SDK/native stream-json frames and persisted transcript records.
    pub fn project(&mut self, frame: &Value) -> Vec<Value> {
        let mut events = Vec::new();
        let raw_owner = if s(frame, "type") == Some("tool_progress") && frame["heartbeat"] == true {
            None
        } else {
            s(frame, "parent_tool_use_id")
        };
        let owner = raw_owner.map(|id| {
            self.aliases
                .get(id)
                .cloned()
                .unwrap_or_else(|| id.to_owned())
        });
        if owner
            .as_ref()
            .is_some_and(|id| self.announces_tasks && !self.tasks.contains_key(id))
        {
            return events;
        }
        if let Some(id) = owner
            .as_deref()
            .filter(|id| !self.announces_tasks && !self.tasks.contains_key(*id))
        {
            self.legacy_child(id, &mut events);
        }
        if owner.is_none() {
            if self.turn.is_none() && frame["type"] == "system" && frame["subtype"] == "init" {
                let turn_id = self.next_turn_id("autonomous");
                events.extend(self.begin_turn(turn_id));
                self.autonomous = true;
            }
            if self.turn.is_none() && frame["type"] == "result" {
                return events;
            }
            if let Some(session) = s(frame, "session_id")
                .or_else(|| s(frame, "sessionId"))
                .or_else(|| s(&frame["session"], "id"))
            {
                if self.session.as_deref() != Some(session) {
                    if let Some(previous) = self.session.as_deref() {
                        let text =
                            format!("Claude switched to a new session: {previous} -> {session}");
                        events.push(
                            self.timeline(json!({"type":"assistant_message","text":text}), None),
                        );
                    }
                    self.session = Some(session.to_owned());
                    events.push(
                        self.event(json!({"type":"thread_started","sessionId":session}), None),
                    );
                }
            }
        }
        match s(frame, "type").unwrap_or("") {
            "system" if owner.is_none() => self.system(frame, &mut events),
            "assistant" => {
                if let Some(id) = owner.as_deref() {
                    if let Some(child) = self.tasks.get_mut(id) {
                        if let Some(model) =
                            super::catalog::observed_model_id(s(&frame["message"], "model"))
                        {
                            child.model = Some(model);
                        }
                        if let Some(effort) = s(frame, "effort") {
                            child.effort = Some(effort.to_owned());
                        }
                    }
                    self.subtitle(id, &mut events);
                }
                let content = &frame["message"]["content"];
                let id = self.message_id(frame, owner.as_deref(), false);
                if let Some(blocks) = content.as_array() {
                    for block in blocks {
                        self.block(block, frame, owner.as_deref(), &mut events);
                    }
                }
                self.text_content(content, &id, owner.as_deref(), true, &mut events);
            }
            "user" => self.user(frame, owner.as_deref(), &mut events),
            "stream_event" => self.stream(frame, owner.as_deref(), &mut events),
            "result" if owner.is_none() => self.result(frame, &mut events),
            "queue-operation" => self.notification(frame, &mut events),
            _ => {}
        }
        if !self.replaying {
            if let Some(id) = owner.as_deref() {
                self.enrich_child_card(id, &mut events);
            }
        }
        stamp(&mut events, frame);
        events
    }

    fn message_id(&mut self, frame: &Value, owner: Option<&str>, stream: bool) -> String {
        let value = if stream { &frame["event"] } else { frame };
        let explicit = s(value, "message_id").or_else(|| s(&value["message"], "id"));
        let lane = self
            .lanes
            .entry(owner.unwrap_or("").to_owned())
            .or_default();
        let id = explicit
            .map(str::to_owned)
            .or_else(|| lane.active.clone())
            .or_else(|| s(frame, "uuid").map(str::to_owned))
            .unwrap_or_else(|| {
                self.counter += 1;
                format!("synthetic-message-{}", self.counter)
            });
        lane.active = Some(id.clone());
        id
    }

    fn text_content(
        &mut self,
        content: &Value,
        id: &str,
        owner: Option<&str>,
        absolute: bool,
        events: &mut Vec<Value>,
    ) {
        let lane = self
            .lanes
            .entry(owner.unwrap_or("").to_owned())
            .or_default();
        if lane.finalized.contains(id) {
            return;
        }
        let (text, reasoning) = fragments(content);
        let state = lane.messages.entry(id.to_owned()).or_default();
        let text = merge_text(&mut state.text, text, absolute);
        let reasoning = merge_text(&mut state.reasoning, reasoning, absolute);
        if !text.is_empty() && !noise(&text) {
            events.push(self.timeline(
                json!({"type":"assistant_message","text":text,"messageId":id}),
                owner,
            ));
        }
        if !reasoning.is_empty() {
            events.push(self.timeline(
                json!({"type":"reasoning","text":reasoning,"messageId":id}),
                owner,
            ));
        }
    }

    fn stream(&mut self, frame: &Value, owner: Option<&str>, events: &mut Vec<Value>) {
        let event = &frame["event"];
        let kind = s(event, "type").unwrap_or("");
        if owner.is_none() {
            let usage = if kind == "message_start" {
                &event["message"]["usage"]
            } else {
                &event["usage"]
            };
            if kind == "message_start" {
                if let Some(input) = number(usage, "input_tokens") {
                    self.usage.input = Some([
                        input,
                        number(usage, "cache_creation_input_tokens").unwrap_or(0.0),
                        number(usage, "cache_read_input_tokens").unwrap_or(0.0),
                    ]);
                    self.usage.output = 0.0;
                }
            } else if kind == "message_delta" && number(usage, "output_tokens").is_some() {
                self.usage.output = number(usage, "output_tokens").unwrap_or(0.0);
                if let Some(input) = self.usage.input.as_mut() {
                    for (i, key) in [
                        "input_tokens",
                        "cache_creation_input_tokens",
                        "cache_read_input_tokens",
                    ]
                    .iter()
                    .enumerate()
                    {
                        if let Some(n) = number(usage, key) {
                            input[i] = n;
                        }
                    }
                }
            }
            if matches!(kind, "message_start" | "message_delta")
                && let Some(used) = self.usage.used()
            {
                events.push(self.event(
                    json!({"type":"usage_updated","usage":self.usage.context(Some(used))}),
                    None,
                ));
            }
        }
        let id = self.message_id(frame, owner, true);
        match kind {
            "message_start" => {
                self.lanes
                    .entry(owner.unwrap_or("").to_owned())
                    .or_default()
                    .blocks
                    .clear();
            }
            "message_stop" => {
                let lane = self
                    .lanes
                    .entry(owner.unwrap_or("").to_owned())
                    .or_default();
                lane.finalized.insert(id.clone());
                lane.messages.remove(&id);
                lane.active = None;
            }
            "content_block_start" => {
                let block = &event["content_block"];
                if let (Some(index), Some(tool_id)) = (event["index"].as_u64(), s(block, "id")) {
                    self.lanes
                        .entry(owner.unwrap_or("").to_owned())
                        .or_default()
                        .blocks
                        .insert(index, tool_id.to_owned());
                }
                self.text_content(block, &id, owner, false, events);
                self.block(block, frame, owner, events);
            }
            "content_block_delta" => {
                let delta = &event["delta"];
                if s(delta, "type") == Some("input_json_delta") {
                    let tool_id = event["index"]
                        .as_u64()
                        .and_then(|index| {
                            self.lanes
                                .get(owner.unwrap_or(""))
                                .and_then(|l| l.blocks.get(&index))
                        })
                        .cloned();
                    if let Some(tool_id) = tool_id {
                        self.tool_delta(
                            &tool_id,
                            s(delta, "partial_json").unwrap_or(""),
                            owner,
                            events,
                        );
                    }
                } else {
                    self.text_content(delta, &id, owner, false, events);
                }
            }
            "content_block_stop" => {
                if let Some(index) = event["index"].as_u64() {
                    self.lanes
                        .entry(owner.unwrap_or("").to_owned())
                        .or_default()
                        .blocks
                        .remove(&index);
                }
            }
            _ => {}
        }
    }

    fn user(&mut self, frame: &Value, owner: Option<&str>, events: &mut Vec<Value>) {
        let content = &frame["message"]["content"];
        let has_results = content
            .as_array()
            .is_some_and(|blocks| blocks.iter().any(|b| s(b, "type") == Some("tool_result")));
        if (frame["isSynthetic"] == true || frame["isMeta"] == true) && !has_results {
            return;
        }
        if self.compacting && owner.is_none() && !has_results {
            self.compacting = false;
            return;
        }
        if let Some(uuid) = s(frame, "uuid") {
            let key = format!("{}:{uuid}", owner.unwrap_or(""));
            if !self.users.insert(key) {
                return;
            }
        }
        if content_text(content).contains("<task-notification>") {
            self.notification(frame, events);
            return;
        }
        let text = user_text(content);
        if !text.is_empty() && !noise(&text) {
            let mut item = json!({"type":"user_message","text":text});
            copy(frame, "uuid", &mut item, "messageId");
            copy(frame, "clientMessageId", &mut item, "clientMessageId");
            events.push(self.timeline(item, owner));
        }
        if let Some(blocks) = content.as_array() {
            for block in blocks {
                self.block(block, frame, owner, events);
            }
        }
    }

    fn block(
        &mut self,
        block: &Value,
        frame: &Value,
        owner: Option<&str>,
        events: &mut Vec<Value>,
    ) {
        let kind = s(block, "type").unwrap_or("");
        if matches!(kind, "tool_use" | "server_tool_use" | "mcp_tool_use") {
            let Some(id) = s(block, "id") else {
                return;
            };
            let name = s(block, "name").unwrap_or("tool");
            if let Some(owner) = owner {
                self.tool_owners.insert(id.to_owned(), owner.to_owned());
            }
            let tool = self.tools.entry(id.to_owned()).or_insert_with(|| Tool {
                name: name.to_owned(),
                input: Value::Null,
                partial: String::new(),
                owner: owner.map(str::to_owned),
                turn: self.turn.clone(),
                started: false,
            });
            tool.name = name.to_owned();
            if let Some(input) = block.get("input").filter(|v| !v.is_null()) {
                tool.input = input.clone();
            }
            let changed = !tool.started;
            tool.started = true;
            let input = tool.input.clone();
            if name == "SubagentHandback" {
                self.handbacks.insert(id.to_owned());
                if let Some(report) = s(&input, "message")
                    && self.emitted_handbacks.insert(id.to_owned())
                {
                    events.push(
                        self.timeline(json!({"type":"assistant_message","text":report}), owner),
                    );
                }
                return;
            }
            if changed {
                events.push(self.timeline(
                    tool_call(name, id, &input, &Value::Null, "running", &Value::Null),
                    owner,
                ));
            }
            if name == "TodoWrite" {
                self.replace_todos(owner, &input["todos"], true);
                events.push(self.todo_event(owner));
            }
        } else if matches!(
            kind,
            "tool_result"
                | "mcp_tool_result"
                | "web_fetch_tool_result"
                | "web_search_tool_result"
                | "code_execution_tool_result"
                | "bash_code_execution_tool_result"
                | "text_editor_code_execution_tool_result"
        ) {
            let Some(id) = s(block, "tool_use_id") else {
                return;
            };
            if self.handbacks.remove(id) {
                self.tools.remove(id);
                return;
            }
            let tool = self.tools.remove(id);
            if let Some(tool) = tool.as_ref() {
                self.task_result(id, tool, frame, owner, events);
            }
            let name = tool
                .as_ref()
                .map(|t| t.name.as_str())
                .or_else(|| s(block, "tool_name"))
                .unwrap_or("tool");
            let input = tool.as_ref().map(|t| &t.input).unwrap_or(&Value::Null);
            let failed = block["is_error"] == true;
            let (content, images) = split_images(&block["content"]);
            let output = if failed {
                Value::Null
            } else {
                tool_output(name, input, &content)
            };
            let mut error = block.clone();
            error["content"] = content;
            let mut item = tool_call(
                name,
                id,
                input,
                &output,
                if failed { "failed" } else { "completed" },
                &error,
            );
            let canonical = self
                .aliases
                .get(id)
                .cloned()
                .unwrap_or_else(|| id.to_owned());
            if let Some(child) = self.tasks.get_mut(&canonical).filter(|c| !c.workflow) {
                let log = child
                    .actions
                    .values()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("\n");
                item["detail"] =
                    subagent_detail(child.title.as_deref(), child.description.as_deref(), &log);
                child.settled_card = Some(item.clone());
            } else if self.replaying && matches!(name, "Task" | "Agent") {
                item["detail"] = subagent_detail(
                    first(input, &["name", "subagent_type"]),
                    s(input, "description"),
                    "",
                );
            }
            events.push(self.timeline(item, owner));
            if !self.announces_tasks && !self.replaying {
                self.set_status(
                    &canonical,
                    if failed { "failed" } else { "completed" },
                    events,
                );
            }
            for image in images {
                match image_item(&image) {
                    Ok(Some(item)) => events.push(self.timeline(item, owner)),
                    Ok(None) => {}
                    Err(error) => {
                        tracing::warn!(%error,"Failed to materialize Claude image");
                        events.push(self.timeline(json!({"type":"assistant_message","text":"Image output was omitted because it was not available as a file path or URL."}),owner));
                    }
                }
            }
        }
    }

    fn tool_delta(&mut self, id: &str, delta: &str, owner: Option<&str>, events: &mut Vec<Value>) {
        let Some(tool) = self.tools.get_mut(id) else {
            return;
        };
        tool.partial.push_str(delta);
        let Some(input) = partial_json(&tool.partial) else {
            return;
        };
        if input == tool.input {
            return;
        }
        tool.input = input;
        if tool.name == "SubagentHandback" {
            return;
        }
        let item = tool_call(
            &tool.name,
            id,
            &tool.input,
            &Value::Null,
            "running",
            &Value::Null,
        );
        events.push(self.timeline(item, owner));
    }

    fn replace_todos(&mut self, owner: Option<&str>, value: &Value, legacy: bool) {
        let lane = self
            .lanes
            .entry(owner.unwrap_or("").to_owned())
            .or_default();
        lane.todos.clear();
        if let Some(tasks) = value.as_array() {
            for (index, task) in tasks.iter().enumerate() {
                if let Some(mut item) = task_item(task) {
                    let id = s(&item, "id")
                        .map(str::to_owned)
                        .or_else(|| legacy.then(|| format!("legacy:{index}")));
                    if let Some(id) = id {
                        item["id"] = json!(id);
                        lane.todos.insert(id, item);
                    }
                }
            }
        }
    }
    fn todo_event(&mut self, owner: Option<&str>) -> Value {
        let items: Vec<_> = self
            .lanes
            .get(owner.unwrap_or(""))
            .map(|l| l.todos.values().cloned().collect())
            .unwrap_or_default();
        self.timeline(json!({"type":"todo","items":items}), owner)
    }
    fn task_result(
        &mut self,
        id: &str,
        tool: &Tool,
        frame: &Value,
        owner: Option<&str>,
        events: &mut Vec<Value>,
    ) {
        if !matches!(tool.name.as_str(), "TaskCreate" | "TaskUpdate" | "TaskList") {
            return;
        }
        let result = frame
            .get("toolUseResult")
            .or_else(|| frame.get("tool_use_result"))
            .unwrap_or(&Value::Null);
        let lane = self
            .lanes
            .entry(owner.unwrap_or("").to_owned())
            .or_default();
        if !lane.applied_tasks.insert(id.to_owned()) || result["success"] == false {
            return;
        }
        match tool.name.as_str() {
            "TaskCreate" => {
                let Some(id) = s(&result["task"], "id").or_else(|| s(result, "taskId")) else {
                    return;
                };
                let Some(text) =
                    s(&result["task"], "subject").or_else(|| s(&tool.input, "subject"))
                else {
                    return;
                };
                let mut item = json!({"id":id,"text":text,"status":"pending","completed":false});
                copy(&tool.input, "activeForm", &mut item, "activeForm");
                lane.todos.insert(id.to_owned(), item);
            }
            "TaskUpdate" => {
                let Some(id) = s(&tool.input, "taskId").or_else(|| s(result, "taskId")) else {
                    return;
                };
                let Some(item) = lane.todos.get_mut(id) else {
                    return;
                };
                let status = s(&tool.input, "status").or_else(|| s(&result["statusChange"], "to"));
                if status == Some("deleted") {
                    lane.todos.remove(id);
                } else {
                    if let Some(status) = status {
                        let status = task_status(status);
                        item["status"] = json!(status);
                        item["completed"] = json!(status == "completed");
                    }
                    copy(&tool.input, "subject", item, "text");
                    copy(&tool.input, "activeForm", item, "activeForm");
                }
            }
            "TaskList" => {
                if !result["tasks"].is_array() {
                    return;
                }
                self.replace_todos(owner, &result["tasks"], false);
            }
            _ => return,
        }
        events.push(self.todo_event(owner));
    }

    fn system(&mut self, frame: &Value, events: &mut Vec<Value>) {
        match s(frame, "subtype").unwrap_or("") {
            "init" => {
                if let Some(model) = s(frame, "model") {
                    if self.model.as_deref() != Some(model) {
                        self.set_context_window(super::catalog::context_window(Some(model)));
                    }
                    self.model = Some(model.to_owned());
                }
            }
            "status" => {
                if s(frame, "status") == Some("compacting") {
                    self.compacting = true;
                    if !self.compaction_open {
                        self.compaction_open = true;
                        events.push(
                            self.timeline(json!({"type":"compaction","status":"loading"}), None),
                        );
                    }
                }
            }
            "compact_boundary" => {
                self.compaction_open = false;
                let metadata = frame
                    .get("compact_metadata")
                    .or_else(|| frame.get("compactMetadata"))
                    .unwrap_or(&Value::Null);
                let mut item = json!({"type":"compaction","status":"completed","trigger":if s(metadata,"trigger")==Some("manual") {"manual"} else {"auto"}});
                copy(metadata, "pre_tokens", &mut item, "preTokens");
                copy(metadata, "preTokens", &mut item, "preTokens");
                self.usage.input = None;
                self.usage.output = 0.0;
                self.usage.compacted =
                    number(metadata, "post_tokens").or_else(|| number(metadata, "postTokens"));
                events.push(self.timeline(item, None));
                events.push(self.event(json!({"type":"usage_updated","usage":self.usage.context(self.usage.compacted)}),None));
            }
            "task_started" => self.task_started(frame, events),
            "task_updated" | "task_progress" | "task_notification" => {
                let id = s(frame, "task_id")
                    .and_then(|id| self.task_ids.get(id))
                    .cloned();
                if let Some(id) = id {
                    if let Some(child) = self.tasks.get_mut(&id) {
                        if let Some(background) = frame["patch"]["is_backgrounded"].as_bool() {
                            child.background = background;
                        }
                        if let Some(tokens) = number(&frame["usage"], "total_tokens") {
                            child.tokens = Some(tokens);
                        }
                    }
                    if s(frame, "subtype") == Some("task_notification")
                        && self.tasks.get(&id).is_some_and(|c| c.workflow)
                    {
                        if let Some(text) = s(frame, "output_file").and_then(read_workflow_result) {
                            let child = self.tasks.get_mut(&id).expect("declared child");
                            if child.last_result.as_deref() != Some(&text) {
                                child.last_result = Some(text.clone());
                                events.push(self.timeline(
                                    json!({"type":"assistant_message","text":text}),
                                    Some(&id),
                                ));
                            }
                        }
                    }
                    self.subtitle(&id, events);
                    if let Some(status) = s(&frame["patch"], "status")
                        .or_else(|| s(frame, "status"))
                        .and_then(child_status)
                    {
                        self.set_status(&id, status, events);
                    }
                } else if s(frame, "subtype") == Some("task_notification") {
                    self.notification(frame, events);
                }
            }
            _ => {}
        }
    }

    fn task_started(&mut self, frame: &Value, events: &mut Vec<Value>) {
        self.announces_tasks = true;
        let (Some(task_id), Some(tool_id)) = (s(frame, "task_id"), s(frame, "tool_use_id")) else {
            return;
        };
        let owner = self.tool_owners.get(tool_id).cloned();
        if let Some(owner) = owner.as_ref() {
            self.task_owners.insert(task_id.to_owned(), owner.clone());
        }
        let kind = s(frame, "task_type");
        if frame["skip_transcript"] == true
            || !kind
                .map(|v| matches!(v, "local_agent" | "local_workflow"))
                .unwrap_or_else(|| s(frame, "subagent_type").is_some())
        {
            return;
        }
        let id = self
            .task_ids
            .get(task_id)
            .cloned()
            .unwrap_or_else(|| tool_id.to_owned());
        self.aliases.insert(tool_id.to_owned(), id.clone());
        self.task_ids.insert(task_id.to_owned(), id.clone());
        let workflow = kind == Some("local_workflow");
        if !self.tasks.contains_key(&id) {
            let title = if workflow {
                Some("Workflow".to_owned())
            } else {
                self.tools
                    .get(tool_id)
                    .and_then(|t| s(&t.input, "name"))
                    .or_else(|| s(frame, "subagent_type"))
                    .map(str::to_owned)
            };
            self.tasks.insert(
                id.clone(),
                Child {
                    title: title.clone(),
                    model: None,
                    effort: None,
                    tokens: None,
                    subtitle: title.clone().unwrap_or_default(),
                    status: "running".to_owned(),
                    background: frame["is_backgrounded"] == true,
                    workflow,
                    turn: self.turn.clone(),
                    last_result: None,
                    description: s(frame, "description").map(str::to_owned),
                    parent: owner.clone(),
                    ..Child::default()
                },
            );
            let mut declaration =
                json!({"type":"upsert","id":id,"toolCallId":id,"status":"running"});
            if let Some(title) = title.as_ref() {
                declaration["title"] = json!(title);
            }
            copy(frame, "description", &mut declaration, "description");
            if let Some(owner) = owner.as_ref() {
                declaration["parentSubagentId"] = json!(owner);
            }
            events.push(self.child_event(declaration));
            if !workflow && owner.is_none() {
                let mut item = tool_call(
                    "Task",
                    &id,
                    &Value::Null,
                    &Value::Null,
                    "running",
                    &Value::Null,
                );
                item["detail"] = subagent_detail(title.as_deref(), s(frame, "description"), "");
                events.push(self.timeline(item, None));
            }
        } else {
            if let Some(value) = frame["is_backgrounded"].as_bool() {
                self.tasks.get_mut(&id).expect("declared child").background = value;
            }
            self.set_status(&id, "running", events);
        }
        if let Some(prompt) = s(frame, if workflow { "description" } else { "prompt" }) {
            events.push(self.timeline(json!({"type":"user_message","text":prompt}), Some(&id)));
        }
    }

    fn set_status(&mut self, id: &str, status: &str, events: &mut Vec<Value>) {
        let Some(child) = self.tasks.get_mut(id) else {
            return;
        };
        if child.status == status {
            return;
        }
        child.status = status.to_owned();
        events.push(self.child_event(json!({"type":"upsert","id":id,"status":status})));
    }
    fn subtitle(&mut self, id: &str, events: &mut Vec<Value>) {
        let Some(child) = self.tasks.get_mut(id) else {
            return;
        };
        let subtitle = subtitle(
            child.title.as_deref(),
            child.model.as_deref(),
            child.effort.as_deref(),
            child.tokens,
        );
        if subtitle.is_empty() || subtitle == child.subtitle {
            return;
        }
        child.subtitle = subtitle.clone();
        events.push(self.child_event(json!({"type":"upsert","id":id,"subtitle":subtitle})));
    }
    pub fn observe_hook(&mut self, input: &Value) -> Vec<Value> {
        let mut events = Vec::new();
        if let (Some(id), Some(effort)) = (
            s(input, "agent_id")
                .and_then(|id| self.task_ids.get(id))
                .cloned(),
            s(&input["effort"], "level"),
        ) {
            if let Some(child) = self.tasks.get_mut(&id) {
                child.effort = Some(effort.to_owned());
            }
            self.subtitle(&id, &mut events);
        }
        events
    }

    fn notification(&mut self, frame: &Value, events: &mut Vec<Value>) {
        let Some(item) = notification_item(frame) else {
            return;
        };
        let task = s(&item["metadata"], "taskId");
        if task.is_some_and(|id| self.task_ids.contains_key(id)) {
            return;
        }
        let tool = s(&item["metadata"], "toolUseId");
        if tool
            .and_then(|id| self.tools.get(id))
            .is_some_and(|t| matches!(t.name.as_str(), "Task" | "Agent" | "Workflow"))
        {
            return;
        }
        let owner = task
            .and_then(|id| self.task_owners.get(id))
            .or_else(|| tool.and_then(|id| self.tool_owners.get(id)))
            .cloned();
        events.push(self.timeline(item, owner.as_deref()));
    }

    fn result(&mut self, frame: &Value, events: &mut Vec<Value>) {
        let usage = self.usage.result(frame, self.model.as_deref());
        if !self.announces_tasks {
            let ids: Vec<_> = self
                .tasks
                .iter()
                .filter(|(_, c)| c.status == "running")
                .map(|(id, _)| id.clone())
                .collect();
            for id in ids {
                self.set_status(
                    &id,
                    if frame["subtype"] == "success" {
                        "completed"
                    } else {
                        "failed"
                    },
                    events,
                );
            }
        }
        if s(frame, "subtype") == Some("success") {
            if !self.assistant_text && number(&frame["usage"], "output_tokens") == Some(0.0) {
                if let Some(text) = s(frame, "result") {
                    let mut item = json!({"type":"assistant_message","text":text});
                    copy(frame, "uuid", &mut item, "messageId");
                    events.push(self.timeline(item, None));
                }
            }
            let mut event = json!({"type":"turn_completed"});
            if let Some(usage) = usage {
                event["usage"] = usage;
            }
            events.push(self.event(event, None));
        } else {
            let error = frame["errors"]
                .as_array()
                .map(|v| {
                    v.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| "Claude run failed".to_owned());
            let mut terminal = self.terminate_turn(false, &error);
            if let Some(event) = terminal.last_mut().filter(|e| e["type"] == "turn_canceled") {
                event["type"] = json!("turn_failed");
                event["error"] = json!(error);
                event
                    .as_object_mut()
                    .expect("event object")
                    .remove("reason");
            }
            events.extend(terminal);
        }
        self.end_turn();
    }

    pub fn interrupt(&mut self) -> Vec<Value> {
        self.terminate_turn(false, "Interrupted")
    }
    pub fn runtime_failed(&mut self, error: &str) -> Vec<Value> {
        self.terminate_turn(true, error)
    }
    fn terminate_turn(&mut self, failed: bool, reason: &str) -> Vec<Value> {
        let mut events = Vec::new();
        let children: Vec<_> = self
            .tasks
            .iter()
            .filter(|(_, c)| c.status == "running" && (failed || !c.background))
            .map(|(id, _)| id.clone())
            .collect();
        for id in children {
            self.set_status(&id, if failed { "failed" } else { "canceled" }, &mut events);
        }
        let tools: Vec<_> = self
            .tools
            .iter()
            .filter(|(id, t)| {
                failed
                    || !self
                        .tasks
                        .get(*id)
                        .or_else(|| t.owner.as_ref().and_then(|id| self.tasks.get(id)))
                        .is_some_and(|c| c.background)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in tools {
            if let Some(tool) = self.tools.remove(&id) {
                if tool.name != "SubagentHandback" {
                    let mut event = self.timeline(
                        tool_call(
                            &tool.name,
                            &id,
                            &tool.input,
                            &Value::Null,
                            if failed { "failed" } else { "canceled" },
                            &json!({"message":reason}),
                        ),
                        tool.owner.as_deref(),
                    );
                    if let Some(turn) = tool.turn {
                        event["turnId"] = json!(turn);
                    }
                    events.push(event);
                }
            }
        }
        if self.turn.is_some() {
            events.push(self.event(
                if failed {
                    json!({"type":"turn_failed","error":reason})
                } else {
                    json!({"type":"turn_canceled","reason":reason})
                },
                None,
            ));
        }
        self.end_turn();
        events
    }
}

impl Usage {
    fn used(&self) -> Option<f64> {
        self.input
            .map(|v| v.iter().sum::<f64>() + self.output)
            .filter(|v| *v > 0.0)
    }
    fn context(&self, used: Option<f64>) -> Value {
        let mut value = json!({});
        if let Some(max) = self.max {
            value["contextWindowMaxTokens"] = json!(max);
        }
        if let Some(used) = used {
            value["contextWindowUsedTokens"] = json!(used);
        }
        value
    }
    fn result(&mut self, frame: &Value, model: Option<&str>) -> Option<Value> {
        let raw = &frame["usage"];
        let result = if raw.is_object() {
            if let Some(max) = model
                .and_then(|id| number(&frame["modelUsage"][id], "contextWindow"))
                .filter(|n| *n > 0.0)
            {
                self.max = Some(max);
            }
            let iteration = raw["iterations"]
                .as_array()
                .and_then(|v| v.iter().rev().find(|v| v.is_object()));
            let used = self
                .used()
                .or_else(|| iteration.and_then(usage_total))
                .or_else(|| {
                    if self.completed == 0 {
                        usage_total(raw)
                    } else {
                        None
                    }
                })
                .or(self.compacted);
            let mut usage = self.context(used);
            for (from, to) in [
                ("input_tokens", "inputTokens"),
                ("cache_read_input_tokens", "cachedInputTokens"),
                ("output_tokens", "outputTokens"),
            ] {
                copy(raw, from, &mut usage, to);
            }
            copy(frame, "total_cost_usd", &mut usage, "totalCostUsd");
            Some(usage)
        } else {
            None
        };
        self.completed += 1;
        self.compacted = None;
        result
    }
}

fn s<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key)?.as_str().filter(|v| !v.trim().is_empty())
}
fn number(value: &Value, key: &str) -> Option<f64> {
    value
        .get(key)?
        .as_f64()
        .filter(|v| v.is_finite() && *v >= 0.0)
}
fn copy(from: &Value, key: &str, to: &mut Value, target: &str) {
    if let Some(value) = from.get(key).filter(|v| !v.is_null()) {
        to[target] = value.clone();
    }
}
fn first<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|key| s(value, key))
}
fn usage_total(value: &Value) -> Option<f64> {
    let total = [
        "input_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
        "output_tokens",
    ]
    .iter()
    .filter_map(|key| number(value, key))
    .sum::<f64>();
    (total > 0.0).then_some(total)
}
fn timestamp(value: &Value) -> Option<String> {
    if let Some(text) = value.as_str() {
        return DateTime::parse_from_rfc3339(text)
            .ok()
            .map(|t| t.to_utc().to_rfc3339_opts(SecondsFormat::Millis, true));
    }
    let n = value.as_i64()?;
    DateTime::<Utc>::from_timestamp_millis(n)
        .map(|t| t.to_rfc3339_opts(SecondsFormat::Millis, true))
}
fn stamp(events: &mut [Value], frame: &Value) {
    let timestamp = timestamp(&frame["timestamp"]);
    for event in events {
        if let Some(time) = timestamp.as_ref() {
            if event["type"] == "provider_subagent" {
                event["event"]["timestamp"] = json!(time);
            } else if event["type"] == "timeline" {
                event["timestamp"] = json!(time);
            }
        }
        // Native turns and Paseo turns are distinct identities.
        if let Some(native) = s(frame, "turn_id").or_else(|| s(frame, "turnId")) {
            event["nativeTurnId"] = json!(native);
        }
    }
}
fn noise(text: &str) -> bool {
    let text = text.trim();
    (text.starts_with("[Request interrupted by user") && text.ends_with(']'))
        || text == "No response requested."
        || text == "No response requested"
        || (text.starts_with("<local-command-stdout>") && text.ends_with("</local-command-stdout>"))
}
fn fragments(content: &Value) -> (String, String) {
    if let Some(text) = content.as_str() {
        return (text.to_owned(), String::new());
    }
    let mut text = String::new();
    let mut reasoning = String::new();
    let blocks = content
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_else(|| std::slice::from_ref(content));
    for block in blocks {
        match s(block, "type") {
            Some("text" | "text_delta") => {
                if let Some(value) = block["text"].as_str() {
                    text.push_str(value);
                }
            }
            Some("thinking" | "thinking_delta") => {
                if let Some(value) = block["thinking"].as_str() {
                    reasoning.push_str(value);
                }
            }
            _ => {}
        }
    }
    (text, reasoning)
}
fn merge_text(current: &mut String, value: String, absolute: bool) -> String {
    if value.is_empty() {
        return value;
    }
    if absolute {
        let suffix = value
            .strip_prefix(current.as_str())
            .unwrap_or(&value)
            .to_owned();
        *current = value;
        suffix
    } else {
        current.push_str(&value);
        value
    }
}
fn content_text(content: &Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_owned();
    }
    if let Some(blocks) = content.as_array() {
        return blocks
            .iter()
            .filter_map(|b| first(b, &["text", "input"]))
            .collect::<Vec<_>>()
            .join("\n");
    }
    if content.is_null() {
        String::new()
    } else {
        serde_json::to_string(content).unwrap_or_default()
    }
}
fn user_text(content: &Value) -> String {
    let raw = if let Some(text) = content.as_str() {
        text.to_owned()
    } else {
        content
            .as_array()
            .map(|blocks| {
                blocks
                    .iter()
                    .filter(|b| !s(b, "type").is_some_and(|v| v.ends_with("tool_result")))
                    .filter_map(|b| first(b, &["text", "input"]))
                    .map(str::trim)
                    .filter(|v| !v.is_empty() && !noise(v))
                    .collect::<Vec<_>>()
                    .join("\n\n")
            })
            .unwrap_or_default()
    };
    if let Some(name) = tag(&raw, "command-name") {
        let args = tag(&raw, "command-args").unwrap_or_default();
        return format!(
            "{}{}{}",
            if name.starts_with('/') { "" } else { "/" },
            name,
            if args.is_empty() {
                String::new()
            } else {
                format!(" {args}")
            }
        );
    }
    raw
}
fn task_status(status: &str) -> &str {
    if matches!(status, "in_progress" | "completed" | "deleted") {
        status
    } else {
        "pending"
    }
}
fn task_item(value: &Value) -> Option<Value> {
    let text = first(value, &["subject", "content", "text"])?;
    let status = task_status(s(value, "status").unwrap_or("pending"));
    if status == "deleted" {
        return None;
    }
    let mut item = json!({"text":text.trim(),"status":status,"completed":status=="completed"});
    if let Some(id) = first(value, &["id", "taskId"]) {
        item["id"] = json!(id.trim());
    }
    if let Some(active) = first(value, &["activeForm", "active_form"]) {
        item["activeForm"] = json!(active.trim());
    }
    Some(item)
}
fn child_status(status: &str) -> Option<&'static str> {
    match status {
        "pending" | "running" | "paused" => Some("running"),
        "completed" => Some("completed"),
        "failed" => Some("failed"),
        "killed" | "stopped" => Some("canceled"),
        _ => None,
    }
}
fn model_label(model: &str) -> String {
    super::catalog::model_label(Some(model))
        .map(str::to_owned)
        .unwrap_or_else(|| model.to_owned())
}
fn subtitle(
    title: Option<&str>,
    model: Option<&str>,
    effort: Option<&str>,
    tokens: Option<f64>,
) -> String {
    let mut parts = Vec::new();
    if let Some(title) = title {
        parts.push(title.to_owned());
    }
    if let Some(model) = model {
        parts.push(model_label(model));
    }
    if let Some(effort) = effort {
        parts.push(if effort == "xhigh" {
            "Extra High".to_owned()
        } else {
            effort
                .split(|c: char| c == '-' || c == '_' || c.is_whitespace())
                .filter(|v| !v.is_empty())
                .map(|v| {
                    let mut chars = v.chars();
                    let mut part = chars.next().unwrap().to_uppercase().to_string();
                    part.extend(chars);
                    part
                })
                .collect::<Vec<_>>()
                .join(" ")
        });
    }
    if let Some(tokens) = tokens.filter(|n| *n > 0.0) {
        parts.push(if tokens < 1000.0 {
            format!("{} tokens", tokens.round())
        } else {
            format!("{}k tokens", (tokens / 100.0).round() / 10.0)
        });
    }
    parts.join(" · ")
}

/// Canonical tool detail mapping shared with permission previews and replay.
pub fn tool_call(
    name: &str,
    id: &str,
    input: &Value,
    output: &Value,
    status: &str,
    error: &Value,
) -> Value {
    let name = name.trim();
    if name == "ExitPlanMode" && input["plan"].is_string() {
        let mut item = json!({"type":"tool_call","callId":id,"name":if status=="running"{name}else{"plan_approval"},"status":if status=="failed"{"completed"}else{status},"error":null,"detail":{"type":"plan","text":input["plan"]},"metadata":{}});
        if matches!(status, "completed" | "failed") {
            item["metadata"]["approved"] = json!(status == "completed");
        }
        return item;
    }
    let speak = name == "speak" || name.ends_with("__speak");
    json!({"type":"tool_call","callId":id,"name":if speak{"speak"}else{name},"status":status,"error":if status=="failed"{if error.is_null(){json!({"message":"Tool call failed"})}else{error.clone()}}else{Value::Null},"detail":tool_detail(if speak{"speak"}else{name},input,output)})
}

fn tool_detail(name: &str, input: &Value, output: &Value) -> Value {
    let unknown = || json!({"type":"unknown","input":input,"output":output});
    let path = first(input, &["path", "file_path", "filePath"])
        .or_else(|| first(output, &["path", "file_path", "filePath"]));
    match name {
        "Bash" | "bash" | "shell" | "exec_command" => {
            let command = input
                .get("command")
                .or_else(|| input.get("cmd"))
                .or_else(|| output.get("command"));
            let command = command.and_then(|v| {
                v.as_str().map(str::to_owned).or_else(|| {
                    v.as_array().map(|v| {
                        v.iter()
                            .filter_map(Value::as_str)
                            .map(str::trim)
                            .filter(|v| !v.is_empty())
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                })
            });
            let Some(command) = command.filter(|v| !v.trim().is_empty()) else {
                return unknown();
            };
            let mut detail = json!({"type":"shell","command":command});
            if let Some(cwd) = first(input, &["cwd", "directory"]) {
                detail["cwd"] = json!(cwd);
            }
            let text = output
                .as_str()
                .or_else(|| {
                    first(
                        output,
                        &[
                            "output",
                            "text",
                            "content",
                            "aggregated_output",
                            "aggregatedOutput",
                        ],
                    )
                })
                .or_else(|| {
                    ["structuredContent", "structured_content", "result"]
                        .iter()
                        .find_map(|key| first(&output[*key], &["output", "text", "content"]))
                });
            if let Some(text) = text.filter(|v| !v.is_empty()) {
                detail["output"] = json!(text);
            }
            if let Some(code) = output
                .get("exitCode")
                .or_else(|| output.get("exit_code"))
                .or_else(|| output["metadata"].get("exitCode"))
                .or_else(|| output["metadata"].get("exit_code"))
            {
                detail["exitCode"] = code.clone();
            }
            detail
        }
        "Read" | "read" | "read_file" | "view_file" => {
            let Some(path) = path else {
                return unknown();
            };
            let mut detail = json!({"type":"read","filePath":path});
            let text = read_output_text(output, 0).unwrap_or_default();
            if !text.is_empty() {
                let (content, offset) = strip_read_gutter(&text);
                detail["content"] = json!(content);
                if let Some(offset) = offset {
                    detail["offset"] = json!(offset);
                }
            }
            copy(input, "offset", &mut detail, "offset");
            copy(input, "limit", &mut detail, "limit");
            detail
        }
        "Write" | "write" | "write_file" | "create_file" => {
            let Some(path) = path else {
                return unknown();
            };
            let mut detail = json!({"type":"write","filePath":path});
            if let Some(content) = first(input, &["content", "new_content", "newContent"])
                .or_else(|| first(output, &["content", "new_content", "newContent"]))
            {
                detail["content"] = json!(content);
            }
            detail
        }
        "Edit" | "MultiEdit" | "multi_edit" | "edit" | "apply_patch" | "apply_diff"
        | "str_replace_editor" => {
            let Some(path) = path else {
                return unknown();
            };
            let mut detail = json!({"type":"edit","filePath":path});
            for (keys, target) in [
                (
                    &["old_string", "old_str", "oldContent", "old_content"][..],
                    "oldString",
                ),
                (
                    &[
                        "new_string",
                        "new_str",
                        "newContent",
                        "new_content",
                        "content",
                    ][..],
                    "newString",
                ),
                (
                    &["patch", "diff", "unified_diff", "unifiedDiff"][..],
                    "unifiedDiff",
                ),
            ] {
                if let Some(text) = first(input, keys).or_else(|| first(output, keys)) {
                    detail[target] = json!(text);
                }
            }
            detail
        }
        "WebSearch" | "web_search" | "search" | "Grep" | "grep" | "Glob" | "glob" => {
            let Some(query) = first(input, &["query", "pattern", "search", "search_term"]) else {
                return unknown();
            };
            let kind = match name {
                "WebSearch" | "web_search" => "web_search",
                "Grep" | "grep" => "grep",
                "Glob" | "glob" => "glob",
                _ => "search",
            };
            let mut detail = json!({"type":"search","query":query,"toolName":kind});
            let parsed = output
                .get("output")
                .filter(|v| v.is_object())
                .unwrap_or(output);
            for key in [
                "content",
                "numFiles",
                "numMatches",
                "mode",
                "durationMs",
                "truncated",
                "durationSeconds",
            ] {
                copy(parsed, key, &mut detail, key);
            }
            if let Some(files) = parsed
                .get("filenames")
                .filter(|v| v.as_array().is_some_and(|v| !v.is_empty()))
            {
                detail["filePaths"] = files.clone();
            }
            if kind == "grep" && detail.get("content").is_none() {
                if let Some(text) = s(parsed, "output") {
                    detail["content"] = json!(text);
                    detail["numFiles"] = json!(0);
                }
            }
            if let Some(results) = parsed["results"].as_array() {
                let web: Vec<_> = results
                    .iter()
                    .filter_map(|v| v["content"].as_array())
                    .flatten()
                    .cloned()
                    .collect();
                let annotations: Vec<_> =
                    results.iter().filter(|v| v.is_string()).cloned().collect();
                if !web.is_empty() {
                    detail["webResults"] = json!(web);
                }
                if !annotations.is_empty() {
                    detail["annotations"] = json!(annotations);
                }
            }
            detail
        }
        "WebFetch" | "web_fetch" | "WebFetchTool" | "web_fetch_tool" | "webfetch" => {
            let Some(url) = s(input, "url").or_else(|| s(output, "url")) else {
                return unknown();
            };
            let mut detail = json!({"type":"fetch","url":url});
            copy(input, "prompt", &mut detail, "prompt");
            for key in ["result", "code", "codeText", "bytes", "durationMs"] {
                copy(output, key, &mut detail, key);
            }
            detail
        }
        "Skill" => {
            let Some(skill) = s(input, "skill") else {
                return unknown();
            };
            let mut detail = json!({"type":"plain_text","label":skill,"icon":"sparkles"});
            if let Some(text) = output.as_str().or_else(|| s(output, "output")) {
                detail["text"] = json!(text);
            }
            detail
        }
        "speak" => {
            if let Some(text) = input
                .as_str()
                .or_else(|| s(input, "text"))
                .filter(|v| !v.trim().is_empty())
            {
                json!({"type":"unknown","input":text.trim(),"output":null})
            } else {
                unknown()
            }
        }
        _ => unknown(),
    }
}
fn strip_read_gutter(text: &str) -> (String, Option<u64>) {
    let mut lines = Vec::new();
    let mut first = None;
    for line in text.split('\n') {
        if line.is_empty() {
            lines.push("");
            continue;
        }
        let trimmed = line.trim_start();
        let digits = trimmed.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return (text.to_owned(), None);
        }
        let rest = &trimmed[digits..];
        let content = rest
            .strip_prefix('→')
            .or_else(|| rest.strip_prefix('\t'))
            .or_else(|| rest.strip_prefix(" |"));
        let Some(content) = content else {
            return (text.to_owned(), None);
        };
        if first.is_none() {
            first = trimmed[..digits].parse().ok();
        }
        lines.push(content);
    }
    (lines.join("\n"), first)
}
fn tool_output(name: &str, input: &Value, content: &Value) -> Value {
    let text = tool_result_text(content);
    let lower = name.to_lowercase();
    if lower.contains("bash")
        || lower.contains("shell")
        || lower.contains("command")
        || input.get("command").is_some()
    {
        let mut output = json!({"type":"command","command":input.get("command").cloned().unwrap_or_else(||json!("command")),"output":text});
        copy(input, "cwd", &mut output, "cwd");
        return output;
    }
    if let Some(path) = s(input, "file_path") {
        if lower.contains("write") {
            return json!({"type":"file_write","filePath":path,"oldContent":"","newContent":s(input,"content").unwrap_or(&text)});
        }
        if lower.contains("edit") || lower.contains("patch") {
            let mut output = json!({"type":"file_edit","filePath":path});
            for (keys, target) in [
                (&["old_str", "old_string"][..], "oldContent"),
                (&["new_str", "new_string"][..], "newContent"),
                (&["patch", "diff"][..], "diff"),
            ] {
                if let Some(value) = first(input, keys) {
                    output[target] = json!(value);
                }
            }
            return output;
        }
        if lower.contains("read") {
            return json!({"type":"file_read","filePath":path,"content":text});
        }
    }
    if text.is_empty() {
        Value::Null
    } else {
        json!({"output":serde_json::from_str::<Value>(&text).unwrap_or_else(|_|json!(text))})
    }
}

/// Native can_use_tool request payload to the public permission shape.
pub fn permission_request(
    provider: &str,
    id: &str,
    native: &Value,
    plan_resume_mode: Option<&str>,
) -> Value {
    let name = first(native, &["tool_name", "toolName"]).unwrap_or("tool");
    let input = native
        .get("input")
        .filter(|v| v.is_object())
        .cloned()
        .unwrap_or_else(|| json!({}));
    let kind = if name == "ExitPlanMode" {
        "plan"
    } else if name == "AskUserQuestion" && input["questions"].is_array() {
        "question"
    } else {
        "tool"
    };
    let tool_id = first(native, &["tool_use_id", "toolUseID"]);
    let mut request = json!({"id":id,"provider":provider,"name":name,"kind":kind,"input":input});
    if let Some(tool) = tool_id {
        request["metadata"] = json!({"toolUseId":tool});
    }
    if name == "ExitPlanMode" {
        if let Some(plan) = s(&input, "plan") {
            if !request["metadata"].is_object() {
                request["metadata"] = json!({});
            }
            request["metadata"]["planText"] = json!(plan);
        }
        let mut actions = vec![
            json!({"id":"reject","label":"Reject","behavior":"deny","variant":"danger","intent":"dismiss"}),
            json!({"id":"implement","label":"Implement","behavior":"allow","variant":"primary","intent":"implement"}),
        ];
        if let Some(mode) = plan_resume_mode.filter(|v| matches!(*v, "auto" | "bypassPermissions"))
        {
            let label = if mode == "auto" {
                "Auto mode"
            } else {
                "Bypass"
            };
            actions.push(json!({"id":"implement_resume","label":format!("Implement with {label}"),"behavior":"allow","variant":"secondary","intent":"implement_resume"}));
        }
        request["actions"] = json!(actions);
    }
    if kind == "tool" {
        request["detail"] = tool_detail(name, &input, &Value::Null);
    }
    if kind == "question" {
        if let Some(questions) = request["input"]["questions"].as_array_mut() {
            for question in questions {
                if question.is_object() {
                    question["allowOther"] = json!(true);
                }
            }
        }
        if let Some(question) = input["questions"]
            .as_array()
            .and_then(|v| v.iter().find(|v| v.is_object()))
        {
            if let Some(title) = s(question, "question") {
                request["title"] = json!(title.trim());
                let labels: Vec<_> = question["options"]
                    .as_array()
                    .map(|v| {
                        v.iter()
                            .filter_map(|o| o.as_str().or_else(|| s(o, "label")))
                            .map(str::trim)
                            .filter(|v| !v.is_empty())
                            .collect()
                    })
                    .unwrap_or_default();
                if !labels.is_empty() {
                    request["description"] = json!(labels.join(" / "));
                }
            }
        }
    }
    if let Some(suggestions) = native
        .get("permission_suggestions")
        .or_else(|| native.get("suggestions"))
    {
        request["suggestions"] = suggestions.clone();
    }
    request
}

/// Validates before the runtime removes a pending permission or writes its reply.
pub fn permission_response(request: &Value, response: &Value) -> Result<Value> {
    match s(response, "behavior") {
        Some("deny") => {
            let mut result = json!({"behavior":"deny","message":response.get("message").cloned().unwrap_or_else(||json!("Permission request denied"))});
            copy(response, "interrupt", &mut result, "interrupt");
            Ok(result)
        }
        Some("allow") => {
            let mut input = response
                .get("updatedInput")
                .cloned()
                .unwrap_or_else(|| request["input"].clone());
            if request["kind"] == "question" {
                let mut merged = request["input"].as_object().cloned().unwrap_or_default();
                if let Some(update) = response["updatedInput"].as_object() {
                    merged.extend(update.clone());
                }
                let mut answers = Map::new();
                if let Some(questions) = merged.get_mut("questions").and_then(Value::as_array_mut) {
                    for question in questions {
                        if let Some(text) = s(question, "question") {
                            let text = text.trim();
                            let answer = response["updatedInput"]["answers"]
                                .get(text)
                                .and_then(Value::as_str)
                                .filter(|v| !v.trim().is_empty())
                                .or_else(|| {
                                    s(question, "header")
                                        .and_then(|header| {
                                            response["updatedInput"]["answers"].get(header.trim())
                                        })
                                        .and_then(Value::as_str)
                                        .filter(|v| !v.trim().is_empty())
                                });
                            if let Some(answer) = answer {
                                answers.insert(text.to_owned(), json!(answer.trim()));
                            }
                        }
                        if let Some(question) = question.as_object_mut() {
                            question.remove("allowOther");
                        }
                    }
                }
                if answers.is_empty() {
                    bail!(
                        "question_answer_required: Permission request '{}' requires updatedInput.answers keyed by a question's full text or header, with a non-empty string value. The request is still pending; retry with that shape.",
                        s(request, "id").unwrap_or("")
                    );
                }
                merged.insert("answers".to_owned(), Value::Object(answers));
                input = Value::Object(merged);
            }
            let mut result = json!({"behavior":"allow","updatedInput":input});
            if let Some(updates) = response["updatedPermissions"].as_array() {
                let updates: Vec<_> = updates
                    .iter()
                    .filter(|u| valid_permission_update(u))
                    .cloned()
                    .collect();
                if !updates.is_empty() {
                    result["updatedPermissions"] = json!(updates);
                }
            }
            Ok(result)
        }
        _ => bail!("Claude permission response requires behavior allow or deny"),
    }
}
fn valid_permission_update(update: &Value) -> bool {
    matches!(
        s(update, "type"),
        Some("addRules" | "replaceRules" | "removeRules")
    ) && update["rules"].is_array()
        && update["behavior"].is_string()
        && update["destination"].is_string()
}
pub fn permission_resolution_events(
    provider: &str,
    request: &Value,
    response: &Value,
) -> Vec<Value> {
    let mut events = Vec::new();
    if request["kind"] == "plan" {
        let id = s(&request["metadata"], "toolUseId")
            .or_else(|| s(request, "id"))
            .unwrap_or("");
        let mut item = tool_call(
            "ExitPlanMode",
            id,
            &request["input"],
            &Value::Null,
            if response["behavior"] == "allow" {
                "completed"
            } else {
                "failed"
            },
            &response["message"],
        );
        item["metadata"]["actionId"] =
            response
                .get("selectedActionId")
                .cloned()
                .unwrap_or_else(|| {
                    json!(if response["behavior"] == "allow" {
                        "implement"
                    } else {
                        "reject"
                    })
                });
        events.push(json!({"type":"timeline","provider":provider,"item":item}));
    } else if request["kind"] == "tool" && response["behavior"] == "deny" {
        let id = s(&request["metadata"], "toolUseId")
            .or_else(|| s(request, "id"))
            .unwrap_or("");
        let error = json!({"message":response.get("message").cloned().unwrap_or_else(||json!("Permission denied"))});
        let item = tool_call(
            s(request, "name").unwrap_or("tool"),
            id,
            &request["input"],
            &Value::Null,
            "failed",
            &error,
        );
        events.push(json!({"type":"timeline","provider":provider,"item":item}));
    }
    events.push(json!({"type":"permission_resolved","provider":provider,"requestId":request["id"],"resolution":response}));
    events
}

// Parse partial JSON values without guessing unfinished keys/literals. Complete JSON goes
// through serde; this parser retains completed fields and an unfinished string's safe prefix.
fn partial_json(text: &str) -> Option<Value> {
    if let Ok(value) = serde_json::from_str(text) {
        return Some(value);
    }
    PartialJson { text, index: 0 }.value(0).map(|v| v.0)
}
struct PartialJson<'a> {
    text: &'a str,
    index: usize,
}
impl PartialJson<'_> {
    fn skip(&mut self) {
        while self
            .text
            .as_bytes()
            .get(self.index)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.index += 1;
        }
    }
    fn string(&mut self) -> Option<(String, bool)> {
        if self.text.as_bytes().get(self.index) != Some(&b'"') {
            return None;
        }
        let start = self.index;
        self.index += 1;
        let mut escaped = false;
        while let Some(&byte) = self.text.as_bytes().get(self.index) {
            self.index += 1;
            if escaped {
                escaped = false;
                continue;
            }
            if byte == b'\\' {
                escaped = true;
                continue;
            }
            if byte == b'"' {
                return serde_json::from_str(&self.text[start..self.index])
                    .ok()
                    .map(|s| (s, true));
            }
        }
        let mut end = self.index;
        // Back up incomplete escapes (including a dangling UTF-16 surrogate) rather than
        // emitting invalid Unicode into the public timeline.
        while end > start + 1 {
            if !self.text.is_char_boundary(end) {
                end -= 1;
                continue;
            }
            let candidate = format!("{}\"", &self.text[start..end]);
            if let Ok(value) = serde_json::from_str::<String>(&candidate) {
                return Some((value, false));
            }
            end -= 1;
        }
        Some((String::new(), false))
    }
    fn value(&mut self, depth: usize) -> Option<(Value, bool)> {
        if depth > 128 {
            return None;
        }
        self.skip();
        match *self.text.as_bytes().get(self.index)? {
            b'"' => self.string().map(|(s, c)| (json!(s), c)),
            b'{' => {
                self.index += 1;
                let mut result = Map::new();
                loop {
                    self.skip();
                    if self.text.as_bytes().get(self.index) == Some(&b'}') {
                        self.index += 1;
                        return Some((Value::Object(result), true));
                    }
                    let Some((key, true)) = self.string() else {
                        return Some((Value::Object(result), false));
                    };
                    self.skip();
                    if self.text.as_bytes().get(self.index) != Some(&b':') {
                        return Some((Value::Object(result), false));
                    }
                    self.index += 1;
                    let Some((value, complete)) = self.value(depth + 1) else {
                        return Some((Value::Object(result), false));
                    };
                    result.insert(key, value);
                    if !complete {
                        return Some((Value::Object(result), false));
                    }
                    self.skip();
                    match self.text.as_bytes().get(self.index) {
                        Some(b',') => self.index += 1,
                        Some(b'}') => {
                            self.index += 1;
                            return Some((Value::Object(result), true));
                        }
                        _ => return Some((Value::Object(result), false)),
                    }
                }
            }
            b'[' => {
                self.index += 1;
                let mut result = Vec::new();
                loop {
                    self.skip();
                    if self.text.as_bytes().get(self.index) == Some(&b']') {
                        self.index += 1;
                        return Some((json!(result), true));
                    }
                    let Some((value, complete)) = self.value(depth + 1) else {
                        return Some((json!(result), false));
                    };
                    result.push(value);
                    if !complete {
                        return Some((json!(result), false));
                    }
                    self.skip();
                    match self.text.as_bytes().get(self.index) {
                        Some(b',') => self.index += 1,
                        Some(b']') => {
                            self.index += 1;
                            return Some((json!(result), true));
                        }
                        _ => return Some((json!(result), false)),
                    }
                }
            }
            _ => {
                let start = self.index;
                while self
                    .text
                    .as_bytes()
                    .get(self.index)
                    .is_some_and(|b| !b.is_ascii_whitespace() && !b",]}".contains(b))
                {
                    self.index += 1;
                }
                let token = &self.text[start..self.index];
                if let Ok(value) = serde_json::from_str(token) {
                    return Some((value, true));
                }
                if token.starts_with('-')
                    || token.as_bytes().first().is_some_and(u8::is_ascii_digit)
                {
                    for end in (1..token.len()).rev() {
                        if let Ok(value) = serde_json::from_str::<serde_json::Number>(&token[..end])
                        {
                            return Some((Value::Number(value), false));
                        }
                    }
                }
                None
            }
        }
    }
}

fn tag(text: &str, name: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = lower.find(&open)? + open.len();
    let end = start + lower[start..].find(&close)?;
    let value = text[start..end].trim();
    (!value.is_empty()).then(|| value.to_owned())
}
fn notification_fields(frame: &Value) -> Option<Value> {
    let raw = if frame["type"] == "user" {
        content_text(&frame["message"]["content"])
    } else {
        content_text(&frame["content"])
    };
    if !(frame["type"] == "system" && frame["subtype"] == "task_notification")
        && !raw.contains("<task-notification>")
    {
        return None;
    }
    let mut fields = json!({});
    for (target, key, tags) in [
        ("taskId", "task_id", &["task-id"][..]),
        (
            "toolUseId",
            "tool_use_id",
            &["tool-use-id", "tool_use_id"][..],
        ),
        ("status", "status", &["status"][..]),
        ("summary", "summary", &["summary"][..]),
        (
            "outputFile",
            "output_file",
            &["output-file", "output_file"][..],
        ),
    ] {
        if let Some(value) = s(frame, key)
            .map(str::to_owned)
            .or_else(|| tags.iter().find_map(|name| tag(&raw, name)))
        {
            fields[target] = json!(value);
        }
    }
    if !raw.is_empty() {
        fields["rawText"] = json!(raw);
    }
    if let Some(id) = first(frame, &["uuid", "message_id"]) {
        fields["messageId"] = json!(id);
    }
    copy(frame, "notificationId", &mut fields, "notificationId");
    Some(fields)
}
fn notification_item(frame: &Value) -> Option<Value> {
    let fields = notification_fields(frame)?;
    let id = s(&fields, "notificationId")
        .map(str::to_owned)
        .or_else(|| {
            first(&fields, &["messageId", "taskId"])
                .map(|v| format!("task_notification_{}", sanitize_id(v)))
        })
        .unwrap_or_else(|| {
            let seed = ["status", "summary", "outputFile", "rawText"]
                .iter()
                .filter_map(|key| s(&fields, key))
                .collect::<Vec<_>>()
                .join("|");
            format!(
                "task_notification_{}",
                &hex::encode(Sha1::digest(if seed.is_empty() {
                    b"task_notification".as_slice()
                } else {
                    seed.as_bytes()
                }))[..12]
            )
        });
    let status = s(&fields, "status").unwrap_or("").to_lowercase();
    let summary = s(&fields, "summary");
    let label = summary.map(str::to_owned).unwrap_or_else(|| {
        if status.is_empty() {
            "Background task notification".to_owned()
        } else {
            format!("Background task {status}")
        }
    });
    let lifecycle = match status.as_str() {
        "failed" | "error" => "failed",
        "canceled" | "cancelled" => "canceled",
        _ => "completed",
    };
    let mut item = json!({"type":"tool_call","callId":id,"name":"task_notification","status":lifecycle,"error":if lifecycle=="failed"{json!({"message":summary.unwrap_or("Background task failed")})}else{Value::Null},"detail":{"type":"plain_text","label":label,"icon":"wrench"},"metadata":{"synthetic":true,"source":"claude_task_notification"}});
    if let Some(text) = first(&fields, &["rawText", "summary"]) {
        item["detail"]["text"] = json!(text);
    }
    for key in ["taskId", "toolUseId", "status", "outputFile"] {
        copy(&fields, key, &mut item["metadata"], key);
    }
    Some(item)
}
fn sanitize_id(value: &str) -> String {
    let mut result = String::new();
    let mut replacing = false;
    for c in value.trim().chars() {
        if c.is_ascii_alphanumeric() || "._:-".contains(c) {
            result.push(c);
            replacing = false;
        } else if !replacing {
            result.push('_');
            replacing = true;
        }
    }
    result
}
fn read_workflow_result(path: &str) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() > 1024 * 1024 {
        return None;
    }
    let parsed: Value = serde_json::from_slice(&bytes).ok()?;
    format_workflow_result(parsed.get("result")?)
}
fn format_workflow_result(value: &Value) -> Option<String> {
    let mut value = value;
    for _ in 0..8 {
        if let Some(object) = value.as_object().filter(|o| o.len() == 1) {
            value = object.values().next()?;
        } else {
            break;
        }
    }
    let text = match value {
        Value::Null => return None,
        Value::String(text) => text.trim().to_owned(),
        Value::Bool(_) | Value::Number(_) => value.to_string(),
        _ => format!(
            "```json\n{}\n```",
            serde_json::to_string_pretty(value).ok()?
        ),
    };
    if text.is_empty() {
        return None;
    }
    if text.encode_utf16().count() <= 100_000 {
        return Some(text);
    }
    let mut end = 0;
    let mut units = 0;
    for (i, c) in text.char_indices() {
        units += c.len_utf16();
        if units > 100_000 {
            break;
        }
        end = i + c.len_utf8();
    }
    Some(format!("{}\n\n…", &text[..end]))
}
fn split_images(content: &Value) -> (Value, Vec<Value>) {
    let Some(blocks) = content.as_array() else {
        return (content.clone(), Vec::new());
    };
    let mut text = Vec::with_capacity(blocks.len());
    let mut images = Vec::new();
    for block in blocks {
        if block["type"] == "image"
            && (block["source"]["data"].is_string() || block["data"].is_string())
        {
            images.push(block.clone());
            text.push(json!({"type":"text","text":"[image]"}));
        } else {
            text.push(block.clone());
        }
    }
    (json!(text), images)
}
fn image_item(image: &Value) -> Result<Option<Value>> {
    let source = image.get("source").unwrap_or(image);
    super::super::prompt::projected_image(&json!({
        "data":source.get("data"),
        "mimeType":first(source,&["media_type","mimeType"])
    }))
}

/// Replay uses the same tool/task mapper but never opens a live or autonomous turn.
pub fn project_history(provider: &str, history: &super::storage::ReplayHistory) -> Vec<Value> {
    Projection::new(provider).project_history(history)
}

#[derive(Default)]
struct ReplayFacts {
    tools: HashMap<String, Value>,
    outcomes: HashMap<String, bool>,
    links: HashMap<String, String>,
    workflows: HashMap<String, String>,
}
impl ReplayFacts {
    fn read(entries: &[Value]) -> Self {
        let mut facts = Self::default();
        for entry in entries {
            if let Some(blocks) = entry["message"]["content"].as_array() {
                for block in blocks {
                    if block["type"] == "tool_use"
                        && matches!(s(block, "name"), Some("Task" | "Agent" | "Workflow"))
                    {
                        if let Some(id) = s(block, "id") {
                            facts.tools.insert(id.to_owned(), block.clone());
                        }
                    }
                }
            }
        }
        for entry in entries {
            if let Some(blocks) = entry["message"]["content"].as_array() {
                for block in blocks {
                    if block["type"] != "tool_result" {
                        continue;
                    }
                    let Some(id) =
                        s(block, "tool_use_id").filter(|id| facts.tools.contains_key(*id))
                    else {
                        continue;
                    };
                    facts
                        .outcomes
                        .insert(id.to_owned(), block["is_error"] == true);
                    let text = content_text(&block["content"]);
                    if let Some((_, rest)) = text.split_once("agentId:") {
                        let agent: String = rest
                            .trim_start()
                            .chars()
                            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                            .collect();
                        if !agent.is_empty() {
                            facts.links.insert(agent, id.to_owned());
                        }
                    }
                    if facts.tools[id]["name"] == "Workflow" {
                        if let Some(run) = text
                            .lines()
                            .find_map(|line| line.strip_prefix("Run ID:").map(str::trim))
                            .filter(|id| id.starts_with("wf_"))
                        {
                            let run: String = run
                                .chars()
                                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                                .collect();
                            facts.workflows.insert(run, id.to_owned());
                        }
                    }
                }
            }
        }
        facts
    }
    fn child_id(&self, child: &super::storage::ReplaySubagent) -> Option<String> {
        child
            .meta
            .as_ref()
            .and_then(|meta| s(meta, "toolUseId"))
            .filter(|id| self.tools.contains_key(*id))
            .map(str::to_owned)
            .or_else(|| self.links.get(&child.agent_id).cloned())
    }
}
impl Projection {
    pub fn project_history(&mut self, history: &super::storage::ReplayHistory) -> Vec<Value> {
        self.end_turn();
        self.replaying = true;
        let mut events = Vec::new();
        let root = ReplayFacts::read(&history.parent_entries);
        let mut resolved: Vec<(String, ReplayFacts)> = Vec::new();
        let mut remaining: Vec<_> = history.subagents.iter().collect();
        remaining.sort_by_key(|child| {
            child
                .meta
                .as_ref()
                .and_then(|m| m["spawnDepth"].as_u64())
                .unwrap_or(1)
        });
        loop {
            let mut progress = false;
            let mut index = 0;
            while index < remaining.len() {
                let child = remaining[index];
                let link = root
                    .child_id(child)
                    .map(|id| (None, id, &root))
                    .or_else(|| {
                        resolved.iter().find_map(|(owner, facts)| {
                            facts
                                .child_id(child)
                                .map(|id| (Some(owner.clone()), id, facts))
                        })
                    });
                let Some((owner, id, facts)) = link else {
                    index += 1;
                    continue;
                };
                let tool = &facts.tools[&id];
                let input = &tool["input"];
                let meta = child.meta.as_ref().unwrap_or(&Value::Null);
                let title = first(input, &["name", "subagent_type"])
                    .or_else(|| s(meta, "agentType"))
                    .map(str::to_owned);
                let description = s(input, "description")
                    .or_else(|| s(meta, "description"))
                    .map(str::to_owned);
                let outcome = facts.outcomes.get(&id).copied();
                self.aliases.insert(id.clone(), id.clone());
                self.task_ids.insert(child.agent_id.clone(), id.clone());
                self.tasks.insert(
                    id.clone(),
                    Child {
                        title: title.clone(),
                        model: None,
                        effort: None,
                        tokens: None,
                        subtitle: title.clone().unwrap_or_default(),
                        status: "running".to_owned(),
                        background: false,
                        workflow: false,
                        turn: None,
                        last_result: None,
                        description: description.clone(),
                        parent: owner.clone(),
                        ..Child::default()
                    },
                );
                let mut declaration =
                    json!({"type":"upsert","id":id,"toolCallId":id,"status":"running"});
                if let Some(title) = title {
                    declaration["title"] = json!(title);
                }
                if let Some(description) = description {
                    declaration["description"] = json!(description);
                }
                if let Some(owner) = owner {
                    declaration["parentSubagentId"] = json!(owner);
                }
                if let Some(time) = child
                    .entries
                    .first()
                    .and_then(|e| timestamp(&e["timestamp"]))
                {
                    declaration["timestamp"] = json!(time);
                }
                events.push(self.child_event(declaration));
                let mut terminal = None;
                for row in notification_replay_entries(&child.entries) {
                    if row["type"] == "assistant" {
                        terminal = if row["message"]["stop_reason"] == "end_turn" {
                            Some("completed")
                        } else {
                            None
                        };
                        if let Some(tokens) = usage_total(&row["message"]["usage"]) {
                            self.tasks.get_mut(&id).expect("declared child").tokens = Some(tokens);
                        }
                    }
                    events.extend(self.history_entry(&row, Some(&id)));
                }
                let mut ending = Vec::new();
                self.subtitle(&id, &mut ending);
                if let Some(status) = outcome
                    .map(|failed| if failed { "failed" } else { "completed" })
                    .or(terminal)
                {
                    self.set_status(&id, status, &mut ending);
                }
                if let Some(row) = child.entries.last() {
                    stamp(&mut ending, row);
                }
                events.extend(ending);
                resolved.push((id, ReplayFacts::read(&child.entries)));
                remaining.remove(index);
                progress = true;
            }
            if !progress {
                break;
            }
        }
        for workflow in &history.workflows {
            let Some(run_id) = s(workflow, "runId") else {
                continue;
            };
            let Some(id) = root.workflows.get(run_id) else {
                continue;
            };
            let description = first(workflow, &["summary", "workflowName"]);
            self.aliases.insert(id.clone(), id.clone());
            self.task_ids.insert(run_id.to_owned(), id.clone());
            self.tasks.insert(
                id.clone(),
                Child {
                    title: Some("Workflow".to_owned()),
                    model: s(workflow, "defaultModel").map(str::to_owned),
                    effort: None,
                    tokens: number(workflow, "totalTokens"),
                    subtitle: String::new(),
                    status: "running".to_owned(),
                    background: false,
                    workflow: true,
                    turn: None,
                    last_result: None,
                    description: description.map(str::to_owned),
                    ..Child::default()
                },
            );
            let mut declaration = json!({"type":"upsert","id":id,"toolCallId":id,"title":"Workflow","status":"running"});
            if let Some(text) = description {
                declaration["description"] = json!(text);
            }
            if let Some(time) = timestamp(&workflow["startTime"]) {
                declaration["timestamp"] = json!(time);
            }
            events.push(self.child_event(declaration));
            if let Some(text) = description {
                let mut prompt =
                    self.timeline(json!({"type":"user_message","text":text}), Some(id));
                if let Some(time) = timestamp(&workflow["startTime"]) {
                    prompt["event"]["timestamp"] = json!(time);
                }
                events.push(prompt);
            }
            let mut texts = HashSet::new();
            if let Some(rows) = history.workflow_entries_by_run_id.get(run_id) {
                for row in notification_replay_entries(rows) {
                    let mapped = self.history_entry(&row, Some(id));
                    for event in &mapped {
                        if event["event"]["item"]["type"] == "assistant_message" {
                            if let Some(text) = s(&event["event"]["item"], "text") {
                                texts.insert(text.trim().to_owned());
                            }
                        }
                    }
                    events.extend(mapped);
                }
            }
            let mut ending = Vec::new();
            if let Some(text) = workflow
                .get("result")
                .and_then(format_workflow_result)
                .filter(|text| !texts.contains(text.trim()))
            {
                ending
                    .push(self.timeline(json!({"type":"assistant_message","text":text}), Some(id)));
            }
            self.subtitle(id, &mut ending);
            let status = match s(workflow, "status")
                .unwrap_or("")
                .trim()
                .to_lowercase()
                .as_str()
            {
                "completed" => "completed",
                "canceled" | "cancelled" | "killed" | "stopped" => "canceled",
                _ => "failed",
            };
            self.set_status(id, status, &mut ending);
            stamp(&mut ending, workflow);
            events.extend(ending);
        }
        // Proven child tool owners were recorded before parent notifications are projected.
        for row in notification_replay_entries(&history.parent_entries) {
            if row["isSidechain"] != true {
                events.extend(self.history_entry(&row, None));
            }
        }
        self.replaying = false;
        events
    }
    fn history_entry(&mut self, row: &Value, owner: Option<&str>) -> Vec<Value> {
        if row["isCompactSummary"] == true {
            return Vec::new();
        }
        if !matches!(
            s(row, "type"),
            Some("user" | "assistant" | "system" | "queue-operation")
        ) {
            return Vec::new();
        }
        if row["type"] == "system"
            && !matches!(
                s(row, "subtype"),
                Some("compact_boundary" | "task_notification")
            )
        {
            return Vec::new();
        }
        if row["type"] == "system" && row["subtype"] == "compact_boundary" {
            let metadata = row
                .get("compactMetadata")
                .or_else(|| row.get("compact_metadata"))
                .unwrap_or(&Value::Null);
            let mut item = json!({"type":"compaction","status":"completed","trigger":if metadata["trigger"]=="manual"{"manual"}else{"auto"}});
            copy(metadata, "preTokens", &mut item, "preTokens");
            copy(metadata, "pre_tokens", &mut item, "preTokens");
            let mut events = vec![self.timeline(item, owner)];
            stamp(&mut events, row);
            return events;
        }
        let mut frame = row.clone();
        if let Some(owner) = owner {
            frame["parent_tool_use_id"] = json!(owner);
        }
        if row["type"] == "assistant" {
            if let Some(id) = s(row, "uuid") {
                frame["message_id"] = json!(id);
            }
            // History UUIDs are independent rows, not consecutive partial snapshots.
            self.lanes
                .entry(owner.unwrap_or("").to_owned())
                .or_default()
                .active = None;
        }
        let mut events = self.project(&frame);
        events.retain(|e| matches!(s(e, "type"), Some("timeline" | "provider_subagent")));
        for event in &mut events {
            let item = if event["type"] == "timeline" {
                &mut event["item"]
            } else {
                &mut event["event"]["item"]
            };
            if item["type"] == "tool_call" && matches!(s(item, "name"), Some("Task" | "Agent")) {
                if let Some(id) = s(item, "callId") {
                    if let Some(tool) = self.tools.get(id) {
                        item["detail"] = subagent_detail(
                            first(&tool.input, &["name", "subagent_type"]),
                            s(&tool.input, "description"),
                            "",
                        );
                    }
                }
            }
        }
        events
    }
}

fn notification_replay_entries(entries: &[Value]) -> Vec<Value> {
    let mut counts: HashMap<String, HashMap<String, u64>> = HashMap::new();
    entries
        .iter()
        .map(|entry| {
            let Some(fields) = notification_fields(entry).filter(|f| s(f, "taskId").is_some())
            else {
                return entry.clone();
            };
            let raw = s(&fields, "rawText").unwrap_or("");
            let mut remainder = raw.to_owned();
            for name in [
                "task-id",
                "tool-use-id",
                "tool_use_id",
                "status",
                "summary",
                "output-file",
                "output_file",
            ] {
                loop {
                    let lower = remainder.to_ascii_lowercase();
                    let open = format!("<{name}>");
                    let close = format!("</{name}>");
                    let Some(start) = lower.find(&open) else {
                        break;
                    };
                    let Some(end) = lower[start + open.len()..]
                        .find(&close)
                        .map(|i| start + open.len() + i + close.len())
                    else {
                        break;
                    };
                    remainder.replace_range(start..end, "");
                }
            }
            for marker in ["<task-notification>", "</task-notification>"] {
                while let Some(index) = remainder.to_ascii_lowercase().find(marker) {
                    remainder.replace_range(index..index + marker.len(), "");
                }
            }
            let signature = json!([
                fields["taskId"],
                fields["toolUseId"],
                fields["status"],
                fields["summary"],
                fields["outputFile"],
                remainder.trim()
            ])
            .to_string();
            let signature = hex::encode(Sha1::digest(signature.as_bytes()));
            let source = if entry["type"] == "queue-operation" {
                format!("queue-{}", s(entry, "operation").unwrap_or("undefined"))
            } else {
                s(entry, "type").unwrap_or("").to_owned()
            };
            let count = counts
                .entry(signature.clone())
                .or_default()
                .entry(source)
                .or_default();
            *count += 1;
            let mut entry = entry.clone();
            entry["notificationId"] = json!(format!("task_notification_{signature}_{count}"));
            entry
        })
        .collect()
}

fn subagent_detail(title: Option<&str>, description: Option<&str>, log: &str) -> Value {
    let mut detail = json!({"type":"sub_agent","log":log,"actions":[]});
    if let Some(title) = title {
        detail["subAgentType"] = json!(title);
    }
    if let Some(description) = description {
        detail["description"] = json!(description);
    }
    detail
}
impl Projection {
    fn legacy_child(&mut self, id: &str, events: &mut Vec<Value>) {
        let input = self.tools.get(id).map(|t| &t.input).unwrap_or(&Value::Null);
        let title = first(input, &["name", "subagent_type"])
            .unwrap_or("Claude subagent")
            .to_owned();
        let description = s(input, "description").map(str::to_owned);
        let parent = self.tool_owners.get(id).cloned();
        self.tasks.insert(
            id.to_owned(),
            Child {
                title: Some(title.clone()),
                description: description.clone(),
                parent: parent.clone(),
                status: "running".to_owned(),
                turn: self.turn.clone(),
                ..Child::default()
            },
        );
        let mut event = json!({"type":"upsert","id":id,"toolCallId":id,"title":title,"description":description,"status":"running"});
        if let Some(parent) = parent {
            event["parentSubagentId"] = json!(parent);
        }
        events.push(self.child_event(event));
    }
    fn enrich_child_card(&mut self, id: &str, events: &mut Vec<Value>) {
        let Some(child) = self.tasks.get_mut(id) else {
            return;
        };
        if child.workflow || child.parent.is_some() {
            return;
        }
        let mut changed = false;
        for event in events.iter() {
            if event["type"] != "provider_subagent"
                || event["event"]["id"] != id
                || event["event"]["type"] != "timeline"
            {
                continue;
            }
            let item = &event["event"]["item"];
            if item["type"] != "tool_call" {
                continue;
            }
            let Some(call_id) = s(item, "callId") else {
                continue;
            };
            let name = s(item, "name").unwrap_or("tool");
            let summary = first(
                &item["detail"],
                &["command", "filePath", "query", "url", "label"],
            )
            .map(|s| s.chars().take(160).collect::<String>());
            let line = summary
                .map(|summary| format!("[{name}] {summary}"))
                .unwrap_or_else(|| format!("[{name}]"));
            if child.actions.get(call_id).and_then(Value::as_str) != Some(&line) {
                child.actions.insert(call_id.to_owned(), json!(line));
                changed = true;
            }
        }
        while child.actions.len() > 200 {
            if let Some(key) = child.actions.keys().next().cloned() {
                child.actions.shift_remove(&key);
            }
        }
        if !changed {
            return;
        }
        let log = child
            .actions
            .values()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("\n");
        let mut card = child.settled_card.clone().unwrap_or_else(|| {
            tool_call(
                "Task",
                id,
                &Value::Null,
                &Value::Null,
                "running",
                &Value::Null,
            )
        });
        card["detail"] =
            subagent_detail(child.title.as_deref(), child.description.as_deref(), &log);
        let turn = child.turn.clone();
        let mut event = self.timeline(card, None);
        if let Some(turn) = turn {
            event["turnId"] = json!(turn);
        }
        events.push(event);
    }
}

fn tool_result_text(content: &Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_owned();
    }
    if let Some(blocks) = content.as_array().filter(|v| {
        v.iter()
            .all(|b| b["type"] == "text" && b["text"].is_string())
    }) {
        return blocks
            .iter()
            .filter_map(|b| b["text"].as_str())
            .collect::<String>();
    }
    let mut canonical = content.clone();
    canonical.sort_all_objects();
    canonical.to_string()
}

fn read_output_text(value: &Value, depth: usize) -> Option<String> {
    if depth > 8 {
        return None;
    }
    if let Some(text) = value.as_str().filter(|v| !v.trim().is_empty()) {
        let trimmed = text.trim();
        if trimmed.starts_with("<path>") {
            let lower = trimmed.to_ascii_lowercase();
            if let (Some(start), Some(end)) = (lower.find("<content>"), lower.rfind("</content>")) {
                if end >= start + 9 {
                    let content = &trimmed[start + 9..end];
                    let content = content
                        .strip_prefix("\r\n")
                        .or_else(|| content.strip_prefix('\n'))
                        .unwrap_or(content);
                    let content = content
                        .strip_suffix("\r\n")
                        .or_else(|| content.strip_suffix('\n'))
                        .unwrap_or(content);
                    return Some(decode_xml(content));
                }
            }
        }
        return Some(text.to_owned());
    }
    if let Some(values) = value.as_array() {
        let parts: Vec<_> = values
            .iter()
            .filter_map(|value| read_output_text(value, depth + 1))
            .collect();
        return (!parts.is_empty()).then(|| parts.join("\n"));
    }
    for key in [
        "text",
        "content",
        "output",
        "data",
        "structuredContent",
        "structured_content",
    ] {
        if let Some(text) = value
            .get(key)
            .and_then(|value| read_output_text(value, depth + 1))
        {
            return Some(text);
        }
    }
    None
}
fn decode_xml(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        output.push_str(&rest[..start]);
        rest = &rest[start..];
        let Some(end) = rest.find(';') else {
            output.push_str(rest);
            return output;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .and_then(|v| u32::from_str_radix(v, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|v| v.parse().ok()))
                .and_then(char::from_u32),
        };
        if let Some(c) = decoded {
            output.push(c);
        } else {
            output.push_str(&rest[..=end]);
        }
        rest = &rest[end + 1..];
    }
    output.push_str(rest);
    output
}
