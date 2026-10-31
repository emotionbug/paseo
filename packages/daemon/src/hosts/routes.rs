use super::*;
use crate::transport::{OutboundFrame, OwnedSubscription};
use sha2::{Digest, Sha256};
struct InputFrame {
    frame: HostFrame,
    permit: tokio::sync::OwnedSemaphorePermit,
}

pub(super) struct Route {
    pub registration: String,
    source: String,
    scope: String,
    input: mpsc::Sender<InputFrame>,
    cancel: CancellationToken,
    budget: Arc<Semaphore>,
    failure: Mutex<Option<(String, String)>>,
    owner: Option<OwnedSubscription>,
}
impl Route {
    pub fn stop(&self, code: &str, message: &str) {
        let mut failure = self.failure.lock().unwrap();
        if failure.is_none() {
            *failure = Some((code.into(), message.into()));
        }
        self.cancel.cancel();
    }
    async fn send(&self, frame: HostFrame) -> Result<()> {
        ensure!(
            !self.cancel.is_cancelled(),
            HostError::new("host_offline", "Host is offline")
        );
        let permit = self
            .budget
            .clone()
            .try_acquire_many_owned(frame.len().try_into()?)
            .map_err(|_| HostError::new("resource_limit", "Connected host buffer limit reached"))?;
        self.input
            .try_send(InputFrame { frame, permit })
            .map_err(|_| HostError::new("resource_limit", "Host input buffer limit reached").into())
    }
}
struct Channel {
    id: String,
    route: Arc<Route>,
    output: mpsc::Receiver<HostFrame>,
}
impl HostsService {
    pub async fn detach_source(&self, source: &str) {
        let routes: Vec<_> = self
            .inner
            .routes
            .lock()
            .unwrap()
            .values()
            .filter(|r| r.source == source)
            .cloned()
            .collect();
        for route in routes {
            route.stop(
                "host_offline",
                "Origin session closed or its authority changed",
            );
        }
    }
    pub async fn revoke_source(&self, source: &str) {
        let routes: Vec<_> = self
            .inner
            .routes
            .lock()
            .unwrap()
            .values()
            .filter(|r| r.source == source)
            .cloned()
            .collect();
        for route in routes {
            route.stop(
                "permission_denied",
                "Caller authority changed. Reconnecting with current grants.",
            );
        }
    }
    pub async fn handle_binary(&self, context: &RequestContext, bytes: Bytes) -> Result<bool> {
        if bytes.first() != Some(&0x30) {
            return Ok(false);
        }
        let (id, offset) = decode_frame(&bytes)?;
        let route = self
            .inner
            .routes
            .lock()
            .unwrap()
            .get(id)
            .filter(|r| r.source == context.source_id())
            .cloned()
            .ok_or_else(|| {
                HostError::new(
                    "permission_denied",
                    "Host route is not owned by this physical source",
                )
            })?;
        if let Err(error) = route.send(HostFrame::Binary(bytes.slice(offset..))).await {
            route.stop("resource_limit", "Host input buffer limit reached");
            return Err(error);
        }
        Ok(true)
    }
    pub(super) async fn close_route(&self, source: &str, id: &str, code: &str, message: &str) {
        let route = self
            .inner
            .routes
            .lock()
            .unwrap()
            .get(id)
            .filter(|r| r.source == source)
            .cloned();
        if let Some(route) = route {
            route.stop(code, message);
        }
    }
    pub(super) async fn route_request(
        &self,
        context: &RequestContext,
        message: &Value,
    ) -> Result<()> {
        match message["type"].as_str().unwrap_or("") {
            "hosts.route.frame.request" => {
                let route = self
                    .inner
                    .routes
                    .lock()
                    .unwrap()
                    .get(field(message, "routeId")?)
                    .filter(|r| r.source == context.source_id())
                    .cloned()
                    .ok_or_else(|| HostError::new("host_unknown", "Unknown routed session"))?;
                route
                    .send(HostFrame::Text(field(message, "data")?.into()))
                    .await
            }
            "hosts.route.close.request" => {
                let routes: Vec<_> = self
                    .inner
                    .routes
                    .lock()
                    .unwrap()
                    .values()
                    .filter(|r| {
                        r.source == context.source_id()
                            && match message["routeId"].as_str() {
                                Some(id) => r.owner.as_ref().is_some_and(|o| o.id() == id),
                                None => message["scopeId"].as_str() == Some(r.scope.as_str()),
                            }
                    })
                    .cloned()
                    .collect();
                for route in routes {
                    route.stop("host_offline", "Host channel closed");
                }
                Ok(())
            }
            "hosts.link.open.request" => self.open_link(context, message).await,
            "hosts.route.open.request" => {
                ensure!(
                    context.subscription_ids("host-route").len() < 32,
                    HostError::new("resource_limit", "Too many routed sessions for this caller")
                );
                let scope = field(message, "scopeId")?;
                validate_scope(scope)?;
                let owner = context.begin_subscription("host-route", None, Some(scope))?;
                let result = self
                    .open_remote(
                        field(message, "serverId")?,
                        scope,
                        &context.actor_source_id(),
                        context.session_id(),
                        context.permissions(),
                        context.source_id(),
                        owner.cancellation(),
                        Some(owner.clone()),
                    )
                    .await;
                let channel = match result {
                    Ok(channel) => channel,
                    Err(error) => {
                        owner.release().await?;
                        return Err(error);
                    }
                };
                if let Err(error)=context.send(json!({"type":"hosts.route.open.response","payload":{"requestId":message["requestId"],"routeId":owner.response_id()}})).await{channel.route.stop("host_offline","Origin session closed");owner.release().await?;return Err(error);}
                owner.activate().await?;
                self.forward_owned(channel, owner, 32 * 1024 * 1024);
                Ok(())
            }
            _ => Ok(()),
        }
    }
    fn forward_owned(&self, mut channel: Channel, owner: OwnedSubscription, limit: usize) {
        let service = self.clone();
        self.spawn(async move{
        while let Some(frame)=channel.output.recv().await{match frame{
            HostFrame::Close{..}=>break,
            frame=>{if emit_frame(&owner,frame,limit).await.is_err(){channel.route.stop("resource_limit","The receiving client cannot keep up with host output");break;}}
        }}
        let failure=channel.route.failure.lock().unwrap().clone().unwrap_or_else(||("host_offline".into(),"Host disconnected".into()));
        let _=owner.emit(json!({"type":"hosts.route.closed","payload":{"subscriptionId":owner.response_id(),"code":failure.0,"message":failure.1}})).await;
        let _=owner.release().await;channel.route.cancel.cancel();service.inner.routes.lock().unwrap().remove(&channel.id);
    });
    }
    async fn open_remote(
        &self,
        server: &str,
        scope: &str,
        actor: &str,
        origin: &str,
        permissions: Vec<String>,
        source: &str,
        cancel: CancellationToken,
        owner: Option<OwnedSubscription>,
    ) -> Result<Channel> {
        let row = self
            .inner
            .rows
            .read()
            .unwrap()
            .iter()
            .find(|r| r["serverId"] == server)
            .cloned()
            .ok_or_else(|| HostError::new("host_unknown", &format!("Unknown host {server}")))?;
        ensure!(
            row["enabled"] == true,
            HostError::new(
                "host_disabled",
                &format!("Enable host {server} before using it")
            )
        );
        ensure!(
            row["status"] == "online",
            HostError::new(
                row["error"]["code"].as_str().unwrap_or("host_offline"),
                row["error"]["message"]
                    .as_str()
                    .unwrap_or("Host is offline")
            )
        );
        let registration = field(&row, "registrationId")?.to_owned();
        let link = self
            .inner
            .live
            .lock()
            .unwrap()
            .get(&registration)
            .cloned()
            .ok_or_else(|| HostError::new("host_offline", "Host is offline"))?;
        let id = owner
            .as_ref()
            .map(|o| o.id().to_owned())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let (input, mut incoming) = mpsc::channel(256);
        let (output_tx, output) = mpsc::channel(1);
        let route = Arc::new(Route {
            registration,
            source: source.into(),
            scope: scope.into(),
            input,
            cancel: cancel.child_token(),
            budget: self.inner.budget.clone(),
            failure: Mutex::new(None),
            owner,
        });
        {
            let mut routes = self.inner.routes.lock().unwrap();
            ensure!(
                routes
                    .values()
                    .filter(|r| !r.registration.is_empty())
                    .count()
                    < 128,
                HostError::new("resource_limit", "Too many routed sessions")
            );
            ensure!(
                route.owner.is_none()
                    || routes
                        .values()
                        .filter(|r| !r.registration.is_empty() && r.source == source)
                        .count()
                        < 32,
                HostError::new("resource_limit", "Too many routed sessions for this caller")
            );
            routes.insert(id.clone(), route.clone());
        }
        let channel_scope = uuid::Uuid::new_v4().to_string();
        let opened = tokio::select! {_=route.cancel.cancelled()=>Err(anyhow!("Host route cancelled")),result=link.request(json!({"type":"hosts.link.open.request","channelId":channel_scope}))=>result};
        let remote = match opened
            .and_then(|value| Ok(field(&value["payload"], "routeId")?.to_owned()))
        {
            Ok(remote) => remote,
            Err(error) => {
                self.inner.routes.lock().unwrap().remove(&id);
                let _=link.sender.json(json!({"type":"session","message":{"type":"hosts.route.close.request","scopeId":channel_scope}})).await;
                return Err(error);
            }
        };
        let (remote_tx, mut remote_rx) = mpsc::channel(1);
        link.channels
            .lock()
            .unwrap()
            .insert(remote.clone(), remote_tx);
        let self_id = field(self.identity()?, "serverId")?.to_owned();
        let transformed = route_client_id(&self_id, actor, origin, scope)?;
        let producer = json!({"serverId":self_id,"sourceId":actor});
        let expected = server.to_owned();
        let route_task = route.clone();
        let service = self.clone();
        let route_id = id.clone();
        self.spawn(async move{
            let run=async{
                let mut hello_sent=false;let mut verified=false;let deadline=tokio::time::sleep(Duration::from_secs(15));tokio::pin!(deadline);
                loop{tokio::select!{biased;
                    _=route_task.cancel.cancelled()=>break,
                    _=link.cancel.cancelled()=>return Err(HostError::new("host_offline","Host disconnected").into()),
                    _=&mut deadline,if !verified=>return Err(HostError::new("host_offline","Host handshake timed out").into()),
                    frame=incoming.recv()=>{
                        let Some(InputFrame{mut frame,permit})=frame else{break};
                        if !hello_sent {let HostFrame::Text(text)=frame else{return Err(HostError::new("invalid_message","Expected hello").into());};let mut hello:Value=serde_json::from_str(&text)?;ensure!(hello["type"]=="hello",HostError::new("invalid_message","Expected hello"));hello["clientId"]=json!(transformed);hello["routed"]=json!(true);hello["producer"]=producer.clone();hello["sessionPermissions"]=json!(permissions);hello.as_object_mut().unwrap().remove("auth");frame=HostFrame::Text(serde_json::to_string(&hello)?);hello_sent=true;}
                        else{ensure!(verified,HostError::new("host_offline","Host identity has not been verified"));}
                        send_nested(&link,&remote,frame,permit).await?;
                    },
                    frame=remote_rx.recv()=>{
                        let Some(frame)=frame else{return Err(anyhow!("Host channel closed"));};
                        let frame=frame?;
                        if let HostFrame::Close{reason,..}=&frame{return Err(HostError::new("host_offline",reason).into());}
                        if !verified{let HostFrame::Text(text)=&frame else{return Err(anyhow!("Invalid host handshake"));};let value:Value=serde_json::from_str(text)?;
                            ensure!(value["type"]!="hello.rejected",HostError::new("authentication_failed","Replace the host credentials"));
                            if value["type"]!="session"||value["message"]["type"]!="status"||value["message"]["payload"]["status"]!="server_info"{continue;}
                            let info=&value["message"]["payload"];ensure!(info["serverId"]==expected,HostError::new("server_id_mismatch","Host identity changed; explicit replacement required"));ensure!(info["features"]["connectedHosts"]==true,HostError::new("unsupported_capability","Update the destination daemon"));verified=true;
                        }
                        tokio::select!{_=route_task.cancel.cancelled()=>break,result=output_tx.send(frame)=>{result.map_err(|_|anyhow!("Host route consumer closed"))?;}}
                    }
                }}Ok::<_,anyhow::Error>(())
            }.await;
            if let Err(error)=run{let failure=error.downcast_ref::<HostError>();route_task.stop(failure.map(|e|e.code.as_str()).unwrap_or("host_offline"),failure.map(|e|e.message.as_str()).unwrap_or("Host transport closed"));}
            let _=link.sender.json(json!({"type":"session","message":{"type":"hosts.route.close.request","routeId":remote}})).await;
            link.channels.lock().unwrap().remove(&remote);drop(output_tx);route_task.cancel.cancel();service.inner.routes.lock().unwrap().remove(&route_id);
        });
        Ok(Channel { id, route, output })
    }
    async fn open_link(&self, context: &RequestContext, message: &Value) -> Result<()> {
        ensure!(
            context.subscription_ids("host-link").len() < 128,
            HostError::new("resource_limit", "Too many host channels")
        );
        let scope = field(message, "channelId")?;
        validate_scope(scope)?;
        let owner = context.begin_subscription("host-link", None, Some(scope))?;
        let id = owner.id().to_owned();
        let cancel = owner.cancellation();
        let (input, mut incoming) = mpsc::channel(256);
        let (outgoing_tx, mut outgoing) = mpsc::channel(1);
        let (admission_tx, admission_rx) = mpsc::channel(1);
        let (output_tx, output) = mpsc::channel(1);
        let route = Arc::new(Route {
            registration: String::new(),
            source: context.source_id().into(),
            scope: scope.into(),
            input,
            cancel: cancel.child_token(),
            budget: Arc::new(Semaphore::new(128 * 1024 * 1024)),
            failure: Mutex::new(None),
            owner: Some(owner.clone()),
        });
        let admitted = {
            let mut routes = self.inner.routes.lock().unwrap();
            if routes
                .values()
                .filter(|r| r.registration.is_empty() && r.source == context.source_id())
                .count()
                >= 128
            {
                false
            } else {
                routes.insert(id.clone(), route.clone());
                true
            }
        };
        if !admitted {
            owner.release().await?;
            return Err(HostError::new("resource_limit", "Too many host channels").into());
        }
        let app = self
            .inner
            .app
            .upgrade()
            .ok_or_else(|| anyhow!("Daemon stopping"))?;
        let transport = app.transport.clone();
        drop(app);
        if let Err(error) = transport
            .admit_host_channel(
                context,
                context.permissions(),
                admission_rx,
                outgoing_tx,
                route.cancel.clone(),
            )
            .await
        {
            route.stop("host_offline", "Could not admit host channel");
            self.inner.routes.lock().unwrap().remove(&id);
            owner.release().await?;
            return Err(error);
        }
        let route_task = route.clone();
        let identity = context.actor_source_id();
        self.spawn(async move{
            let mut first=true;
            let result:Result<()>=async{loop{tokio::select!{
                _=route_task.cancel.cancelled()=>break,
                frame=incoming.recv()=>{let Some(InputFrame{frame,permit})=frame else{break};let frame=if first{first=false;let HostFrame::Text(text)=frame else{return Err(HostError::new("permission_denied","Expected hello").into());};let mut hello:Value=serde_json::from_str(&text)?;ensure!(hello["type"]=="hello"&&hello["clientId"].is_string(),HostError::new("permission_denied","Invalid hello"));hello["clientId"]=json!(link_client_id(&identity,hello["clientId"].as_str().unwrap())?);hello["routed"]=json!(true);hello.as_object_mut().unwrap().remove("auth");HostFrame::Text(serde_json::to_string(&hello)?)}else{frame};
                    tokio::select!{_=route_task.cancel.cancelled()=>break,result=admission_tx.send(to_outbound(frame))=>{result.map_err(|_|anyhow!("Host channel input closed"))?;}}drop(permit);
                },
                frame=outgoing.recv()=>{let Some(frame)=frame else{break};let frame=match frame{OutboundFrame::Text(text)=>HostFrame::Text(text),OutboundFrame::Binary(bytes)=>HostFrame::Binary(bytes),OutboundFrame::Flush(ack)=>HostFrame::Flush(ack),OutboundFrame::Close{code,reason}=>{route_task.stop(if code==1009{"resource_limit"}else if code==1008{"permission_denied"}else{"host_offline"},&reason);break;},OutboundFrame::Ping(_)|OutboundFrame::Pong(_)=>continue};tokio::select!{_=route_task.cancel.cancelled()=>break,result=output_tx.send(frame)=>{result.map_err(|_|anyhow!("Host channel output closed"))?;}}}
            }}Ok(())}.await;
            if let Err(error)=result{let failure=error.downcast_ref::<HostError>();route_task.stop(failure.map(|e|e.code.as_str()).unwrap_or("host_offline"),failure.map(|e|e.message.as_str()).unwrap_or("Host channel closed"));}route_task.cancel.cancel();
        });
        context.send(json!({"type":"hosts.link.open.response","payload":{"requestId":message["requestId"],"routeId":owner.response_id()}})).await?;
        owner.activate().await?;
        self.forward_owned(
            Channel { id, route, output },
            owner,
            64 * 1024 * 1024 - 65536,
        );
        Ok(())
    }
    pub async fn tool_client(
        &self,
        server_id: &str,
        cancel: CancellationToken,
    ) -> Result<HostClient> {
        let scope = uuid::Uuid::new_v4().to_string();
        let permissions = crate::transport::permissions::OWNER_PERMISSIONS
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        let mut channel = self
            .open_remote(
                server_id,
                &scope,
                "mcp",
                "mcp",
                permissions,
                "mcp",
                cancel,
                None,
            )
            .await?;
        channel.route.send(HostFrame::Text(serde_json::to_string(&json!({"type":"hello","clientId":uuid::Uuid::new_v4().to_string(),"clientType":"cli","protocolVersion":1,"capabilities":client_capabilities()}))?)).await?;
        let ready = tokio::time::timeout(Duration::from_secs(15), channel.output.recv()).await;
        let frame = match ready {
            Ok(Some(frame)) => frame,
            _ => {
                channel
                    .route
                    .stop("host_offline", "Host handshake timed out");
                let failure = channel.route.failure.lock().unwrap().clone().unwrap();
                return Err(HostError::new(&failure.0, &failure.1).into());
            }
        };
        let HostFrame::Text(text) = frame else {
            return Err(HostError::new("host_offline", "Host handshake failed").into());
        };
        let value: Value = serde_json::from_str(&text)?;
        let info = value["message"]["payload"].clone();
        let pending: Arc<Mutex<HashMap<String, (String, oneshot::Sender<Result<Value>>)>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let (events_tx, events) = mpsc::channel(256);
        let pending_task = pending.clone();
        let route = channel.route.clone();
        let task_route = route.clone();
        self.spawn(async move{
            let mut heartbeat=tokio::time::interval(Duration::from_secs(10));
            loop{tokio::select!{
                _=task_route.cancel.cancelled()=>break,
                _=heartbeat.tick()=>{if task_route.send(HostFrame::Text("{\"type\":\"ping\"}".into())).await.is_err(){break;}},
                frame=channel.output.recv()=>{let Some(HostFrame::Text(text))=frame else{break};let Ok(value)=serde_json::from_str::<Value>(&text)else{break};if value["type"]=="ping"{let _=task_route.send(HostFrame::Text("{\"type\":\"pong\"}".into())).await;continue;}if value["type"]!="session"{continue;}let message=&value["message"];
                    let delivered=if let Some(id)=message["payload"]["requestId"].as_str(){let mut pending=pending_task.lock().unwrap();let matches=pending.get(id).is_some_and(|(expected,_)|message["type"]==expected.as_str()||message["type"]=="rpc_error");if matches{let(_,tx)=pending.remove(id).unwrap();let result=if message["type"]=="rpc_error"{Err(remote_error(message).into())}else{Ok(message.clone())};let _=tx.send(result);true}else{false}}else{false};
                    if !delivered&&events_tx.try_send(message.clone()).is_err(){task_route.stop("resource_limit","Host event consumer cannot keep up");break;}
                }
            }}pending_task.lock().unwrap().clear();task_route.cancel.cancel();
        });
        Ok(HostClient {
            route,
            pending,
            events: AsyncMutex::new(events),
            info,
        })
    }
}

