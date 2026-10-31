use super::{Handler, Peer};
use crate::{state::AppState, transport::TransportService};
use anyhow::{Context, Result, bail};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, Weak},
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpListener,
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
enum Target {
    Tcp { host: String, port: u16 },
    Socket(PathBuf),
    Pipe(String),
}

fn parse(value: &str) -> Result<Target> {
    if let Some(path) = value.strip_prefix("pipe://") {
        return Ok(Target::Pipe(path.to_owned()));
    }
    if value.starts_with(r"\\.\pipe\") {
        return Ok(Target::Pipe(value.to_owned()));
    }
    if let Some(path) = value.strip_prefix("unix://") {
        return Ok(Target::Socket(path.into()));
    }
    if value.starts_with('/') || value.starts_with('~') {
        return Ok(Target::Socket(value.into()));
    }
    if value.as_bytes().get(1) == Some(&b':')
        && matches!(value.as_bytes().get(2), Some(b'\\' | b'/'))
    {
        bail!("Windows drive paths are not listen targets");
    }
    if value.trim().bytes().all(|c| c.is_ascii_digit()) && !value.trim().is_empty() {
        return Ok(Target::Tcp {
            host: "127.0.0.1".into(),
            port: value.trim().parse().context("Invalid listen port")?,
        });
    }
    let (host, port) = value.rsplit_once(':').context("Invalid listen string")?;
    Ok(Target::Tcp {
        host: if host.is_empty() {
            "127.0.0.1".into()
        } else {
            host.trim_matches(['[', ']']).to_owned()
        },
        port: port.parse().context("Invalid listen port")?,
    })
}

enum Bound {
    Tcp(TcpListener),
    #[cfg(unix)]
    Unix {
        listener: tokio::net::UnixListener,
        ownership: SocketOwnership,
    },
    #[cfg(windows)]
    Pipe {
        listener: tokio::net::windows::named_pipe::NamedPipeServer,
        path: String,
    },
}

#[cfg(unix)]
struct SocketOwnership {
    path: PathBuf,
    device: u64,
    inode: u64,
}
#[cfg(unix)]
impl Drop for SocketOwnership {
    fn drop(&mut self) {
        use std::os::unix::fs::MetadataExt;
        if let Ok(metadata) = std::fs::symlink_metadata(&self.path) {
            if metadata.dev() == self.device && metadata.ino() == self.inode {
                if let Err(error) = std::fs::remove_file(&self.path) {
                    tracing::warn!(%error, path = %self.path.display(), "Failed to remove owned HTTP socket");
                }
            }
        }
    }
}

async fn bind_target(target: Target, home: &std::path::Path) -> Result<(Bound, Value)> {
    match target {
        Target::Tcp { host, port } => {
            let listener = TcpListener::bind((host.as_str(), port)).await?;
            let port = listener.local_addr()?.port();
            Ok((
                Bound::Tcp(listener),
                json!({"type":"tcp", "host":host, "port":port}),
            ))
        }
        Target::Socket(path) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                let path = if path == std::path::Path::new("~") {
                    home.to_path_buf()
                } else if let Ok(rest) = path.strip_prefix("~") {
                    home.join(rest)
                } else {
                    path
                };
                // Do not unlink an existing path: it may be another process's live listener.
                let listener = tokio::net::UnixListener::bind(&path)?;
                let metadata = tokio::fs::symlink_metadata(&path).await?;
                let ownership = SocketOwnership {
                    path: path.clone(),
                    device: metadata.dev(),
                    inode: metadata.ino(),
                };
                Ok((
                    Bound::Unix {
                        listener,
                        ownership,
                    },
                    json!({"type":"socket", "path":path}),
                ))
            }
            #[cfg(not(unix))]
            {
                let _ = (path, home);
                bail!("Unix sockets are unsupported on this platform")
            }
        }
        Target::Pipe(path) => {
            #[cfg(windows)]
            {
                let listener = tokio::net::windows::named_pipe::ServerOptions::new()
                    .first_pipe_instance(true)
                    .create(&path)?;
                let metadata = json!({"type":"pipe", "path":path});
                Ok((Bound::Pipe { listener, path }, metadata))
            }
            #[cfg(not(windows))]
            {
                let _ = path;
                bail!("Windows named pipes are unsupported on this platform")
            }
        }
    }
}

pub struct HttpListener {
    listeners: Vec<(Bound, bool)>,
    handler: Arc<Handler>,
    cancel: CancellationToken,
}

pub async fn bind(
    app: Weak<AppState>,
    transport: Arc<TransportService>,
    cancel: CancellationToken,
) -> Result<HttpListener> {
    let config = transport.config_snapshot();
    let state = app.upgrade().context("Daemon shut down")?;
    let target = parse(
        config["listen"]
            .as_str()
            .context("Missing daemon listen configuration")?,
    )?;
    let (bound, listen) = bind_target(target, &state.user_home).await?;
    let mut listeners = vec![(bound, false)];
    if let Some(proxy_listen) = config["serviceProxy"]["listen"]
        .as_str()
        .filter(|s| !s.is_empty())
    {
        let (bound, _) = bind_target(parse(proxy_listen)?, &state.user_home).await?;
        listeners.push((bound, true));
    }
    drop(state);
    Ok(HttpListener {
        listeners,
        handler: Arc::new(Handler {
            app,
            transport,
            listen,
            shutdown: cancel.clone(),
        }),
        cancel,
    })
}

