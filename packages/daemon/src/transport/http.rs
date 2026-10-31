//! Native daemon HTTP boundary. Service-host classification precedes every daemon route.
mod listener;
mod policy;
mod proxy;
pub use listener::{HttpListener, bind};

use crate::{
    state::AppState,
    transport::{Admission, TransportService},
};
use axum::body::Body;
use bytes::Bytes;
use http::{HeaderValue, Method, Request, Response, StatusCode};
use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use std::{
    net::IpAddr,
    sync::{Arc, Weak},
};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        handshake::derive_accept_key,
        protocol::{CloseFrame, Message, Role, WebSocketConfig, frame::coding::CloseCode},
    },
};
use tokio_util::{io::ReaderStream, sync::CancellationToken};

#[derive(Clone, Copy)]
struct Peer {
    ip: Option<IpAddr>,
    tcp: bool,
}
struct Handler {
    app: Weak<AppState>,
    transport: Arc<TransportService>,
    listen: Value,
    shutdown: CancellationToken,
}

fn plain(status: StatusCode, text: &'static str) -> Response<Body> {
    let mut response = Response::new(Body::from(text));
    *response.status_mut() = status;
    response.headers_mut().insert(
        "content-type",
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}
fn json_response(status: StatusCode, value: Value) -> Response<Body> {
    let mut response = Response::new(Body::from(value.to_string()));
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert("content-type", HeaderValue::from_static("application/json"));
    response
}
fn error(status: StatusCode, message: &'static str) -> Response<Body> {
    json_response(status, json!({"error":message}))
}
fn empty(status: StatusCode) -> Response<Body> {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = status;
    response
}
fn header<'a>(request: &'a Request<Incoming>, name: &str) -> Option<&'a str> {
    request.headers().get(name)?.to_str().ok()
}
fn query(request: &Request<Incoming>, key: &str) -> Option<String> {
    url::form_urlencoded::parse(request.uri().query().unwrap_or("").as_bytes())
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.into_owned())
}
fn download_token<B>(request: &Request<B>) -> Option<String> {
    let mut tokens = url::form_urlencoded::parse(request.uri().query().unwrap_or("").as_bytes())
        .filter(|(name, _)| name == "token");
    let (_, token) = tokens.next()?;
    if tokens.next().is_some() || token.trim().is_empty() {
        return None;
    }
    Some(token.into_owned())
}
async fn body_json(request: Request<Incoming>) -> Result<Value, Response<Body>> {
    // express.json() defaults to 100 KiB and only parses JSON media types.
    let json_type = request
        .headers()
        .get("content-type")
        .and_then(|h| h.to_str().ok())
        .is_some_and(|h| {
            h.split(';')
                .next()
                .is_some_and(|s| s.trim().eq_ignore_ascii_case("application/json"))
        });
    if !json_type {
        return Ok(Value::Null);
    }
    let bytes = http_body_util::Limited::new(request.into_body(), 100 * 1024)
        .collect()
        .await
        .map_err(|failure| {
            if failure.is::<http_body_util::LengthLimitError>() {
                error(StatusCode::PAYLOAD_TOO_LARGE, "Request body too large")
            } else {
                error(StatusCode::BAD_REQUEST, "Invalid request body")
            }
        })?
        .to_bytes();
    if bytes.is_empty() {
        return Ok(json!({}));
    }
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "Invalid JSON"))?;
    if !value.is_object() && !value.is_array() {
        return Err(error(StatusCode::BAD_REQUEST, "Invalid JSON"));
    }
    Ok(value)
}

impl Handler {
    async fn handle(
        self: Arc<Self>,
        request: Request<Incoming>,
        peer: Peer,
        proxy_only: bool,
        cancel: CancellationToken,
    ) -> Response<Body> {
        let Some(app) = self.app.upgrade() else {
            return error(StatusCode::SERVICE_UNAVAILABLE, "Server shutting down");
        };
        let host = header(&request, "host").unwrap_or("");
        let classification = app.terminals.classify_service_host(host).await;
        // Unknown service names MUST NOT inherit daemon credentials or daemon routes.
        match classification["type"].as_str() {
            Some("registered-service") => {
                let config = self.transport.config_snapshot();
                let protocol = forwarded_protocol(&request, peer, &config, proxy_only).to_owned();
                return proxy::forward(
                    request,
                    &classification["route"],
                    peer,
                    self.shutdown.child_token(),
                    &protocol,
                )
                .await;
            }
            Some("known-service-miss") => return plain(StatusCode::NOT_FOUND, "404 Not Found"),
            Some("daemon") if !proxy_only => {}
            _ => return plain(StatusCode::NOT_FOUND, "404 Not Found"),
        }
        let config = self.transport.config_snapshot();
        if peer.tcp && !policy::host_allowed(header(&request, "host"), &config["hostnames"]) {
            return error(StatusCode::FORBIDDEN, "Invalid Host header");
        }
        let origin = header(&request, "origin").map(str::to_owned);
        let mut response = if request.method() == Method::OPTIONS {
            empty(StatusCode::NO_CONTENT)
        } else {
            self.route(request, peer, app, &config, cancel).await
        };
        policy::cors(
            response.headers_mut(),
            origin.as_deref(),
            &config,
            &self.listen,
        );
        response
    }

