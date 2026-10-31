use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    sync::Arc,
};

use anyhow::{Result, anyhow, bail, ensure};
use serde_json::{Value, json};

use super::transport::Connection;

/// MSP snapshots remain intact; only the outward projection uses native events.
pub struct Projection {
    host: Arc<Connection>,
    native_id: String,
    cwd: String,
    provider: String,
    turn_id: Option<String>,
    messages: HashMap<String, String>,
    timeline: Timeline,
    children: Vec<Child>,
}

struct Child {
    id: String,
    cursor: Option<String>,
    timeline: Timeline,
}

#[derive(Default)]
struct Timeline {
    items: HashMap<String, Value>,
}

impl Projection {
    pub fn new(host: Arc<Connection>, native_id: String, cwd: String) -> Self {
        Self {
            host,
            native_id,
            cwd,
            provider: "muse".into(),
            turn_id: None,
            messages: HashMap::new(),
            timeline: Timeline::default(),
            children: Vec::new(),
        }
    }

    pub fn set_provider(&mut self, provider: &str) {
        self.provider = provider.into();
    }
    pub fn set_turn_id(&mut self, turn_id: Option<String>) {
        self.turn_id = turn_id;
    }
    pub fn correlate(
        &mut self,
        command_id: impl Into<String>,
        client_message_id: impl Into<String>,
    ) {
        self.messages
            .insert(command_id.into(), client_message_id.into());
    }

    pub async fn fold(&mut self, item: Value) -> Result<Vec<Value>> {
        let item = normalize_item(item)?;
        let child_id = eligible_child(&item).map(str::to_owned);
        let call_id = item.get("callId").unwrap_or(&item["itemId"]).clone();
        let timestamp = item.get("recordedAt").cloned();
        let client_id = item
            .get("commandId")
            .and_then(Value::as_str)
            .and_then(|id| self.messages.get(id))
            .map(String::as_str);
        let (accepted, projected) = self
            .timeline
            .fold(&self.host, &self.native_id, item, client_id)
            .await?;
        let mut events = Vec::new();
        if let Some(item) = projected {
            events.push(self.timeline_event(item, timestamp));
        }
        if accepted && let Some(id) = child_id {
            if !self.children.iter().any(|child| child.id == id) {
                let result = self
                    .host
                    .request("session/read", json!({"sessionId":id}))
                    .await?;
                let session = object(&result, "session")?;
                string(session, "sessionId")?;
                string(&result, "viewCursor")?;
                let cwd = match session.get("workspaceRoot") {
                    Some(Value::String(value)) => value.as_str(),
                    Some(Value::Null) => &self.cwd,
                    _ => bail!("Muse child workspaceRoot must be a string or null"),
                };
                optional_string(session, "title")?;
                events.push(
                    json!({"type":"provider_subagent","provider":self.provider,"event":{
                        "type":"upsert","id":id,"parentSubagentId":null,"toolCallId":call_id,
                        "title":session.get("title").unwrap_or(&Value::Null),"description":null,
                        "status":"running","cwd":cwd
                    }}),
                );
                self.children.push(Child {
                    id: id.clone(),
                    cursor: None,
                    timeline: Timeline::default(),
                });
            }
            events.extend(self.page_child(&id).await?);
        }
        Ok(events)
    }

    pub fn delta(&mut self, params: &Value) -> Result<Vec<Value>> {
        Ok(self
            .timeline
            .delta(params)?
            .map(|item| self.timeline_event(item, None))
            .into_iter()
            .collect())
    }

    pub async fn child_notify(&mut self, session_id: &str, event: &Value) -> Result<Vec<Value>> {
        let Some(child) = self
            .children
            .iter_mut()
            .find(|child| child.id == session_id)
        else {
            return Ok(Vec::new());
        };
        let method = string(event, "method")?;
        let params = object(event, "params")?;
        let (item, timestamp) = match method {
            "item/started" | "item/updated" | "item/completed" => {
                let item = normalize_item(object(params, "item")?.clone())?;
                let timestamp = item.get("recordedAt").cloned();
                let (_, projected) = child
                    .timeline
                    .fold(&self.host, session_id, item, None)
                    .await?;
                (projected, timestamp)
            }
            "item/delta" => (child.timeline.delta(params)?, None),
            _ => (None, None),
        };
        Ok(item
            .map(|item| {
                let mut event = json!({"type":"timeline","id":session_id,"item":item});
                if let Some(timestamp) = timestamp {
                    event["timestamp"] = timestamp;
                }
                json!({"type":"provider_subagent","provider":self.provider,"event":event})
            })
            .into_iter()
            .collect())
    }

