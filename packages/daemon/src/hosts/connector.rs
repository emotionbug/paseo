use super::{
    HostError, HostFrame,
    target::{Endpoint, Target, ssh_args},
};
use crate::{
    process::{CommandSpec, ManagedProcess},
    relay_crypto::{Frame, SecureChannel, SecurityMode},
};
use anyhow::{Result, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::{OwnedSemaphorePermit, Semaphore, mpsc},
};
use tokio_tungstenite::tungstenite::{
    Message, client::IntoClientRequest, protocol::WebSocketConfig,
};
use tokio_util::sync::CancellationToken;

trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}
trait SocketIo: futures_util::Sink<Message, Error = tokio_tungstenite::tungstenite::Error>
    + futures_util::Stream<Item = std::result::Result<Message, tokio_tungstenite::tungstenite::Error>>
    + Unpin
    + Send
{
}
impl<T> SocketIo for T where
    T: futures_util::Sink<Message, Error = tokio_tungstenite::tungstenite::Error>
        + futures_util::Stream<
            Item = std::result::Result<Message, tokio_tungstenite::tungstenite::Error>,
        > + Unpin
        + Send
{
}
type Socket = Box<dyn SocketIo>;
struct Queued {
    frame: HostFrame,
    _global: OwnedSemaphorePermit,
    _local: OwnedSemaphorePermit,
}
#[derive(Clone)]
pub(crate) struct Sender {
    tx: mpsc::Sender<Queued>,
    global: Arc<Semaphore>,
    local: Arc<Semaphore>,
    cancel: CancellationToken,
}
impl Sender {
    pub async fn send(&self, frame: HostFrame) -> Result<()> {
        let size = frame
            .len()
            .try_into()
            .map_err(|_| HostError::new("resource_limit", "Host socket buffer limit reached"))?;
        let global = self
            .global
            .clone()
            .try_acquire_many_owned(size)
            .map_err(|_| HostError::new("resource_limit", "Connected host buffer limit reached"))?;
        self.send_reserved(frame, global).await
    }
    pub async fn send_reserved(
        &self,
        frame: HostFrame,
        mut global: OwnedSemaphorePermit,
    ) -> Result<()> {
        let size: u32 = frame
            .len()
            .try_into()
            .map_err(|_| HostError::new("resource_limit", "Host socket buffer limit reached"))?;
        if frame.len() > global.num_permits() {
            global.merge(
                self.global
                    .clone()
                    .try_acquire_many_owned((frame.len() - global.num_permits()).try_into()?)
                    .map_err(|_| {
                        HostError::new("resource_limit", "Connected host buffer limit reached")
                    })?,
            );
        } else {
            let excess = global.num_permits() - frame.len();
            drop(global.split(excess));
        }
        let local = self
            .local
            .clone()
            .try_acquire_many_owned(size)
            .map_err(|_| HostError::new("resource_limit", "Host socket buffer limit reached"))?;
        tokio::select! { _=self.cancel.cancelled()=>bail!("Host transport closed"), result=self.tx.send(Queued{frame,_global:global,_local:local})=>result.map_err(|_|anyhow!("Host transport closed")) }
    }
    pub async fn json(&self, message: serde_json::Value) -> Result<()> {
        self.send(HostFrame::Text(serde_json::to_string(&message)?))
            .await
    }
}
pub(crate) struct Connection {
    pub sender: Sender,
    pub incoming: mpsc::Receiver<Result<HostFrame>>,
    pub cancel: CancellationToken,
    pub task: tokio::task::JoinHandle<()>,
    pub security: Option<SecurityMode>,
}
impl Connection {
    pub async fn connect(
        target: &Target,
        minimum: SecurityMode,
        global: Arc<Semaphore>,
        parent: CancellationToken,
    ) -> Result<Self> {
        let cancel = parent.child_token();
        let mut child = None;
        let result =
            tokio::time::timeout(Duration::from_secs(10), open(target, &cancel, &mut child)).await;
        let mut socket = match result {
            Ok(Ok(socket)) => socket,
            other => {
                let detail = if let Some(process) = child.as_ref() {
                    let _ = process.terminate(Duration::from_millis(300)).await;
                    process.stderr_tail()
                } else {
                    String::new()
                };
                if ssh_auth_failure(&detail) {
                    return Err(HostError::new("authentication_failed","SSH authentication or host verification failed. Check the SSH configuration on this daemon.").into());
                }
                return match other {
                Ok(Err(error))=>{
                    if error.downcast_ref::<tokio_tungstenite::tungstenite::Error>().is_some_and(|error|
                        matches!(error,tokio_tungstenite::tungstenite::Error::Http(response) if matches!(response.status().as_u16(),401|403))) {
                        Err(HostError::new("authentication_failed","Host rejected the credentials. Replace the connection credentials.").into())
                    }else{Err(error)}
                },
                _=>Err(anyhow!("Host connection timed out"))
            };
            }
        };
        let mut crypto = target
            .security
            .as_ref()
            .map(|(key, floor)| {
                SecureChannel::initiator(
                    crate::relay_crypto::import_public_key(key)?,
                    minimum.max(*floor),
                )
            })
            .transpose()?;
        if let Some(channel) = crypto.as_mut() {
            let handshake = async {
                let mut retry = tokio::time::interval(Duration::from_secs(1));
                loop {
                    tokio::select! {
                        _=cancel.cancelled()=>bail!("Host connection cancelled"),
                        _=retry.tick()=>{if let Some(hello)=channel.hello(){socket.send(Message::Text(hello.into())).await?;}},
                        message=socket.next()=>{
                            match message.ok_or_else(||anyhow!("Host security transport closed"))?? {
                                Message::Ping(bytes)=>socket.send(Message::Pong(bytes)).await?,
                                Message::Text(text)=>{let update=channel.receive(Frame::Text(text.to_string()))?;if let Some(frame)=update.outbound{socket.send(to_message(frame)).await?;}if update.opened.is_some(){break;}},
                                Message::Binary(bytes)=>{let update=channel.receive(Frame::Binary(bytes.to_vec()))?;if let Some(frame)=update.outbound{socket.send(to_message(frame)).await?;}if update.opened.is_some(){break;}},
                                Message::Close(_)=>bail!("Host security transport closed"), _=>{}
                            }
                        }
                    }
                }
                Ok::<_, anyhow::Error>(())
            };
            if let Err(error) = tokio::time::timeout(Duration::from_secs(15), handshake)
                .await
                .map_err(anyhow::Error::from)
                .and_then(|v| v)
            {
                cancel.cancel();
                if let Some(child) = child {
                    let _ = child.terminate(Duration::from_millis(300)).await;
                }
                return Err(error);
            }
        }
        let security = crypto.as_ref().and_then(SecureChannel::security_mode);
        let (tx, mut rx) = mpsc::channel::<Queued>(256);
        let (incoming_tx, incoming) = mpsc::channel(1);
        let sender = Sender {
            tx,
            global,
            local: Arc::new(Semaphore::new(128 * 1024 * 1024)),
            cancel: cancel.clone(),
        };
        let task_cancel = cancel.clone();
        let task = tokio::spawn(async move {
            let run = async {
                loop {
                    tokio::select! {biased;
                        _=task_cancel.cancelled()=>break,
                        item=rx.recv()=>{
                            let Some(item)=item else{break};
                            if let HostFrame::Close{code,reason}=&item.frame {socket.send(Message::Close(Some(tokio_tungstenite::tungstenite::protocol::CloseFrame{code:(*code).into(),reason:reason.clone().into()}))).await?;break;}
                            let size=item.frame.len();
                            let Queued{frame,_global,_local}=item;
                            if let HostFrame::Flush(ack)=frame {
                                let result=socket.flush().await.map_err(anyhow::Error::from);
                                let failure=result.as_ref().err().map(ToString::to_string);
                                let _=ack.send(result);
                                if let Some(failure)=failure{return Err(anyhow!(failure));}
                                continue;
                            }
                            let wire=if let Some(channel)=crypto.as_mut(){
                                let plain=match frame{HostFrame::Text(text)=>Frame::Text(text),HostFrame::Binary(bytes)=>Frame::Binary(bytes.into()),HostFrame::Close{..}|HostFrame::Flush(_)=>unreachable!()};
                                to_message(channel.encode(plain)?)
                            }else{match frame{HostFrame::Text(text)=>Message::Text(text.into()),HostFrame::Binary(bytes)=>Message::Binary(bytes),HostFrame::Close{..}|HostFrame::Flush(_)=>unreachable!()}};
                            // Cipher expansion is reserved too, until the write completes.
                            let extra=wire.len().saturating_sub(size);
                            let extra_global=if extra>0{Some(_global.semaphore().clone().try_acquire_many_owned(extra.try_into()?).map_err(|_|HostError::new("resource_limit","Connected host buffer limit reached"))?)}else{None};
                            let extra_local=if extra>0{Some(_local.semaphore().clone().try_acquire_many_owned(extra.try_into()?).map_err(|_|HostError::new("resource_limit","Host socket buffer limit reached"))?)}else{None};
                            socket.send(wire).await?;drop((_global,_local,extra_global,extra_local));
                        },
                        message=socket.next()=>{
                            let message=message.ok_or_else(||anyhow!("Host transport closed"))??;
                            let wire=match message {
                                Message::Text(text)=>Some(HostFrame::Text(text.to_string())),Message::Binary(bytes)=>Some(HostFrame::Binary(bytes)),
                                Message::Ping(bytes)=>{socket.send(Message::Pong(bytes)).await?;None},
                                Message::Close(close)=>{if close.is_some_and(|c|c.code==tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Library(4401)){return Err(HostError::new("authentication_failed","Host rejected the credentials. Replace the connection credentials.").into());}bail!("Host transport closed")},_=>None
                            };
                            if let Some(wire)=wire {
                                let plain=if let Some(channel)=crypto.as_mut(){
                                    let encrypted=match wire{HostFrame::Text(text)=>Frame::Text(text),HostFrame::Binary(bytes)=>Frame::Binary(bytes.into()),HostFrame::Close{..}|HostFrame::Flush(_)=>unreachable!()};
                                    let update=channel.receive(encrypted)?;if let Some(reply)=update.outbound{socket.send(to_message(reply)).await?;}
                                    update.message.map(|frame|match frame{Frame::Text(text)=>HostFrame::Text(text),Frame::Binary(bytes)=>HostFrame::Binary(bytes.into())})
                                }else{Some(wire)};
                                if let Some(frame)=plain {tokio::select!{_=task_cancel.cancelled()=>break,result=incoming_tx.send(Ok(frame))=>{result.map_err(|_|anyhow!("Host consumer closed"))?;}}}
                            }
                        }
                    }
                }
                Ok(())
            };
            let result: Result<()> =
                tokio::select! {_=task_cancel.cancelled()=>Ok(()),result=run=>result};
            if let Some(channel) = crypto.as_mut() {
                channel.close();
            }
            if let Some(process) = child {
                let _ = process.terminate(Duration::from_millis(500)).await;
            }
            if let Err(error) = result {
                tokio::select! {_=task_cancel.cancelled()=>{},_=incoming_tx.send(Err(error))=>{}}
            }
            task_cancel.cancel();
        });
        Ok(Self {
            sender,
            incoming,
            cancel,
            task,
            security,
        })
    }
}
fn to_message(frame: Frame) -> Message {
    match frame {
        Frame::Text(text) => Message::Text(text.into()),
        Frame::Binary(bytes) => Message::Binary(bytes.into()),
    }
}
fn ssh_auth_failure(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    [
        "permission denied",
        "authentication failed",
        "host key verification failed",
        "remote host identification has changed",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}
async fn open(
    target: &Target,
    cancel: &CancellationToken,
    child: &mut Option<Arc<ManagedProcess>>,
) -> Result<Socket> {
    let config = WebSocketConfig::default()
        .max_message_size(Some(if target.security.is_some() {
            32 * 1024 * 1024
        } else {
            100 * 1024 * 1024
        }))
        .max_frame_size(Some(100 * 1024 * 1024));
    let (url, stream): (String, Option<Box<dyn Stream>>) = match &target.endpoint {
        Endpoint::Socket { url, ipc: None } => (url.clone(), None),
        Endpoint::Socket {
            url,
            ipc: Some(path),
        } => {
            let path = if let Some(rest) = path.strip_prefix("~/") {
                dirs::home_dir()
                    .ok_or_else(|| anyhow!("User home unavailable"))?
                    .join(rest)
            } else {
                path.into()
            };
            #[cfg(unix)]
            let stream: Box<dyn Stream> = Box::new(tokio::net::UnixStream::connect(&path).await?);
            #[cfg(windows)]
            let stream: Box<dyn Stream> =
                Box::new(tokio::net::windows::named_pipe::ClientOptions::new().open(&path)?);
            (url.clone(), Some(stream))
        }
        Endpoint::Ssh {
            host,
            port,
            daemon_port,
        } => {
            let (process, mut stdout) = ManagedProcess::spawn(CommandSpec {
                program: "ssh".into(),
                args: ssh_args(host, *port, *daemon_port),
                cwd: None,
                env: Default::default(),
                clear_env: false,
            })
            .await?;
            *child = Some(process.clone());
            let (stream, pump) = tokio::io::duplex(65536);
            let (mut reader, mut writer) = tokio::io::split(pump);
            let stop = cancel.clone();
            tokio::spawn(async move {
                let upload = async {
                    let mut buffer = [0u8; 16384];
                    loop {
                        let n = reader.read(&mut buffer).await?;
                        if n == 0 {
                            break;
                        }
                        process.write_bytes(&buffer[..n]).await?;
                    }
                    process.close_stdin().await?;
                    Ok::<_, anyhow::Error>(())
                };
                let download = async {
                    tokio::io::copy(&mut stdout, &mut writer).await?;
                    writer.shutdown().await?;
                    Ok::<_, anyhow::Error>(())
                };
                tokio::select! {_=stop.cancelled()=>{},_=async{tokio::try_join!(upload,download)}=>{}}
            });
            ("ws://127.0.0.1/ws".into(), Some(Box::new(stream)))
        }
    };
    let mut request = url.into_client_request()?;
    if target.security.is_none() {
        if let Some(password) = &target.password {
            if password.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
                request
                    .headers_mut()
                    .insert("authorization", format!("Bearer {password}").parse()?);
            }
        }
    }
    if let Some(stream) = stream {
        Ok(Box::new(
            tokio_tungstenite::client_async_with_config(request, stream, Some(config))
                .await?
                .0,
        ))
    } else {
        Ok(Box::new(
            tokio_tungstenite::connect_async_with_config(request, Some(config), false)
                .await?
                .0,
        ))
    }
}