    async fn route(
        &self,
        request: Request<Incoming>,
        peer: Peer,
        app: Arc<AppState>,
        config: &Value,
        cancel: CancellationToken,
    ) -> Response<Body> {
        let path = request.uri().path();
        if path == "/ws" {
            return self.websocket(request, config).await;
        }
        if request.method() == Method::POST && path == "/api/terminal-activity" {
            if !policy::activity_peer(peer.ip) {
                return error(StatusCode::FORBIDDEN, "Forbidden");
            }
            let body = match body_json(request).await {
                Ok(value) => value,
                Err(response) => return response,
            };
            let Some(id) = body["terminalId"].as_str().filter(|s| !s.is_empty()) else {
                return error(StatusCode::BAD_REQUEST, "Invalid terminal activity report");
            };
            let Some(token) = body["token"].as_str().filter(|s| !s.is_empty()) else {
                return error(StatusCode::BAD_REQUEST, "Invalid terminal activity report");
            };
            let Some(event @ ("running" | "idle" | "needs-input")) = body["state"].as_str() else {
                return error(StatusCode::BAD_REQUEST, "Invalid terminal activity report");
            };
            return match app.terminals.report_activity(id, token, event).await {
                Ok(true) => empty(StatusCode::NO_CONTENT),
                Ok(false) => error(StatusCode::FORBIDDEN, "Forbidden"),
                Err(_) => error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to update terminal activity",
                ),
            };
        }
        let bearer_free = matches!(path, "/api/health" | "/api/files/download" | "/mcp/agents");
        if !bearer_free {
            let authorized = if path == "/api/status" {
                self.transport
                    .authenticate_status(header(&request, "authorization"))
                    .await
            } else {
                self.transport
                    .authenticate_bearer(header(&request, "authorization"))
                    .await
            };
            if !authorized {
                return error(StatusCode::UNAUTHORIZED, "Unauthorized");
            }
        }
        let is_get = request.method() == Method::GET || request.method() == Method::HEAD;
        if is_get && path == "/api/health" {
            return json_response(
                StatusCode::OK,
                json!({"status":"ok", "timestamp":chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)}),
            );
        }
        if is_get && path == "/api/status" {
            let identity = self.transport.identity();
            let listen = if self.listen["type"] == "tcp" {
                format!(
                    "{}:{}",
                    policy::authority_host(self.listen["host"].as_str().unwrap_or("")),
                    self.listen["port"]
                )
            } else {
                self.listen["path"].as_str().unwrap_or("").to_owned()
            };
            return json_response(
                StatusCode::OK,
                json!({"status":"server_info", "serverId":identity["serverId"], "hostname":hostname::get().ok().and_then(|s| s.into_string().ok()).unwrap_or_default(), "version":env!("CARGO_PKG_VERSION"), "listen":listen}),
            );
        }
        if is_get && path == "/api/files/download" {
            let Some(token) = download_token(&request) else {
                return error(StatusCode::BAD_REQUEST, "Missing download token");
            };
            return match app.files.open_download(token.trim()).await {
                Ok(Some(download)) => {
                    let mime = match HeaderValue::from_str(&download.mime_type) {
                        Ok(v) => v,
                        Err(_) => {
                            return error(StatusCode::INTERNAL_SERVER_ERROR, "Failed to read file");
                        }
                    };
                    let disposition = match HeaderValue::from_str(&download.content_disposition) {
                        Ok(v) => v,
                        Err(_) => {
                            return error(StatusCode::INTERNAL_SERVER_ERROR, "Failed to read file");
                        }
                    };
                    let mut response =
                        Response::new(Body::from_stream(ReaderStream::new(download.file)));
                    response.headers_mut().insert("content-type", mime);
                    response
                        .headers_mut()
                        .insert("content-disposition", disposition);
                    if let Ok(length) = HeaderValue::from_str(&download.size.to_string()) {
                        response.headers_mut().insert("content-length", length);
                    }
                    response
                }
                Ok(None) => error(StatusCode::FORBIDDEN, "Invalid or expired token"),
                Err(_) => error(StatusCode::NOT_FOUND, "File not found"),
            };
        }
        if path == "/mcp/agents"
            && matches!(
                *request.method(),
                Method::POST | Method::GET | Method::DELETE
            )
        {
            return self.mcp(request, app, cancel).await;
        }
        plain(StatusCode::NOT_FOUND, "404 Not Found")
    }

    async fn websocket(&self, mut request: Request<Incoming>, config: &Value) -> Response<Body> {
        if !policy::valid_ws_protocols(request.headers()) {
            return error(StatusCode::BAD_REQUEST, "Invalid WebSocket protocol");
        }
        if header(&request, "host").is_some()
            && !policy::host_allowed(header(&request, "host"), &config["hostnames"])
        {
            return error(StatusCode::FORBIDDEN, "Host not allowed");
        }
        if let Some(origin) = header(&request, "origin") {
            if !policy::allowed_origin(origin, config, &self.listen)
                && !header(&request, "host").is_some_and(|host| policy::same_origin(origin, host))
            {
                return error(StatusCode::FORBIDDEN, "Origin not allowed");
            }
        }
        let valid_connection = header(&request, "connection").is_some_and(|value| {
            value
                .split(',')
                .any(|s| s.trim().eq_ignore_ascii_case("upgrade"))
        });
        let key = header(&request, "sec-websocket-key");
        use base64::Engine;
        if request.method() != Method::GET
            || !valid_connection
            || !header(&request, "upgrade").is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
            || header(&request, "sec-websocket-version") != Some("13")
            || !key.is_some_and(|v| {
                base64::engine::general_purpose::STANDARD
                    .decode(v)
                    .is_ok_and(|v| v.len() == 16)
            })
        {
            return error(StatusCode::BAD_REQUEST, "Invalid WebSocket upgrade");
        }
        let accept = derive_accept_key(key.unwrap().as_bytes());
        let credential = policy::ws_bearer(request.headers());
        // Original attachAuthenticatedSocket: a wrong header password still upgrades,
        // then closes 4401 "Incorrect password" so clients stop retrying it.
        let mut rejected = false;
        let admission = if let Some(credential) = credential {
            rejected = !self.transport.authenticate_bearer(Some(&credential)).await;
            Some(Admission::owner())
        } else {
            None
        };
        let protocol = policy::ws_protocol(
            request.headers(),
            config["auth"]["password"]
                .as_str()
                .is_some_and(|s| !s.is_empty()),
        );
        let upgraded = hyper::upgrade::on(&mut request);
        let transport = self.transport.clone();
        let cancel = self.shutdown.child_token();
        tokio::spawn(async move {
            let operation = async {
                let socket = TokioIo::new(upgraded.await?);
                let config = WebSocketConfig::default()
                    .max_message_size(Some(100 * 1024 * 1024))
                    .max_frame_size(Some(100 * 1024 * 1024));
                let mut socket =
                    WebSocketStream::from_raw_socket(socket, Role::Server, Some(config)).await;
                if rejected {
                    use futures_util::SinkExt;
                    socket
                        .send(Message::Close(Some(CloseFrame {
                            code: CloseCode::Library(4401),
                            reason: "Incorrect password".into(),
                        })))
                        .await?;
                    return Ok(());
                }
                transport
                    .run_socket(socket, admission, cancel.clone())
                    .await
            };
            tokio::select! { _ = cancel.cancelled() => {}, result = operation => if let Err(error) = result { tracing::debug!(%error, "WebSocket closed"); } }
        });
        let mut response = empty(StatusCode::SWITCHING_PROTOCOLS);
        response
            .headers_mut()
            .insert("connection", HeaderValue::from_static("Upgrade"));
        response
            .headers_mut()
            .insert("upgrade", HeaderValue::from_static("websocket"));
        response.headers_mut().insert(
            "sec-websocket-accept",
            HeaderValue::from_str(&accept).expect("base64 accept value"),
        );
        if let Some(protocol) = protocol.and_then(|p| HeaderValue::from_str(&p).ok()) {
            response
                .headers_mut()
                .insert("sec-websocket-protocol", protocol);
        }
        response
    }

    async fn mcp(
        &self,
        request: Request<Incoming>,
        app: Arc<AppState>,
        cancel: CancellationToken,
    ) -> Response<Body> {
        let method = request.method().as_str().to_owned();
        let caller =
            query(&request, "callerAgentId").or_else(|| query(&request, "callerAgentId[]"));
        let authorization = header(&request, "authorization").map(str::to_owned);
        if let Some(response) = app
            .mcp
            .http_preflight(caller.as_deref(), authorization.as_deref())
            .await
        {
            let status =
                StatusCode::from_u16(response.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            return match response.body {
                Some(body) => json_response(status, body),
                None => empty(status),
            };
        }
        let sse = method == "POST";
        if sse {
            if let Some(response) = mcp_media_error(request.headers()) {
                return response;
            }
        }
        let body = match body_json(request).await {
            Ok(value) => value,
            Err(response) => return response,
        };
        if !sse {
            let token = cancel.child_token();
            let _guard = token.clone().drop_guard();
            let response = app
                .mcp
                .http_request(
                    &method,
                    caller.as_deref(),
                    authorization.as_deref(),
                    body,
                    token,
                )
                .await;
            let status =
                StatusCode::from_u16(response.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            return match response.body {
                Some(body) => json_response(status, body),
                None => empty(status),
            };
        }
        let token = cancel.child_token();
        let (updates_tx, mut updates_rx) = tokio::sync::mpsc::channel::<Value>(32);
        let (events_tx, mut events_rx) = tokio::sync::mpsc::channel::<McpEvent>(32);
        let service_token = token.clone();
        tokio::spawn(async move {
            let request = app.mcp.http_request_streaming(
                &method,
                caller.as_deref(),
                authorization.as_deref(),
                body,
                service_token.clone(),
                Some(updates_tx),
            );
            tokio::pin!(request);
            loop {
                tokio::select! {
                    _ = service_token.cancelled() => break,
                    update = updates_rx.recv(), if !updates_rx.is_closed() || !updates_rx.is_empty() => {
                        if let Some(update) = update { if events_tx.send(McpEvent::Update(update)).await.is_err() { break; } }
                    }
                    response = &mut request => {
                        while let Ok(update) = updates_rx.try_recv() { if events_tx.send(McpEvent::Update(update)).await.is_err() { return; } }
                        let _ = events_tx.send(McpEvent::Final(response.status, response.body)).await;
                        break;
                    }
                }
            }
        });
        let guard = token.drop_guard();
        let Some(first) = events_rx.recv().await else {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal MCP server error",
            );
        };
        if let McpEvent::Final(status, body) = first {
            let status = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            if status != StatusCode::OK {
                return body.map_or_else(|| empty(status), |body| json_response(status, body));
            }
            return match body {
                Some(body) => sse_response(Body::from(sse_event(&body))),
                None => empty(status),
            };
        }
        let stream = futures_util::stream::unfold(
            (Some(first), events_rx, guard),
            |(first, mut receiver, guard)| async move {
                let event = match first {
                    Some(event) => Some(event),
                    None => receiver.recv().await,
                }?;
                let body = match event {
                    McpEvent::Update(body) | McpEvent::Final(_, Some(body)) => body,
                    McpEvent::Final(_, None) => return None,
                };
                Some((
                    Ok::<_, std::convert::Infallible>(Bytes::from(sse_event(&body))),
                    (None, receiver, guard),
                ))
            },
        );
        sse_response(Body::from_stream(stream))
    }
}