    pub async fn refresh_children(&mut self) -> Result<Vec<Value>> {
        let mut events = Vec::new();
        for index in 0..self.children.len() {
            let id = self.children[index].id.clone();
            events.extend(self.page_child(&id).await?);
        }
        Ok(events)
    }

    async fn page_child(&mut self, id: &str) -> Result<Vec<Value>> {
        let mut events = Vec::new();
        loop {
            let child = self
                .children
                .iter()
                .find(|child| child.id == id)
                .expect("known child");
            let mut params = json!({"sessionId":id,"limit":1000,"direction":"forward"});
            if let Some(cursor) = &child.cursor {
                params["cursor"] = json!(cursor);
            }
            let page = self.host.request("view/page", params).await?;
            let page_events = array(&page, "events")?;
            let next = match page.get("nextCursor") {
                Some(Value::String(cursor)) => Some(cursor.clone()),
                Some(Value::Null) => None,
                _ => bail!("Muse page nextCursor must be a string or null"),
            };
            for event in page_events {
                events.extend(self.child_notify(id, event).await?);
                let params = object(event, "params")?;
                optional_string(params, "sessionId")?;
                if let Some(cursor) = optional_string(params, "viewCursor")? {
                    if !cursor.is_empty() {
                        self.children
                            .iter_mut()
                            .find(|child| child.id == id)
                            .expect("known child")
                            .cursor = Some(cursor.into());
                    }
                }
            }
            let Some(next) = next else {
                break;
            };
            self.children
                .iter_mut()
                .find(|child| child.id == id)
                .expect("known child")
                .cursor = Some(next);
        }
        Ok(events)
    }

    pub fn close_children(&mut self) -> Vec<Value> {
        self.children
            .drain(..)
            .map(|child| {
                json!({
                    "type":"provider_subagent","provider":self.provider,
                    "event":{"type":"upsert","id":child.id,"status":"completed"}
                })
            })
            .collect()
    }

    fn timeline_event(&self, item: Value, timestamp: Option<Value>) -> Value {
        let mut event = json!({"type":"timeline","provider":self.provider,"item":item});
        if let Some(turn_id) = &self.turn_id {
            event["turnId"] = json!(turn_id);
        }
        if let Some(timestamp) = timestamp {
            event["timestamp"] = timestamp;
        }
        event
    }
}

impl Timeline {
    async fn fold(
        &mut self,
        host: &Connection,
        native_id: &str,
        item: Value,
        client_id: Option<&str>,
    ) -> Result<(bool, Option<Value>)> {
        if item["kind"] == "reminderChild" {
            return Ok((false, None));
        }
        let id = string(&item, "itemId")?;
        let previous = self.items.get(id);
        if previous.is_some_and(|previous| revision(previous) >= revision(&item)) {
            return Ok((false, None));
        }
        let patch =
            if item["status"] == "completed" {
                if let Some(reference) = item.get("patchRef") {
                    let output = host.request("item/readOutput", json!({
                    "sessionId":native_id,"itemId":id,"outputRef":string(reference,"id")?
                })).await?;
                    ensure!(
                        output["encoding"] == "utf8" && output["eof"] == true,
                        "invalidPatch: Muse edit patch is not a complete UTF-8 document"
                    );
                    Some(unified_diff(string(&output, "content")?)?)
                } else {
                    None
                }
            } else {
                None
            };
        let old_text = previous.and_then(|previous| snapshot_text(previous, &item));
        let projected = timeline_item(&item, old_text.as_deref(), client_id, patch.as_deref())?;
        self.items.insert(id.to_owned(), item);
        Ok((true, projected))
    }