pub struct HostClient {
    route: Arc<Route>,
    pending: Arc<Mutex<HashMap<String, (String, oneshot::Sender<Result<Value>>)>>>,
    events: AsyncMutex<mpsc::Receiver<Value>>,
    info: Value,
}
impl HostClient {
    pub fn server_info(&self) -> &Value {
        &self.info
    }
    pub fn features(&self) -> &Value {
        &self.info["features"]
    }
    pub async fn request(&self, mut message: Value, response_type: &str) -> Result<Value> {
        let unlimited = matches!(
            message["type"].as_str(),
            Some("agent.create.request" | "workspace.create.request" | "wait_for_finish_request")
        );
        let id = message["requestId"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        message["requestId"] = json!(id);
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().unwrap();
            ensure!(!pending.contains_key(&id), "Request ID already pending");
            pending.insert(id.clone(), (response_type.into(), tx));
        }
        let result=async{self.route.send(HostFrame::Text(serde_json::to_string(&json!({"type":"session","message":message}))?)).await.map_err(|e|anyhow!("Host send failed: {e}"))?;
            tokio::select!{_=self.route.cancel.cancelled()=>Err(anyhow!("Host transport closed")),result=async{
                if unlimited {rx.await.map_err(|_|anyhow!("Host transport closed"))?}
                else{tokio::time::timeout(Duration::from_secs(60),rx).await.map_err(|_|anyhow!("Timeout waiting for message"))?.map_err(|_|anyhow!("Host transport closed"))?}
            }=>result}
        }.await;
        self.pending.lock().unwrap().remove(&id);
        result
    }
    pub async fn next_event(&self) -> Result<Value> {
        let mut events = self.events.lock().await;
        tokio::select! {_=self.route.cancel.cancelled()=>Err(anyhow!("Host transport closed")),event=events.recv()=>event.ok_or_else(||anyhow!("Host transport closed"))}
    }
    pub async fn close(&self) -> Result<()> {
        self.route.stop("host_offline", "Host client closed");
        Ok(())
    }
}
impl Drop for HostClient {
    fn drop(&mut self) {
        self.route.stop("host_offline", "Host client closed");
    }
}
fn validate_scope(scope: &str) -> Result<()> {
    ensure!(
        !scope.is_empty() && scope.encode_utf16().count() <= 256,
        "Invalid host scope"
    );
    Ok(())
}
fn route_client_id(server: &str, actor: &str, origin: &str, scope: &str) -> Result<String> {
    Ok(format!(
        "route-{}",
        hex::encode(Sha256::digest(serde_json::to_vec(&[
            server, actor, origin, scope
        ])?))
    ))
}
fn link_client_id(identity: &str, client: &str) -> Result<String> {
    Ok(format!(
        "link-{}",
        hex::encode(Sha256::digest(serde_json::to_vec(&[identity, client])?))
    ))
}
fn to_outbound(frame: HostFrame) -> OutboundFrame {
    match frame {
        HostFrame::Text(text) => OutboundFrame::Text(text),
        HostFrame::Binary(bytes) => OutboundFrame::Binary(bytes),
        HostFrame::Close { code, reason } => OutboundFrame::Close { code, reason },
        HostFrame::Flush(ack) => OutboundFrame::Flush(ack),
    }
}
async fn send_nested(
    link: &Link,
    id: &str,
    frame: HostFrame,
    permit: tokio::sync::OwnedSemaphorePermit,
) -> Result<()> {
    let outer = match frame {
        HostFrame::Text(text) => HostFrame::Text(serde_json::to_string(
            &json!({"type":"session","message":{"type":"hosts.route.frame.request","routeId":id,"data":text,"binary":false}}),
        )?),
        HostFrame::Binary(bytes) => HostFrame::Binary(encode_frame(id, &bytes)?),
        HostFrame::Close { .. } => HostFrame::Text(serde_json::to_string(
            &json!({"type":"session","message":{"type":"hosts.route.close.request","routeId":id}}),
        )?),
        HostFrame::Flush(ack) => HostFrame::Flush(ack),
    };
    link.sender.send_reserved(outer, permit).await
}
async fn emit_frame(owner: &OwnedSubscription, frame: HostFrame, limit: usize) -> Result<()> {
    match frame {
        HostFrame::Text(text) => {
            let message = json!({"type":"hosts.route.frame","payload":{"subscriptionId":owner.response_id(),"data":text,"binary":false}});
            let size = serde_json::to_vec(&json!({"type":"session","message":message}))?.len();
            ensure!(
                owner
                    .context()
                    .buffered_amount()
                    .checked_add(size)
                    .is_some_and(|n| n <= limit),
                "Host output limit reached"
            );
            owner.emit(message).await
        }
        HostFrame::Binary(bytes) => {
            let frame = encode_frame(owner.response_id(), &bytes)?;
            ensure!(
                owner
                    .context()
                    .buffered_amount()
                    .checked_add(frame.len())
                    .is_some_and(|n| n <= limit),
                "Host output limit reached"
            );
            owner.emit_binary(frame).await
        }
        HostFrame::Flush(ack) => {
            let result = owner.context().flush().await;
            let failure = result.as_ref().err().map(ToString::to_string);
            let _ = ack.send(result);
            if let Some(failure) = failure {
                return Err(anyhow!(failure));
            }
            Ok(())
        }
        HostFrame::Close { .. } => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn context(source: &str) -> RequestContext {
        RequestContext::attach(
            source.into(),
            "session".into(),
            "client".into(),
            json!({}),
            json!({"id":"owner"}),
            Arc::new(RwLock::new(
                ["workspace.read".into(), "workspace.write".into()]
                    .into_iter()
                    .collect(),
            )),
            false,
            None,
            CancellationToken::new(),
        )
        .0
    }
    #[tokio::test]
    async fn binary_route_id_does_not_authorize_a_different_physical_source() {
        let service = HostsService::new(Weak::new());
        let (input, mut incoming) = mpsc::channel(4);
        let route = Arc::new(Route {
            registration: "registration".into(),
            source: "first".into(),
            scope: "scope".into(),
            input,
            cancel: CancellationToken::new(),
            budget: service.inner.budget.clone(),
            failure: Mutex::new(None),
            owner: None,
        });
        service
            .inner
            .routes
            .lock()
            .unwrap()
            .insert("route".into(), route);
        let frame = encode_frame("route", &[2, 7, 42]).unwrap();
        let denied = service
            .handle_binary(&context("replacement"), frame.clone())
            .await
            .unwrap_err();
        assert_eq!(
            denied.downcast_ref::<HostError>().unwrap().code,
            "permission_denied"
        );
        let missing = service
            .handle_binary(
                &context("replacement"),
                encode_frame("absent", &[2, 7, 42]).unwrap(),
            )
            .await
            .unwrap_err();
        assert_eq!(missing.to_string(), denied.to_string());
        assert!(matches!(
            incoming.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        service
            .handle_binary(&context("first"), frame)
            .await
            .unwrap();
        let received = incoming.recv().await.unwrap();
        assert!(matches!(&received.frame,HostFrame::Binary(bytes) if bytes.as_ref()==[2,7,42]));
    }
    #[tokio::test]
    async fn input_budget_is_reserved_before_queueing_and_released_on_drop() {
        let (input, mut incoming) = mpsc::channel(4);
        let budget = Arc::new(Semaphore::new(8));
        let route = Route {
            registration: "registration".into(),
            source: "source".into(),
            scope: "scope".into(),
            input,
            cancel: CancellationToken::new(),
            budget: budget.clone(),
            failure: Mutex::new(None),
            owner: None,
        };
        route
            .send(HostFrame::Binary(Bytes::from_static(&[1, 2, 3, 4, 5])))
            .await
            .unwrap();
        assert_eq!(budget.available_permits(), 3);
        assert!(
            route
                .send(HostFrame::Binary(Bytes::from_static(&[1, 2, 3, 4])))
                .await
                .is_err()
        );
        drop(incoming.recv().await.unwrap());
        assert_eq!(budget.available_permits(), 8);
    }
    #[tokio::test]
    async fn routed_flush_acknowledgment_waits_for_the_physical_writer() {
        let permissions = crate::transport::permissions::OWNER_PERMISSIONS
            .iter()
            .map(|p| (*p).to_owned())
            .collect();
        let (context, mut writer) = RequestContext::attach(
            "physical".into(),
            "session".into(),
            "client".into(),
            json!({"owned_subscriptions":true}),
            json!({"id":"owner"}),
            Arc::new(RwLock::new(permissions)),
            false,
            None,
            CancellationToken::new(),
        );
        let request = context.for_request(
            json!({"type":"hosts.link.open.request","requestId":"open","channelId":"scope"}),
        );
        let owner = request.begin_subscription("host-link", None, None).unwrap();
        owner.activate().await.unwrap();
        let (ack, mut acknowledged) = oneshot::channel();
        let task = tokio::spawn(async move {
            emit_frame(
                &owner,
                HostFrame::Text("{\"type\":\"session\",\"message\":{\"type\":\"result\"}}".into()),
                64 * 1024 * 1024,
            )
            .await
            .unwrap();
            emit_frame(&owner, HostFrame::Flush(ack), 64 * 1024 * 1024)
                .await
                .unwrap();
        });
        let first = writer.recv().await.unwrap();
        assert!(matches!(&first.frame, OutboundFrame::Text(_)));
        drop(first);
        let mut barrier = writer.recv().await.unwrap();
        assert!(matches!(
            acknowledged.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        let OutboundFrame::Flush(physical_ack) =
            std::mem::replace(&mut barrier.frame, OutboundFrame::Pong(Bytes::new()))
        else {
            panic!("Expected ordered physical flush barrier")
        };
        drop(barrier);
        physical_ack.send(Ok(())).unwrap();
        acknowledged.await.unwrap().unwrap();
        task.await.unwrap();
    }
}