fn mcp_media_error(headers: &http::HeaderMap) -> Option<Response<Body>> {
    let accept = headers
        .get("accept")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let accepts = |media: &str| {
        accept.split(',').any(|part| {
            part.split(';')
                .next()
                .is_some_and(|part| part.trim().eq_ignore_ascii_case(media))
        })
    };
    let (status, message) = if !accepts("application/json") || !accepts("text/event-stream") {
        (
            StatusCode::NOT_ACCEPTABLE,
            "Not Acceptable: Client must accept both application/json and text/event-stream",
        )
    } else if !headers
        .get("content-type")
        .and_then(|h| h.to_str().ok())
        .is_some_and(|part| {
            part.split(';')
                .next()
                .is_some_and(|part| part.trim().eq_ignore_ascii_case("application/json"))
        })
    {
        (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Unsupported Media Type: Content-Type must be application/json",
        )
    } else {
        return None;
    };
    Some(json_response(
        status,
        json!({"jsonrpc":"2.0","error":{"code":-32000,"message":message},"id":null}),
    ))
}

enum McpEvent {
    Update(Value),
    Final(u16, Option<Value>),
}
fn sse_event(value: &Value) -> String {
    format!("event: message\ndata: {value}\n\n")
}
fn sse_response(body: Body) -> Response<Body> {
    let mut response = Response::new(body);
    response.headers_mut().insert(
        "content-type",
        HeaderValue::from_static("text/event-stream"),
    );
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-cache"));
    response
}