    fn delta(&mut self, params: &Value) -> Result<Option<Value>> {
        let id = string(params, "itemId")?;
        let delta = string(params, "delta")?;
        let field = optional_string(params, "field")?.unwrap_or("text");
        let Some(item) = self.items.get_mut(id) else {
            return Ok(None);
        };
        if item["status"] != "inProgress" {
            return Ok(None);
        }
        let old_text = snapshot_text(item, item).map(Cow::into_owned);
        match field {
            "text" | "output" => {
                let key = if field == "output" {
                    "visibleOutput"
                } else {
                    "text"
                };
                if let Value::String(text) = &mut item[key] {
                    text.push_str(delta);
                }
            }
            _ => {
                let Some(index) = field.strip_prefix("summary.").filter(|index| {
                    !index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit())
                }) else {
                    return Ok(None);
                };
                let index: usize = index
                    .parse()
                    .map_err(|_| anyhow!("Muse summary index is too large"))?;
                let length = index
                    .checked_add(1)
                    .ok_or_else(|| anyhow!("Muse summary index is too large"))?;
                let summary = item["summary"].as_array_mut().expect("normalized summary");
                if summary.len() < length {
                    summary
                        .try_reserve(length - summary.len())
                        .map_err(|_| anyhow!("Muse summary index is too large"))?;
                    summary.resize(length, Value::Null);
                }
                if summary[index].is_null() {
                    summary[index] = Value::String(String::new());
                }
                if let Value::String(text) = &mut summary[index] {
                    text.push_str(delta);
                }
            }
        }
        timeline_item(item, old_text.as_deref(), None, None)
    }
}

fn revision(item: &Value) -> f64 {
    item["revision"].as_f64().expect("validated revision")
}

fn normalize_item(mut item: Value) -> Result<Value> {
    ensure!(item.is_object(), "Muse item must be an object");
    for key in ["itemId", "kind", "status"] {
        string(&item, key)?;
    }
    let revision = item
        .get("revision")
        .and_then(Value::as_f64)
        .ok_or_else(|| anyhow!("Muse item revision must be an integer"))?;
    ensure!(
        revision.fract() == 0.0,
        "Muse item revision must be an integer"
    );
    for key in [
        "childSessionId",
        "role",
        "objective",
        "entryId",
        "commandId",
        "recordedAt",
        "displayText",
        "callId",
        "commandText",
        "failureReason",
    ] {
        optional_string(&item, key)?;
    }
    if let Some(turn_id) = item.get("turnId") {
        ensure!(
            turn_id.is_null() || turn_id.is_string(),
            "Muse turnId must be a string or null"
        );
    }
    for (key, default) in [
        ("text", ""),
        ("message", ""),
        ("tool", ""),
        ("args", "{}"),
        ("visibleOutput", ""),
        ("fallbackText", ""),
    ] {
        if item.get(key).is_none() {
            item[key] = json!(default);
        }
        string(&item, key)?;
    }
    if item.get("summary").is_none() {
        item["summary"] = json!([]);
    }
    ensure!(
        array(&item, "summary")?.iter().all(Value::is_string),
        "Muse summary must contain strings"
    );
    for key in ["exitCode", "tokensBefore"] {
        if let Some(value) = item.get(key) {
            ensure!(value.is_number(), "Muse {key} must be a number");
        }
    }
    if let Some(patch) = item.get("patchRef") {
        string(patch, "id")?;
    }
    Ok(item)
}

fn eligible_child(item: &Value) -> Option<&str> {
    let kind = item["kind"].as_str()?;
    let tool = item["tool"].as_str()?;
    if matches!(kind, "subagent" | "workflow")
        || tool == "workflow"
        || tool.starts_with("subagent_")
    {
        item.get("childSessionId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
    } else {
        None
    }
}

fn snapshot_text<'a>(previous: &'a Value, item: &Value) -> Option<Cow<'a, str>> {
    if previous["kind"] != item["kind"] {
        return None;
    }
    match item["kind"].as_str()? {
        "agentMessage" => Some(previous["text"].as_str()?.into()),
        "userMessage" => Some(
            previous
                .get("displayText")
                .unwrap_or(&previous["text"])
                .as_str()?
                .into(),
        ),
        "reasoning" => Some(Cow::Owned(summary_text(previous))),
        _ => None,
    }
}

