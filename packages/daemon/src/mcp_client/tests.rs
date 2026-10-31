use std::{
    collections::BTreeMap,
    convert::Infallible,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::State as AxumState,
    http::{Method, Request, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};
use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio::{sync::mpsc, task::JoinHandle};
use tokio_util::sync::CancellationToken;

use super::{McpConnection, schema, sse};

#[derive(Clone)]
struct Seen {
    method: Method,
    headers: axum::http::HeaderMap,
    body: Option<Value>,
}

#[derive(Default)]
struct Server {
    seen: Mutex<Vec<Seen>>,
    pending: Mutex<Option<Value>>,
    cancelled: Mutex<Option<Value>>,
    events: Mutex<Option<mpsc::Sender<Bytes>>>,
    bad_endpoint: bool,
    bad_version: bool,
}

struct Loopback {
    url: String,
    server: Arc<Server>,
    task: JoinHandle<()>,
}

impl Drop for Loopback {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Loopback {
    async fn start(server: Server) -> Self {
        let server = Arc::new(server);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let router = Router::new()
            .route("/mcp", any(handle))
            .route("/sse", any(handle))
            .route("/messages", any(handle))
            .with_state(server.clone());
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        Self { url, server, task }
    }

    async fn connect(&self, kind: &str) -> anyhow::Result<Arc<McpConnection>> {
        let path = if kind == "sse" { "/sse" } else { "/mcp" };
        McpConnection::connect(
            "fixture",
            &json!({"type":kind,"url":format!("{}{path}", self.url),
            "headers":{"Authorization":"Bearer fixture-secret","x-mcp-test":"present"}}),
            std::path::Path::new("."),
            &BTreeMap::new(),
            CancellationToken::new(),
        )
        .await
    }

    async fn wait_for(&self, predicate: impl Fn(&Seen) -> bool) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if self.server.seen.lock().unwrap().iter().any(&predicate) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("expected MCP request did not arrive");
    }
}

fn json_response(message: Value) -> Response {
    let mut response = axum::Json(message).into_response();
    response
        .headers_mut()
        .insert("mcp-session-id", "fixture-session".parse().unwrap());
    response
}

fn event_bytes(message: &Value) -> Bytes {
    Bytes::from(format!("event: message\ndata: {message}\n\n"))
}

fn stream_response(bytes: Bytes) -> Response {
    (
        [("content-type", "Text/Event-Stream; charset=utf-8")],
        Body::from(bytes),
    )
        .into_response()
}

async fn handle(AxumState(server): AxumState<Arc<Server>>, request: Request<Body>) -> Response {
    let (parts, body) = request.into_parts();
    let bytes = to_bytes(body, 1024 * 1024).await.unwrap();
    let body = if bytes.is_empty() {
        None
    } else {
        Some(serde_json::from_slice::<Value>(&bytes).unwrap())
    };
    server.seen.lock().unwrap().push(Seen {
        method: parts.method.clone(),
        headers: parts.headers.clone(),
        body: body.clone(),
    });
    if parts.method == Method::DELETE {
        return StatusCode::NO_CONTENT.into_response();
    }
    if parts.method == Method::GET {
        if parts.uri.path() == "/sse" {
            let (sender, receiver) = mpsc::channel::<Bytes>(32);
            let endpoint = if server.bad_endpoint {
                "http://invalid.example/messages"
            } else {
                "/messages?session=fixture"
            };
            sender
                .try_send(Bytes::from(format!(
                    ": keep-alive\r\nevent: endpoint\r\ndata: {endpoint}\r\n\r\n"
                )))
                .unwrap();
            *server.events.lock().unwrap() = Some(sender);
            let stream =
                tokio_stream::wrappers::ReceiverStream::new(receiver).map(Ok::<_, Infallible>);
            return (
                [("content-type", "text/event-stream")],
                Body::from_stream(stream),
            )
                .into_response();
        }
        if parts
            .headers
            .get("last-event-id")
            .is_some_and(|id| id == "resume-token")
        {
            let reply = server
                .pending
                .lock()
                .unwrap()
                .take()
                .expect("one resume response");
            return stream_response(event_bytes(&reply));
        }
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let message = body.unwrap();
    let method = message.get("method").and_then(Value::as_str).unwrap_or("");
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => {
            json!({"protocolVersion":if server.bad_version { "2099-01-01" } else { "2024-11-05" },
            "capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}})
        }
        "notifications/initialized" => return StatusCode::ACCEPTED.into_response(),
        "notifications/cancelled" => {
            *server.cancelled.lock().unwrap() = Some(message["params"]["requestId"].clone());
            return StatusCode::ACCEPTED.into_response();
        }
        "tools/list" if message["params"]["cursor"] == "page-two" => {
            json!({"tools":[{"name":"wait","inputSchema":{"type":"object"}}]})
        }
        "tools/list" => json!({"tools":[{"name":"echo","inputSchema":{"type":"object"},
            "outputSchema":{"type":"object","properties":{"count":{"type":"integer"}},"required":["count"]}}],"nextCursor":"page-two"}),
        "tools/call" => {
            let name = message["params"]["name"].as_str().unwrap();
            if name == "wait" {
                return StatusCode::ACCEPTED.into_response();
            }
            if name == "rpc_error" {
                return json_response(
                    json!({"jsonrpc":"2.0","id":id,"error":{"code":-32602,"message":"fixture error","data":{"detail":1}}}),
                );
            }
            let result = json!({"content":[{"type":"text","text":"fixture result"},
                {"type":"image","mimeType":"image/png","data":"Zg"},
                {"type":"resource","resource":{"uri":"fixture://document","text":"hello"}},
                {"type":"resource_link","name":"document","uri":"fixture://document"}],
                "structuredContent":{"count":if message["params"]["arguments"]["invalid"] == true { json!("wrong") } else { json!(7) }},
                "_meta":{"fixture":true},"extension":{"retained":true}});
            let reply = json!({"jsonrpc":"2.0","id":id,"result":result});
            if name == "resume" {
                *server.pending.lock().unwrap() = Some(reply);
                return stream_response(Bytes::from_static(
                    b"id: resume-token\nretry: 1\ndata:\n\n",
                ));
            }
            if name == "late" {
                let stale = server.cancelled.lock().unwrap().clone().unwrap();
                return json_response(
                    json!([{"jsonrpc":"2.0","id":stale,"result":{"content":[{"type":"text","text":"stale"}]}},reply]),
                );
            }
            if parts.uri.path() != "/messages" {
                let ping = json!({"jsonrpc":"2.0","id":"server-ping","method":"ping"});
                let mut stream = event_bytes(&ping).to_vec();
                stream.extend_from_slice(&event_bytes(&reply));
                return stream_response(Bytes::from(stream));
            }
            result
        }
        "" => return StatusCode::ACCEPTED.into_response(), // Client response to ping.
        _ => return StatusCode::BAD_REQUEST.into_response(),
    };
    let reply = json!({"jsonrpc":"2.0","id":id,"result":result});
    if parts.uri.path() == "/messages" {
        server
            .events
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .try_send(event_bytes(&reply))
            .unwrap();
        StatusCode::ACCEPTED.into_response()
    } else {
        json_response(reply)
    }
}

#[tokio::test]
async fn http_negotiates_paginates_correlates_sse_and_deletes_session() {
    let fixture = Loopback::start(Server::default()).await;
    let client = fixture.connect("http").await.unwrap();
    let tools = client.list_tools(CancellationToken::new()).await.unwrap();
    assert_eq!(tools.len(), 2);
    let result = client
        .call_tool("echo", json!({}), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(result["structuredContent"]["count"], 7);
    assert_eq!(result["extension"]["retained"], true);
    assert_eq!(result["content"][1]["data"], "Zg");
    fixture
        .wait_for(|record| {
            record
                .body
                .as_ref()
                .is_some_and(|body| body["id"] == "server-ping" && body["result"].is_object())
        })
        .await;
    assert!(
        client
            .call_tool("echo", json!({"invalid":true}), CancellationToken::new())
            .await
            .is_err()
    );
    let error = client
        .call_tool("rpc_error", json!({}), CancellationToken::new())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("-32602"));
    client.close().await.unwrap();
    client.close().await.unwrap();
    let seen = fixture.server.seen.lock().unwrap();
    assert_eq!(
        seen.iter()
            .filter(|record| record.method == Method::DELETE)
            .count(),
        1
    );
    for record in seen.iter() {
        assert_eq!(record.headers["authorization"], "Bearer fixture-secret");
        assert_eq!(record.headers["x-mcp-test"], "present");
        if record
            .body
            .as_ref()
            .is_some_and(|body| body["method"] == "initialize")
        {
            assert!(record.headers.get("mcp-protocol-version").is_none());
        } else {
            assert_eq!(record.headers["mcp-protocol-version"], "2024-11-05");
            assert_eq!(record.headers["mcp-session-id"], "fixture-session");
        }
    }
}

#[tokio::test]
async fn cancellation_removes_pending_id_and_ignores_late_response() {
    let fixture = Loopback::start(Server::default()).await;
    let client = fixture.connect("http").await.unwrap();
    let cancel = CancellationToken::new();
    let call_client = client.clone();
    let call_cancel = cancel.clone();
    let call =
        tokio::spawn(async move { call_client.call_tool("wait", json!({}), call_cancel).await });
    fixture
        .wait_for(|record| {
            record
                .body
                .as_ref()
                .is_some_and(|body| body["params"]["name"] == "wait")
        })
        .await;
    cancel.cancel();
    assert!(call.await.unwrap().is_err());
    fixture
        .wait_for(|record| {
            record
                .body
                .as_ref()
                .is_some_and(|body| body["method"] == "notifications/cancelled")
        })
        .await;
    let result = client
        .call_tool("late", json!({}), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(result["content"][0]["text"], "fixture result");
    client.close().await.unwrap();
    let seen = fixture.server.seen.lock().unwrap();
    assert_eq!(
        seen.iter()
            .filter(|record| record
                .body
                .as_ref()
                .is_some_and(|body| body["params"]["name"] == "wait"))
            .count(),
        1
    );
}

#[tokio::test]
async fn dropping_request_future_notifies_server_and_drop_cleans_session() {
    let fixture = Loopback::start(Server::default()).await;
    let client = fixture.connect("http").await.unwrap();
    let call_client = client.clone();
    let call = tokio::spawn(async move {
        call_client
            .call_tool("wait", json!({}), CancellationToken::new())
            .await
    });
    fixture
        .wait_for(|record| {
            record
                .body
                .as_ref()
                .is_some_and(|body| body["params"]["name"] == "wait")
        })
        .await;
    call.abort();
    assert!(call.await.unwrap_err().is_cancelled());
    fixture
        .wait_for(|record| {
            record
                .body
                .as_ref()
                .is_some_and(|body| body["method"] == "notifications/cancelled")
        })
        .await;
    drop(client);
    fixture
        .wait_for(|record| record.method == Method::DELETE)
        .await;
}

#[tokio::test]
async fn request_deadline_sends_cancellation() {
    let fixture = Loopback::start(Server::default()).await;
    let client = fixture.connect("http").await.unwrap();
    let result = client
        .request(
            "tools/call",
            json!({"name":"wait","arguments":{}}),
            CancellationToken::new(),
            tokio::time::Instant::now() + Duration::from_millis(50),
        )
        .await;
    assert!(result.unwrap_err().to_string().contains("timed out"));
    fixture
        .wait_for(|record| {
            record.body.as_ref().is_some_and(|body| {
                body["method"] == "notifications/cancelled"
                    && body["params"]["reason"] == "Request timed out"
            })
        })
        .await;
    client.close().await.unwrap();
}

#[tokio::test]
async fn resumed_response_uses_get_last_event_id_without_reposting_tool() {
    let fixture = Loopback::start(Server::default()).await;
    let client = fixture.connect("http").await.unwrap();
    let result = client
        .call_tool("resume", json!({}), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(result["content"][0]["text"], "fixture result");
    client.close().await.unwrap();
    let seen = fixture.server.seen.lock().unwrap();
    assert_eq!(
        seen.iter()
            .filter(|record| record
                .body
                .as_ref()
                .is_some_and(|body| body["params"]["name"] == "resume"))
            .count(),
        1
    );
    assert!(seen.iter().any(|record| {
        record.method == Method::GET
            && record
                .headers
                .get("last-event-id")
                .is_some_and(|id| id == "resume-token")
    }));
}

#[tokio::test]
async fn legacy_sse_uses_announced_endpoint_and_closes_event_stream() {
    let fixture = Loopback::start(Server::default()).await;
    let client = fixture.connect("sse").await.unwrap();
    assert_eq!(
        client
            .list_tools(CancellationToken::new())
            .await
            .unwrap()
            .len(),
        2
    );
    let result = client
        .call_tool("echo", json!({}), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(result["structuredContent"]["count"], 7);
    client.close().await.unwrap();
    assert!(
        !fixture
            .server
            .seen
            .lock()
            .unwrap()
            .iter()
            .any(|record| record.method == Method::DELETE)
    );
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if fixture
                .server
                .events
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .is_closed()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn invalid_negotiation_and_cross_origin_sse_endpoint_are_rejected() {
    let fixture = Loopback::start(Server {
        bad_version: true,
        ..Server::default()
    })
    .await;
    assert!(fixture.connect("http").await.is_err());
    assert!(
        fixture
            .server
            .seen
            .lock()
            .unwrap()
            .iter()
            .any(|record| record.method == Method::DELETE)
    );
    let fixture = Loopback::start(Server {
        bad_endpoint: true,
        ..Server::default()
    })
    .await;
    assert!(fixture.connect("sse").await.is_err());
    assert!(
        !fixture
            .server
            .seen
            .lock()
            .unwrap()
            .iter()
            .any(|record| record.method == Method::POST)
    );
}

#[cfg(unix)]
#[tokio::test]
async fn stdio_owns_process_env_cwd_out_of_order_replies_and_cancellation() {
    // A real, deliberately tiny POSIX fixture; production never inserts a shell
    // around a configured MCP executable. No Node/helper runtime is needed.
    let home = tempfile::tempdir().unwrap();
    let script = r#"
IFS= read -r initialize || exit 1
printf '%s\n' '{"jsonrpc":"2.0","id":0,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}}}'
IFS= read -r initialized || exit 1
IFS= read -r list || exit 1
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"tools":[{"name":"fixture","inputSchema":{"type":"object"}}]}}'
IFS= read -r first || exit 1
IFS= read -r second || exit 1
printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"second"}]}}'
printf '{"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"%s:%s"}]}}\n' "$PWD" "$MCP_FIXTURE_ENV"
IFS= read -r waiting || exit 1
printf ready > waiting
IFS= read -r cancelled || exit 1
case "$cancelled" in *'"method":"notifications/cancelled"'*'"requestId":4'*) printf cancelled > cancelled ;; *) exit 2 ;; esac
IFS= read -r eof
"#;
    let config = json!({"type":"stdio","command":"/bin/sh","args":["-c",script],"env":{"MCP_FIXTURE_ENV":"overlay"}});
    let client = McpConnection::connect(
        "stdio-fixture",
        &config,
        home.path(),
        &BTreeMap::from([("MCP_FIXTURE_ENV".into(), "base".into())]),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    client.list_tools(CancellationToken::new()).await.unwrap();
    let (first, second) = tokio::join!(
        client.call_tool("fixture", json!({}), CancellationToken::new()),
        client.call_tool("fixture", json!({}), CancellationToken::new())
    );
    assert_eq!(
        first.unwrap()["content"][0]["text"],
        format!("{}:overlay", home.path().display())
    );
    assert_eq!(second.unwrap()["content"][0]["text"], "second");
    let cancel = CancellationToken::new();
    let call_client = client.clone();
    let call_cancel = cancel.clone();
    let call = tokio::spawn(async move {
        call_client
            .call_tool("fixture", json!({}), call_cancel)
            .await
    });
    wait_file(home.path().join("waiting")).await;
    cancel.cancel();
    assert!(call.await.unwrap().is_err());
    wait_file(home.path().join("cancelled")).await;
    client.close().await.unwrap();
    if let super::Transport::Stdio(process) = client.transport.as_ref() {
        tokio::time::timeout(Duration::from_secs(1), process.wait())
            .await
            .unwrap()
            .unwrap();
    } else {
        unreachable!()
    }
}