impl HttpListener {
    pub fn listen(&self) -> Value {
        self.handler.listen.clone()
    }
    pub fn local_http_endpoint(&self) -> Option<String> {
        let listen = &self.handler.listen;
        if listen["type"] != "tcp" {
            return None;
        }
        let host = match listen["host"].as_str()? {
            "0.0.0.0" => "127.0.0.1",
            "::" | "[::]" => "::1",
            host => host,
        };
        Some(format!(
            "http://{}:{}",
            super::policy::authority_host(host),
            listen["port"].as_u64()?
        ))
    }
    pub async fn start(self) -> Result<()> {
        let mut listeners = JoinSet::new();
        for (bound, proxy_only) in self.listeners {
            listeners.spawn(accept(
                bound,
                self.handler.clone(),
                proxy_only,
                self.cancel.clone(),
            ));
        }
        let mut result = Ok(());
        while let Some(joined) = listeners.join_next().await {
            match joined {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    result = Err(error);
                    self.cancel.cancel();
                }
                Err(error) => {
                    result = Err(error.into());
                    self.cancel.cancel();
                }
            }
        }
        result
    }
}

enum Accepted {
    Tcp(tokio::net::TcpStream, std::net::IpAddr),
    #[cfg(unix)]
    Unix(tokio::net::UnixStream),
    #[cfg(windows)]
    Pipe(tokio::net::windows::named_pipe::NamedPipeServer),
}

impl Bound {
    async fn accept(&mut self) -> Result<Accepted> {
        match self {
            Bound::Tcp(listener) => {
                let (socket, peer) = listener.accept().await?;
                Ok(Accepted::Tcp(socket, peer.ip()))
            }
            #[cfg(unix)]
            Bound::Unix {
                listener,
                ownership,
            } => {
                let _ = ownership;
                Ok(Accepted::Unix(listener.accept().await?.0))
            }
            #[cfg(windows)]
            Bound::Pipe { listener, path } => {
                listener.connect().await?;
                let next = tokio::net::windows::named_pipe::ServerOptions::new().create(path)?;
                Ok(Accepted::Pipe(std::mem::replace(listener, next)))
            }
        }
    }
}

async fn accept(
    mut bound: Bound,
    handler: Arc<Handler>,
    proxy_only: bool,
    cancel: CancellationToken,
) -> Result<()> {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            Some(_) = connections.join_next(), if !connections.is_empty() => {},
            result = bound.accept() => {
                match result? {
                    Accepted::Tcp(socket, ip) => { connections.spawn(serve(socket, Peer { ip: Some(ip), tcp: true }, handler.clone(), proxy_only, cancel.clone())); }
                    #[cfg(unix)]
                    Accepted::Unix(socket) => { connections.spawn(serve(socket, Peer { ip: None, tcp: false }, handler.clone(), proxy_only, cancel.clone())); }
                    #[cfg(windows)]
                    Accepted::Pipe(socket) => { connections.spawn(serve(socket, Peer { ip: None, tcp: false }, handler.clone(), proxy_only, cancel.clone())); }
                }
            }
        }
    }
    while connections.join_next().await.is_some() {}
    Ok(())
}

async fn serve<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    socket: S,
    peer: Peer,
    handler: Arc<Handler>,
    proxy_only: bool,
    cancel: CancellationToken,
) {
    let connection_cancel = cancel.child_token();
    let _guard = connection_cancel.clone().drop_guard();
    let request_cancel = connection_cancel.clone();
    let service = hyper::service::service_fn(move |request| {
        let handler = handler.clone();
        let cancel = request_cancel.clone();
        async move {
            Ok::<_, std::convert::Infallible>(
                handler.handle(request, peer, proxy_only, cancel).await,
            )
        }
    });
    let connection = hyper::server::conn::http1::Builder::new()
        .serve_connection(TokioIo::new(socket), service)
        .with_upgrades();
    tokio::select! {
        _ = cancel.cancelled() => {},
        result = connection => if let Err(error) = result { tracing::debug!(%error, "HTTP connection closed"); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_native_targets_and_rejects_invalid_ports() {
        assert!(matches!(
            parse("[::1]:0").unwrap(),
            Target::Tcp { port: 0, .. }
        ));
        assert!(matches!(
            parse("6767").unwrap(),
            Target::Tcp { port: 6767, .. }
        ));
        assert!(matches!(
            parse("unix:///tmp/paseo.sock").unwrap(),
            Target::Socket(_)
        ));
        assert!(matches!(parse(r"\\.\pipe\paseo").unwrap(), Target::Pipe(_)));
        assert!(parse("localhost:65536").is_err());
        assert!(parse(r"C:\paseo.sock").is_err());
    }
    #[tokio::test]
    async fn loopback_bind_reports_actual_ephemeral_port() {
        let (bound, metadata) = bind_target(
            parse("127.0.0.1:0").unwrap(),
            std::path::Path::new("/unused"),
        )
        .await
        .unwrap();
        let Bound::Tcp(listener) = bound else {
            panic!("expected TCP");
        };
        assert_ne!(metadata["port"], 0);
        let client = tokio::net::TcpStream::connect(listener.local_addr().unwrap());
        let (client, accepted) = tokio::join!(client, listener.accept());
        assert!(client.is_ok());
        assert!(accepted.unwrap().1.ip().is_loopback());
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn unix_socket_cleanup_does_not_unlink_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("daemon.sock");
        let (listener, _) = bind_target(Target::Socket(path.clone()), directory.path())
            .await
            .unwrap();
        assert!(
            bind_target(Target::Socket(path.clone()), directory.path())
                .await
                .is_err()
        );
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, "replacement").unwrap();
        drop(listener);
        assert_eq!(std::fs::read_to_string(path).unwrap(), "replacement");
    }
}