fn summary_text(item: &Value) -> String {
    let summary = item["summary"].as_array().expect("normalized summary");
    let length = summary
        .iter()
        .map(|line| line.as_str().unwrap_or("").len())
        .sum::<usize>()
        + summary.len().saturating_sub(1);
    let mut text = String::with_capacity(length);
    for (index, line) in summary.iter().enumerate() {
        if index > 0 {
            text.push('\n');
        }
        text.push_str(line.as_str().unwrap_or(""));
    }
    text
}

fn timeline_item(
    item: &Value,
    previous_text: Option<&str>,
    client_id: Option<&str>,
    patch: Option<&str>,
) -> Result<Option<Value>> {
    let id = string(item, "itemId")?;
    let kind = string(item, "kind")?;
    let result = match kind {
        "userMessage" => {
            let text = item
                .get("displayText")
                .unwrap_or(&item["text"])
                .as_str()
                .expect("normalized text");
            if previous_text == Some(text) {
                return Ok(None);
            }
            let mut result = json!({"type":"user_message","messageId":id,"text":text});
            if let Some(client_id) = client_id {
                result["clientMessageId"] = json!(client_id);
            }
            result
        }
        "agentMessage" | "reasoning" => {
            let text: Cow<'_, str> = if kind == "reasoning" {
                Cow::Owned(summary_text(item))
            } else {
                Cow::Borrowed(string(item, "text")?)
            };
            let delta = text
                .strip_prefix(previous_text.unwrap_or(""))
                .unwrap_or(&text);
            if delta.is_empty() {
                return Ok(None);
            }
            if kind == "reasoning" {
                json!({"type":"reasoning","text":delta})
            } else {
                json!({"type":"assistant_message","messageId":id,"text":delta})
            }
        }
        "compaction" => {
            let mut result = json!({"type":"compaction","status":if item["status"] == "inProgress" {"loading"} else {"completed"}});
            copy(item, "tokensBefore", &mut result, "preTokens");
            result
        }
        "toolCall" | "workflow" | "subagent" => {
            let detail = if kind == "toolCall" {
                tool_detail(item, patch)
            } else {
                subagent_detail(item, None)
            };
            let (status, error) = match string(item, "status")? {
                "inProgress" => ("running", Value::Null),
                "cancelled" => ("canceled", Value::Null),
                "completed" => ("completed", Value::Null),
                _ => (
                    "failed",
                    item.get("failureReason").unwrap_or(&item["status"]).clone(),
                ),
            };
            json!({"type":"tool_call","callId":item.get("callId").unwrap_or(&item["itemId"]),
                "name":if kind == "toolCall" { string(item,"tool")? } else {kind},
                "status":status,"error":error,"detail":detail})
        }
        _ => json!({"type":"tool_call","callId":id,"name":kind,"status":"completed","error":null,
            "detail":{"type":"plain_text","label":kind,"text":item["fallbackText"]}}),
    };
    Ok(Some(result))
}

fn tool_detail(item: &Value, patch: Option<&str>) -> Value {
    let unknown = || json!({"type":"unknown","input":item["args"],"output":item["visibleOutput"]});
    let Ok(args) = serde_json::from_str::<Value>(item["args"].as_str().expect("normalized args"))
    else {
        return unknown();
    };
    if !args.is_object()
        || [
            "objective",
            "role",
            "command",
            "path",
            "find",
            "replace",
            "content",
            "query",
            "pattern",
            "url",
        ]
        .iter()
        .any(|key| args.get(*key).is_some_and(|value| !value.is_string()))
    {
        return unknown();
    }
    let arg = |key| args.get(key).and_then(Value::as_str).unwrap_or("");
    match item["tool"].as_str().expect("normalized tool") {
        "workflow" | "subagent_spawn" | "subagent_wait" | "subagent_read_result" => {
            subagent_detail(item, Some(&args))
        }
        "shell" | "bash" => {
            let mut detail = json!({"type":"shell","command":item.get("commandText").and_then(Value::as_str).unwrap_or_else(||arg("command")),"output":item["visibleOutput"]});
            copy(item, "exitCode", &mut detail, "exitCode");
            detail
        }
        "read_file" => {
            json!({"type":"read","filePath":arg("path"),"content":item["visibleOutput"]})
        }
        "edit_file" => {
            let mut detail = json!({"type":"edit","filePath":arg("path")});
            copy(&args, "find", &mut detail, "oldString");
            copy(&args, "replace", &mut detail, "newString");
            if let Some(patch) = patch {
                detail["unifiedDiff"] = json!(patch);
            }
            detail
        }
        "write_file" => {
            let mut detail = json!({"type":"write","filePath":arg("path")});
            copy(&args, "content", &mut detail, "content");
            detail
        }
        "grep" | "glob" | "ls" | "web_search" => json!({"type":"search",
            "query":([arg("query"),arg("pattern"),arg("path")].into_iter().find(|value| !value.is_empty()).unwrap_or("")),"content":item["visibleOutput"]}),
        "web_fetch" => json!({"type":"fetch","url":arg("url"),"result":item["visibleOutput"]}),
        _ => unknown(),
    }
}