fn forwarded_protocol<'a>(
    request: &'a Request<Incoming>,
    peer: Peer,
    config: &Value,
    proxy_only: bool,
) -> &'a str {
    let trusted = proxy_only
        || config["trustedProxies"].as_bool() == Some(true)
        || config["trustedProxies"].as_array().is_some_and(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .any(|value| peer.ip.is_some_and(|ip| proxy_address_matches(ip, value)))
        });
    if trusted {
        header(request, "x-forwarded-proto")
            .and_then(|v| v.split(',').next())
            .map(str::trim)
            .unwrap_or("http")
    } else {
        "http"
    }
}
fn proxy_address_matches(ip: IpAddr, range: &str) -> bool {
    let ip = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        ip => ip,
    };
    match range {
        "loopback" => ip.is_loopback(),
        "linklocal" => match ip {
            IpAddr::V4(v4) => v4.is_link_local(),
            IpAddr::V6(v6) => (v6.segments()[0] & 0xffc0) == 0xfe80,
        },
        "uniquelocal" => match ip {
            IpAddr::V4(v4) => v4.is_private(),
            IpAddr::V6(v6) => (v6.segments()[0] & 0xfe00) == 0xfc00,
        },
        _ => {
            let (address, bits) = range
                .split_once('/')
                .map_or((range, None), |(a, b)| (a, Some(b)));
            let Ok(address) = address.parse::<IpAddr>() else {
                return false;
            };
            match (ip, address) {
                (IpAddr::V4(ip), IpAddr::V4(address)) => {
                    let Some(bits) = bits
                        .map_or(Some(32), |b| b.parse::<u32>().ok())
                        .filter(|b| *b <= 32)
                    else {
                        return false;
                    };
                    let mask = u32::MAX.checked_shl(32 - bits).unwrap_or(0);
                    u32::from(ip) & mask == u32::from(address) & mask
                }
                (IpAddr::V6(ip), IpAddr::V6(address)) => {
                    let Some(bits) = bits
                        .map_or(Some(128), |b| b.parse::<u32>().ok())
                        .filter(|b| *b <= 128)
                    else {
                        return false;
                    };
                    let mask = u128::MAX.checked_shl(128 - bits).unwrap_or(0);
                    u128::from(ip) & mask == u128::from(address) & mask
                }
                _ => false,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn download_tokens_must_be_a_single_nonempty_query_value() {
        for query in ["", "?token=", "?token=one&token=two", "?token[]=one"] {
            let request = Request::builder()
                .uri(format!("/api/files/download{query}"))
                .body(())
                .unwrap();
            assert!(download_token(&request).is_none());
        }
        let request = Request::builder()
            .uri("/api/files/download?token=capability")
            .body(())
            .unwrap();
        assert_eq!(download_token(&request).as_deref(), Some("capability"));
    }
    #[test]
    fn mcp_media_negotiation_requires_both_streamable_types() {
        let mut headers = http::HeaderMap::new();
        assert_eq!(
            mcp_media_error(&headers).unwrap().status(),
            StatusCode::NOT_ACCEPTABLE
        );
        headers.insert(
            "accept",
            HeaderValue::from_static("application/json, text/event-stream"),
        );
        assert_eq!(
            mcp_media_error(&headers).unwrap().status(),
            StatusCode::UNSUPPORTED_MEDIA_TYPE
        );
        headers.insert(
            "content-type",
            HeaderValue::from_static("application/json; charset=utf-8"),
        );
        assert!(mcp_media_error(&headers).is_none());
    }
    #[test]
    fn trusted_proxy_ranges_respect_address_family_and_prefix() {
        assert!(proxy_address_matches(
            "::ffff:127.0.0.1".parse().unwrap(),
            "loopback"
        ));
        assert!(proxy_address_matches(
            "192.168.1.2".parse().unwrap(),
            "192.168.0.0/16"
        ));
        assert!(!proxy_address_matches(
            "192.169.1.2".parse().unwrap(),
            "192.168.0.0/16"
        ));
        assert!(!proxy_address_matches("::1".parse().unwrap(), "0.0.0.0/0"));
        assert!(!proxy_address_matches(
            "127.0.0.1".parse().unwrap(),
            "127.0.0.1/33"
        ));
    }
}
