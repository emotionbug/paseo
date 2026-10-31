use super::{Grant, McpService, public_schema};
use crate::providers::api::NativeToolInputError;
use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};
use tokio::{io::AsyncWriteExt, sync::mpsc};
use tokio_util::sync::CancellationToken;

pub struct McpHttpResponse {
    pub status: u16,
    pub body: Option<Value>,
}
fn response(status: u16, body: Value) -> McpHttpResponse {
    McpHttpResponse {
        status,
        body: Some(body),
    }
}
fn rpc_error(id: Value, code: i32, message: impl Into<String>, data: Option<Value>) -> Value {
    let mut error = json!({"code":code,"message":message.into()});
    if let Some(data) = data {
        error["data"] = data;
    }
    json!({"jsonrpc":"2.0","id":id,"error":error})
}
fn tool_error_result(text: String) -> Value {
    json!({"content":[{"type":"text","text":text}],"isError":true})
}
/// Text of the MCP SDK's `createToolError`: validation issues use its
/// "Input validation error" wording with dotted paths; other failures keep
/// the thrown message.
fn tool_call_error(name: &str, error: &anyhow::Error) -> Value {
    let issues = error
        .downcast_ref::<NativeToolInputError>()
        .and_then(|input| input.issues.as_ref())
        .and_then(Value::as_array)
        .filter(|issues| !issues.is_empty());
    let Some(issues) = issues else {
        return tool_error_result(error.to_string());
    };
    let lines: Vec<String> = issues
        .iter()
        .map(|issue| {
            let message = issue["message"].as_str().unwrap_or("Invalid input");
            let path = issue["path"].as_array().map(Vec::as_slice).unwrap_or(&[]);
            if path.is_empty() {
                return message.to_owned();
            }
            let mut dotted = String::new();
            for (index, segment) in path.iter().enumerate() {
                let index_segment = segment
                    .as_u64()
                    .map(|n| n.to_string())
                    .or_else(|| {
                        segment
                            .as_str()
                            .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                            .map(str::to_owned)
                    });
                let text = segment
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| segment.to_string());
                match (index, index_segment) {
                    (0, _) => dotted.push_str(&text),
                    (_, Some(number)) => dotted.push_str(&format!("[{number}]")),
                    (_, None) => dotted.push_str(&format!(".{text}")),
                }
            }
            format!("{message} at {dotted}")
        })
        .collect();
    tool_error_result(format!(
        "MCP error -32602: Input validation error: Invalid arguments for tool {name}: {}",
        lines.join("\n")
    ))
}
enum Authority {
    Agent(Arc<Grant>),
    Owner,
}
impl McpService {
    async fn authorize(
        &self,
        caller: Option<&str>,
        authorization: Option<&str>,
    ) -> Result<Authority> {
        if let Some(token) = authorization.and_then(crate::transport::extract_http_bearer_token) {
            if let Some(grant) = self.grants.read().await.get(token).cloned() {
                if grant.cancel.is_cancelled() || caller != Some(grant.agent_id.as_str()) {
                    return Err(anyhow!("Unauthorized"));
                }
                return Ok(Authority::Agent(grant));
            }
        }
        // A run's caller cannot be chosen by query string under owner credentials.
        // Owner tools use owner scope, and run capabilities bind exact agent IDs.
        if caller.is_none()
            && self
                .app()?
                .transport
                .authenticate_bearer(authorization)
                .await
        {
            return Ok(Authority::Owner);
        }
        Err(anyhow!("Unauthorized"))
    }
    /// HTTP header/body validation follows authentication, not the reverse.
    pub async fn http_preflight(
        &self,
        caller_agent_id: Option<&str>,
        authorization: Option<&str>,
    ) -> Option<McpHttpResponse> {
        let app = match self.app() {
            Ok(app) => app,
            Err(_) => return Some(response(503, json!({"error":"Daemon shutting down"}))),
        };
        if app.config.snapshot()["mcpEnabled"] == false {
            return Some(response(
                404,
                json!({"error":"Agent MCP endpoint disabled"}),
            ));
        }
        if self
            .authorize(caller_agent_id, authorization)
            .await
            .is_err()
        {
            return Some(response(401, json!({"error":"Unauthorized"})));
        }
        None
    }
    pub async fn http_request(
        &self,
        method: &str,
        caller_agent_id: Option<&str>,
        authorization: Option<&str>,
        body: Value,
        cancel: CancellationToken,
    ) -> McpHttpResponse {
        self.http_request_streaming(method, caller_agent_id, authorization, body, cancel, None)
            .await
    }
    pub async fn http_request_streaming(
        &self,
        method: &str,
        caller_agent_id: Option<&str>,
        authorization: Option<&str>,
        body: Value,
        cancel: CancellationToken,
        updates: Option<mpsc::Sender<Value>>,
    ) -> McpHttpResponse {
        let app = match self.app() {
            Ok(app) => app,
            Err(_) => return response(503, json!({"error":"Daemon shutting down"})),
        };
        if app.config.snapshot()["mcpEnabled"] == false {
            return response(404, json!({"error":"Agent MCP endpoint disabled"}));
        }
        let authority = match self.authorize(caller_agent_id, authorization).await {
            Ok(auth) => auth,
            Err(_) => return response(401, json!({"error":"Unauthorized"})),
        };
        if method != "POST" {
            return response(
                405,
                rpc_error(Value::Null, -32000, "Method not allowed", None),
            );
        }
        let body = self.rpc(&authority, body, cancel, updates).await;
        McpHttpResponse {
            status: if body
                .as_ref()
                .is_some_and(|v| v.pointer("/error/code") == Some(&json!(-32600)))
            {
                400
            } else if body.is_some() {
                200
            } else {
                202
            },
            body,
        }
    }
    async fn rpc(
        &self,
        authority: &Authority,
        request: Value,
        cancel: CancellationToken,
        updates: Option<mpsc::Sender<Value>>,
    ) -> Option<Value> {
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let method = request.get("method").and_then(Value::as_str);
        if request["jsonrpc"] != "2.0"
            || method.is_none()
            || request
                .get("id")
                .is_some_and(|v| !v.is_string() && !v.is_number() && !v.is_null())
        {
            return Some(rpc_error(id, -32600, "Invalid Request", None));
        }
        let method = method.unwrap();
        if request.get("id").is_none() {
            return None;
        }
        let (caller, policy) = match authority {
            Authority::Agent(grant) => (Some(grant.agent_id.as_str()), grant.policy.as_ref()),
            Authority::Owner => (None, None),
        };
        if let Authority::Agent(grant) = authority {
            if grant.cancel.is_cancelled() {
                return Some(rpc_error(
                    id,
                    -32000,
                    "Agent MCP capability has been revoked",
                    None,
                ));
            }
        }
        let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
        if !params.is_object() {
            return Some(rpc_error(
                id,
                -32602,
                "Invalid params: expected object",
                None,
            ));
        }
        let result: Result<Value> = match method {
            "initialize" => {
                if !params["protocolVersion"].is_string()
                    || !params["capabilities"].is_object()
                    || !params
                        .pointer("/clientInfo/name")
                        .is_some_and(Value::is_string)
                    || !params
                        .pointer("/clientInfo/version")
                        .is_some_and(Value::is_string)
                {
                    return Some(rpc_error(id, -32602, "Invalid initialize params", None));
                }
                let requested = params["protocolVersion"].as_str().unwrap_or("");
                let version = if matches!(requested, "2024-11-05" | "2025-03-26" | "2025-06-18") {
                    requested
                } else {
                    "2025-06-18"
                };
                Ok(
                    json!({"protocolVersion":version,"capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":"agent-mcp","version":"2.0.0"}}),
                )
            }
            "ping" => Ok(json!({})),
            "tools/list" => self.definitions(caller, policy, false).await.map(
                |tools| json!({"tools":tools.into_iter().map(public_schema).collect::<Vec<_>>()}),
            ),
            "tools/call" => {
                let Some(name) = params["name"].as_str() else {
                    return Some(rpc_error(
                        id,
                        -32602,
                        "Invalid params: name is required",
                        None,
                    ));
                };
                // The source McpServer only registers catalog tools; an unknown
                // name is a tool error result, not a JSON-RPC error.
                match self.definitions(caller, policy, false).await {
                    Ok(tools) if tools.iter().any(|tool| tool["name"] == name) => {}
                    Ok(_) => {
                        return Some(
                            json!({"jsonrpc":"2.0","id":id,"result":tool_error_result(format!("MCP error -32602: Tool {name} not found"))}),
                        );
                    }
                    Err(error) => {
                        return Some(
                            json!({"jsonrpc":"2.0","id":id,"result":tool_error_result(error.to_string())}),
                        );
                    }
                }
                let input = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                let token = params.pointer("/_meta/progressToken").cloned();
                let (tool_updates, forwarder) = if let (Some(sender), Some(token)) =
                    (updates, token)
                {
                    let (tx, mut rx) = mpsc::channel::<Value>(32);
                    let c = cancel.clone();
                    let task = tokio::spawn(async move {
                        let mut progress = 0u64;
                        while let Some(update) =
                            tokio::select! {_ = c.cancelled()=>None,v=rx.recv()=>v}
                        {
                            progress += 1;
                            let message = update
                                .get("message")
                                .and_then(Value::as_str)
                                .map(str::to_owned)
                                .unwrap_or_else(|| update.to_string());
                            let notification = json!({"jsonrpc":"2.0","method":"notifications/progress","params":{"progressToken":token,"progress":progress,"message":message,"_meta":{"paseo":{"update":update}}}});
                            if tokio::select! {_ = c.cancelled()=>true,r=sender.send(notification)=>r.is_err()}
                            {
                                break;
                            }
                        }
                    });
                    (Some(tx), Some(task))
                } else {
                    (None, None)
                };
                let operation = match authority {
                    Authority::Agent(g) => g.cancel.child_token(),
                    Authority::Owner => cancel.child_token(),
                };
                let _operation_guard = operation.clone().drop_guard();
                let watch = operation.clone();
                let client_cancel = cancel.clone();
                let watcher = tokio::spawn(async move {
                    tokio::select! {_ = client_cancel.cancelled()=>watch.cancel(),_ = watch.cancelled()=>{}}
                });
                let result = self
                    .execute_bound(caller, policy, name, input, operation.clone(), tool_updates)
                    .await;
                operation.cancel();
                watcher.abort();
                if let Some(task) = forwarder {
                    let _ = task.await;
                }
                // McpServer turns every tool failure, including input validation,
                // into a CallToolResult with isError rather than a JSON-RPC error.
                Ok(result.unwrap_or_else(|error| tool_call_error(name, &error)))
            }
            _ => return Some(rpc_error(id, -32601, "Method not found", None)),
        };
        Some(match result {
            Ok(value) => json!({"jsonrpc":"2.0","id":id,"result":value}),
            Err(error) => {
                if let Some(input) = error.downcast_ref::<NativeToolInputError>() {
                    rpc_error(
                        id,
                        if input.status == 400 { -32602 } else { -32603 },
                        &input.message,
                        Some(json!({"status":input.status,"issues":input.issues})),
                    )
                } else {
                    rpc_error(id, -32603, error.to_string(), None)
                }
            }
        })
    }
}

