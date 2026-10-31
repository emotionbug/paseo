use super::{Peer, plain};
use axum::body::Body;
use http::{HeaderMap, HeaderValue, Request, Response, StatusCode};
use hyper::body::Incoming;
use hyper_util::rt::TokioIo;
use serde_json::Value;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;

struct ProxyBody {
    incoming: Incoming,
    _guard: tokio_util::sync::DropGuard,
}
impl hyper::body::Body for ProxyBody {
    type Data = bytes::Bytes;
    type Error = hyper::Error;
    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
        hyper::body::Body::poll_frame(std::pin::Pin::new(&mut self.incoming), cx)
    }
    fn is_end_stream(&self) -> bool {
        hyper::body::Body::is_end_stream(&self.incoming)
    }
    fn size_hint(&self) -> hyper::body::SizeHint {
        hyper::body::Body::size_hint(&self.incoming)
    }
}

fn strip_hop_headers(headers: &mut HeaderMap) {
    for name in [
        "connection",
        "transfer-encoding",
        "keep-alive",
        "upgrade",
        "proxy-connection",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
    ] {
        headers.remove(name);
    }
}

fn forwarded_headers(
    headers: &mut HeaderMap,
    peer: Peer,
    route: &Value,
    upgrade: bool,
    protocol: &str,
) {
    let host = headers.get("host").cloned().unwrap_or_else(|| {
        HeaderValue::from_str(route["hostname"].as_str().unwrap_or("localhost"))
            .unwrap_or(HeaderValue::from_static("localhost"))
    });
    let upgrade_value = headers.get("upgrade").cloned();
    strip_hop_headers(headers);
    let address = peer
        .ip
        .map(|ip| ip.to_string())
        .unwrap_or_else(|| "127.0.0.1".to_owned());
    if let Ok(value) = HeaderValue::from_str(&address) {
        headers.insert("x-forwarded-for", value);
    }
    if let Ok(value) = HeaderValue::from_str(protocol) {
        headers.insert("x-forwarded-proto", value);
    }
    if let Ok(host_text) = host.to_str() {
        if let Some((_, port)) = host_text.rsplit_once(':') {
            if port.parse::<u16>().is_ok_and(|port| port > 0) {
                if let Ok(value) = HeaderValue::from_str(port) {
                    headers.insert("x-forwarded-port", value);
                }
            }
        }
    }
    headers.insert("x-forwarded-host", host);
    if upgrade {
        headers.insert("connection", HeaderValue::from_static("Upgrade"));
        headers.insert(
            "upgrade",
            upgrade_value.unwrap_or(HeaderValue::from_static("websocket")),
        );
    }
}

fn route_port(value: &Value) -> Option<u16> {
    let port = value.as_f64()?;
    if port.is_finite() && port.fract() == 0.0 && (1.0..=65535.0).contains(&port) {
        Some(port as u16)
    } else {
        None
    }
}

