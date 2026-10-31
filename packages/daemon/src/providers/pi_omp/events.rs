//! Stateful projection of Pi/OMP RPC frames into the provider-neutral event wire format.
macro_rules! cached_regex {
    ($pattern:literal) => {{
        static REGEX: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
            regex::Regex::new($pattern).expect("static provider pattern")
        });
        &*REGEX
    }};
}
mod extensions;
mod notices;
mod permissions;
mod tools;

pub use notices::custom_item;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet, VecDeque};
pub use tools::tool_item;
use uuid::Uuid;

fn array(v: &Value) -> &[Value] {
    v.as_array().map(Vec::as_slice).unwrap_or(&[])
}
fn string(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}
fn first<'a>(values: &[&'a Value]) -> Option<&'a str> {
    values
        .iter()
        .find_map(|v| v.as_str().filter(|s| !s.trim().is_empty()))
}
fn put(v: &mut Value, key: &str, value: Option<&str>) {
    if let Some(value) = value {
        v[key] = json!(value);
    }
}

pub struct Mapper {
    family: String,
    provider: String,
    turn: Option<String>,
    started: bool,
    turn_started: bool,
    assistant: Option<String>,
    tools: HashMap<String, (String, Value)>,
    permissions: permissions::Permissions,
    extensions: extensions::Extensions,
    settled: Vec<Value>,
    terminal: Option<Value>,
    usage: Option<Value>,
    last_todo: Option<Value>,
    assistant_index: usize,
    custom_index: usize,
    users: HashSet<String>,
    clients: VecDeque<(String, Option<String>)>,
    child_mappers: HashMap<String, Box<Mapper>>,
    bridged: HashMap<String, Value>,
    no_turn_output: Vec<String>,
    stream_text: String,
    stream_thinking: String,
    suppress_next_start: bool,
    card_updates: HashMap<String, (std::time::Instant, bool)>,
}
impl Mapper {
    pub fn new(family: &str, provider: &str) -> Self {
        Self {
            family: family.into(),
            provider: provider.into(),
            turn: None,
            started: false,
            turn_started: false,
            assistant: None,
            tools: HashMap::new(),
            permissions: permissions::Permissions::default(),
            extensions: extensions::Extensions::default(),
            settled: Vec::new(),
            terminal: None,
            usage: None,
            last_todo: None,
            assistant_index: 0,
            custom_index: 0,
            users: HashSet::new(),
            clients: VecDeque::new(),
            child_mappers: HashMap::new(),
            bridged: HashMap::new(),
            no_turn_output: Vec::new(),
            stream_text: String::new(),
            stream_thinking: String::new(),
            card_updates: HashMap::new(),
            suppress_next_start: false,
        }
    }
    pub fn begin_turn(&mut self, turn_id: &str) {
        self.turn = Some(turn_id.into());
        self.started = false;
        self.turn_started = false;
        self.suppress_next_start = false;
        self.assistant = None;
        self.settled.clear();
        self.terminal = None;
        self.no_turn_output.clear();
        self.stream_text.clear();
        self.stream_thinking.clear();
    }
    pub fn announce_turn(&mut self) -> Value {
        self.suppress_next_start = true;
        self.event("turn_started")
    }
    #[cfg(test)]
    pub fn active_turn_id(&self) -> Option<&str> {
        self.turn.as_deref()
    }
    pub fn end_turn(&mut self) {
        self.turn = None;
        self.started = false;
        self.turn_started = false;
        self.suppress_next_start = false;
        self.assistant = None;
        self.settled.clear();
        self.terminal = None;
        self.tools.clear();
        self.clients.clear();
        self.no_turn_output.clear();
        self.stream_text.clear();
        self.stream_thinking.clear();
        self.card_updates.clear();
    }
    pub fn pending_permissions(&self) -> Vec<Value> {
        self.permissions.pending()
    }
    pub fn permission_response(&mut self, id: &str, response: &Value) -> anyhow::Result<Value> {
        self.permissions.respond(id, response)
    }
    pub fn drain_responses(&mut self) -> Vec<Value> {
        self.permissions.drain()
    }
    pub fn drain_child_sessions(&mut self) -> Vec<Value> {
        self.extensions.drain_children()
    }
    pub fn poll(&mut self) -> Vec<Value> {
        let events = self.extensions.poll();
        let mut out = self.subagents(events);
        let due: Vec<String> = self
            .card_updates
            .iter_mut()
            .filter_map(|(id, (last, dirty))| {
                if *dirty && last.elapsed() >= std::time::Duration::from_millis(500) {
                    *dirty = false;
                    *last = std::time::Instant::now();
                    Some(id.clone())
                } else {
                    None
                }
            })
            .collect();
        for id in due {
            if let Some((name, args)) = self.tools.get(&id).cloned() {
                out.extend(self.tool_events(
                    &id,
                    &name,
                    &args,
                    "running",
                    &Value::Null,
                    &Value::Null,
                    false,
                ));
            }
        }
        out
    }
    pub fn queue_client_message(&mut self, text: &str, client_message_id: Option<&str>) {
        self.clients
            .push_back((text.into(), client_message_id.map(str::to_owned)));
    }
    pub fn forget_client_message(&mut self, text: &str, client_message_id: Option<&str>) {
        if let Some(index) = self.clients.iter().rposition(|(queued_text, queued_id)| {
            queued_text == text && queued_id.as_deref() == client_message_id
        }) {
            self.clients.remove(index);
        }
    }
    pub fn set_bridged_tools(&mut self, tools: &Value) {
        self.bridged.clear();
        for pair in array(tools) {
            if let Some(name) = pair[0].as_str() {
                self.bridged.insert(name.into(), pair[1].clone());
            }
        }
    }
    pub fn submitted_user_entry(&mut self, entry: &Value) -> Vec<Value> {
        let (Some(id), Some(text)) = (first(&[&entry["id"]]), entry["text"].as_str()) else {
            return Vec::new();
        };
        let client = self
            .clients
            .iter()
            .position(|(t, _)| t == text)
            .and_then(|i| self.clients.remove(i))
            .or_else(|| self.clients.pop_front())
            .and_then(|(_, id)| id);
        let mut item = json!({"type":"user_message","text":text,"messageId":id.trim()});
        put(&mut item, "clientMessageId", client.as_deref());
        vec![self.timeline(item)]
    }
    fn delta(&mut self, message: &Value, thinking: bool, text: &str) -> Value {
        if thinking {
            self.stream_thinking.push_str(text);
            self.timeline(json!({"type":"reasoning","text":text}))
        } else {
            self.stream_text.push_str(text);
            let id = self
                .assistant
                .get_or_insert_with(|| {
                    first(&[&message["responseId"]])
                        .map(str::to_owned)
                        .unwrap_or_else(|| Uuid::new_v4().to_string())
                })
                .clone();
            self.timeline(json!({"type":"assistant_message","text":text,"messageId":id}))
        }
    }
    fn cumulative(&mut self, message: &Value) -> Vec<Value> {
        let mut text = String::new();
        let mut thinking = String::new();
        for block in array(&message["content"]) {
            match string(&block["type"]) {
                "text" => text.push_str(string(&block["text"])),
                "thinking" => thinking.push_str(string(&block["thinking"])),
                _ => {}
            }
        }
        let mut out = Vec::new();
        if let Some(suffix) = thinking
            .strip_prefix(self.stream_thinking.as_str())
            .filter(|s| !s.is_empty())
        {
            out.push(self.delta(message, true, suffix));
        }
        if let Some(suffix) = text
            .strip_prefix(self.stream_text.as_str())
            .filter(|s| !s.is_empty())
        {
            out.push(self.delta(message, false, suffix));
        }
        out
    }
    fn event(&self, kind: &str) -> Value {
        let mut v = json!({"type":kind,"provider":self.provider});
        put(&mut v, "turnId", self.turn.as_deref());
        v
    }
    fn timeline(&self, item: Value) -> Value {
        let mut v = self.event("timeline");
        v["item"] = item;
        v
    }
    fn subagents(&self, events: Vec<Value>) -> Vec<Value> {
        events
            .into_iter()
            .map(|event| json!({"type":"provider_subagent","provider":self.provider,"event":event}))
            .collect()
    }
    fn todo_event(&mut self, item: Value, replay: bool) -> Option<Value> {
        if self.family == "omp" && !replay {
            if let Some(old) = &self.last_todo {
                let a = array(&old["items"]);
                let b = array(&item["items"]);
                if a.len() == b.len()
                    && a.iter()
                        .zip(b)
                        .all(|(a, b)| a["text"] == b["text"] && a["completed"] == b["completed"])
                {
                    return None;
                }
            }
            self.last_todo = Some(item.clone());
        }
        Some(self.timeline(item))
    }
    fn tool_events(
        &mut self,
        id: &str,
        name: &str,
        args: &Value,
        status: &str,
        result: &Value,
        error: &Value,
        replay: bool,
    ) -> Vec<Value> {
        let mut item = tool_item(&self.family, id, name, args, status, result, error);
        if let Some(identity) = self.bridged.get(name) {
            item = json!({"type":"tool_call","callId":id,"name":format!("{} / {}",string(&identity["server"]),string(&identity["tool"])),"status":status,"error":if status=="failed"{error.clone()}else{Value::Null},"detail":{"type":"unknown","input":args,"output":result}});
        }
        let mut extension_events = Vec::new();
        if self.family != "omp" {
            extension_events = self.extensions.tool(id, name, args, status, result);
            if let Some(detail) = self.extensions.followup_detail(name, args, result) {
                item["detail"] = detail;
            }
        }
        if self.family == "omp" && !item.is_null() {
            self.extensions.card(id, &mut item["detail"]);
            if replay && name == "subagent" {
                let mut targets: Vec<&str> = array(&args["poll"])
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .collect();
                if !targets.is_empty() && targets.len() == array(&args["poll"]).len() {
                    targets.sort_unstable();
                    item["callId"] = json!(format!("omp-poll:{}", targets.join(",")));
                }
            }
        }
        let mut out = Vec::new();
        if !item.is_null() {
            out.push(self.timeline(item));
        }
        out.extend(self.subagents(extension_events));
        if !(replay && self.family == "omp")
            && (status == "completed" || (self.family == "omp" && status != "running"))
        {
            if let Some(todo) = extensions::todo(&self.family, name, result) {
                if let Some(event) = self.todo_event(todo, replay) {
                    out.push(event);
                }
            }
        }
        out
    }
    pub fn map(&mut self, frame: &Value) -> Vec<Value> {
        let mut out = Vec::new();
        let kind = string(&frame["type"]);
        if self.family == "omp" {
            if let Some(item) = notices::runtime(frame) {
                out.push(json!({"type":"timeline","provider":self.provider,"item":item}));
                return out;
            }
        }
        match kind{
            "command_output" if self.turn.is_some()=>{
                let ansi=cached_regex!(r"\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07]*(?:\x07|\x1b\\))");
                let text=ansi.replace_all(string(&frame["text"]),"").trim().to_owned();
                if !text.is_empty(){if self.started{out.push(self.timeline(json!({"type":"assistant_message","text":text})));}else{self.no_turn_output.push(text);}}
            }
            "prompt_result" if frame["agentInvoked"]==false && self.turn.is_some() && !self.started=>{
                if let Some((text,client))=self.clients.pop_front(){let mut item=json!({"type":"user_message","text":text});put(&mut item,"clientMessageId",client.as_deref());out.push(self.timeline(item));}
                for text in std::mem::take(&mut self.no_turn_output){out.push(self.timeline(json!({"type":"assistant_message","text":text})));}
                let mut event=self.event("turn_completed");if let Some(error)=first(&[&frame["error"]]){event["type"]=json!("turn_failed");event["error"]=json!(error);out.extend(self.cancel_work());}
                self.end_turn();out.push(event);
            }
            "agent_start"=>{self.started=true;self.no_turn_output.clear();if let Some(id)=frame["sessionId"].as_str(){out.push(json!({"type":"thread_started","provider":self.provider,"sessionId":id}));}},
            "turn_start"=>{if self.suppress_next_start{self.suppress_next_start=false;}else if self.family=="omp"||!self.turn_started{out.push(self.event("turn_started"));}self.started=true;self.turn_started=true;self.no_turn_output.clear();},
            "thinking_level_changed"=>{if let Some(level)=frame["level"].as_str().filter(|s|matches!(*s,"off"|"minimal"|"low"|"medium"|"high"|"xhigh")){out.push(json!({"type":"thinking_option_changed","provider":self.provider,"thinkingOptionId":level}));}},
            "message_start"=>{if frame["message"]["role"]=="assistant"{self.assistant=first(&[&frame["message"]["responseId"]]).map(str::to_owned);self.stream_text.clear();self.stream_thinking.clear();}},
            "message_update"=>{
                if frame.get("message").is_some_and(|m|!m.is_null()&&m["role"]!="assistant"){return out;}
                let update=&frame["assistantMessageEvent"];match string(&update["type"]){
                    "text_delta"=>out.push(self.delta(&frame["message"],false,update["delta"].as_str().unwrap_or(""))),
                    "thinking_delta"=>out.push(self.delta(&frame["message"],true,update["delta"].as_str().unwrap_or(""))),_=>{}
                }
                if frame["message"]["role"]=="assistant"{out.extend(self.cumulative(&frame["message"]));}
            }
            "message_end"=>{
                let message=&frame["message"];match string(&message["role"]){
                    "assistant"=>{out.extend(self.cumulative(message));self.assistant=None;self.stream_text.clear();self.stream_thinking.clear();self.terminal=Some(message.clone());if self.family=="omp"{for block in array(&message["content"]){if block["type"]=="toolCall"&&block["name"]=="ask"{self.permissions.start(&self.family,&self.provider,string(&block["id"]),"ask",&block["arguments"]);}}}},
                    "custom"=>{let changes=self.extensions.custom(message);out.extend(self.subagents(changes));let id=format!("{}-custom-{}",self.provider,Uuid::new_v4());if let Some(mut item)=custom_item(&self.family,message,&id){if self.family=="omp"&&item["type"]=="user_message"{if let Some((text,client))=self.clients.pop_front(){item["text"]=json!(text);put(&mut item,"clientMessageId",client.as_deref());}else{return out;}}out.push(self.timeline(item));}},
                    "user" if self.family=="omp"=>{let id=first(&[&message["id"],&message["entryId"]]);if id.is_some_and(|id|!self.users.insert(id.into())){return out;}let text=tools::content_text(&message["content"]);if !text.is_empty(){let mut item=json!({"type":"user_message","text":text});put(&mut item,"messageId",id);if let Some((_,client))=self.clients.pop_front(){put(&mut item,"clientMessageId",client.as_deref());}out.push(self.timeline(item));}},_=>{}
                }
            }
            "tool_execution_start"=>{let id=string(&frame["toolCallId"]);let name=string(&frame["toolName"]);let args=&frame["args"];self.tools.insert(id.into(),(name.into(),args.clone()));if let Some(req)=self.permissions.start(&self.family,&self.provider,id,name,args){let mut e=self.event("permission_requested");e["request"]=req;out.push(e);}out.extend(self.tool_events(id,name,args,"running",&Value::Null,&Value::Null,false));},
            "tool_execution_update"=>{let id=string(&frame["toolCallId"]);if let Some((name,args))=self.tools.get(id).cloned(){out.extend(self.tool_events(id,&name,&args,"running",&frame["partialResult"],&Value::Null,false));}},
            "tool_execution_end"=>{let id=string(&frame["toolCallId"]);let (name,args)=self.tools.remove(id).unwrap_or_else(||(string(&frame["toolName"]).into(),Value::Null));self.permissions.finish(id,&name);let result=&frame["result"];let failed=frame["isError"]==true||(self.family=="omp"&&name=="web_search"&&result["details"]["error"].is_string());let error=if !failed{Value::Null}else if self.family=="omp"{json!(tools::failure(result))}else{result.clone()};out.extend(self.tool_events(id,&name,&args,if failed{"failed"}else{"completed"},result,&error,false));if name=="task"{self.extensions.finish_card(id);self.card_updates.remove(id);}},
            "extension_ui_request"=>{
                if frame["method"]=="notify"{let message=string(&frame["message"]);if let Some(changes)=self.extensions.notification(message){out.extend(self.subagents(changes));}else if !["PASEO_ENTRY_CAPTURE ","PASEO_SUBMITTED_USER_ENTRY ","PASEO_COMMAND_RESULT "].iter().any(|prefix|message.strip_prefix(*prefix).and_then(|body|serde_json::from_str::<Value>(body).ok()).is_some_and(|v|v.is_object()))&&!message.is_empty(){out.push(self.timeline(json!({"type":"notification","level":match string(&frame["notifyType"]){"warning"=>"warning","error"=>"error",_=>"info"},"message":message})));}}
                else if self.family=="omp"&&frame["method"]=="open_url"&&frame["url"].is_string(){let url=string(&frame["url"]);let mut lines=vec![format!("[Open URL]({url})"),format!("URL: {url}")];if let Some(url)=frame["launchUrl"].as_str(){lines.push(format!("Launch URL: {url}"));}if let Some(instructions)=frame["instructions"].as_str(){lines.push(String::new());lines.push(instructions.into());}out.push(self.timeline(json!({"type":"assistant_message","text":lines.join("\n")})));}
                else if let Some(request)=self.permissions.dialog(&self.family,&self.provider,frame){let mut e=self.event("permission_requested");e["request"]=request;out.push(e);}
            }
            "compaction_start"|"compaction_end"=>out.push(self.timeline(json!({"type":"compaction","status":if kind.ends_with("start"){"loading"}else{"completed"},"trigger":if frame["reason"]=="manual"{"manual"}else{"auto"}}))),
            "auto_retry_start" if self.family!="omp"=>out.push(self.timeline(json!({"type":"error","message":format!("Provider retry (attempt {}): {}",frame["attempt"],string(&frame["errorMessage"]))}))),
            "session_stats"=>{let stats=&frame["stats"];let number=|v:&Value|if v.is_number(){v.clone()}else{json!(0)};let mut usage=json!({"inputTokens":number(&stats["tokens"]["input"]),"cachedInputTokens":number(&stats["tokens"]["cacheRead"]),"outputTokens":number(&stats["tokens"]["output"]),"totalCostUsd":number(&stats["cost"])});for (from,to) in [("contextWindow","contextWindowMaxTokens"),("tokens","contextWindowUsedTokens")]{if stats["contextUsage"][from].is_number(){usage[to]=stats["contextUsage"][from].clone();}}let nonzero=usage.as_object().unwrap().iter().any(|(k,v)|k.starts_with("contextWindow")||v.as_f64().unwrap_or(0.)!=0.);if nonzero&&self.usage.as_ref()!=Some(&usage){self.usage=Some(usage.clone());let mut e=json!({"type":"usage_updated","provider":self.provider,"usage":usage});if let Some(id)=frame["turnId"].as_str(){e["turnId"]=json!(id);}out.push(e);}},
            "todo_reminder" if self.family=="omp"=>{if let Some(item)=extensions::omp_todos(array(&frame["todos"])){if let Some(mut e)=self.todo_event(item,false){e.as_object_mut().unwrap().remove("turnId");out.push(e);}}},
            "subagent_lifecycle"|"subagent_progress" if self.family=="omp"=>{
                let payload=&frame["payload"];let parent=string(&payload["parentToolCallId"]);let mapped=self.extensions.omp_event(kind,payload);
                if let Some((name,args))=self.tools.get(parent).cloned(){
                    let now=std::time::Instant::now();let emit=if let Some((last,dirty))=self.card_updates.get_mut(parent){if last.elapsed()>=std::time::Duration::from_millis(500){*last=now;*dirty=false;true}else{*dirty=true;false}}else{self.card_updates.insert(parent.into(),(now,false));true};
                    if emit{out.extend(self.tool_events(parent,&name,&args,"running",&Value::Null,&Value::Null,false));}
                }
                if let Some(e)=mapped{out.extend(self.subagents(vec![e]));}
            },
            "subagent_event" if self.family=="omp"=>{let p=&frame["payload"];if p["event"]["type"]=="message_end"{let id=string(&p["id"]);let child=self.child_mappers.entry(id.into()).or_insert_with(||Box::new(Mapper::new("omp",&self.provider)));for e in child.replay_message(&p["event"]["message"],None){if e["type"]=="timeline"{let mut event=json!({"type":"timeline","id":id,"item":e["item"]});if let Some(ts)=e.get("timestamp"){event["timestamp"]=ts.clone();}out.push(json!({"type":"provider_subagent","provider":self.provider,"event":event}));}}}},
            "agent_end"|"agent_settled"=>{
                if self.turn.is_none()&&!(self.family=="omp"&&self.started){return out;}
                if self.family!="omp"&&kind=="agent_end"&&frame.get("willRetry").is_some(){self.settled=array(&frame["messages"]).to_vec();return out;}
                let messages=if kind=="agent_settled"{&self.settled[..]}else{array(&frame["messages"])};
                let last=messages.iter().rev().find(|m|m["role"]=="assistant").cloned().or_else(||if self.family=="omp"{self.terminal.clone()}else{None});
                if self.family=="omp"&&last.is_none(){return out;}
                let mut e=self.event("turn_completed");if let Some(last)=last{
                    if self.family=="omp"&&string(&last["stopReason"]).eq_ignore_ascii_case("aborted"){e["type"]=json!("turn_canceled");e["reason"]=json!("interrupted");}
                    else if first(&[&last["errorMessage"]]).is_some(){e["type"]=json!("turn_failed");e["error"]=json!(terminal_error(&self.family,&last));}
                }
                if e["type"]!="turn_completed"{out.extend(self.cancel_work());}self.end_turn();out.push(e);
            }
            "process_exit"=>{if self.turn.is_some(){let mut e=self.event("turn_failed");e["error"]=json!(frame["error"].as_str().unwrap_or("Provider process exited"));out.extend(self.cancel_work());self.end_turn();out.push(e);}},
            _=>{}
        }
        out
    }
    pub fn cancel_work(&mut self) -> Vec<Value> {
        let calls: Vec<_> = self.tools.drain().collect();
        let mut out = Vec::new();
        for (id, (name, args)) in calls {
            out.extend(self.tool_events(
                &id,
                &name,
                &args,
                "canceled",
                &Value::Null,
                &Value::Null,
                false,
            ));
        }
        let events = self.extensions.cancel();
        out.extend(self.subagents(events));
        out
    }
    pub fn replay_message(&mut self, message: &Value, message_id: Option<&str>) -> Vec<Value> {
        let mut out = Vec::new();
        match string(&message["role"]) {
            "user" => {
                let text = tools::content_text(&message["content"]);
                if !text.is_empty() {
                    let mut item = json!({"type":"user_message","text":text});
                    put(&mut item, "messageId", message_id);
                    out.push(self.timeline(item));
                }
            }
            "assistant" => {
                self.assistant_index += 1;
                let id = first(&[&message["responseId"]])
                    .or(message_id)
                    .map(str::to_owned)
                    .unwrap_or_else(|| {
                        format!(
                            "{}-history-assistant-{}",
                            self.provider, self.assistant_index
                        )
                    });
                for block in array(&message["content"]) {
                    match string(&block["type"]){"text" if block["text"].as_str().is_some_and(|s|!s.is_empty())=>out.push(self.timeline(json!({"type":"assistant_message","text":block["text"],"messageId":id}))),"thinking" if block["thinking"].as_str().is_some_and(|s|!s.is_empty())=>out.push(self.timeline(json!({"type":"reasoning","text":block["thinking"]}))),"toolCall"=>{let id=string(&block["id"]);let name=string(&block["name"]);self.tools.insert(id.into(),(name.into(),block["arguments"].clone()));out.extend(self.tool_events(id,name,&block["arguments"],"running",&Value::Null,&Value::Null,true));},_=>{}}
                }
            }
            "toolResult" => {
                let id = string(&message["toolCallId"]);
                let (name, args) = self
                    .tools
                    .remove(id)
                    .unwrap_or_else(|| (string(&message["toolName"]).into(), Value::Null));
                let mut result = json!({"content":message["content"]});
                if let Some(d) = message.get("details") {
                    result["details"] = d.clone();
                }
                let failed = message["isError"] == true
                    || (self.family == "omp"
                        && name == "web_search"
                        && result["details"]["error"].is_string());
                let error = if failed {
                    json!(if self.family == "omp" {
                        tools::failure(&result)
                    } else {
                        tools::text(&result).unwrap_or_else(|| "Tool call failed".into())
                    })
                } else {
                    Value::Null
                };
                out.extend(self.tool_events(
                    id,
                    &name,
                    &args,
                    if failed { "failed" } else { "completed" },
                    &result,
                    &error,
                    true,
                ));
            }
            "custom" => {
                if self.family != "omp" {
                    let events = self.extensions.custom(message);
                    out.extend(self.subagents(events));
                }
                if message["display"] != false
                    && !tools::content_text(&message["content"]).is_empty()
                {
                    let hook = self.family == "omp"
                        && (message["customType"] == "advisor"
                            || notices::system_notice(&tools::content_text(&message["content"]))
                                .is_some());
                    let native_id = if self.family == "omp" {
                        first(&[&message["id"]])
                    } else {
                        None
                    };
                    if !hook && native_id.is_none() {
                        self.custom_index += 1;
                    }
                    let id = format!(
                        "{}-custom-{}",
                        self.provider,
                        native_id
                            .map(str::to_owned)
                            .unwrap_or_else(|| self.custom_index.to_string())
                    );
                    if let Some(mut item) = custom_item(&self.family, message, &id) {
                        if item["type"] == "user_message" {
                            item["messageId"] = json!(format!("{id}-user"));
                        }
                        out.push(self.timeline(item));
                    }
                }
            }
            "bashExecution" => {
                out.push(self.timeline(json!({"type":"tool_call","callId":format!("{}-bash-{}",self.family,message["timestamp"].as_str().map(str::to_owned).unwrap_or_else(||message["timestamp"].to_string())),"name":"bash","status":if message["cancelled"]==true{"canceled"}else{"completed"},"detail":{"type":"shell","command":message["command"],"output":message["output"],"exitCode":message["exitCode"]},"error":null})));
            }
            _ => {}
        }
        if self.family == "omp" {
            if let Some(timestamp) = timestamp(message.get("timestamp")) {
                for event in &mut out {
                    if event["type"] == "timeline" {
                        event["timestamp"] = json!(timestamp);
                    }
                }
            }
        }
        out
    }
}
fn timestamp(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) => Some(s.clone()),
        v => chrono::DateTime::from_timestamp_millis(v.as_i64()?)
            .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
    }
}
fn terminal_error(family: &str, message: &Value) -> String {
    let mut details = Vec::new();
    for key in ["stopReason", "responseModel", "responseId"] {
        if let Some(v) = first(&[&message[key]]) {
            details.push(format!("{key}={v}"));
        }
    }
    if let (Some(p), Some(m)) = (first(&[&message["provider"]]), first(&[&message["model"]])) {
        details.insert(usize::min(1, details.len()), format!("model={p}/{m}"));
    }
    let partial = array(&message["content"])
        .iter()
        .filter_map(|b| match string(&b["type"]) {
            "text" => b["text"].as_str(),
            "thinking" => b["thinking"].as_str(),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    if !partial.trim().is_empty() {
        details.push(format!(
            "partial={}",
            json!(partial.trim().chars().take(500).collect::<String>())
        ));
    }
    let headline = first(&[&message["errorMessage"]])
        .map(str::trim)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{} turn failed", if family == "omp" { "OMP" } else { "Pi" }));
    if details.is_empty() {
        headline
    } else {
        format!("{headline} ({})", details.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streams_missing_snapshots_and_reconciles_terminal_content() {
        let mut mapper = Mapper::new("pi", "custom-pi");
        mapper.begin_turn("turn");
        let first=mapper.map(&json!({"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"hello"}}));
        let id = first[0]["item"]["messageId"].clone();
        assert_eq!(first[0]["provider"], "custom-pi");
        assert_eq!(first[0]["turnId"], "turn");
        let second=mapper.map(&json!({"type":"message_update","message":{"role":"assistant","content":[{"type":"text","text":"hello world"}]}}));
        assert_eq!(second[0]["item"]["text"], " world");
        assert_eq!(second[0]["item"]["messageId"], id);
        let end=mapper.map(&json!({"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"hello world"}]}}));
        assert!(end.is_empty());
    }

    #[test]
    fn omp_approval_returns_native_choice_instead_of_question_answer() {
        let mut mapper = Mapper::new("omp", "work-omp");
        let events=mapper.map(&json!({"type":"extension_ui_request","id":"approve-1","method":"select","title":"Allow tool: bash\nCommand: printf hello","options":["Approve","Deny"]}));
        assert_eq!(events[0]["request"]["kind"], "tool");
        assert_eq!(
            events[0]["request"]["detail"],
            json!({"type":"shell","command":"printf hello"})
        );
        assert_eq!(
            mapper
                .permission_response("approve-1", &json!({"behavior":"allow"}))
                .unwrap(),
            json!({"type":"extension_ui_response","id":"approve-1","value":"Approve"})
        );
        assert!(mapper.pending_permissions().is_empty());
    }

    #[test]
    fn omp_multi_answer_handles_commas_and_custom_editor() {
        let mut mapper = Mapper::new("omp", "omp");
        mapper.map(&json!({"type":"tool_execution_start","toolCallId":"ask","toolName":"ask","args":{"questions":[{"question":"Pick","multi":true,"options":[{"label":"A, B"},{"label":"C"}]}]}}));
        let request = json!({"type":"extension_ui_request","id":"q1","method":"select","title":"Pick","options":["A, B","C",OTHER_FOR_TEST,"✓ Done selecting"]});
        let events = mapper.map(&request);
        assert_eq!(
            events[0]["request"]["input"]["questions"][0]["multiSelect"],
            true
        );
        let reply=mapper.permission_response("q1",&json!({"behavior":"allow","updatedInput":{"answers":{"Response":"A, B, C, my choice"}}})).unwrap();
        assert_eq!(reply["value"], "A, B");
        let mut next = request.clone();
        next["id"] = json!("q2");
        assert!(mapper.map(&next).is_empty());
        assert_eq!(mapper.drain_responses()[0]["value"], "C");
        next["id"] = json!("q3");
        mapper.map(&next);
        assert_eq!(mapper.drain_responses()[0]["value"], OTHER_FOR_TEST);
        mapper.map(
            &json!({"type":"extension_ui_request","id":"editor","method":"editor","title":"Other"}),
        );
        assert_eq!(mapper.drain_responses()[0]["value"], "my choice");
    }
    const OTHER_FOR_TEST: &str = "Other (type your own)";

    #[test]
    fn rpiv_permission_can_be_answered_before_native_dialog() {
        let mut mapper = Mapper::new("pi", "pi");
        let events=mapper.map(&json!({"type":"tool_execution_start","toolCallId":"question-tool","toolName":"ask_user_question","args":{"questions":[{"question":"Choose","header":"Choice","options":[{"label":"One","description":"first"},{"label":"Two","description":"second"}]}]}}));
        assert_eq!(events[0]["request"]["id"], "rpiv-question:question-tool");
        assert!(
            mapper
                .permission_response(
                    "rpiv-question:question-tool",
                    &json!({"behavior":"allow","updatedInput":{"answers":{"Choice":"Two"}}})
                )
                .unwrap()
                .is_null()
        );
        assert!(mapper.map(&json!({"type":"extension_ui_request","id":"native","method":"select","options":["1. One — first","2. Two — second","3. Type something."]})).is_empty());
        assert_eq!(
            mapper.drain_responses(),
            vec![json!({"type":"extension_ui_response","id":"native","value":"2. Two — second"})]
        );
    }

    #[test]
    fn pi_settled_defers_completion_and_keeps_public_provider() {
        let mut mapper = Mapper::new("pi", "custom");
        mapper.begin_turn("a");
        assert!(
            mapper
                .map(&json!({"type":"agent_end","willRetry":false,"messages":[]}))
                .is_empty()
        );
        assert_eq!(
            mapper.map(&json!({"type":"agent_settled"})),
            vec![json!({"type":"turn_completed","provider":"custom","turnId":"a"})]
        );
        assert_eq!(mapper.active_turn_id(), None);
    }

    #[test]
    fn gotgenes_remaps_native_agent_and_discovers_child_transcript() {
        let mut mapper = Mapper::new("pi", "pi");
        mapper.map(&json!({"type":"tool_execution_start","toolCallId":"spawn","toolName":"subagent","args":{"subagent_type":"reviewer","prompt":"Review"}}));
        let events=mapper.map(&json!({"type":"tool_execution_end","toolCallId":"spawn","toolName":"subagent","result":{"content":[{"type":"text","text":"Output file: /tmp/reviewer.jsonl"}],"details":{"agentId":"native-child","status":"running"}}}));
        assert!(events.iter().any(|e| e["type"] == "provider_subagent"
            && e["event"]["id"] == "spawn"
            && e["event"]["status"] == "running"));
        assert_eq!(
            mapper.drain_child_sessions(),
            vec![json!({"id":"spawn","file":"/tmp/reviewer.jsonl"})]
        );
        let events=mapper.map(&json!({"type":"message_end","message":{"role":"custom","customType":"subagent-notification","display":false,"details":{"id":"native-child","status":"completed"}}}));
        assert_eq!(events[0]["event"]["status"], "completed");
    }
}

#[cfg(test)]
mod native_turn_tests {
    use super::*;

    #[test]
    fn omp_native_handoff_turn_completes_without_paseo_turn_id() {
        let mut mapper = Mapper::new("omp", "work-omp");
        mapper.map(&json!({"type":"agent_start","sessionId":"native-session"}));
        assert_eq!(
            mapper.map(&json!({"type":"turn_start"})),
            vec![json!({"type":"turn_started","provider":"work-omp"})]
        );
        let events=mapper.map(&json!({"type":"agent_end","messages":[{"role":"assistant","content":[],"stopReason":"stop"}]}));
        assert_eq!(
            events,
            vec![json!({"type":"turn_completed","provider":"work-omp"})]
        );
        assert!(
            mapper
                .map(&json!({"type":"agent_end","messages":[]}))
                .is_empty()
        );
    }
}