fn subagent_detail(item: &Value, args: Option<&Value>) -> Value {
    let mut detail = json!({"type":"sub_agent","log":(["message","visibleOutput","fallbackText"].iter()
        .filter_map(|key| item.get(*key).and_then(Value::as_str)).find(|value| !value.is_empty()).unwrap_or(""))});
    if let Some(description) = item
        .get("objective")
        .or_else(|| item.get("entryId"))
        .or_else(|| args.and_then(|args| args.get("objective")))
    {
        detail["description"] = description.clone();
    }
    if let Some(role) = item
        .get("role")
        .or_else(|| args.and_then(|args| args.get("role")))
    {
        detail["subAgentType"] = role.clone();
    }
    copy(item, "childSessionId", &mut detail, "childSessionId");
    detail
}

pub fn unified_diff(content: &str) -> Result<String> {
    use std::fmt::Write;
    let patch: Value = serde_json::from_str(content)?;
    let mut output = String::new();
    for file in array(&patch, "files")? {
        let path = string(file, "path")?;
        writeln!(&mut output, "--- a/{path}\n+++ b/{path}")?;
        for hunk in array(file, "hunks")? {
            for key in ["oldStart", "oldLines", "newStart", "newLines"] {
                ensure!(
                    hunk.get(key).is_some_and(Value::is_number),
                    "Muse patch {key} must be numeric"
                );
            }
            writeln!(
                &mut output,
                "@@ -{},{} +{},{} @@",
                hunk["oldStart"], hunk["oldLines"], hunk["newStart"], hunk["newLines"]
            )?;
            let lines = array(hunk, "lines")?;
            for (index, line) in lines.iter().enumerate() {
                if index > 0 {
                    output.push('\n');
                }
                output.push_str(
                    line.as_str()
                        .ok_or_else(|| anyhow!("Muse patch lines must be strings"))?,
                );
            }
            output.push('\n');
        }
    }
    Ok(output)
}

