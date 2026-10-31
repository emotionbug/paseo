use super::{Timeline, limit_item_content, merge_tool_items, now};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::collections::HashMap;

#[derive(Default)]
struct Buffer {
    entries: Vec<Value>,
    tools: HashMap<String, usize>,
    last_flush: Option<u64>,
    deadline: Option<u64>,
}
/// Clock-driven coalescer. The manager schedules `flush_due` at `next_deadline`.
/// Times are monotonic milliseconds; emitted values retain provider/turn metadata.
pub struct StreamCoalescer {
    buffers: HashMap<String, Buffer>,
    window_ms: u64,
}
impl Default for StreamCoalescer {
    fn default() -> Self {
        Self::new(60)
    }
}
impl StreamCoalescer {
    pub fn new(window_ms: u64) -> Self {
        Self {
            buffers: HashMap::new(),
            window_ms,
        }
    }
    pub fn handle(&mut self, agent_id: &str, event: &Value, now_ms: u64) -> Option<Vec<Value>> {
        if event["type"] != "timeline"
            || !matches!(
                event["item"]["type"].as_str(),
                Some("assistant_message" | "reasoning" | "tool_call")
            )
        {
            return None;
        }
        let tool = event["item"]["type"] == "tool_call";
        if !tool && event["item"]["text"] == "" {
            return Some(vec![]);
        }
        let buffer = self.buffers.entry(agent_id.into()).or_default();
        if tool {
            let id = event["item"]["callId"].as_str().unwrap_or("");
            if let Some(&index) = buffer.tools.get(id) {
                let mut entry = event.clone();
                entry["item"] = merge_tool_items(&buffer.entries[index]["item"], &event["item"]);
                buffer.entries[index] = entry;
            } else {
                buffer.tools.insert(id.into(), buffer.entries.len());
                buffer.entries.push(event.clone());
            }
        } else {
            buffer.entries.push(event.clone());
        }
        let terminal = tool
            && matches!(
                event["item"]["status"].as_str(),
                Some("completed" | "failed" | "canceled")
            );
        if terminal
            || (buffer.deadline.is_none()
                && buffer
                    .last_flush
                    .is_none_or(|last| now_ms.saturating_sub(last) >= self.window_ms))
        {
            return Some(self.flush_for(agent_id, now_ms));
        }
        if buffer.deadline.is_none() {
            buffer.deadline = Some(now_ms.saturating_add(self.window_ms));
        }
        Some(vec![])
    }
    pub fn next_deadline(&self) -> Option<u64> {
        self.buffers.values().filter_map(|b| b.deadline).min()
    }
    pub fn flush_for(&mut self, agent_id: &str, now_ms: u64) -> Vec<Value> {
        let Some(buffer) = self.buffers.get_mut(agent_id) else {
            return vec![];
        };
        buffer.deadline = None;
        if buffer.entries.is_empty() {
            return vec![];
        }
        buffer.last_flush = Some(now_ms);
        buffer.tools.clear();
        let mut output: Vec<Value> = Vec::with_capacity(buffer.entries.len());
        for mut entry in std::mem::take(&mut buffer.entries) {
            if let Some(previous) = output.last_mut() {
                let ty = entry["item"]["type"].as_str();
                if matches!(ty, Some("assistant_message" | "reasoning"))
                    && ty == previous["item"]["type"].as_str()
                    && entry.get("provider") == previous.get("provider")
                    && entry.get("turnId") == previous.get("turnId")
                    && (ty == Some("reasoning")
                        || entry["item"].get("messageId") == previous["item"].get("messageId"))
                {
                    if let Some(Value::String(text)) = previous["item"].get_mut("text") {
                        text.push_str(entry["item"]["text"].as_str().unwrap_or(""));
                    }
                    continue;
                }
            }
            entry
                .as_object_mut()
                .expect("timeline event")
                .remove("type");
            entry["agentId"] = json!(agent_id);
            output.push(entry);
        }
        output
    }
    pub fn flush_due(&mut self, now_ms: u64) -> Vec<Value> {
        let ids: Vec<String> = self
            .buffers
            .iter()
            .filter(|(_, b)| b.deadline.is_some_and(|d| d <= now_ms))
            .map(|(id, _)| id.clone())
            .collect();
        ids.into_iter()
            .flat_map(|id| self.flush_for(&id, now_ms))
            .collect()
    }
    pub fn flush_all(&mut self, now_ms: u64) -> Vec<Value> {
        let ids: Vec<String> = self.buffers.keys().cloned().collect();
        ids.into_iter()
            .flat_map(|id| self.flush_for(&id, now_ms))
            .collect()
    }
}

