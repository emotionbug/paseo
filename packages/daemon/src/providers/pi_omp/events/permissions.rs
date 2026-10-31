use super::{array, first, put, string};
use serde_json::{Value, json};
use std::collections::VecDeque;
const OTHER: &str = "Other (type your own)";
const FREEFORM: &str = "✏️ Type custom response...";
#[derive(Default)]
pub struct Permissions {
    requests: Vec<Value>,
    responses: Vec<Value>,
    ask: Vec<Value>,
    pending: Option<Pending>,
    pi_ask: Value,
    pi_pending: Option<(String, Option<String>)>,
    rpiv: Option<Rpiv>,
}
struct Pending {
    title: String,
    selected: VecDeque<String>,
    custom: Option<String>,
    editor: bool,
}
struct Rpiv {
    id: String,
    questions: Vec<Value>,
    answers: Option<Value>,
    cancelled: bool,
    index: usize,
    custom: bool,
    deferred: Option<Value>,
}
pub fn frame(id: &str, body: Value) -> Value {
    let mut v = body;
    v["type"] = json!("extension_ui_response");
    v["id"] = json!(id);
    v
}
fn optional(s: &str) -> bool {
    cached_regex!(r"(?i)\b(optional|skip)\b").is_match(s)
}
fn matches(title: &str, q: &str) -> bool {
    let re = cached_regex!(r"^\(\d+ selected\) ");
    let title = re.replace(title, "");
    title == q
        || title
            .strip_prefix(q)
            .is_some_and(|s| cached_regex!(r"^ \(\d+/\d+\)$").is_match(s))
}
impl Permissions {
    pub fn pending(&self) -> Vec<Value> {
        self.requests.clone()
    }
    pub fn drain(&mut self) -> Vec<Value> {
        std::mem::take(&mut self.responses)
    }
    pub fn start(
        &mut self,
        family: &str,
        provider: &str,
        id: &str,
        name: &str,
        args: &Value,
    ) -> Option<Value> {
        if family == "omp" && name == "ask" {
            self.ask = array(&args["questions"])
                .iter()
                .filter(|q| q["question"].is_string() && q["options"].is_array())
                .cloned()
                .collect();
            self.pending = None;
        }
        if family != "omp" {
            self.pi_ask = if name == "ask_user" {
                args.clone()
            } else {
                Value::Null
            };
            if name == "ask_user_question" && valid_rpiv(args) {
                let questions = array(&args["questions"]).to_vec();
                self.rpiv = Some(Rpiv {
                    id: id.into(),
                    questions: questions.clone(),
                    answers: None,
                    cancelled: false,
                    index: 0,
                    custom: false,
                    deferred: None,
                });
                let normalized:Vec<Value>=questions.iter().map(|q|json!({"question":q["question"],"header":q["header"],"options":q["options"],"multiSelect":q["multiSelect"].as_bool().unwrap_or(false),"allowOther":true})).collect();
                let req = json!({"id":format!("rpiv-question:{id}"),"provider":provider,"name":"Pi ask_user_question","kind":"question","title":questions[0]["question"],"input":{"questions":normalized}});
                self.requests.push(req.clone());
                return Some(req);
            }
        }
        None
    }
    pub fn finish(&mut self, id: &str, name: &str) {
        if name == "ask" {
            self.ask.clear();
            self.pending = None;
        }
        if name == "ask_user" {
            self.pi_ask = Value::Null;
            self.pi_pending = None;
        }
        if self.rpiv.as_ref().is_some_and(|r| r.id == id) {
            self.rpiv = None;
        }
    }
    pub fn dialog(&mut self, family: &str, provider: &str, event: &Value) -> Option<Value> {
        let method = string(&event["method"]);
        let id = string(&event["id"]);
        if family != "omp" {
            if let Some(r) = self.rpiv.as_mut() {
                if matches!(method, "select" | "input") {
                    if r.answers.is_none() && !r.cancelled {
                        r.deferred = Some(event.clone());
                    } else {
                        self.responses.push(frame(id, rpiv_reply(r, event)));
                    }
                    return None;
                }
            }
            if method == "input" {
                if let Some((comment, freeform)) = self.pi_pending.as_mut() {
                    if optional(string(&event["placeholder"])) {
                        self.responses.push(frame(id, json!({"value":comment})));
                        self.pi_pending = None;
                        return None;
                    }
                    if let Some(value) = freeform.take() {
                        self.responses.push(frame(id, json!({"value":value})));
                        return None;
                    }
                }
            }
        } else if let Some(p) = self.pending.as_mut() {
            if p.editor && method == "editor" && p.custom.is_some() {
                let value = p.custom.take().unwrap();
                self.pending = None;
                self.responses.push(frame(id, json!({"value":value})));
                return None;
            }
            if !p.editor && method == "select" && matches(string(&event["title"]), &p.title) {
                let options = array(&event["options"]);
                let response = if p
                    .selected
                    .front()
                    .is_some_and(|c| options.iter().any(|v| v.as_str() == Some(c)))
                {
                    json!({"value":p.selected.pop_front().unwrap()})
                } else if p.custom.is_some() && options.iter().any(|v| v == OTHER) {
                    p.editor = true;
                    json!({"value":OTHER})
                } else {
                    let done = options
                        .iter()
                        .filter_map(Value::as_str)
                        .find(|s| s.ends_with(" Done selecting"))
                        .map(str::to_owned);
                    self.pending = None;
                    done.map(|v| json!({"value":v}))
                        .unwrap_or(json!({"cancelled":true}))
                };
                self.responses.push(frame(id, response));
                return None;
            }
        }
        let mut req = if family == "omp" {
            approval(event, provider).or_else(|| self.ask_dialog(event, provider))
        } else {
            None
        };
        if req.is_none() {
            req = generic(event, provider, if family == "omp" { "OMP" } else { "Pi" });
        }
        let mut req = req?;
        if family != "omp"
            && method == "select"
            && self.pi_ask["allowComment"] == true
            && self.pi_ask["allowMultiple"] != true
        {
            let options: Vec<&str> = array(&event["options"])
                .iter()
                .filter_map(Value::as_str)
                .filter(|s| *s != FREEFORM)
                .collect();
            let allow = self.pi_ask["allowFreeform"] != false
                || options.len() != array(&event["options"]).len();
            req["name"] = json!("Pi ask_user");
            req["input"]["questions"][0]["options"] = json!(
                options
                    .iter()
                    .map(|s| json!({"label":s}))
                    .collect::<Vec<_>>()
            );
            if allow {
                req["input"]["questions"][0]["allowOther"] = json!(true);
            }
            req["input"]["questions"].as_array_mut().unwrap().push(json!({"question":"Optional comment","header":"Comment","options":[],"multiSelect":false,"placeholder":"Optional comment (press Enter to skip)...","allowEmpty":true}));
            req["metadata"]["combinedAskUser"] = json!("ask_user_select_optional_comment");
            req["metadata"]["commentHeader"] = json!("Comment");
            req["metadata"]["selectOptions"] = json!(options);
            if allow {
                req["metadata"]["freeformSentinel"] = json!(FREEFORM);
            }
        }
        self.requests.push(req.clone());
        Some(req)
    }
    fn ask_dialog(&self, event: &Value, provider: &str) -> Option<Value> {
        if event["method"] != "select" || !array(&event["options"]).iter().any(|o| o == OTHER) {
            return None;
        }
        let q = self
            .ask
            .iter()
            .find(|q| matches(string(&event["title"]), string(&q["question"])))?;
        let options: Vec<Value> = array(&event["options"])
            .iter()
            .enumerate()
            .filter_map(|(i, o)| {
                let label = o.as_str()?;
                if label == OTHER || label.ends_with(" Done selecting") {
                    return None;
                }
                let mut v = json!({"label":label});
                let description = array(&q["options"])
                    .iter()
                    .find(|o| {
                        o["label"] == label
                            || o["label"].as_str()
                                == Some(label.strip_suffix(" (Recommended)").unwrap_or(label))
                    })
                    .and_then(|o| o["description"].as_str())
                    .or(event["optionDetails"][i]["description"].as_str());
                put(&mut v, "description", description);
                Some(v)
            })
            .collect();
        Some(
            json!({"id":event["id"],"provider":provider,"name":"OMP ask","kind":"question","title":q["question"],"input":{"questions":[{"question":q["question"],"header":"Response","options":options,"multiSelect":q["multi"]==true,"allowOther":true}]},"metadata":{"ompAsk":true,"answerHeader":"Response","optionLabels":options.iter().map(|o|o["label"].clone()).collect::<Vec<_>>()}}),
        )
    }
    pub fn respond(&mut self, id: &str, response: &Value) -> anyhow::Result<Value> {
        let pos = self
            .requests
            .iter()
            .position(|r| r["id"] == id)
            .ok_or_else(|| anyhow::anyhow!("No pending permission request with id '{id}'"))?;
        let req = self.requests.remove(pos);
        let allow = response["behavior"] == "allow";
        let answers = &response["updatedInput"]["answers"];
        let answer = answers
            .as_object()
            .and_then(|m| m.values().find_map(Value::as_str));
        if id.starts_with("rpiv-question:") {
            if let Some(r) = self.rpiv.as_mut() {
                r.answers = if allow && answers.is_object() {
                    Some(answers.clone())
                } else {
                    None
                };
                r.cancelled = !allow || r.answers.is_none();
                if let Some(dialog) = r.deferred.take() {
                    return Ok(frame(string(&dialog["id"]), rpiv_reply(r, &dialog)));
                }
                return Ok(Value::Null);
            }
        }
        if req["metadata"]["toolApproval"] == "omp_rpc_ui_tool_approval" {
            return Ok(frame(id, json!({"value":if allow{"Approve"}else{"Deny"}})));
        }
        if req["metadata"]["combinedAskUser"] == "ask_user_select_optional_comment" {
            let Some(selected) = answers["Response"].as_str().filter(|_| allow) else {
                self.pi_pending = None;
                return Ok(frame(id, json!({"cancelled":true})));
            };
            let custom = req["metadata"]["freeformSentinel"].is_string()
                && !array(&req["metadata"]["selectOptions"])
                    .iter()
                    .any(|v| v == selected);
            self.pi_pending = Some((
                string(&answers["Comment"]).into(),
                custom.then(|| selected.into()),
            ));
            return Ok(frame(
                id,
                json!({"value":if custom{FREEFORM}else{selected}}),
            ));
        }
        if req["metadata"]["ompAsk"] == true {
            let Some(answer) = answers["Response"].as_str().filter(|_| allow) else {
                self.pending = None;
                return Ok(frame(id, json!({"cancelled":true})));
            };
            let labels: Vec<&str> = array(&req["metadata"]["optionLabels"])
                .iter()
                .filter_map(Value::as_str)
                .collect();
            let multi = req["input"]["questions"][0]["multiSelect"] == true;
            let (mut selected, custom) = if multi {
                split(answer, &labels)
            } else if labels.contains(&answer) {
                (VecDeque::from([answer.into()]), None)
            } else {
                (VecDeque::new(), Some(answer.into()))
            };
            if selected.is_empty() && custom.is_none() {
                return Ok(frame(id, json!({"cancelled":true})));
            }
            let value = selected.pop_front().unwrap_or_else(|| OTHER.into());
            if multi || custom.is_some() {
                self.pending = Some(Pending {
                    title: string(&req["title"]).into(),
                    selected,
                    editor: value == OTHER,
                    custom,
                });
            } else {
                self.pending = None;
            }
            return Ok(frame(id, json!({"value":value})));
        }
        Ok(frame(
            id,
            if !allow || answer.is_none() {
                json!({"cancelled":true})
            } else if req["metadata"]["extensionUiMethod"] == "confirm" {
                json!({"confirmed":answer.unwrap().trim().eq_ignore_ascii_case("yes")})
            } else {
                json!({"value":answer.unwrap()})
            },
        ))
    }
}
fn split(answer: &str, labels: &[&str]) -> (VecDeque<String>, Option<String>) {
    let mut remaining = answer;
    let mut selected = VecDeque::new();
    while !remaining.is_empty() {
        let label = labels
            .iter()
            .copied()
            .filter(|s| !selected.iter().any(|v| v == s))
            .filter(|s| {
                remaining == *s
                    || remaining
                        .strip_prefix(*s)
                        .is_some_and(|r| r.starts_with(", "))
            })
            .max_by_key(|s| s.len());
        let Some(label) = label else {
            break;
        };
        selected.push_back(label.to_owned());
        remaining = if remaining == label {
            ""
        } else {
            &remaining[label.len() + 2..]
        };
    }
    (selected, (!remaining.is_empty()).then(|| remaining.into()))
}
fn generic(event: &Value, provider: &str, label: &str) -> Option<Value> {
    let method = string(&event["method"]);
    let mut options = Vec::new();
    let mut extra = json!({});
    let title = match method {
        "select" => {
            options = array(&event["options"])
                .iter()
                .enumerate()
                .filter_map(|(i, v)| {
                    v.as_str().map(|s| {
                        let mut o = json!({"label":s});
                        if label == "OMP" {
                            put(
                                &mut o,
                                "description",
                                first(&[&event["optionDetails"][i]["description"]]),
                            );
                        }
                        o
                    })
                })
                .collect();
            event["title"].as_str().unwrap_or("Select an option").into()
        }
        "input" => {
            let p = string(&event["placeholder"]);
            if !p.is_empty() {
                extra["placeholder"] = json!(p);
            }
            if optional(p) {
                extra["allowEmpty"] = json!(true);
                extra["dismissLabel"] = json!("Skip");
                if cached_regex!(r"(?i)\bcomment\b")
                    .is_match(&format!("{}\n{p}", string(&event["title"])))
                {
                    "Optional comment".into()
                } else {
                    "Optional response".into()
                }
            } else {
                event["title"].as_str().unwrap_or("Enter a value").into()
            }
        }
        "editor" => event["title"].as_str().unwrap_or("Edit text").into(),
        "confirm" => {
            options = vec![json!({"label":"Yes"}), json!({"label":"No"})];
            [event["title"].as_str(), event["message"].as_str()]
                .into_iter()
                .flatten()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n")
        }
        _ => return None,
    };
    let mut q = json!({"question":title,"header":"Response","options":options,"multiSelect":false});
    q.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    Some(
        json!({"id":event["id"],"provider":provider,"name":format!("{label} {method}"),"kind":"question","title":title,"input":{"questions":[q]},"metadata":{"extensionUiMethod":method,"answerHeader":"Response"}}),
    )
}
fn approval(e: &Value, provider: &str) -> Option<Value> {
    if e["method"] != "select" || e["options"] != json!(["Approve", "Deny"]) {
        return None;
    }
    let title = e["title"].as_str()?;
    let (line, body) = title.split_once('\n').unwrap_or((title, ""));
    let name = line.trim().strip_prefix("Allow tool: ")?.trim();
    let (args, detail, description) = match name {
        "bash" => {
            let captures =
                cached_regex!(r"(?s)(?:^|\r?\n)[\t ]*Command:[\t ]?(.*)$").captures(body)?;
            let command = captures.get(1)?.as_str();
            if command.is_empty() {
                return None;
            }
            (
                json!({"command":command}),
                json!({"type":"shell","command":command}),
                format!("Command: {command}"),
            )
        }
        "edit" => {
            let path = body.lines().find_map(|l| {
                l.trim()
                    .strip_prefix("File:")
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
            })?;
            (
                json!({"path":path}),
                json!({"type":"edit","filePath":path}),
                format!("File: {path}"),
            )
        }
        "write" => {
            let lines: Vec<&str> = body.lines().collect();
            let i = lines.iter().position(|l| l.trim().starts_with("Path:"))?;
            let path = lines[i].trim().strip_prefix("Path:")?.trim();
            if path.is_empty() {
                return None;
            }
            let c = lines
                .iter()
                .enumerate()
                .find(|(j, l)| *j > i && l.trim() == "Content:")?
                .0;
            let content = lines[c + 1..].join("\n");
            (
                json!({"path":path,"content":content}),
                json!({"type":"write","filePath":path,"content":content}),
                format!("Path: {path}"),
            )
        }
        _ => return None,
    };
    Some(
        json!({"id":e["id"],"provider":provider,"name":name,"kind":"tool","title":format!("Allow tool: {name}"),"description":description,"detail":detail,"actions":[{"id":"deny","label":"Deny","behavior":"deny","variant":"danger","intent":"dismiss"},{"id":"approve","label":"Approve","behavior":"allow","variant":"primary"}],"metadata":{"extensionUiMethod":"select","toolApproval":"omp_rpc_ui_tool_approval","toolName":name,"toolArgs":args,"approveValue":"Approve","denyValue":"Deny"}}),
    )
}
fn valid_rpiv(args: &Value) -> bool {
    let qs = array(&args["questions"]);
    if qs.is_empty() || qs.len() > 4 {
        return false;
    }
    let mut seen = std::collections::HashSet::new();
    qs.iter().all(|q| {
        if !q["question"].is_string()
            || !q["header"].is_string()
            || !seen.insert(string(&q["question"]))
        {
            return false;
        }
        let os = array(&q["options"]);
        let mut labels = std::collections::HashSet::new();
        (2..=4).contains(&os.len())
            && os.iter().all(|o| {
                o["label"].is_string()
                    && o["description"].is_string()
                    && !matches!(string(&o["label"]), "Other" | "Type something." | "Next")
                    && labels.insert(string(&o["label"]))
            })
    })
}
fn rpiv_reply(r: &mut Rpiv, event: &Value) -> Value {
    if r.cancelled {
        return json!({"cancelled":true});
    }
    let Some(q) = r.questions.get(r.index) else {
        return json!({"cancelled":true});
    };
    let Some(answer) = r
        .answers
        .as_ref()
        .and_then(|a| a[string(&q["header"])].as_str())
    else {
        return json!({"cancelled":true});
    };
    if r.custom {
        r.custom = false;
        r.index += 1;
        return json!({"value":answer});
    }
    let options = array(&q["options"]);
    if q["multiSelect"] == true {
        let indices: Option<Vec<usize>> = answer
            .split(", ")
            .map(|s| options.iter().position(|o| o["label"] == s).map(|i| i + 1))
            .collect();
        r.index += 1;
        return json!({"value":indices.map(|v|v.iter().map(usize::to_string).collect::<Vec<_>>().join(",")).unwrap_or_else(||answer.into())});
    }
    if let Some(i) = options.iter().position(|o| o["label"] == answer) {
        r.index += 1;
        return json!({"value":event["options"][i].as_str().map(str::to_owned).unwrap_or_else(||format!("{}. {} — {}",i+1,answer,string(&options[i]["description"]))) });
    }
    r.custom = true;
    json!({"value":event["options"][options.len()].as_str().map(str::to_owned).unwrap_or_else(||format!("{}. Type something.",options.len()+1))})
}