pub(super) async fn forward(
    mut request: Request<Incoming>,
    route: &Value,
    peer: Peer,
    cancel: CancellationToken,
    protocol: &str,
) -> Response<Body> {
    let cancel = cancel.child_token();
    let guard = cancel.clone().drop_guard();
    let Some(port) = route_port(&route["port"]) else {
        return plain(StatusCode::BAD_GATEWAY, "502 Bad Gateway");
    };
    let upgrade = request.headers().contains_key("upgrade");
    let downstream = upgrade.then(|| hyper::upgrade::on(&mut request));
    forwarded_headers(
        request.headers_mut(),
        peer,
        route,
        upgrade,
        if upgrade { "http" } else { protocol },
    );
    // Never resolve a service-controlled hostname: upstreams are loopback only.
    let addresses = [
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
        SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), port),
    ];
    let stream = tokio::select! {
        _ = cancel.cancelled() => return plain(StatusCode::SERVICE_UNAVAILABLE, "Server shutting down"),
        result = TcpStream::connect(&addresses[..]) => match result {
            Ok(stream) => stream,
            Err(_) => return plain(StatusCode::BAD_GATEWAY, "502 Bad Gateway"),
        }
    };
    let (mut sender, connection) =
        match hyper::client::conn::http1::handshake(TokioIo::new(stream)).await {
            Ok(value) => value,
            Err(_) => return plain(StatusCode::BAD_GATEWAY, "502 Bad Gateway"),
        };
    let driver_cancel = cancel.clone();
    tokio::spawn(async move {
        tokio::select! { _ = driver_cancel.cancelled() => {}, _ = connection.with_upgrades() => {} }
    });
    let mut response = tokio::select! {
        _ = cancel.cancelled() => return plain(StatusCode::SERVICE_UNAVAILABLE, "Server shutting down"),
        response = sender.send_request(request) => match response {
            Ok(response) => response,
            Err(_) => return plain(StatusCode::BAD_GATEWAY, "502 Bad Gateway"),
        }
    };
    if response.status() == StatusCode::SWITCHING_PROTOCOLS && upgrade {
        let upstream = hyper::upgrade::on(&mut response);
        tokio::spawn(async move {
            let _guard = guard;
            let bridge = async {
                let (downstream, upstream) = tokio::try_join!(downstream.unwrap(), upstream)?;
                let mut downstream = TokioIo::new(downstream);
                let mut upstream = TokioIo::new(upstream);
                tokio::io::copy_bidirectional(&mut downstream, &mut upstream).await?;
                Ok::<_, anyhow::Error>(())
            };
            tokio::select! { _ = cancel.cancelled() => {}, result = bridge => {
                if let Err(error) = result { tracing::debug!(%error, "Service upgrade closed"); }
            } }
        });
        response.map(Body::new)
    } else {
        strip_hop_headers(response.headers_mut());
        response.map(|incoming| {
            Body::new(ProxyBody {
                incoming,
                _guard: guard,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn service_ports_accept_integral_json_floats_only() {
        assert_eq!(route_port(&json!(4321.0)), Some(4321));
        assert_eq!(route_port(&json!(65535)), Some(65535));
        for port in [
            json!(0),
            json!(-1),
            json!(65536),
            json!(4321.5),
            json!("4321"),
            Value::Null,
        ] {
            assert_eq!(route_port(&port), None);
        }
    }
    #[test]
    fn forwarding_preserves_authority_not_spoofed_peer() {
        let mut headers = HeaderMap::new();
        headers.insert("host", HeaderValue::from_static("dev--app.localhost:6767"));
        headers.insert("x-forwarded-for", HeaderValue::from_static("10.0.0.1"));
        headers.insert("x-forwarded-port", HeaderValue::from_static("443"));
        headers.insert("connection", HeaderValue::from_static("keep-alive"));
        forwarded_headers(
            &mut headers,
            Peer {
                ip: Some("127.0.0.1".parse().unwrap()),
                tcp: true,
            },
            &json!({"hostname":"dev--app.localhost"}),
            false,
            "http",
        );
        assert_eq!(headers["x-forwarded-host"], "dev--app.localhost:6767");
        assert_eq!(headers["x-forwarded-port"], "6767");
        assert_eq!(headers["x-forwarded-for"], "127.0.0.1");
        assert!(!headers.contains_key("connection"));
    }

    #[tokio::test]
    async fn loopback_proxy_preserves_http_status_body_and_authority() {
        use http_body_util::BodyExt;
        let upstream = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_port = upstream.local_addr().unwrap().port();
        let upstream_task = tokio::spawn(async move {
            let (socket, _) = upstream.accept().await.unwrap();
            let service = hyper::service::service_fn(|request: Request<Incoming>| async move {
                assert_eq!(
                    request.uri().path_and_query().unwrap().as_str(),
                    "/asset?q=1"
                );
                assert_eq!(
                    request.headers()["x-forwarded-host"],
                    "dev--sample.localhost:6767"
                );
                assert_eq!(request.headers()["x-forwarded-port"], "6767");
                assert_eq!(request.headers()["x-forwarded-for"], "127.0.0.1");
                let mut response =
                    Response::new(Body::from(bytes::Bytes::from_static(b"\x00\xffnative\n")));
                *response.status_mut() = StatusCode::PARTIAL_CONTENT;
                response.headers_mut().insert(
                    "content-type",
                    HeaderValue::from_static("application/octet-stream"),
                );
                Ok::<_, std::convert::Infallible>(response)
            });
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(socket), service)
                .await;
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let cancel = CancellationToken::new();
        let proxy_cancel = cancel.clone();
        let proxy_task = tokio::spawn(async move {
            let (socket, peer) = listener.accept().await.unwrap();
            let service = hyper::service::service_fn(move |request| {
                let cancel = proxy_cancel.clone();
                async move {
                    let response = forward(
                        request,
                        &json!({"hostname":"dev--sample.localhost","port":upstream_port}),
                        Peer {
                            ip: Some(peer.ip()),
                            tcp: true,
                        },
                        cancel,
                        "http",
                    )
                    .await;
                    Ok::<_, std::convert::Infallible>(response)
                }
            });
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(socket), service)
                .with_upgrades()
                .await;
        });
        let socket = TcpStream::connect(address).await.unwrap();
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(socket))
            .await
            .unwrap();
        let client_task = tokio::spawn(connection);
        let request = Request::builder()
            .uri("/asset?q=1")
            .header("host", "dev--sample.localhost:6767")
            .header("x-forwarded-for", "198.51.100.1")
            .body(Body::empty())
            .unwrap();
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            sender.send_request(request),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            response.headers()["content-type"],
            "application/octet-stream"
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(body.as_ref(), b"\x00\xffnative\n");
        drop(sender);
        cancel.cancel();
        client_task.abort();
        proxy_task.abort();
        upstream_task.abort();
    }

    #[tokio::test]
    async fn websocket_upgrade_keeps_binary_frames_and_connection_lifetime() {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};
        let upstream = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = upstream.local_addr().unwrap().port();
        let upstream_task = tokio::spawn(async move {
            let (socket, _) = upstream.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(socket).await.unwrap();
            socket
                .send(Message::Binary(bytes::Bytes::from_static(
                    b"\0\xffupstream",
                )))
                .await
                .unwrap();
            assert_eq!(
                socket.next().await.unwrap().unwrap().into_data().as_ref(),
                b"\0client"
            );
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let cancel = CancellationToken::new();
        let proxy_cancel = cancel.clone();
        let proxy_task = tokio::spawn(async move {
            let (socket, peer) = listener.accept().await.unwrap();
            let service = hyper::service::service_fn(move |request| {
                let cancel = proxy_cancel.clone();
                async move {
                    Ok::<_, std::convert::Infallible>(
                        forward(
                            request,
                            &json!({"hostname":"dev--sample.localhost","port":port}),
                            Peer {
                                ip: Some(peer.ip()),
                                tcp: true,
                            },
                            cancel,
                            "http",
                        )
                        .await,
                    )
                }
            });
            hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(socket), service)
                .with_upgrades()
                .await
                .unwrap();
        });
        let mut request = format!("ws://{address}/socket")
            .into_client_request()
            .unwrap();
        request.headers_mut().insert(
            "host",
            HeaderValue::from_static("dev--sample.localhost:6767"),
        );
        let (mut socket, response) = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            tokio_tungstenite::connect_async(request),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
        let message = tokio::time::timeout(std::time::Duration::from_secs(5), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(message.into_data().as_ref(), b"\0\xffupstream");
        socket
            .send(Message::Binary(bytes::Bytes::from_static(b"\0client")))
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), upstream_task)
            .await
            .unwrap()
            .unwrap();
        cancel.cancel();
        proxy_task.await.unwrap();
    }
}
