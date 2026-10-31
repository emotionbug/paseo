use super::tools::{content_text, text};
use super::{array, first, put, string};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
#[derive(Default)]
pub struct Extensions {
    agents: HashMap<String, (String, bool)>,
    runs: HashMap<String, String>,
    async_runs: HashMap<String, (String, bool)>,
    seen_files: HashSet<String>,
    pending_files: HashMap<String, String>,
    children: Vec<Value>,
    states: HashMap<String, Value>,
    cards: HashMap<String, BTreeMap<usize, Card>>,
}
#[derive(Default)]
struct Card {
    description: Option<String>,
    session: Option<String>,
    lines: Vec<(String, String)>,
}
impl Extensions {
    pub fn drain_children(&mut self) -> Vec<Value> {
        std::mem::take(&mut self.children)
    }
    fn child(&mut self, id: &str, file: Option<&str>) {
        if let Some(file) = file.filter(|s| !s.trim().is_empty()) {
            if self.seen_files.insert(file.into()) {
                self.children.push(json!({"id":id,"file":file}));
            }
        }
    }
    pub fn tool(
        &mut self,
        id: &str,
        name: &str,
        args: &Value,
        status: &str,
        result: &Value,
    ) -> Vec<Value> {
        let mut out = Vec::new();
        let d = &result["details"];
        if matches!(name, "Agent" | "subagent")
            && first(&[&args["subagent_type"]]).is_some()
            && first(&[&args["prompt"]]).is_some()
        {
            let tintin = name == "Agent";
            let title = string(&args["subagent_type"]);
            let description = args["description"]
                .as_str()
                .unwrap_or(string(&args["prompt"]));
            if status == "running" {
                out.push(json!({"type":"upsert","id":id,"title":title,"description":description,"toolCallId":id,"status":"running"}));
            } else if let (Some(agent), Some(state)) =
                (first(&[&d["agentId"]]), d["status"].as_str())
            {
                self.agents.insert(agent.into(), (id.into(), tintin));
                let file = output_file(result).or_else(|| self.pending_files.remove(agent));
                self.child(id, file.as_deref());
                out.push(json!({"type":"upsert","id":id,"title":title,"description":description,"toolCallId":id,"status":agent_status(state,tintin)}));
            } else if status == "failed" {
                out.push(json!({"type":"upsert","id":id,"status":"failed"}));
            } else if !tintin {
                out.push(json!({"type":"remove","id":id}));
            }
        } else if name == "subagent"
            && args["action"].is_null()
            && first(&[&args["agent"]]).is_some()
            && first(&[&args["task"]]).is_some()
        {
            let base = json!({"type":"upsert","id":id,"title":args["agent"],"description":args["task"],"toolCallId":id,"status":"running"});
            if status == "running" {
                out.push(base);
            } else if d["mode"].is_string() && d["results"].is_array() && d["mode"] != "management"
            {
                if let Some(run) = d["runId"].as_str() {
                    self.runs.insert(run.into(), id.into());
                }
                if array(&d["results"]).is_empty()
                    && (d["asyncId"].is_string() || args["async"] == true)
                {
                    if status != "failed" {
                        if let Some(dir) = first(&[&d["asyncDir"]]) {
                            self.async_runs.insert(dir.into(), (id.into(), true));
                        }
                    }
                    let mut v = base;
                    v["status"] = json!(if status == "failed" {
                        "failed"
                    } else {
                        "running"
                    });
                    out.push(v);
                } else if array(&d["results"]).is_empty() && status == "failed" {
                    let mut v = base;
                    v["status"] = json!("failed");
                    out.push(v);
                } else {
                    out.extend(self.rows(
                        id,
                        array(&d["results"]),
                        args["task"].as_str(),
                        status == "failed",
                    ));
                }
            } else if status == "failed" {
                let mut v = base;
                v["status"] = json!("failed");
                out.push(v);
            }
        } else if matches!(name, "get_subagent_result" | "steer_subagent") && status != "running" {
            if let Some((owner, tintin)) = self.agents.get(string(&args["agent_id"])).cloned() {
                if let Some(state) = d["status"].as_str().filter(|_| d["agentId"].is_string()) {
                    if !tintin {
                        self.child(&owner, d["transcriptPath"].as_str());
                    }
                    out.push(
                        json!({"type":"upsert","id":owner,"status":agent_status(state,tintin)}),
                    );
                }
            }
        } else if name == "bg_wait" && status != "running" {
            for c in array(&d["completions"]) {
                if let Some(owner) = self.runs.get(string(&c["runId"])).cloned() {
                    out.extend(self.rows(&owner, array(&c["results"]), None, false));
                }
            }
        }
        out
    }
    pub fn followup_detail(&self, name: &str, args: &Value, result: &Value) -> Option<Value> {
        if !matches!(name, "get_subagent_result" | "steer_subagent")
            || !self.agents.contains_key(string(&args["agent_id"]))
        {
            return None;
        }
        let mut d = json!({"type":"sub_agent","log":text(result).unwrap_or_default().trim()});
        put(
            &mut d,
            "description",
            result["details"]["description"].as_str(),
        );
        Some(d)
    }
    fn rows(
        &mut self,
        owner: &str,
        rows: &[Value],
        description: Option<&str>,
        failed: bool,
    ) -> Vec<Value> {
        let mut out = Vec::new();
        for (index, row) in rows.iter().enumerate() {
            if first(&[&row["agent"]]).is_none() {
                continue;
            }
            let id = if rows.len() == 1 {
                owner.into()
            } else {
                format!("{owner}:{}", row["index"].as_u64().unwrap_or(index as u64))
            };
            let mut v = json!({"type":"upsert","id":id,"title":row["agent"],"toolCallId":owner,"status":if failed||row["success"]==false||(row["success"]!=true&&row["exitCode"]!=0){"failed"}else{"completed"}});
            put(&mut v, "description", description);
            self.child(&id, row["sessionFile"].as_str());
            out.push(v);
        }
        out
    }
    pub fn custom(&mut self, message: &Value) -> Vec<Value> {
        let mut out = Vec::new();
        let kind = string(&message["customType"]);
        if matches!(kind, "subagent-notification" | "subagent-update") {
            let d = &message["details"];
            let rows = std::iter::once(d).chain(array(&d["others"]));
            for row in rows {
                if let Some((owner, tintin)) = self.agents.get(string(&row["id"])).cloned() {
                    self.child(&owner, row["outputFile"].as_str());
                    let mut v = json!({"type":"upsert","id":owner,"status":agent_status(string(&row["status"]),tintin)});
                    put(&mut v, "description", row["description"].as_str());
                    out.push(v);
                }
            }
        } else if kind == "subagent-notify" {
            let body = content_text(&message["content"]);
            let header = body.lines().next().unwrap_or("");
            let status = if cached_regex!(r"^Background tasks completed \(\d+\):").is_match(header)
            {
                Some("completed")
            } else {
                cached_regex!(r"^(?:Background task|Detached foreground task) (completed|failed|stopped): \*\*").captures(header).map(|c|if &c[1]=="stopped"{"canceled"}else if &c[1]=="failed"{"failed"}else{"completed"})
            };
            if let Some(status) = status {
                let mut owner = None;
                for line in body.lines() {
                    if let Some(dir) = line.strip_prefix("Retention-managed async directory: ") {
                        owner = self.async_runs.get(dir).map(|v| v.0.clone());
                        if let Some(id) = &owner {
                            out.push(json!({"type":"upsert","id":id,"status":status}));
                        }
                    } else if let Some(file) = line.strip_prefix("Session file: ") {
                        if let Some(id) = owner.take() {
                            self.child(&id, Some(file));
                        }
                    }
                }
            }
        }
        out
    }
    pub fn notification(&mut self, message: &str) -> Option<Vec<Value>> {
        let raw = message.strip_prefix("PASEO_GOTGENES_CHILD_SESSION ")?;
        let Ok(v) = serde_json::from_str::<Value>(raw) else {
            return Some(Vec::new());
        };
        let (Some(agent), Some(file)) = (first(&[&v["agentId"]]), first(&[&v["file"]])) else {
            return Some(Vec::new());
        };
        if let Some((id, _)) = self.agents.get(agent).cloned() {
            self.child(&id, Some(file));
            Some(vec![json!({"type":"upsert","id":id,"status":"running"})])
        } else {
            self.pending_files.insert(agent.into(), file.into());
            Some(Vec::new())
        }
    }
    pub fn poll(&mut self) -> Vec<Value> {
        let mut out = Vec::new();
        let runs: Vec<(String, String)> = self
            .async_runs
            .iter()
            .filter(|(_, v)| v.1)
            .map(|(d, v)| (d.clone(), v.0.clone()))
            .collect();
        for (dir, owner) in runs {
            let Ok(bytes) = std::fs::read(std::path::Path::new(&dir).join("status.json")) else {
                continue;
            };
            let Ok(v) = serde_json::from_slice::<Value>(&bytes) else {
                continue;
            };
            if !v["state"].is_string() || !v["steps"].is_array() {
                continue;
            }
            let state = string(&v["state"]);
            let terminal = matches!(state, "complete" | "failed" | "stopped");
            let steps = array(&v["steps"]);
            for (i, step) in steps.iter().enumerate() {
                if first(&[&step["agent"]]).is_none() {
                    continue;
                }
                let id = if steps.len() == 1 {
                    owner.clone()
                } else {
                    format!("{owner}:{i}")
                };
                let status = if !terminal {
                    "running"
                } else if state == "stopped" || step["status"] == "stopped" {
                    "canceled"
                } else if state == "failed" || matches!(string(&step["status"]), "failed" | "error")
                {
                    "failed"
                } else {
                    "completed"
                };
                out.push(json!({"type":"upsert","id":id,"title":step["agent"],"status":status}));
                self.child(&id, step["sessionFile"].as_str());
            }
            if terminal {
                if steps.is_empty() {
                    out.push(json!({"type":"upsert","id":owner,"status":if state=="complete"{"completed"}else{"failed"}}));
                }
                if let Some(r) = self.async_runs.get_mut(&dir) {
                    r.1 = false;
                }
            }
        }
        out
    }
    pub fn omp_event(&mut self, kind: &str, payload: &Value) -> Option<Value> {
        let progress = kind == "subagent_progress";
        let id = if progress {
            string(&payload["progress"]["id"])
        } else {
            string(&payload["id"])
        };
        if id.is_empty() {
            return None;
        }
        let state=self.states.entry(id.into()).or_insert_with(||json!({"type":"upsert","id":id,"title":"OMP subagent","description":null,"toolCallId":null,"status":"running"}));
        if let Some(title) = first(&[&payload["agent"]]) {
            state["title"] = json!(title);
        }
        let data = if progress {
            &payload["progress"]
        } else {
            payload
        };
        if let Some(v) = data
            .get("description")
            .or_else(|| payload.get("assignment"))
        {
            state["description"] = v.clone();
        }
        if let Some(v) = payload.get("parentToolCallId") {
            state["toolCallId"] = v.clone();
        }
        let status = string(&data["status"]);
        state["status"] = json!(match status {
            "completed" | "failed" => status,
            "aborted" => "canceled",
            _ => "running",
        });
        if let Some(model) = first(&[&data["resolvedModel"]]) {
            state["resolvedModel"] = json!(model);
        }
        let mut event = state.clone();
        if let Some(model) = first(&[&state["resolvedModel"]]) {
            let model = model.trim();
            let model = model
                .split_once('/')
                .filter(|(p, m)| !p.is_empty() && !m.is_empty())
                .map(|(p, m)| format!("{m} ({p})"))
                .unwrap_or_else(|| model.into());
            event["title"] = json!(format!("{} · {}", string(&state["title"]).trim(), model));
        }
        event.as_object_mut().unwrap().remove("resolvedModel");
        if let Some(parent) = first(&[&payload["parentToolCallId"]]) {
            let i = payload["index"].as_u64().unwrap_or(0) as usize;
            let card = self
                .cards
                .entry(parent.into())
                .or_default()
                .entry(i)
                .or_default();
            if let Some(s) = first(&[&data["description"]]) {
                card.description = Some(s.into());
            }
            if let Some(s) = first(&[&payload["sessionFile"]]) {
                card.session = Some(s.into());
            }
            if !progress {
                append(
                    card,
                    format!("lifecycle:{status}:{id}"),
                    format!("{id} {status}"),
                );
            } else {
                for (prefix, tool) in std::iter::once(("current", &data["currentTool"]))
                    .chain(array(&data["recentTools"]).iter().map(|v| ("tool", v)))
                {
                    if let Some(name) = first(&[&tool["tool"], &tool["name"]]) {
                        let args = first(&[&tool["args"]])
                            .map(str::to_owned)
                            .or_else(|| {
                                (!tool["input"].is_null()).then(|| {
                                    tool["input"]
                                        .as_str()
                                        .map(str::to_owned)
                                        .unwrap_or_else(|| tool["input"].to_string())
                                })
                            })
                            .unwrap_or_default();
                        append(
                            card,
                            format!(
                                "{prefix}:{name}:{args}{}",
                                tool.get("endMs")
                                    .map(|v| format!(":{v}"))
                                    .unwrap_or_default()
                            ),
                            if args.is_empty() {
                                format!("[{name}]")
                            } else {
                                format!("[{name}] {args}")
                            },
                        );
                    }
                }
                for line in array(&data["recentOutput"]) {
                    if let Some(s) =
                        first(&[line, &line["text"], &line["output"], &line["content"]])
                    {
                        append(card, format!("output:{s}"), s.into());
                    }
                }
                if status != "running" {
                    append(
                        card,
                        format!("status:{status}:{id}"),
                        format!("{id} {status}"),
                    );
                }
            }
        }
        Some(event)
    }
    pub fn card(&self, id: &str, detail: &mut Value) {
        if detail["type"] != "sub_agent" {
            return;
        }
        let Some(cards) = self.cards.get(id) else {
            return;
        };
        let Some((_, first)) = cards.first_key_value() else {
            return;
        };
        if detail["description"].is_null() {
            put(detail, "description", first.description.as_deref());
        }
        put(detail, "childSessionId", first.session.as_deref());
        let count = cards.keys().max().copied().unwrap_or(0) + 1;
        let lines: Vec<String> = cards
            .iter()
            .flat_map(|(i, c)| {
                c.lines.iter().map(move |(_, s)| {
                    if count > 1 {
                        format!("[{}/{}] {s}", i + 1, count)
                    } else {
                        s.clone()
                    }
                })
            })
            .collect();
        if !lines.is_empty() {
            detail["log"] = json!(lines.join("\n"));
        }
    }
    pub fn finish_card(&mut self, id: &str) {
        self.cards.remove(id);
    }
    pub fn cancel(&mut self) -> Vec<Value> {
        let mut out = Vec::new();
        for state in self.states.values_mut() {
            if state["status"] == "running" {
                state["status"] = json!("canceled");
                let mut v = state.clone();
                v.as_object_mut().unwrap().remove("resolvedModel");
                out.push(v);
            }
        }
        self.cards.clear();
        out
    }
}
fn append(card: &mut Card, key: String, text: String) {
    if card.lines.iter().any(|(k, _)| k == &key) {
        return;
    }
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return;
    }
    let text = if text.chars().count() > 240 {
        format!("{}...", text.chars().take(240).collect::<String>())
    } else {
        text
    };
    card.lines.push((key, text));
    if card.lines.len() > 200 {
        card.lines.remove(0);
    }
}
fn agent_status(s: &str, tintin: bool) -> &str {
    match s {
        "completed" => "completed",
        "steered" if tintin => "completed",
        "error" => "failed",
        "aborted" | "stopped" => "canceled",
        _ => "running",
    }
}
fn output_file(result: &Value) -> Option<String> {
    text(result)?.lines().find_map(|l| {
        l.strip_prefix("Output file:")
            .map(str::trim)
            .filter(|s| !s.is_empty() && !s.chars().any(char::is_whitespace))
            .map(str::to_owned)
    })
}
pub fn todo(family: &str, name: &str, result: &Value) -> Option<Value> {
    let d = &result["details"];
    if family == "omp" {
        if name != "todo" {
            return None;
        }
        let phases = d["phases"].as_array()?;
        let tasks: Vec<Value> = phases
            .iter()
            .flat_map(|p| array(&p["tasks"]).iter().cloned())
            .collect();
        return omp_todos(&tasks);
    }
    if !d["error"].is_null() {
        return None;
    }
    if name == "todo" {
        if let Some(tasks) = d["tasks"].as_array() {
            let items:Vec<Value>=tasks.iter().filter(|t|t["status"]!="deleted").map(|t|{let mut item=json!({"id":t["id"].as_str().map(str::to_owned).unwrap_or_else(||t["id"].to_string()),"text":t["subject"],"status":t["status"],"completed":t["status"]=="completed"});put(&mut item,"activeForm",first(&[&t["activeForm"]]));item}).collect();
            return Some(json!({"type":"todo","items":items}));
        }
        if let Some(tasks) = d["todos"].as_array() {
            return Some(
                json!({"type":"todo","items":tasks.iter().map(|t|json!({"id":t["id"].to_string(),"text":t["text"],"status":if t["done"]==true{"completed"}else{"pending"},"completed":t["done"]})).collect::<Vec<_>>()}),
            );
        }
    }
    if matches!(
        name,
        "create_goal" | "get_goal" | "update_goal" | "set_goal_tasks" | "update_goal_task"
    ) && d["version"] == 3
        && d["goal"]["taskList"]["tasks"].is_array()
    {
        let mut items = Vec::new();
        goal_tasks(
            array(&d["goal"]["taskList"]["tasks"]),
            string(&d["goal"]["currentTaskId"]),
            &mut items,
        );
        return Some(json!({"type":"todo","items":items}));
    }
    None
}
fn goal_tasks(tasks: &[Value], current: &str, out: &mut Vec<Value>) {
    for t in tasks {
        out.push(json!({"id":t["id"],"text":t["title"],"status":if t["status"]!="pending"{"completed"}else if t["id"]==current{"in_progress"}else{"pending"},"completed":t["status"]!="pending"}));
        goal_tasks(array(&t["subtasks"]), current, out);
    }
}
pub fn omp_todos(tasks: &[Value]) -> Option<Value> {
    if tasks.is_empty() {
        return None;
    }
    Some(
        json!({"type":"todo","items":tasks.iter().filter(|t|t["status"]!="abandoned").map(|t|{let status=string(&t["status"]);let content=string(&t["content"]);json!({"text":if status=="blocked"{format!("{} ({})",content,first(&[&t["blocker"]]).map(|s|format!("blocked: {s}")).unwrap_or_else(||"blocked".into()))}else{content.into()},"status":if matches!(status,"completed"|"in_progress"){status}else{"pending"},"completed":status=="completed"})}).collect::<Vec<_>>()}),
    )
}