#[cfg(unix)]
async fn wait_file(path: std::path::PathBuf) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[test]
fn sse_chunk_independent_lines_multiline_data_and_bounds() {
    let mut parser = sse::Parser::default();
    let mut events = Vec::new();
    for byte in "\u{feff}: comment\r\nevent: message\rdata: {\ndata: \"text\":\"é\"}\r\nid: cursor\nretry: 13\n\n".bytes() {
        if let Some(event) = parser.push(byte).unwrap() { events.push(event); }
    }
    let event = events.pop().unwrap();
    assert_eq!(event.kind, "message");
    assert_eq!(event.data, "{\n\"text\":\"é\"}");
    assert_eq!(event.id.as_deref(), Some("cursor"));
    assert_eq!(event.retry_ms, Some(13));
    for _ in 0..super::MAX_MESSAGE {
        parser.push(b'x').unwrap();
    }
    assert!(parser.push(b'x').is_err());
}

#[test]
fn result_validation_preserves_extensions_and_rejects_invalid_content() {
    let mut structured = json!({"structuredContent":{"ok":true},"extension":1});
    schema::call_result(&mut structured).unwrap();
    assert_eq!(structured["content"], json!([]));
    assert_eq!(structured["extension"], 1);
    for content in [
        json!({"type":"text","text":7}),
        json!({"type":"image","mimeType":"image/png","data":"!"}),
        json!({"type":"resource","resource":{"uri":"fixture://x","blob":"a"}}),
    ] {
        assert!(schema::call_result(&mut json!({"content":[content]})).is_err());
    }
    assert!(
        schema::rpc(
            &json!({"jsonrpc":"2.0","id":1,"result":{},"error":{"code":-1,"message":"bad"}})
        )
        .is_err()
    );
}
