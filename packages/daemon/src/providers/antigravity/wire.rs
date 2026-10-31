// Native agy stream-json schema and projections, from antigravity-provider/server/internal/{wire,timeline}.ts.
use std::collections::{HashMap, HashSet};

use anyhow::Result;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct Error {
    pub code: &'static str,
    pub message: String,
}
impl Error {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}
pub fn diagnostic(message: &str) -> String {
    static AUTH: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new("(?i)auth|sign.in|log.in").expect("constant diagnostic expression")
    });
    if AUTH.is_match(message) {
        "Antigravity authentication failed. Run `agy` and sign in, then retry.".into()
    } else {
        message.into()
    }
}

#[derive(Deserialize)]
#[serde(tag = "event")]
pub enum Frame {
    #[serde(rename = "init")]
    Init {
        conversation_id: String,
        // Required by the wire schema; its contents are not used.
        #[serde(rename = "init")]
        _init: Init,
    },
    #[serde(rename = "step_update")]
    Step { step_update: Step },
    #[serde(rename = "result")]
    Result { result: TurnResult },
}
#[derive(Deserialize)]
pub struct Init {
    #[serde(default, rename = "model")]
    pub _model: Option<String>,
}
#[derive(Deserialize, PartialEq, Eq)]
pub enum StepState {
    ACTIVE,
    DONE,
    ERROR,
}
#[derive(Deserialize)]
pub struct Step {
    #[serde(rename = "conversation_id")]
    pub _conversation_id: String,
    pub step_index: u64,
    pub state: StepState,
    pub step_type: String,
    #[serde(default)]
    pub text_delta: String,
    #[serde(default)]
    pub tool_name: String,
    #[serde(default)]
    pub tool_info: Option<Tool>,
    #[serde(default)]
    pub subagent_info: Option<Subagents>,
    #[serde(default)]
    pub usage: Option<Usage>,
}
#[derive(Deserialize)]
pub struct Usage {
    pub input_tokens: f64,
    pub output_tokens: f64,
    #[serde(default)]
    pub cache_read_tokens: f64,
}
#[derive(Deserialize)]
pub struct Tool {
    pub name: String,
    #[serde(default)]
    pub parameters: serde_json::Map<String, Value>,
    #[serde(default)]
    pub output: String,
    #[serde(default)]
    pub error: Option<ToolError>,
}
#[derive(Deserialize, serde::Serialize)]
pub struct ToolError {
    pub r#type: String,
    pub message: String,
}
#[derive(Deserialize)]
pub struct Subagents {
    pub subagents: Vec<Child>,
}
#[derive(Deserialize, serde::Serialize)]
pub struct Child {
    pub type_name: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub initial_prompt: String,
    #[serde(default)]
    pub conversation_id: Option<String>,
    #[serde(default)]
    pub log_uri: String,
    #[serde(default)]
    pub workspace_uris: Vec<String>,
}
#[derive(Deserialize, PartialEq, Eq)]
pub enum Status {
    SUCCESS,
    ERROR,
}
#[derive(Deserialize)]
pub struct TurnResult {
    #[serde(rename = "conversation_id")]
    pub _conversation_id: String,
    pub status: Status,
    #[serde(default, rename = "response")]
    pub _response: String,
    #[serde(default = "default_error")]
    pub error: String,
    #[serde(default)]
    pub denied_actions: Vec<Denial>,
}
fn default_error() -> String {
    "Antigravity turn failed".into()
}
#[derive(Deserialize)]
pub struct Denial {
    pub action: String,
    pub display_name: String,
}

pub fn decode(line: &str) -> Result<Frame> {
    let frame: Frame = serde_json::from_str(line)
        .map_err(|e| Error::new("INVALID_FRAME", format!("Invalid Antigravity output: {e}")))?;
    match &frame {
        Frame::Init {
            conversation_id, ..
        } if conversation_id.is_empty() => {
            return Err(Error::new(
                "INVALID_FRAME",
                "Invalid Antigravity output: empty conversation ID",
            )
            .into());
        }
        Frame::Step {
            step_update: Step {
                usage: Some(usage), ..
            },
        } if [
            usage.input_tokens,
            usage.output_tokens,
            usage.cache_read_tokens,
        ]
        .iter()
        .any(|v| !v.is_finite() || *v < 0.) =>
        {
            return Err(Error::new(
                "INVALID_FRAME",
                "Invalid Antigravity output: negative usage",
            )
            .into());
        }
        _ => {}
    }
    Ok(frame)
}