pub fn todo(input: &Value) -> Result<Value> {
    string(input, "viewCursor")?;
    let items = array(input, "items")?
        .iter()
        .map(|item| {
            let text = string(item, "text")?;
            let status = match string(item, "status")? {
                "inProgress" => "in_progress",
                "completed" => "completed",
                _ => "pending",
            };
            optional_string(item, "activeForm")?;
            let mut projected =
                json!({"text":text,"status":status,"completed":status == "completed"});
            copy(item, "activeForm", &mut projected, "activeForm");
            Ok(projected)
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(json!({"type":"todo","items":items}))
}

pub struct Questions {
    host: Arc<Connection>,
    native_id: String,
    provider: String,
    turn_id: Option<String>,
    pending: Vec<Value>,
    settled: HashSet<String>,
    responses: HashMap<String, Value>,
}

impl Questions {
    pub fn new(host: Arc<Connection>, native_id: String) -> Self {
        Self {
            host,
            native_id,
            provider: "muse".into(),
            turn_id: None,
            pending: Vec::new(),
            settled: HashSet::new(),
            responses: HashMap::new(),
        }
    }
    pub fn set_provider(&mut self, provider: &str) {
        self.provider = provider.into();
    }
    pub fn set_turn_id(&mut self, turn_id: Option<String>) {
        self.turn_id = turn_id;
    }

    pub fn requested(&mut self, input: &Value) -> Result<Vec<Value>> {
        validate_question(input)?;
        let id = string(input, "userInputId")?;
        if self.settled.contains(id) {
            return Ok(Vec::new());
        }
        if let Some(pending) = self
            .pending
            .iter_mut()
            .find(|pending| pending["userInputId"] == id)
        {
            *pending = input.clone();
        } else {
            self.pending.push(input.clone());
        }
        let request = permission_request(input, &self.provider);
        Ok(vec![self.event(
            json!({"type":"permission_requested","provider":self.provider,"request":request}),
        )])
    }

    pub fn resolved(&mut self, input: &Value) -> Result<Vec<Value>> {
        let id = string(input, "userInputId")?;
        self.settled.insert(id.into());
        self.pending.retain(|pending| pending["userInputId"] != id);
        let response = self
            .responses
            .remove(id)
            .unwrap_or_else(|| json!({"behavior":"deny","message":"Resolved by provider"}));
        Ok(vec![self.event(json!({"type":"permission_resolved","provider":self.provider,"requestId":id,"resolution":response}))])
    }

    pub fn pending(&self) -> Vec<Value> {
        self.pending
            .iter()
            .map(|request| permission_request(request, &self.provider))
            .collect()
    }

    pub async fn answer(&mut self, id: &str, response: &Value) -> Result<bool> {
        let Some(request) = self
            .pending
            .iter()
            .find(|pending| pending["userInputId"] == id)
        else {
            return Ok(false);
        };
        let (method, params) = if response["behavior"] == "deny" {
            optional_string(response, "message")?;
            let mut params = json!({"sessionId":self.native_id,"userInputId":id});
            copy(response, "message", &mut params, "reason");
            ("userInput/cancel", params)
        } else {
            let answers = question_answers(
                request,
                response
                    .get("updatedInput")
                    .ok_or_else(|| anyhow!("Muse question answer requires updatedInput"))?,
            )?;
            (
                "userInput/answer",
                json!({"sessionId":self.native_id,"userInputId":id,"answers":answers}),
            )
        };
        let previous = self.responses.insert(id.into(), response.clone());
        let result = self.host.command(method, params).await.and_then(|ack| {
            ensure!(
                ack.is_object(),
                "Muse question acknowledgement must be an object"
            );
            Ok(ack)
        });
        if let Err(error) = result {
            if let Some(previous) = previous {
                self.responses.insert(id.into(), previous);
            } else {
                self.responses.remove(id);
            }
            return Err(error);
        }
        Ok(true)
    }

    fn event(&self, mut event: Value) -> Value {
        if let Some(turn_id) = &self.turn_id {
            event["turnId"] = json!(turn_id);
        }
        event
    }
}

fn validate_question(input: &Value) -> Result<()> {
    string(input, "userInputId")?;
    string(input, "toolName")?;
    for question in array(input, "questions")? {
        for key in ["id", "header", "question"] {
            string(question, key)?;
        }
        for option in array(question, "options")? {
            string(option, "label")?;
            optional_string(option, "description")?;
        }
        let selection = object(question, "selection")?;
        ensure!(
            matches!(string(selection, "mode")?, "single" | "multiple"),
            "Muse question selection mode must be single or multiple"
        );
        for key in ["minSelections", "maxSelections"] {
            if let Some(value) = selection.get(key) {
                ensure!(value.is_number(), "Muse question {key} must be numeric");
            }
        }
    }
    Ok(())
}

fn permission_request(input: &Value, provider: &str) -> Value {
    let questions = input["questions"].as_array().expect("validated questions").iter().map(|question| {
        let options = question["options"].as_array().expect("validated options").iter().map(|option| {
            json!({"label":option["label"],"description":option.get("description").and_then(Value::as_str).unwrap_or("")})
        }).collect::<Vec<_>>();
        json!({"id":question["id"],"header":question["header"],"question":question["question"],"options":options,
            "multiSelect":question["selection"]["mode"] == "multiple","allowOther":true,
            "allowEmpty":question["selection"]["minSelections"].as_f64() == Some(0.0)})
    }).collect::<Vec<_>>();
    json!({"id":input["userInputId"],"provider":provider,"name":input["toolName"],"kind":"question","input":{"questions":questions}})
}

fn question_answers(request: &Value, input: &Value) -> Result<Vec<Value>> {
    let supplied = object(input, "answers")?
        .as_object()
        .expect("object checked");
    ensure!(
        supplied.values().all(Value::is_string),
        "Muse question answers must be strings"
    );
    array(request, "questions")?
        .iter()
        .map(|question| {
            let header = string(question, "header")?;
            let text = supplied
                .get(header)
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("missingAnswer: Answer {header}"))?;
            let options = array(question, "options")?;
            let known = |label: &str| options.iter().any(|option| option["label"] == label);
            if question["selection"]["mode"] == "single" {
                Ok(if known(text) {
                    json!({"questionId":question["id"],"selectedLabel":text})
                } else {
                    json!({"questionId":question["id"],"freeText":text})
                })
            } else {
                let labels: Vec<&str> = text
                    .split(',')
                    .map(str::trim)
                    .filter(|label| !label.is_empty())
                    .collect();
                Ok(if labels.iter().all(|label| known(label)) {
                    json!({"questionId":question["id"],"selectedLabels":labels})
                } else {
                    json!({"questionId":question["id"],"freeText":text})
                })
            }
        })
        .collect()
}

fn copy(source: &Value, key: &str, target: &mut Value, target_key: &str) {
    if let Some(value) = source.get(key) {
        target[target_key] = value.clone();
    }
}
fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Muse {key} must be a string"))
}
fn optional_string<'a>(value: &'a Value, key: &str) -> Result<Option<&'a str>> {
    value
        .get(key)
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| anyhow!("Muse {key} must be a string"))
        })
        .transpose()
}
fn object<'a>(value: &'a Value, key: &str) -> Result<&'a Value> {
    value
        .get(key)
        .filter(|value| value.is_object())
        .ok_or_else(|| anyhow!("Muse {key} must be an object"))
}
fn array<'a>(value: &'a Value, key: &str) -> Result<&'a Vec<Value>> {
    value
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("Muse {key} must be an array"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(kind: &str, status: &str) -> Value {
        normalize_item(json!({"itemId":"i","kind":kind,"revision":1,"status":status})).unwrap()
    }

    #[test]
    fn native_message_projection_emits_suffixes_and_replacement_text() {
        let mut message = item("agentMessage", "inProgress");
        message["text"] = json!("Hello");
        assert_eq!(
            timeline_item(&message, Some("Hel"), None, None).unwrap(),
            Some(json!({"type":"assistant_message","messageId":"i","text":"lo"}))
        );
        assert!(
            timeline_item(&message, Some("Hello"), None, None)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            timeline_item(&message, Some("Old"), None, None)
                .unwrap()
                .unwrap()["text"],
            "Hello"
        );
        let mut user = item("userMessage", "completed");
        user["text"] = json!("native instructions and prompt");
        user["displayText"] = json!("prompt");
        assert_eq!(
            timeline_item(&user, None, Some("client-1"), None).unwrap(),
            Some(
                json!({"type":"user_message","messageId":"i","text":"prompt","clientMessageId":"client-1"})
            )
        );
        assert!(
            timeline_item(&user, Some("prompt"), None, None)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn deltas_default_to_text_and_stop_after_completion() {
        let mut timeline = Timeline::default();
        timeline
            .items
            .insert("i".into(), item("agentMessage", "inProgress"));
        assert_eq!(
            timeline
                .delta(&json!({"itemId":"i","delta":"Hello"}))
                .unwrap()
                .unwrap()["text"],
            "Hello"
        );
        assert_eq!(
            timeline
                .delta(&json!({"itemId":"i","delta":" world"}))
                .unwrap()
                .unwrap()["text"],
            " world"
        );
        assert_eq!(timeline.items["i"]["text"], "Hello world");
        timeline.items.get_mut("i").unwrap()["status"] = json!("completed");
        assert!(
            timeline
                .delta(&json!({"itemId":"i","delta":" ignored"}))
                .unwrap()
                .is_none()
        );
        assert!(
            timeline
                .delta(&json!({"itemId":"absent","delta":" ignored"}))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn reasoning_sparse_summary_deltas_preserve_line_boundaries() {
        let mut timeline = Timeline::default();
        timeline
            .items
            .insert("i".into(), item("reasoning", "inProgress"));
        assert_eq!(
            timeline
                .delta(&json!({"itemId":"i","field":"summary.2","delta":"Third"}))
                .unwrap()
                .unwrap()["text"],
            "\n\nThird"
        );
        assert_eq!(
            timeline
                .delta(&json!({"itemId":"i","field":"summary.2","delta":" line"}))
                .unwrap()
                .unwrap()["text"],
            " line"
        );
        assert!(
            timeline
                .delta(&json!({"itemId":"i","field":"unsupported","delta":"x"}))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn structured_tools_keep_native_identity_and_patch() {
        let mut tool = item("toolCall", "completed");
        tool["tool"] = json!("edit_file");
        tool["args"] = json!("{\"path\":\"file.rs\",\"find\":\"old\",\"replace\":\"new\"}");
        let diff = unified_diff(r#"{"files":[{"path":"file.rs","hunks":[{"oldStart":1,"oldLines":1,"newStart":1,"newLines":1,"lines":["-old","+new"]}]}]}"#).unwrap();
        assert_eq!(
            diff,
            "--- a/file.rs\n+++ b/file.rs\n@@ -1,1 +1,1 @@\n-old\n+new\n"
        );
        assert_eq!(
            timeline_item(&tool, None, None, Some(&diff)).unwrap(),
            Some(json!({
                "type":"tool_call","callId":"i","name":"edit_file","status":"completed","error":null,
                "detail":{"type":"edit","filePath":"file.rs","oldString":"old","newString":"new","unifiedDiff":diff}
            }))
        );
        tool["args"] = json!("{\"path\":42}");
        assert_eq!(tool_detail(&tool, None)["type"], "unknown");
        tool["tool"] = json!("subagent_read_result");
        tool["args"] = json!("{}");
        tool["visibleOutput"] = json!("child result");
        assert_eq!(
            tool_detail(&tool, None),
            json!({"type":"sub_agent","log":"child result"})
        );
        assert!(eligible_child(&tool).is_none());
        tool["childSessionId"] = json!("real-child");
        assert_eq!(eligible_child(&tool), Some("real-child"));
    }

    #[test]
    fn todo_and_question_answers_preserve_selection_behavior() {
        assert_eq!(
            todo(&json!({"viewCursor":"c","items":[
                {"text":"First","status":"inProgress","activeForm":"Doing first"},
                {"text":"Second","status":"completed"},{"text":"Third","status":"unknown"}
            ]}))
            .unwrap(),
            json!({"type":"todo","items":[
                {"text":"First","status":"in_progress","completed":false,"activeForm":"Doing first"},
                {"text":"Second","status":"completed","completed":true},
                {"text":"Third","status":"pending","completed":false}
            ]})
        );
        let request = json!({"userInputId":"q","toolName":"ask","questions":[
            {"id":"color","header":"Color","question":"Which?","options":[{"label":"Blue"},{"label":"Green"}],
                "selection":{"mode":"single"}},
            {"id":"many","header":"Many","question":"Which ones?","options":[{"label":"One"},{"label":"Two"}],
                "selection":{"mode":"multiple","minSelections":0}}
        ]});
        validate_question(&request).unwrap();
        let native = permission_request(&request, "custom-muse");
        assert_eq!(native["provider"], "custom-muse");
        assert_eq!(
            native["input"]["questions"][0]["options"][0]["description"],
            ""
        );
        assert_eq!(native["input"]["questions"][1]["allowEmpty"], true);
        assert_eq!(
            question_answers(
                &request,
                &json!({"answers":{"Color":"Blue","Many":" One, Two, "}})
            )
            .unwrap(),
            vec![
                json!({"questionId":"color","selectedLabel":"Blue"}),
                json!({"questionId":"many","selectedLabels":["One","Two"]})
            ]
        );
        assert_eq!(
            question_answers(
                &request,
                &json!({"answers":{"Color":"Other","Many":"One, Other"}})
            )
            .unwrap(),
            vec![
                json!({"questionId":"color","freeText":"Other"}),
                json!({"questionId":"many","freeText":"One, Other"})
            ]
        );
        assert!(question_answers(&request, &json!({"answers":{"Color":"Blue"}})).is_err());
    }
}