/// Native replacement for the shipped Node stdio-to-local-socket CLI bridge.
/// Socket creation and grant ownership belong to the provider that exposes it.
#[cfg(unix)]
pub async fn stdio_socket_bridge(socket: &Path) -> Result<()> {
    let stream = tokio::net::UnixStream::connect(socket).await?;
    let (mut read, mut write) = stream.into_split();
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    tokio::try_join!(
        async {
            tokio::io::copy(&mut stdin, &mut write).await?;
            write.shutdown().await
        },
        async {
            tokio::io::copy(&mut read, &mut stdout).await?;
            stdout.flush().await
        }
    )?;
    Ok(())
}
#[cfg(windows)]
pub async fn stdio_socket_bridge(socket: &Path) -> Result<()> {
    let stream = tokio::net::windows::named_pipe::ClientOptions::new().open(socket)?;
    let (mut read, mut write) = tokio::io::split(stream);
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    tokio::try_join!(
        async {
            tokio::io::copy(&mut stdin, &mut write).await?;
            write.shutdown().await
        },
        async {
            tokio::io::copy(&mut read, &mut stdout).await?;
            stdout.flush().await
        }
    )?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rpc_errors_keep_request_identity() {
        let response = rpc_error(
            json!("request-7"),
            -32602,
            "Bad input",
            Some(json!({"status":400,"issues":[]})),
        );
        assert_eq!(response["id"], "request-7");
        assert_eq!(response["error"]["data"]["status"], 400);
        assert!(response.get("result").is_none());
    }
    #[tokio::test]
    async fn capability_binds_caller_and_revocation_cancels_execution() {
        let service = McpService::new(std::sync::Weak::new());
        let cancelled = CancellationToken::new();
        service.grants.write().await.insert(
            "secret".into(),
            Arc::new(Grant {
                agent_id: "agent-a".into(),
                policy: None,
                cancel: cancelled.clone(),
            }),
        );
        assert!(
            service
                .authorize(Some("agent-a"), Some("Bearer secret"))
                .await
                .is_ok()
        );
        assert!(
            service
                .authorize(Some("agent-b"), Some("Bearer secret"))
                .await
                .is_err()
        );
        assert!(
            service
                .authorize(None, Some("Bearer secret"))
                .await
                .is_err()
        );
        service.revoke_agent("agent-a").await;
        assert!(cancelled.is_cancelled());
        assert!(
            service
                .authorize(Some("agent-a"), Some("Bearer secret"))
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn protocol_negotiates_and_does_not_reply_to_notifications() {
        let service = McpService::new(std::sync::Weak::new());
        let cancel = CancellationToken::new();
        let reply=service.rpc(&Authority::Owner,json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}}),cancel.clone(),None).await.unwrap();
        assert_eq!(reply["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(reply["result"]["serverInfo"]["name"], "agent-mcp");
        assert!(
            service
                .rpc(
                    &Authority::Owner,
                    json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
                    cancel.clone(),
                    None
                )
                .await
                .is_none()
        );
        let error = service
            .rpc(
                &Authority::Owner,
                json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{}}),
                cancel.clone(),
                None,
            )
            .await
            .unwrap();
        assert_eq!(error["error"]["code"], -32602);
        let error = service
            .rpc(
                &Authority::Owner,
                json!({"jsonrpc":"2.0","id":"unknown","method":"not/a/method"}),
                cancel,
                None,
            )
            .await
            .unwrap();
        assert_eq!(error["error"]["code"], -32601);
        assert_eq!(error["id"], "unknown");
    }
    #[tokio::test]
    async fn unknown_tool_is_a_tool_error_result() {
        let service = McpService::new(std::sync::Weak::new());
        let reply = service
            .rpc(
                &Authority::Owner,
                json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"no_such_tool","arguments":{}}}),
                CancellationToken::new(),
                None,
            )
            .await
            .unwrap();
        assert!(reply.get("error").is_none());
        assert_eq!(reply["result"]["isError"], true);
        assert_eq!(
            reply["result"]["content"][0]["text"],
            "MCP error -32602: Tool no_such_tool not found"
        );
    }
    #[test]
    fn tool_failures_use_sdk_error_text() {
        let validation = anyhow::Error::from(NativeToolInputError {
            status: 400,
            issues: Some(json!([
                {"path":["cron"],"message":"Invalid cron expression"},
                {"path":["response","updatedPermissions","0"],"message":"Expected object"},
                {"path":[],"message":"Unrecognized key"}
            ])),
            message: "[]".into(),
        });
        let result = tool_call_error("create_schedule", &validation);
        assert_eq!(result["isError"], true);
        assert_eq!(
            result["content"][0]["text"],
            "MCP error -32602: Input validation error: Invalid arguments for tool create_schedule: Invalid cron expression at cron\nExpected object at response.updatedPermissions[0]\nUnrecognized key"
        );
        let handler = anyhow::Error::from(NativeToolInputError {
            status: 400,
            issues: None,
            message: "Schedule not found: s1".into(),
        });
        assert_eq!(
            tool_call_error("pause_schedule", &handler)["content"][0]["text"],
            "Schedule not found: s1"
        );
    }
}
