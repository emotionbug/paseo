//! Agent-bound MCP authority and stateless JSON-RPC server.
mod agent_tools;
mod protocol;
mod remote_tools;
mod schedule_tools;
mod workspace_tools;

use crate::{
    providers::api::{LaunchContext, NativeToolExecutor, NativeToolInputError},
    state::AppState,
};
use anyhow::{Result, anyhow};
use async_trait::async_trait;
pub use protocol::{McpHttpResponse, stdio_socket_bridge};
use rand::RngCore;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Weak},
};
use tokio::sync::{RwLock, mpsc};
use tokio_util::sync::CancellationToken;

/// Paseo tools exposed with a destination selector. A local call keeps the
/// source wrapper semantics: serverId is consumed, echoed in the result, and
/// handler failures become routed tool errors.
const HOST_TOOLS: &[&str] = &[
    "create_agent",
    "create_workspace",
    "list_workspaces",
    "send_agent_prompt",
    "get_agent_status",
    "get_agent_activity",
    "list_agents",
];
struct Grant {
    agent_id: String,
    /// Provider tool policy captured when the run launched, like the source
    /// manager's per-agent policy; config edits apply on the next launch.
    policy: Option<Value>,
    cancel: CancellationToken,
}
pub struct McpService {
    app: Weak<AppState>,
    grants: RwLock<HashMap<String, Arc<Grant>>>,
}
impl McpService {
    pub fn new(app: Weak<AppState>) -> Self {
        Self {
            app,
            grants: RwLock::new(HashMap::new()),
        }
    }
    fn app(&self) -> Result<Arc<AppState>> {
        self.app
            .upgrade()
            .ok_or_else(|| anyhow!("MCP service is shut down"))
    }
    pub async fn caller_context(&self, agent_id: &str) -> Option<Value> {
        if let Some(app) = self.app.upgrade() {
            if app.speech.is_active_for_agent(agent_id).await {
                return Some(
                    json!({"childAgentDefaultLabels":{},"allowCustomCwd":false,"enableVoiceTools":true}),
                );
            }
        }
        None
    }
    pub async fn revoke_agent(&self, agent_id: &str) {
        self.grants.write().await.retain(|_, grant| {
            if grant.agent_id == agent_id {
                grant.cancel.cancel();
                false
            } else {
                true
            }
        });
    }
    pub async fn shutdown(&self) -> Result<()> {
        for (_, grant) in self.grants.write().await.drain() {
            grant.cancel.cancel();
        }
        Ok(())
    }
    pub async fn launch_context(
        &self,
        agent_id: &str,
        config: &Value,
        env: BTreeMap<String, String>,
    ) -> Result<LaunchContext> {
        let app = self.app()?;
        let daemon = app.config.snapshot();
        let tools_enabled = daemon["mcpEnabled"] != false && daemon["mcpInjectIntoAgents"] != false;
        let policy = if tools_enabled {
            provider_tool_policy(&daemon, config)
        } else {
            Some(json!({"enabled":false}))
        };
        // A provider policy with enabled=false withholds the runtime server and
        // native catalog entirely, as the source passes no MCP base URL.
        let enabled = tools_enabled && policy_enabled(policy.as_ref());
        let cancel = app.cancel.child_token();
        let mut servers = strip_internal_servers(
            config
                .get("mcpServers")
                .cloned()
                .unwrap_or_else(|| json!({})),
        );
        let mut executor = None;
        let mut catalog = json!([]);
        // The provider-runtime manifest follows only the daemon switches; a
        // per-provider policy withholds the bound catalog, not the manifest.
        let manifest = if tools_enabled {
            Value::Array(
                self.definitions(None, None, true)
                    .await?
                    .into_iter()
                    .map(public_schema)
                    .collect(),
            )
        } else {
            json!([])
        };
        if enabled {
            let mut bytes = [0u8; 32];
            rand::rngs::OsRng.fill_bytes(&mut bytes);
            let token = hex::encode(bytes);
            let grant = Arc::new(Grant {
                agent_id: agent_id.to_owned(),
                policy: policy.clone(),
                cancel: cancel.clone(),
            });
            let grant_guard = cancel.clone().drop_guard();
            {
                let mut grants = self.grants.write().await;
                grants.retain(|_, old| {
                    if old.agent_id == agent_id || old.cancel.is_cancelled() {
                        old.cancel.cancel();
                        false
                    } else {
                        true
                    }
                });
                grants.insert(token.clone(), grant);
            }
            catalog = Value::Array(
                self.definitions(Some(agent_id), policy.as_ref(), false)
                    .await?
                    .into_iter()
                    .map(public_schema)
                    .collect(),
            );
            if !servers.as_object().is_some_and(|s| s.contains_key("paseo")) {
                let endpoint = app
                    .transport
                    .local_http_endpoint()
                    .ok_or_else(|| anyhow!("MCP HTTP endpoint is not bound"))?;
                let mut url =
                    url::Url::parse(&format!("{}/mcp/agents", endpoint.trim_end_matches('/')))?;
                url.query_pairs_mut().append_pair("callerAgentId", agent_id);
                servers["paseo"] = json!({"type":"http","url":url.as_str(),"headers":{"Authorization":format!("Bearer {token}")}});
            }
            executor = Some(Arc::new(BoundExecutor {
                app: self.app.clone(),
                token,
            }) as Arc<dyn NativeToolExecutor>);
            if cancel.is_cancelled() {
                return Err(anyhow!("Agent MCP launch was revoked"));
            }
            grant_guard.disarm();
        } else {
            self.revoke_agent(agent_id).await;
        }
        Ok(LaunchContext {
            agent_id: agent_id.into(),
            home: app.home.clone(),
            executable: app.executable.clone(),
            env,
            mcp_servers: servers,
            tool_catalog: catalog,
            tool_manifest: manifest,
            tool_executor: executor,
            cancel,
        })
    }
    async fn definitions(
        &self,
        caller: Option<&str>,
        policy: Option<&Value>,
        manifest: bool,
    ) -> Result<Vec<Value>> {
        let context = if let Some(id) = caller {
            self.caller_context(id).await
        } else {
            None
        };
        let voice = context
            .as_ref()
            .is_some_and(|v| v["enableVoiceTools"] == true || v["voiceOnly"] == true);
        let mut tools = Vec::new();
        if voice {
            tools.push(json!({"name":"speak","title":"Speak","description":"Speak text to the user via daemon-managed voice output. Blocks until playback completes.","inputSchema":{"type":"object","properties":{"text":{"type":"string","minLength":1,"maxLength":4000,"x-trim":true}},"required":["text"],"additionalProperties":true}}));
        }
        if !context.as_ref().is_some_and(|v| v["voiceOnly"] == true) {
            tools.push(json!({"name":"list_hosts","title":"List hosts","description":"Discover this host and registered destination hosts for fleet tools.","inputSchema":{"type":"object","properties":{},"additionalProperties":true}}));
            tools.extend(workspace_tools::definitions(caller.is_some() || manifest));
            tools.extend(agent_tools::definitions(caller.is_some() || manifest));
            tools.extend(schedule_tools::definitions(caller.is_some() || manifest));
        }
        for tool in &mut tools {
            if HOST_TOOLS.contains(&tool["name"].as_str().unwrap_or("")) {
                tool["inputSchema"]["properties"]["serverId"] = json!({"type":"string","minLength":1,"description":"Destination server ID from list_hosts; omit for this host."});
            }
        }
        if !manifest {
            tools.retain(|tool| tool_enabled(policy, tool["name"].as_str().unwrap_or("")));
        }
        Ok(tools)
    }
    async fn execute_grant(
        &self,
        token: &str,
        name: &str,
        input: Value,
        cancel: CancellationToken,
        updates: Option<mpsc::Sender<Value>>,
    ) -> Result<Value> {
        let grant = self
            .grants
            .read()
            .await
            .get(token)
            .cloned()
            .ok_or_else(|| input_error("Agent MCP capability has been revoked"))?;
        if grant.cancel.is_cancelled() {
            return Err(input_error("Agent MCP capability has been revoked"));
        }
        let operation = grant.cancel.child_token();
        let _operation_guard = operation.clone().drop_guard();
        let watcher = operation.clone();
        let task = tokio::spawn(async move {
            tokio::select! {_ = cancel.cancelled()=>watcher.cancel(),_ = watcher.cancelled()=>{}}
        });
        let result = self
            .execute_bound(
                Some(&grant.agent_id),
                grant.policy.as_ref(),
                name,
                input,
                operation.clone(),
                updates,
            )
            .await;
        operation.cancel();
        task.abort();
        result
    }
    async fn execute_bound(
        &self,
        caller: Option<&str>,
        policy: Option<&Value>,
        name: &str,
        mut input: Value,
        cancel: CancellationToken,
        updates: Option<mpsc::Sender<Value>>,
    ) -> Result<Value> {
        let app = self.app()?;
        let definition = self
            .definitions(caller, policy, false)
            .await?
            .into_iter()
            .find(|d| d["name"] == name)
            .ok_or_else(|| input_error(format!("Paseo tool not found: {name}")))?;
        validate_input(&definition["inputSchema"], &mut input)?;
        if cancel.is_cancelled() {
            return Err(anyhow!("Tool execution cancelled"));
        }
        let result = if name == "list_hosts" {
            remote_tools::execute(&app, caller, name, input, cancel, updates).await?
        } else if HOST_TOOLS.contains(&name) {
            if remote_tools::is_local(&app, input.get("serverId").and_then(Value::as_str)).await? {
                let server_id = remote_tools::server_identity(&app)?;
                // The source host wrapper destructures serverId before the local
                // handler, then reports every local outcome against its destination.
                if let Some(object) = input.as_object_mut() {
                    object.remove("serverId");
                }
                match execute_local(&app, caller, name, input, cancel, updates).await {
                    Ok(result) => with_server_id(result, &server_id),
                    Err(error) => remote_tools::failure(&server_id, &error),
                }
            } else {
                remote_tools::execute(&app, caller, name, input, cancel, updates).await?
            }
        } else {
            execute_local(&app, caller, name, input, cancel, updates).await?
        };
        Ok(model_visible(result))
    }
}
async fn execute_local(
    app: &Arc<AppState>,
    caller: Option<&str>,
    name: &str,
    input: Value,
    cancel: CancellationToken,
    updates: Option<mpsc::Sender<Value>>,
) -> Result<Value> {
    if name == "speak" {
        let id = caller
            .ok_or_else(|| input_error("speak is only available to agent-scoped tool sessions"))?;
        app.speech
            .speak(id, input["text"].as_str().unwrap_or(""), cancel)
            .await?;
        Ok(json!({"content":[],"structuredContent":{"ok":true}}))
    } else if name.contains("schedule") || name.ends_with("heartbeat") {
        schedule_tools::execute(app, caller, name, input).await
    } else if matches!(
        name,
        "create_workspace"
            | "list_workspaces"
            | "archive_workspace"
            | "rename_workspace"
            | "list_workspace_scripts"
            | "start_workspace_script"
            | "stop_workspace_script"
            | "list_terminals"
            | "create_terminal"
            | "kill_terminal"
            | "capture_terminal"
            | "send_terminal_keys"
    ) {
        workspace_tools::execute(app, caller, name, input, cancel, updates).await
    } else {
        agent_tools::execute(app, caller, name, input, cancel, updates).await
    }
}
/// Mirrors `{...response.structuredContent, serverId: destination}`.
fn with_server_id(mut result: Value, server_id: &str) -> Value {
    if !result["structuredContent"].is_object() {
        result["structuredContent"] = json!({});
    }
    result["structuredContent"]["serverId"] = json!(server_id);
    result
}
struct BoundExecutor {
    app: Weak<AppState>,
    token: String,
}
#[async_trait]
impl NativeToolExecutor for BoundExecutor {
    async fn execute_streaming(
        &self,
        name: &str,
        input: Value,
        cancel: CancellationToken,
        updates: Option<mpsc::Sender<Value>>,
    ) -> Result<Value> {
        let app = self
            .app
            .upgrade()
            .ok_or_else(|| anyhow!("MCP tool owner closed"))?;
        app.mcp
            .execute_grant(&self.token, name, input, cancel, updates)
            .await
    }
}
pub(crate) fn actor(caller: Option<&str>) -> Value {
    match caller {
        Some(id) => json!({"kind":"agent","agentId":id}),
        None => json!({"kind":"owner"}),
    }
}
pub(crate) fn input_error(message: impl Into<String>) -> anyhow::Error {
    NativeToolInputError {
        status: 400,
        issues: None,
        message: message.into(),
    }
    .into()
}
/// `providers[provider].paseoTools`, the source `resolvePaseoToolPolicy` lookup.
fn provider_tool_policy(daemon: &Value, config: &Value) -> Option<Value> {
    let provider = config.get("provider").and_then(Value::as_str)?;
    daemon
        .get("providerOverrides")?
        .get(provider)?
        .get("paseoTools")
        .cloned()
}
fn policy_enabled(policy: Option<&Value>) -> bool {
    !policy.is_some_and(|p| p["enabled"] == false)
}
fn tool_enabled(policy: Option<&Value>, name: &str) -> bool {
    name == "speak"
        || (policy_enabled(policy)
            && !policy
                .and_then(|p| p["disabledTools"].as_array())
                .is_some_and(|list| list.iter().any(|n| n == name)))
}
/// Drop only the daemon-owned runtime server, preserving an explicitly configured
/// third-party server also named paseo. Tokens never flow into stored config.
pub fn strip_internal_servers(mut servers: Value) -> Value {
    if !servers.is_object() {
        return json!({});
    }
    let internal = servers.get("paseo").is_some_and(|s| {
        matches!(s["type"].as_str(), Some("http" | "sse"))
            && s["url"]
                .as_str()
                .and_then(|u| url::Url::parse(u).ok())
                .is_some_and(|u| u.path() == "/mcp/agents")
    });
    if internal {
        servers.as_object_mut().unwrap().remove("paseo");
    }
    servers
}
fn normalize_input(schema: &Value, input: &mut Value) {
    if schema["x-trim"] == true {
        if let Some(s) = input.as_str() {
            *input = json!(s.trim());
        }
    }
    if let (Some(props), Some(obj)) = (schema["properties"].as_object(), input.as_object_mut()) {
        if schema["x-strip-unknown"] == true {
            obj.retain(|key, _| props.contains_key(key));
        }
        for (key, prop) in props {
            if !obj.contains_key(key) {
                if let Some(default) = prop.get("default") {
                    obj.insert(key.clone(), default.clone());
                }
            }
            if let Some(v) = obj.get_mut(key) {
                normalize_input(prop, v);
            }
        }
    }
    if let Some(values) = input.as_array_mut() {
        if let Some(items) = schema.get("items") {
            for value in values {
                normalize_input(items, value);
            }
        }
    }
    for key in ["oneOf", "anyOf"] {
        if let Some(variants) = schema[key].as_array() {
            for variant in variants {
                if let Some(properties) = variant["properties"].as_object() {
                    let discriminators: Vec<_> = properties
                        .iter()
                        .filter_map(|(key, schema)| schema.get("const").map(|value| (key, value)))
                        .collect();
                    if !discriminators.is_empty()
                        && discriminators
                            .iter()
                            .all(|(key, value)| input.get(key.as_str()) == Some(*value))
                    {
                        normalize_input(variant, input);
                        break;
                    }
                }
            }
        }
    }
}
fn validate_input(schema: &Value, input: &mut Value) -> Result<()> {
    normalize_input(schema, input);
    let validator = jsonschema::validator_for(schema).map_err(|e| NativeToolInputError {
        status: 500,
        issues: None,
        message: format!("Invalid tool schema: {e}"),
    })?;
    let issues:Vec<Value>=validator.iter_errors(input).map(|error|json!({"code":"invalid_value","path":error.instance_path().to_string().split('/').skip(1).map(|v|v.replace("~1","/").replace("~0","~")).collect::<Vec<_>>(),"message":error.to_string()})).collect();
    if !issues.is_empty() {
        return Err(NativeToolInputError {
            status: 400,
            message: serde_json::to_string_pretty(&issues)?,
            issues: Some(json!(issues)),
        }
        .into());
    }
    Ok(())
}
fn public_schema(mut value: Value) -> Value {
    fn strip(v: &mut Value) {
        match v {
            Value::Object(o) => {
                o.remove("x-trim");
                o.remove("x-strip-unknown");
                for value in o.values_mut() {
                    strip(value);
                }
            }
            Value::Array(a) => {
                for value in a {
                    strip(value);
                }
            }
            _ => {}
        }
    }
    strip(&mut value);
    value
}
fn model_visible(mut result: Value) -> Value {
    if result.get("structuredContent").is_none()
        || result["content"].as_array().is_some_and(|a| !a.is_empty())
    {
        return result;
    }
    let structured = &result["structuredContent"];
    let mut lines = Vec::new();
    if let Some(object) = structured.as_object() {
        for (key, value) in object {
            if let Some(array) = value.as_array() {
                lines.push(format!("{key}_count={}", array.len()));
                let ids: Option<Vec<&str>> = array
                    .iter()
                    .map(|v| v["id"].as_str().filter(|s| !s.is_empty()))
                    .collect();
                if let Some(ids) = ids.filter(|v| !v.is_empty()) {
                    lines.push(format!("{key}_ids={}", ids.join(",")));
                }
            }
        }
    }
    let formatted =
        serde_json::to_string_pretty(structured).unwrap_or_else(|_| structured.to_string());
    let text = if lines.is_empty() {
        formatted
    } else {
        format!("{}\n\n{formatted}", lines.join("\n"))
    };
    result["content"] = json!([{"type":"text","text":text}]);
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn policy_never_reintroduces_disabled_tool() {
        assert!(!tool_enabled(
            Some(&json!({"enabled":false})),
            "create_agent"
        ));
        assert!(tool_enabled(
            Some(&json!({"enabled":false,"disabledTools":["speak"]})),
            "speak"
        ));
        assert!(!tool_enabled(
            Some(&json!({"disabledTools":["create_agent"]})),
            "create_agent"
        ));
    }
    #[test]
    fn runtime_server_is_removed_without_losing_custom_servers() {
        let s = strip_internal_servers(
            json!({"paseo":{"type":"http","url":"http://localhost:123/mcp/agents?callerAgentId=x","headers":{"Authorization":"Bearer secret"}},"other":{"type":"stdio","command":"example"}}),
        );
        assert!(s.get("paseo").is_none());
        assert_eq!(s["other"]["command"], "example");
        let custom = json!({"paseo":{"type":"http","url":"https://example.org/mcp"}});
        assert_eq!(strip_internal_servers(custom.clone()), custom);
    }
    #[test]
    fn input_errors_preserve_status_and_issues() {
        let mut input = json!({"n":"wrong"});
        let error = validate_input(
            &json!({"type":"object","properties":{"n":{"type":"integer"}}}),
            &mut input,
        )
        .unwrap_err();
        let error = error.downcast_ref::<NativeToolInputError>().unwrap();
        assert_eq!(error.status, 400);
        assert_eq!(error.issues.as_ref().unwrap()[0]["path"], json!(["n"]));
    }
    #[test]
    fn structured_arrays_remain_model_visible() {
        let v = model_visible(
            json!({"content":[],"structuredContent":{"agents":[{"id":"a"},{"id":"b"}]}}),
        );
        assert!(
            v["content"][0]["text"]
                .as_str()
                .unwrap()
                .starts_with("agents_count=2\nagents_ids=a,b\n\n")
        );
    }
    #[test]
    fn provider_policy_comes_from_provider_overrides() {
        let daemon = json!({"providerOverrides":{"codex":{"paseoTools":{"disabledTools":["kill_agent"]}}}});
        let policy = provider_tool_policy(&daemon, &json!({"provider":"codex"}));
        assert!(!tool_enabled(policy.as_ref(), "kill_agent"));
        assert!(tool_enabled(policy.as_ref(), "list_agents"));
        assert!(provider_tool_policy(&daemon, &json!({"provider":"claude"})).is_none());
        assert!(!policy_enabled(Some(&json!({"enabled":false}))));
        assert!(policy_enabled(None));
    }
    #[tokio::test]
    async fn host_tools_advertise_server_selector_and_policy_filters_catalog() {
        let service = McpService::new(Weak::new());
        let tools = service.definitions(None, None, false).await.unwrap();
        for tool in &tools {
            let name = tool["name"].as_str().unwrap();
            assert_eq!(
                tool["inputSchema"]["properties"].get("serverId").is_some(),
                HOST_TOOLS.contains(&name),
                "{name}"
            );
        }
        let policy = json!({"disabledTools":["archive_agent"]});
        let filtered = service
            .definitions(None, Some(&policy), false)
            .await
            .unwrap();
        assert!(!filtered.iter().any(|tool| tool["name"] == "archive_agent"));
        assert!(filtered.iter().any(|tool| tool["name"] == "create_agent"));
    }
    #[test]
    fn local_host_results_echo_destination() {
        let result = with_server_id(
            json!({"content":[],"structuredContent":{"agentId":"a"},"isError":false}),
            "srv_local",
        );
        assert_eq!(
            result["structuredContent"],
            json!({"agentId":"a","serverId":"srv_local"})
        );
    }
}