pub struct Turn {
    pub id: String,
    usage_steps: HashSet<u64>,
    usage: [f64; 3],
    tools: serde_json::Map<String, Value>,
}
impl Turn {
    pub fn new() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            usage_steps: HashSet::new(),
            usage: [0.; 3],
            tools: serde_json::Map::new(),
        }
    }
    pub fn step(&mut self, step: Step, provider: &str) -> Vec<Value> {
        let id = format!("{}:{}", self.id, step.step_index);
        let mut events = Vec::new();
        if step.step_type == "agent_response" {
            // The plugin adapter converts native cumulative snapshots back into deltas.
            if !step.text_delta.is_empty() {
                events.push(self.timeline(
                    provider,
                    json!({"type":"assistant_message","messageId":id,"text":step.text_delta}),
                ));
            }
            if step.state == StepState::DONE
                && let Some(usage) = step.usage
                && self.usage_steps.insert(step.step_index)
            {
                self.usage[0] += usage.input_tokens;
                self.usage[1] += usage.output_tokens;
                self.usage[2] += usage.cache_read_tokens;
                events.push(json!({"type":"usage_updated","provider":provider,"turnId":self.id,
                    "usage":{"inputTokens":self.usage[0],"outputTokens":self.usage[1],"cachedInputTokens":self.usage[2]}}));
            }
        } else if step.step_type == "tool" || step.step_type == "subagent" {
            let item = tool_item(&step, &id);
            events.push(self.timeline(provider, item.clone()));
            self.tools.insert(id, item);
        }
        events
    }
    pub fn timeline(&self, provider: &str, item: Value) -> Value {
        json!({"type":"timeline","provider":provider,"turnId":self.id,"item":item})
    }
    pub fn finish(&self, provider: &str, status: &str) -> Vec<Value> {
        self.tools
            .values()
            .filter(|item| item["status"] == "running")
            .map(|item| {
                let mut item = item.clone();
                item["status"] = json!(status);
                item["error"] = if status == "failed" {
                    json!("Antigravity turn failed")
                } else {
                    Value::Null
                };
                self.timeline(provider, item)
            })
            .collect()
    }
}

fn tool_item(step: &Step, id: &str) -> Value {
    let detail = if let Some(child) = step
        .subagent_info
        .as_ref()
        .and_then(|s| s.subagents.first())
    {
        let mut value = json!({"type":"sub_agent","subAgentType":child.type_name,"description":child.initial_prompt,"log":child.log_uri});
        if let Some(id) = &child.conversation_id {
            value["childSessionId"] = json!(id);
        }
        value
    } else if let Some(info) = &step.tool_info {
        let path = info
            .parameters
            .get("AbsolutePath")
            .filter(|v| match v {
                Value::Null | Value::Bool(false) => false,
                Value::String(s) => !s.is_empty(),
                Value::Number(n) => n.as_f64() != Some(0.),
                _ => true,
            })
            .or_else(|| info.parameters.get("TargetFile"))
            .and_then(Value::as_str);
        match (
            info.name.as_str(),
            path,
            info.parameters.get("CommandLine").and_then(Value::as_str),
        ) {
            ("run_command", _, Some(command)) => {
                json!({"type":"shell","command":command,"output":info.output})
            }
            ("view_file", Some(path), _) => {
                json!({"type":"read","filePath":path,"content":info.output})
            }
            ("write_to_file", Some(path), _) => json!({"type":"write","filePath":path}),
            ("replace_file_content" | "multi_replace_file_content", Some(path), _) => {
                json!({"type":"edit","filePath":path})
            }
            _ => {
                json!({"type":"unknown","input":info.parameters,"output":info.error.as_ref().map(|e| json!(e)).unwrap_or_else(|| json!(info.output))})
            }
        }
    } else {
        json!({"type":"plain_text","label":step.tool_name,"text":""})
    };
    let status = match step.state {
        StepState::ACTIVE => "running",
        StepState::DONE => "completed",
        StepState::ERROR => "failed",
    };
    let error = if step.state == StepState::ERROR {
        json!(
            step.tool_info
                .as_ref()
                .and_then(|t| t.error.as_ref())
                .map(|e| e.message.as_str())
                .unwrap_or("Antigravity tool failed")
        )
    } else {
        Value::Null
    };
    let mut item = json!({"type":"tool_call","callId":id,"name":if step.tool_name.is_empty() { "Antigravity tool" } else { &step.tool_name },"detail":detail,"status":status,"error":error});
    if let Some(children) = &step.subagent_info {
        item["metadata"] = json!({"subagents":children.subagents});
    }
    item
}

pub fn denial_title(
    actions: &[Denial],
    reported: &mut HashMap<(String, String), usize>,
) -> Option<String> {
    let mut counts = HashMap::new();
    let mut fresh = 0;
    let mut command = false;
    for action in actions {
        let key = (action.action.clone(), action.display_name.clone());
        let count = counts.entry(key.clone()).or_insert(0);
        *count += 1;
        if *count > reported.get(&key).copied().unwrap_or(0) {
            fresh += 1;
            command = action.action == "command";
        }
    }
    for (key, count) in counts {
        let entry = reported.entry(key).or_insert(0);
        *entry = (*entry).max(count);
    }
    match fresh {
        0 => None,
        1 => Some(
            if command {
                "Shell command denied"
            } else {
                "Action denied"
            }
            .into(),
        ),
        n => Some(format!("{n} actions denied")),
    }
}
