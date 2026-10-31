use super::tools::{map_tool, text};
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

pub fn s(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}
pub fn event(kind: &str) -> Value {
    json!({"type":kind,"provider":"opencode"})
}
pub fn timeline(item: Value) -> Value {
    json!({"type":"timeline","provider":"opencode","item":item})
}
pub fn subagent(item: Value) -> Value {
    json!({"type":"provider_subagent","provider":"opencode","event":item})
}
pub fn session_id(e: &Value) -> &str {
    let p = &e["properties"];
    [
        p.get("sessionID"),
        p["info"].get("sessionID"),
        p["part"].get("sessionID"),
        p["info"].get("id"),
    ]
    .into_iter()
    .flatten()
    .find_map(Value::as_str)
    .unwrap_or("")
}
pub fn diagnostic(v: &Value) -> String {
    v.as_str()
        .or_else(|| v["message"].as_str())
        .or_else(|| v["data"]["message"].as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| v.to_string())
}
pub fn model(info: &Value) -> Option<String> {
    let m = if info["role"] == "user" {
        &info["model"]
    } else {
        info
    };
    let provider = text(&m["providerID"])?;
    let id = text(&m["modelID"])?;
    Some(format!("{provider}/{id}"))
}
pub fn summary(info: &Value) -> bool {
    info["role"] == "assistant"
        && (info["summary"] == true
            || info["agent"] == "compaction"
            || info["mode"] == "compaction")
}
pub fn structured(info: &Value) -> Option<String> {
    info.get("structured").and_then(|v| {
        if let Some(t) = v.as_str() {
            (!t.trim().is_empty()).then(|| t.trim().to_owned())
        } else {
            Some(v.to_string())
        }
    })
}
pub fn todos(value: &Value) -> Value {
    let items: Vec<_> = value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| {
            let content = text(&t["content"])?.trim();
            if content.is_empty() {
                return None;
            }
            let status = match s(&t["status"]) {
                "completed" => "completed",
                "in_progress" | "inProgress" => "in_progress",
                _ => "pending",
            };
            Some(json!({"text":content,"status":status,"completed":status == "completed"}))
        })
        .collect();
    json!({"type":"todo","items":items})
}
fn tool(part: &Value) -> Option<Value> {
    let st = &part["state"];
    map_tool(
        s(&part["tool"]),
        text(&part["callID"])
            .filter(|s| !s.trim().is_empty())
            .or_else(|| text(&part["id"]))
            .unwrap_or(""),
        &st["input"],
        s(&st["status"]),
        &st["output"],
        &st["error"],
        &st["metadata"],
    )
}
fn todo_data(v: &Value) -> Option<Value> {
    if v.is_array() {
        return Some(v.clone());
    }
    if let Some(v) = v.get("todos") {
        if v.is_array() {
            return Some(v.clone());
        }
    }
    v.as_str()
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .and_then(|v| todo_data(&v))
}
fn replay(item: Value, info: &Value, part: &Value) -> Value {
    let mut e = timeline(item);
    if let Some(ms) = [
        part["time"].get("start"),
        part["time"].get("end"),
        info["time"].get("created"),
        info["time"].get("completed"),
    ]
    .into_iter()
    .flatten()
    .find_map(Value::as_i64)
    {
        if let Some(time) = DateTime::<Utc>::from_timestamp_millis(ms) {
            e["timestamp"] = json!(time.to_rfc3339_opts(SecondsFormat::Millis, true));
        }
    }
    e
}
pub fn history_message(message: &Value) -> Vec<Value> {
    let info = &message["info"];
    let parts = message["parts"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if summary(info) {
        return vec![];
    }
    if info["role"] == "user" {
        let content: String = parts
            .iter()
            .filter(|p| p["type"] == "text" && p["synthetic"] != true)
            .map(|p| s(&p["text"]))
            .collect();
        return if content.is_empty() {
            vec![]
        } else {
            vec![replay(
                json!({"type":"user_message","text":content,"messageId":info["id"]}),
                info,
                &Value::Null,
            )]
        };
    }
    let mut out = Vec::new();
    let mut has_text = false;
    for part in parts {
        let item = match s(&part["type"]) {
            "text" if !s(&part["text"]).is_empty() => {
                has_text = true;
                Some(json!({"type":"assistant_message","text":part["text"],"messageId":info["id"]}))
            }
            "reasoning" if !s(&part["text"]).is_empty() => {
                Some(json!({"type":"reasoning","text":part["text"]}))
            }
            "tool" if s(&part["tool"]).trim().eq_ignore_ascii_case("todowrite") => {
                todo_data(&part["state"]["input"])
                    .or_else(|| todo_data(&part["state"]["output"]))
                    .map(|v| todos(&v))
            }
            "tool" => tool(part),
            _ => None,
        };
        if let Some(item) = item {
            out.push(replay(item, info, part));
        }
    }
    if !has_text {
        if let Some(text) = structured(info) {
            out.push(replay(
                json!({"type":"assistant_message","text":text,"messageId":info["id"]}),
                info,
                &Value::Null,
            ));
        }
    }
    out
}
pub fn filter_reverted(messages: &mut Vec<Value>, info: &Value) {
    let revert = &info["revert"];
    if !s(&revert["messageID"]).is_empty() && revert["partID"].is_null() {
        if let Some(index) = messages
            .iter()
            .position(|m| m["info"]["id"] == revert["messageID"])
        {
            messages.truncate(index);
        }
    }
}

#[derive(Default)]
pub struct Track {
    pub roles: HashMap<String, String>,
    pub pending_text: Option<String>,
    pub client_id: Option<String>,
    pub steers: HashMap<String, (String, Option<String>)>,
    users: HashSet<String>,
    parts: HashMap<String, (String, String, bool)>,
    part_types: HashMap<String, String>,
    summaries: HashSet<String>,
    structured: HashSet<String>,
    compactions: HashSet<String>,
    pub suppress: bool,
    pub usage: Value,
    cost: f64,
    steps: HashMap<String, Value>,
    pub tasks: HashMap<String, Value>,
    fingerprints: HashMap<String, Value>,
}
impl Track {
    pub fn reset_turn(&mut self) {
        self.pending_text = None;
        self.client_id = None;
        self.steers.clear();
        self.parts.clear();
        self.part_types.clear();
        self.summaries.clear();
        self.compactions.clear();
        self.suppress = false;
        self.tasks.clear();
        self.steps.clear();
        let window = self.usage.get("contextWindowMaxTokens").cloned();
        self.usage = json!({});
        if let Some(window) = window {
            self.usage["contextWindowMaxTokens"] = window;
        }
    }
    pub fn hydrate(&mut self, messages: &[Value]) {
        for m in messages {
            let info = &m["info"];
            self.roles
                .insert(s(&info["id"]).into(), s(&info["role"]).into());
            if info["role"] == "user" {
                self.users.insert(s(&info["id"]).into());
            }
        }
        if let Some(m) = messages.last() {
            self.fingerprints
                .insert(s(&m["info"]["id"]).into(), m["info"].clone());
            for p in m["parts"].as_array().into_iter().flatten() {
                self.fingerprints.insert(s(&p["id"]).into(), p.clone());
            }
        }
    }
    pub fn hydrate_completed(&mut self, messages: &[Value]) {
        let mut last = None;
        for m in messages {
            let info = &m["info"];
            if info["role"] == "assistant" && info["time"]["completed"].is_null() {
                continue;
            }
            self.roles
                .insert(s(&info["id"]).into(), s(&info["role"]).into());
            if info["role"] == "user" {
                self.users.insert(s(&info["id"]).into());
            }
            last = Some(m);
        }
        if let Some(m) = last {
            self.fingerprints
                .insert(s(&m["info"]["id"]).into(), m["info"].clone());
            for p in m["parts"].as_array().into_iter().flatten() {
                self.fingerprints.insert(s(&p["id"]).into(), p.clone());
            }
        }
    }
    fn emit_user(&mut self, id: &str, text: String, client: Option<String>, out: &mut Vec<Value>) {
        if text.trim().is_empty() || !self.users.insert(id.into()) {
            return;
        }
        let mut item = json!({"type":"user_message","text":text,"messageId":id});
        if let Some(client) = client {
            item["clientMessageId"] = json!(client);
        }
        out.push(timeline(item));
    }
    fn usage_step(&mut self, p: &Value, out: &mut Vec<Value>) {
        if self.steps.get(s(&p["id"])) == Some(p) {
            return;
        }
        if let Some(old) = self.steps.insert(s(&p["id"]).into(), p.clone()) {
            self.cost -= old["cost"].as_f64().filter(|n| *n > 0.0).unwrap_or(0.0);
        }
        self.cost += p["cost"].as_f64().filter(|n| *n > 0.0).unwrap_or(0.0);
        if !self.usage.is_object() {
            self.usage = json!({});
        }
        for (source, dest) in [("input", "inputTokens"), ("output", "outputTokens")] {
            if p["tokens"][source].as_f64().is_some_and(|n| n > 0.0) {
                self.usage[dest] = p["tokens"][source].clone();
            }
        }
        if p["tokens"]["cache"]["read"]
            .as_f64()
            .is_some_and(|n| n > 0.0)
        {
            self.usage["cachedInputTokens"] = p["tokens"]["cache"]["read"].clone();
        }
        let total = token_sum(&p["tokens"]);
        if total > 0.0 {
            self.usage["contextWindowUsedTokens"] = json!(total);
        }
        if self.cost > 0.0 {
            self.usage["totalCostUsd"] = json!(self.cost);
        }
        if self.usage.as_object().is_some_and(|u| !u.is_empty()) {
            out.push(json!({"type":"usage_updated","provider":"opencode","usage":self.usage}));
        }
    }
    fn translate(&mut self, e: &Value, windows: &HashMap<String, f64>) -> Vec<Value> {
        let p = &e["properties"];
        let mut out = Vec::new();
        match s(&e["type"]) {
            "message.updated" => {
                let info = &p["info"];
                let id = s(&info["id"]);
                self.roles.insert(id.into(), s(&info["role"]).into());
                if self.fingerprints.get(id) == Some(info) {
                    return out;
                }
                self.fingerprints.remove(id);
                if info["role"] == "user" {
                    let pending = self.steers.remove(id).or_else(|| {
                        self.pending_text
                            .clone()
                            .map(|t| (t, self.client_id.clone()))
                    });
                    if let Some((text, client)) = pending {
                        self.emit_user(id, text, client, &mut out);
                    }
                } else if info["role"] == "assistant" {
                    if self.suppress || summary(info) {
                        self.summaries.insert(id.into());
                        return out;
                    }
                    if let Some(window) = model(info).and_then(|m| windows.get(&m)) {
                        if !self.usage.is_object() {
                            self.usage = json!({});
                        }
                        self.usage["contextWindowMaxTokens"] = json!(window);
                    }
                    if !info["time"]["completed"].is_null() && !self.structured.contains(id) {
                        if let Some(text) = structured(info) {
                            self.structured.insert(id.into());
                            out.push(timeline(
                                json!({"type":"assistant_message","text":text,"messageId":id}),
                            ));
                        }
                    }
                }
            }
            "message.part.updated" => {
                let part = &p["part"];
                let id = s(&part["id"]);
                let mid = s(&part["messageID"]);
                let kind = s(&part["type"]);
                if self.fingerprints.get(id) == Some(part) {
                    return out;
                }
                self.fingerprints.remove(id);
                self.part_types.insert(id.into(), kind.into());
                if self.summaries.contains(mid) {
                    return out;
                }
                let user = self.roles.get(mid).is_some_and(|r| r == "user");
                if self.suppress && kind == "text" && !user {
                    self.summaries.insert(mid.into());
                    return out;
                }
                match kind {
                    "text" if user => {if part["synthetic"]!=true{self.emit_user(mid,s(&part["text"]).into(),None,&mut out);}},
                    "text"|"reasoning" => {
                        if part["time"]["end"].is_null() || part["time"]["end"]==0 {return out;}
                        let full=s(&part["text"]); let previous=self.parts.get(id);
                        if previous.is_some_and(|p|p.2){return out;}
                        let emitted=previous.filter(|p|p.0==mid).map(|p|p.1.as_str()).unwrap_or("");
                        if !full.starts_with(emitted){tracing::warn!(part_id=id,message_id=mid,"OpenCode final part differs from streamed content");return out;}
                        let suffix=full[emitted.len()..].to_owned();self.parts.insert(id.into(),(mid.into(),full.into(),true));
                        if !suffix.is_empty(){out.push(timeline(if kind=="reasoning"{json!({"type":"reasoning","text":suffix})}else{json!({"type":"assistant_message","text":suffix,"messageId":mid})}));}
                    }
                    "tool" if !s(&part["tool"]).trim().eq_ignore_ascii_case("todowrite") => {if let Some(mut item)=tool(part){if item["detail"]["type"]=="sub_agent"{let id=s(&item["callId"]).to_owned();if item["detail"]["childSessionId"].is_null(){if let Some(old)=self.tasks.get(&id){if !old["detail"]["childSessionId"].is_null(){item["detail"]["childSessionId"]=old["detail"]["childSessionId"].clone();}}}self.tasks.insert(id,item.clone());}out.push(timeline(item));}},
                    "compaction" if self.compactions.insert(id.into()) => out.push(timeline(json!({"type":"compaction","status":"loading","trigger":if part["auto"]==true{"auto"}else{"manual"}}))),
                    "step-finish" => self.usage_step(part,&mut out), _=>{}
                }
            }
            "message.part.delta" => {
                let id = s(&p["partID"]);
                let mid = text(&p["messageID"]).unwrap_or(id);
                let delta = s(&p["delta"]);
                let field = s(&p["field"]);
                let reasoning = field == "reasoning"
                    || self.part_types.get(id).is_some_and(|s| s == "reasoning");
                if delta.is_empty()
                    || field.is_empty()
                    || self.summaries.contains(mid)
                    || (!reasoning
                        && (field != "text" || self.roles.get(mid).is_some_and(|r| r == "user")))
                {
                    return out;
                }
                if !reasoning && self.suppress {
                    self.summaries.insert(mid.into());
                    return out;
                }
                if !id.is_empty() {
                    let materialized = self
                        .parts
                        .entry(id.into())
                        .or_insert_with(|| (mid.into(), String::new(), false));
                    if materialized.2 {
                        return out;
                    }
                    materialized.1.push_str(delta);
                }
                out.push(timeline(if reasoning {
                    json!({"type":"reasoning","text":delta})
                } else {
                    json!({"type":"assistant_message","text":delta,"messageId":mid})
                }));
            }
            "todo.updated" => out.push(timeline(todos(&p["todos"]))),
            "session.compacted" => {
                out.push(timeline(json!({"type":"compaction","status":"completed"})))
            }
            "session.status" if p["status"]["type"] == "retry" => {
                let message = s(&p["status"]["message"]).trim();
                let mut text = format!("Provider retry (attempt {})", p["status"]["attempt"]);
                if !message.is_empty() {
                    text.push_str(": ");
                    text.push_str(message);
                }
                out.push(timeline(json!({"type":"error","message":text})));
            }
            "session.idle" | "session.status"
                if e["type"] == "session.idle" || p["status"]["type"] == "idle" =>
            {
                let mut end = event("turn_completed");
                if self.usage.as_object().is_some_and(|u| !u.is_empty()) {
                    end["usage"] = self.usage.clone();
                }
                self.suppress = false;
                self.summaries.clear();
                self.part_types.clear();
                self.compactions.clear();
                out.push(end);
            }
            "session.error" => {
                self.suppress = false;
                self.summaries.clear();
                self.part_types.clear();
                self.compactions.clear();
                out.push(if p["error"]["name"]=="MessageAbortedError"{json!({"type":"turn_canceled","provider":"opencode","reason":"interrupted"})}else{json!({"type":"turn_failed","provider":"opencode","error":diagnostic(&p["error"])})});
            }
            "session.created" | "session.updated" => {
                if let Some(cost) = p["info"]["cost"].as_f64().filter(|n| *n > 0.0) {
                    self.cost = self.cost.max(cost);
                    if !self.usage.is_object() {
                        self.usage = json!({});
                    }
                    self.usage["totalCostUsd"] = json!(self.cost);
                }
                out.push(json!({"type":"thread_started","provider":"opencode","sessionId":p["info"]["id"]}));
            }
            _ => {}
        }
        out
    }
}
fn token_sum(tokens: &Value) -> f64 {
    [
        &tokens["input"],
        &tokens["output"],
        &tokens["reasoning"],
        &tokens["cache"]["read"],
        &tokens["cache"]["write"],
    ]
    .into_iter()
    .filter_map(Value::as_f64)
    .filter(|n| *n > 0.0)
    .sum()
}

pub struct Child {
    pub info: Value,
    pub track: Track,
    presentation: Value,
    last_subtitle: String,
    title_emitted: bool,
    linked: String,
    pub status: Value,
}
pub struct Projection {
    pub id: String,
    pub cwd: String,
    pub root: Track,
    pub children: HashMap<String, Child>,
    pub windows: HashMap<String, f64>,
}
impl Projection {
    pub fn new(id: String, cwd: String) -> Self {
        Self {
            id,
            cwd,
            root: Track::default(),
            children: HashMap::new(),
            windows: HashMap::new(),
        }
    }
    pub fn owns(&self, id: &str) -> bool {
        id == self.id || self.children.contains_key(id)
    }
    pub fn directory(&self, id: &str) -> String {
        self.children
            .get(id)
            .and_then(|c| text(&c.info["directory"]))
            .unwrap_or(&self.cwd)
            .to_owned()
    }
    pub fn detect(&mut self, info: &Value, status: Option<&str>) -> Vec<Value> {
        let id = s(&info["id"]);
        let parent = text(&info["parentID"])
            .or_else(|| text(&info["parentId"]))
            .unwrap_or("");
        if id.is_empty() || id == self.id || !self.owns(parent) || self.children.contains_key(id) {
            return vec![];
        }
        let mut child = Child {
            info: info.clone(),
            track: Track::default(),
            presentation: json!({}),
            last_subtitle: String::new(),
            title_emitted: false,
            linked: String::new(),
            status: Value::Null,
        };
        let mut up = json!({"type":"upsert","id":id,"parentSubagentId":if parent==self.id{Value::Null}else{json!(parent)}});
        for (source, dest) in [("title", "description"), ("directory", "cwd")] {
            if let Some(t) = text(&info[source]) {
                up[dest] = json!(t);
            }
        }
        if let Some(status) = status {
            up["status"] = json!(status);
        }
        presentation(
            &mut child,
            &json!({"agent":info["agent"],"modelID":info["model"]["id"],"variant":info["model"]["variant"]}),
            &mut up,
        );
        self.children.insert(id.into(), child);
        let mut out = vec![subagent(up)];
        let track = if parent == self.id {
            Some(&mut self.root)
        } else {
            self.children.get_mut(parent).map(|c| &mut c.track)
        };
        if let Some(track) = track {
            let waiting: Vec<_> = track
                .tasks
                .iter()
                .filter(|(_, t)| {
                    t["status"] == "running" && t["detail"]["childSessionId"].is_null()
                })
                .map(|(id, _)| id.clone())
                .collect();
            if waiting.len() == 1 {
                if let Some(t) = track.tasks.get_mut(&waiting[0]) {
                    t["detail"]["childSessionId"] = json!(id);
                    let item = t.clone();
                    let ev = if parent == self.id {
                        timeline(item.clone())
                    } else {
                        subagent(json!({"type":"timeline","id":parent,"item":item}))
                    };
                    out.push(ev);
                }
            }
        }
        out.extend(self.links());
        out
    }
    fn links(&mut self) -> Vec<Value> {
        let linked: Vec<Value> = self
            .root
            .tasks
            .values()
            .chain(self.children.values().flat_map(|c| c.track.tasks.values()))
            .filter(|t| text(&t["detail"]["childSessionId"]).is_some())
            .cloned()
            .collect();
        let mut out = vec![];
        for item in linked {
            let id = s(&item["detail"]["childSessionId"]);
            if let Some(child) = self.children.get_mut(id) {
                let call = s(&item["callId"]);
                if child.linked == call {
                    continue;
                }
                child.linked = call.into();
                let mut up = json!({"type":"upsert","id":id,"toolCallId":call});
                for (key, dest) in [("subAgentType", "title"), ("description", "description")] {
                    if let Some(t) = text(&item["detail"][key]) {
                        up[dest] = json!(t);
                    }
                }
                if !up["title"].is_null() {
                    child.title_emitted = true;
                }
                presentation(
                    child,
                    &json!({"agent":item["detail"]["subAgentType"]}),
                    &mut up,
                );
                out.push(subagent(up));
            }
        }
        out
    }
    pub fn translate(&mut self, e: &Value) -> Vec<Value> {
        let id = session_id(e);
        let kind = s(&e["type"]);
        if matches!(kind, "session.created" | "session.updated") && id != self.id {
            return self.detect(&e["properties"]["info"], Some("running"));
        }
        if !self.owns(id) {
            return vec![];
        }
        if kind == "session.deleted" {
            self.children.remove(id);
            return vec![subagent(json!({"type":"remove","id":id}))];
        }
        if matches!(kind, "permission.asked" | "question.asked") {
            return permission(e).into_iter().collect();
        }
        if id == self.id {
            let mut out = self.root.translate(e, &self.windows);
            out.extend(self.links());
            return out;
        }
        let child = self.children.get_mut(id).unwrap();
        let translated = child.track.translate(e, &self.windows);
        let mut out = vec![];
        let mut running = false;
        if kind == "message.updated" && e["properties"]["info"]["role"] == "assistant" {
            let mut up = json!({"type":"upsert","id":id});
            presentation(child, &e["properties"]["info"], &mut up);
            if up.as_object().unwrap().len() > 2 {
                out.push(subagent(up));
            }
        }
        if kind == "session.status" && e["properties"]["status"]["type"] == "busy" {
            running = true;
            out.push(subagent(
                json!({"type":"upsert","id":id,"status":"running"}),
            ));
        }
        for ev in translated {
            match s(&ev["type"]){"timeline"=>{if !running{out.push(subagent(json!({"type":"upsert","id":id,"status":"running"})));running=true;}let mut nested=json!({"type":"timeline","id":id,"item":ev["item"]});if let Some(t)=ev.get("timestamp"){nested["timestamp"]=t.clone();}out.push(subagent(nested));},"turn_completed"|"turn_failed"|"turn_canceled"=>out.push(subagent(json!({"type":"upsert","id":id,"status":match s(&ev["type"]){"turn_completed"=>"completed","turn_failed"=>"failed",_=>"canceled"}}))),_=>{}}
        }
        out.extend(self.links());
        out
    }
    pub fn child_presentation(&mut self, id: &str, info: &Value) -> Option<Value> {
        let c = self.children.get_mut(id)?;
        let mut up = json!({"type":"upsert","id":id});
        presentation(c, info, &mut up);
        (up.as_object()?.len() > 2).then(|| subagent(up))
    }
}
fn presentation(child: &mut Child, info: &Value, up: &mut Value) {
    for (src, dst) in [
        ("agent", "agent"),
        ("modelID", "model"),
        ("variant", "variant"),
    ] {
        if let Some(v) = text(&info[src]).map(str::trim).filter(|s| !s.is_empty()) {
            child.presentation[dst] = json!(v);
        }
    }
    if !info["time"]["completed"].is_null() {
        let total = token_sum(&info["tokens"]);
        if total > 0.0 {
            child.presentation["tokens"] = json!(total);
        }
    }
    let agent = s(&child.presentation["agent"]);
    if !child.title_emitted && !agent.is_empty() {
        up["title"] = json!(agent);
        child.title_emitted = true;
    }
    let mut parts = Vec::new();
    for key in ["agent", "model"] {
        if let Some(v) = text(&child.presentation[key]) {
            parts.push(v.to_owned());
        }
    }
    if let Some(v) = text(&child.presentation["variant"]) {
        parts.push(if v == "xhigh" {
            "Extra High".into()
        } else {
            humanize(v)
        });
    }
    if let Some(n) = child.presentation["tokens"].as_f64() {
        parts.push(if n < 1000.0 {
            format!("{} tokens", n.round())
        } else {
            format!("{}k tokens", (n / 100.0).round() / 10.0)
        });
    }
    let subtitle = parts.join(" · ");
    if !subtitle.is_empty() && subtitle != child.last_subtitle {
        up["subtitle"] = json!(subtitle);
        child.last_subtitle = subtitle;
    }
}
fn humanize(s: &str) -> String {
    s.split(|c: char| c == '_' || c == '-' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .map(|p| {
            let mut c = p.chars();
            let first = c.next().unwrap().to_uppercase().collect::<String>();
            format!("{first}{}", c.as_str())
        })
        .collect::<Vec<_>>()
        .join(" ")
}
fn permission(e: &Value) -> Option<Value> {
    let p = &e["properties"];
    let id = text(&p["id"])?;
    let request = if e["type"] == "question.asked" {
        let questions:Vec<_>=p["questions"].as_array().into_iter().flatten().filter(|q|text(&q["question"]).is_some()&&text(&q["header"]).is_some()).map(|q|{let mut value=json!({"question":q["question"],"header":q["header"],"options":q.get("options").cloned().unwrap_or_else(||json!([])),"allowOther":true});if q["multiple"]==true{value["multiSelect"]=json!(true);}value}).collect();
        if questions.is_empty() {
            return None;
        }
        let mut metadata = json!({"source":"opencode_question"});
        if let Some(tool) = p["tool"].as_object() {
            metadata.as_object_mut().unwrap().extend(tool.clone());
        }
        json!({"id":id,"provider":"opencode","name":"question","kind":"question","title":"Question","input":{"questions":questions},"metadata":metadata})
    } else {
        let name = s(&p["permission"]);
        let metadata = &p["metadata"];
        let field = |keys: &[&str]| {
            keys.iter()
                .find_map(|k| text(&metadata[*k]).map(str::trim).filter(|s| !s.is_empty()))
                .or_else(|| {
                    keys.iter().find_map(|k| {
                        text(&metadata["input"][*k])
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                    })
                })
        };
        let command = field(&["command", "cmd", "shellCommand"]);
        let cwd = field(&["cwd", "directory", "path", "workdir"]);
        let reason = field(&["reason", "purpose", "description", "message"]);
        let patterns: Vec<_> = p["patterns"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let mut input = json!({});
        if !patterns.is_empty() {
            input["patterns"] = json!(patterns);
        }
        if metadata.is_object() {
            input["metadata"] = metadata.clone();
        }
        if p["tool"].is_object() {
            input["tool"] = p["tool"].clone();
        }
        if let Some(command) = command {
            input["command"] = json!(command);
        }
        let detail = if let Some(command) = command {
            let mut d = json!({"type":"shell","command":command});
            if let Some(cwd) = cwd {
                d["cwd"] = json!(cwd);
            }
            d
        } else {
            let mut i = input.clone();
            i["permission"] = json!(name);
            json!({"type":"unknown","input":i,"output":null})
        };
        let title = match name {
            "external_directory" => "Access external directory".into(),
            "bash" => "Run shell command".into(),
            "read" | "read_file" => "Read files".into(),
            "write" | "write_file" | "create_file" => "Write files".into(),
            "edit" | "apply_patch" | "apply_diff" => "Edit files".into(),
            _ => {
                let t = humanize(name);
                if t.is_empty() {
                    "Permission request".into()
                } else {
                    t
                }
            }
        };
        let mut request = json!({"id":id,"provider":"opencode","name":name,"kind":"tool","title":title,"input":input,"detail":detail,"actions":[{"id":"deny","label":"Deny","behavior":"deny","variant":"danger","intent":"dismiss"},{"id":"allow_always","label":"Allow always","behavior":"allow","variant":"secondary"},{"id":"allow_once","label":"Allow once","behavior":"allow","variant":"primary"}]});
        let mut description = reason.map(|s| vec![s.to_owned()]).unwrap_or_default();
        if !patterns.is_empty() {
            description.push(format!("Scope: {}", patterns.join(", ")));
        }
        if !description.is_empty() {
            request["description"] = json!(description.join(" - "));
        }
        request
    };
    Some(json!({"type":"permission_requested","provider":"opencode","request":request}))
}