#[derive(Default)]
pub struct SubagentStore {
    descriptors: Vec<Value>,
    timelines: HashMap<(String, String), Timeline>,
}
impl SubagentStore {
    pub fn apply(&mut self, parent_id: &str, provider: &str, event: &Value) -> Result<Value> {
        let id = event["id"]
            .as_str()
            .context("Provider subagent event missing id")?;
        let key = (parent_id.to_owned(), id.to_owned());
        let index = self
            .descriptors
            .iter()
            .position(|d| d["parentAgentId"] == parent_id && d["id"] == id);
        match event["type"].as_str() {
            Some("remove") => {
                if let Some(index) = index {
                    self.descriptors.remove(index);
                }
                self.timelines.remove(&key);
                Ok(json!({"type":"remove","parentAgentId":parent_id,"subagentId":id}))
            }
            Some("timeline") => {
                let timeline = self.timelines.entry(key).or_default();
                let row = timeline.append(limit_item_content(event["item"].clone()), event);
                Ok(
                    json!({"type":"timeline","parentAgentId":parent_id,"subagentId":id,"provider":provider,"row":row,"epoch":timeline.epoch()}),
                )
            }
            Some("upsert") => {
                self.timelines.entry(key).or_default();
                let previous = index.map(|i| &self.descriptors[i]);
                let timestamp = event["timestamp"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(now);
                let mut descriptor = json!({"id":id,"parentAgentId":parent_id,"provider":provider,
                    "status":event.get("status").filter(|status|!status.is_null()).or_else(||previous.and_then(|p|p.get("status")).filter(|status|!status.is_null())).cloned().unwrap_or(json!("running")),
                    "createdAt":previous.and_then(|p|p.get("createdAt")).cloned().unwrap_or(json!(timestamp)),"updatedAt":timestamp});
                for field in [
                    "title",
                    "description",
                    "toolCallId",
                    "cwd",
                    "subtitle",
                    "parentSubagentId",
                ] {
                    descriptor[field] = event
                        .get(field)
                        .or_else(|| previous.and_then(|p| p.get(field)))
                        .cloned()
                        .unwrap_or(Value::Null);
                }
                if let Some(index) = index {
                    self.descriptors[index] = descriptor.clone();
                } else {
                    self.descriptors.push(descriptor.clone());
                }
                Ok(json!({"type":"upsert","subagent":descriptor}))
            }
            _ => bail!("Unknown provider subagent event"),
        }
    }
    pub fn list(&self, parent_id: &str) -> Vec<Value> {
        let mut result: Vec<Value> = self
            .descriptors
            .iter()
            .filter(|d| d["parentAgentId"] == parent_id)
            .cloned()
            .collect();
        result.sort_by(|a, b| a["createdAt"].as_str().cmp(&b["createdAt"].as_str()));
        result
    }
    pub fn list_all(&self) -> &[Value] {
        &self.descriptors
    }
    pub fn get(&self, parent_id: &str, id: &str) -> Option<&Value> {
        self.descriptors
            .iter()
            .find(|d| d["parentAgentId"] == parent_id && d["id"] == id)
    }
    pub fn fetch_timeline(&self, parent_id: &str, id: &str, options: &Value) -> Result<Value> {
        Ok(self
            .timelines
            .get(&(parent_id.into(), id.into()))
            .with_context(|| format!("Unknown agent '{parent_id}\0{id}'"))?
            .fetch(options))
    }
    pub fn checkpoint(&self, parent_id: &str) -> Value {
        let timelines=self.timelines.iter().filter(|((parent,_),_)|parent==parent_id).map(|((_,id),timeline)|json!({"id":id,"epoch":timeline.epoch(),"rows":timeline.rows(),"nextSeq":timeline.next_seq()})).collect::<Vec<_>>();
        json!({"version":1,"parentAgentId":parent_id,"descriptors":self.list(parent_id),"timelines":timelines})
    }
    pub fn restore_parent(&mut self, checkpoint: &Value) -> Result<()> {
        if checkpoint["version"] != 1 {
            bail!("Invalid subagent checkpoint version");
        }
        let parent = checkpoint["parentAgentId"]
            .as_str()
            .context("Missing subagent checkpoint parent")?;
        let descriptors = checkpoint["descriptors"]
            .as_array()
            .context("Missing subagent descriptors")?;
        let timelines = checkpoint["timelines"]
            .as_array()
            .context("Missing subagent timelines")?;
        if descriptors.iter().any(|descriptor| {
            descriptor["parentAgentId"] != parent
                || descriptor["id"].as_str().is_none()
                || descriptor["provider"].as_str().is_none()
        }) {
            bail!("Subagent checkpoint descriptor identity mismatch");
        }
        let mut restored = Vec::new();
        for timeline in timelines {
            let id = timeline["id"]
                .as_str()
                .context("Missing subagent timeline id")?;
            restored.push((
                (parent.to_owned(), id.to_owned()),
                Timeline::seed(timeline)?,
            ));
        }
        self.delete_parent(parent);
        self.descriptors.extend(descriptors.iter().cloned());
        self.timelines.extend(restored);
        Ok(())
    }
    pub fn delete_parent(&mut self, parent_id: &str) -> Vec<Value> {
        let ids: Vec<String> = self
            .descriptors
            .iter()
            .filter(|d| d["parentAgentId"] == parent_id)
            .filter_map(|d| d["id"].as_str().map(str::to_owned))
            .collect();
        self.descriptors.retain(|d| d["parentAgentId"] != parent_id);
        self.timelines.retain(|(parent, _), _| parent != parent_id);
        ids.into_iter()
            .map(|id| json!({"type":"remove","parentAgentId":parent_id,"subagentId":id}))
            .collect()
    }
}
