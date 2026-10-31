use anyhow::{Result, anyhow, bail};
use bytes::Bytes;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use tokio::sync::{Mutex as AsyncMutex, mpsc};
use tokio_util::sync::CancellationToken;

use super::permissions;

pub const MAX_BUFFERED_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_FRAME_BYTES: usize = 100 * 1024 * 1024;

#[derive(Debug)]
pub enum OutboundFrame {
    Text(String),
    Binary(Bytes),
    Ping(Bytes),
    Pong(Bytes),
    Close { code: u16, reason: String },
    Flush(tokio::sync::oneshot::Sender<Result<()>>),
}
impl OutboundFrame {
    pub fn byte_len(&self) -> usize {
        match self {
            Self::Text(v) => v.len(),
            Self::Binary(v) | Self::Ping(v) | Self::Pong(v) => v.len(),
            Self::Close { reason, .. } => reason.len() + 2,
            Self::Flush(_) => 0,
        }
    }
}

pub(crate) struct Source {
    pub id: String,
    pub session_id: String,
    pub client_id: String,
    pub capabilities: Value,
    pub app_version: Option<String>,
    activity: RwLock<Value>,
    pub principal: Value,
    pub permissions: Arc<RwLock<HashSet<String>>>,
    pub cancel: CancellationToken,
    pub modern: bool,
    pub routed: AtomicBool,
    pub sender: mpsc::Sender<QueuedFrame>,
    pub queued: Arc<AtomicUsize>,
    handshake_pending: AtomicBool,
    subscriptions: Mutex<HashMap<String, Arc<SubscriptionState>>>,
    operations: Mutex<HashMap<String, CancellationToken>>,
    slots: Mutex<HashMap<u8, String>>,
    transfers: Mutex<HashSet<String>>,
    events: Mutex<HashMap<String, (HashSet<String>, bool)>>,
}

pub(crate) struct QueuedFrame {
    pub frame: OutboundFrame,
    bytes: usize,
    queue: Arc<AtomicUsize>,
}
impl Drop for QueuedFrame {
    fn drop(&mut self) {
        self.queue.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

#[derive(Clone)]
enum Owner {
    Source,
    Request(Arc<RequestOwner>),
    Subscription(Arc<SubscriptionState>),
    Operation(Arc<OperationState>),
}
struct RequestOwner {
    message: Value,
    active: AtomicBool,
}
struct OperationState {
    id: String,
    cancel: CancellationToken,
    accepted: HashSet<String>,
}
struct SubscriptionState {
    id: String,
    response_id: String,
    family: String,
    legacy_slot: String,
    request: Value,
    cancel: CancellationToken,
    delivery: AsyncMutex<SubscriptionDelivery>,
}
struct SubscriptionDelivery {
    priming: bool,
    pending: VecDeque<QueuedFrame>,
    bytes: usize,
}

/// Immutable physical-source identity. Cloning never redirects delivery to a resumed socket.
#[derive(Clone)]
pub struct RequestContext {
    pub(crate) source: Arc<Source>,
    owner: Owner,
}

impl RequestContext {
    pub(crate) fn attach(
        id: String,
        session_id: String,
        client_id: String,
        capabilities: Value,
        principal: Value,
        permissions: Arc<RwLock<HashSet<String>>>,
        routed: bool,
        app_version: Option<String>,
        cancel: CancellationToken,
    ) -> (Self, mpsc::Receiver<QueuedFrame>) {
        let modern = has_capability(&capabilities, "owned_subscriptions");
        let (sender, receiver) = mpsc::channel(256);
        let source = Arc::new(Source {
            id,
            session_id,
            client_id,
            capabilities,
            principal,
            permissions,
            cancel,
            modern,
            routed: AtomicBool::new(routed),
            app_version,
            activity: RwLock::new(Value::Null),
            sender,
            queued: Arc::new(AtomicUsize::new(0)),
            handshake_pending: AtomicBool::new(true),
            subscriptions: Mutex::new(HashMap::new()),
            operations: Mutex::new(HashMap::new()),
            slots: Mutex::new(HashMap::new()),
            transfers: Mutex::new(HashSet::new()),
            events: Mutex::new(HashMap::new()),
        });
        (
            Self {
                source,
                owner: Owner::Source,
            },
            receiver,
        )
    }
    pub fn source_id(&self) -> &str {
        &self.source.id
    }
    pub fn session_id(&self) -> &str {
        &self.source.session_id
    }
    pub fn client_id(&self) -> &str {
        &self.source.client_id
    }
    pub fn capabilities(&self) -> &Value {
        &self.source.capabilities
    }
    pub fn principal(&self) -> &Value {
        &self.source.principal
    }
    pub fn app_version(&self) -> Option<&str> {
        self.source.app_version.as_deref()
    }
    pub fn current_activity(&self) -> Value {
        self.source.activity.read().unwrap().clone()
    }
    pub(crate) fn set_activity(&self, activity: Value) {
        *self.source.activity.write().unwrap() = activity;
    }
    pub fn cancellation(&self) -> CancellationToken {
        match &self.owner {
            Owner::Subscription(s) => s.cancel.clone(),
            Owner::Operation(o) => o.cancel.clone(),
            _ => self.source.cancel.clone(),
        }
    }
    pub fn allows(&self, permission: &str) -> bool {
        self.source.permissions.read().unwrap().contains(permission)
    }
    pub fn permissions(&self) -> Vec<String> {
        let mut p: Vec<_> = self
            .source
            .permissions
            .read()
            .unwrap()
            .iter()
            .cloned()
            .collect();
        p.sort();
        p
    }
    pub fn is_modern(&self) -> bool {
        self.source.modern
    }
    pub fn is_routed(&self) -> bool {
        self.source.routed.load(Ordering::Acquire)
    }
    pub fn buffered_amount(&self) -> usize {
        self.source.queued.load(Ordering::Acquire)
    }
    pub fn actor_source_id(&self) -> String {
        self.source
            .principal
            .get("sourceId")
            .and_then(Value::as_str)
            .or_else(|| self.source.principal.get("id").and_then(Value::as_str))
            .unwrap_or("owner")
            .to_owned()
    }
    pub fn is_live(&self) -> bool {
        !self.source.cancel.is_cancelled() && !self.cancellation().is_cancelled()
    }
    pub fn request(&self) -> Option<&Value> {
        match &self.owner {
            Owner::Request(r) => Some(&r.message),
            _ => None,
        }
    }
    pub(crate) fn from_source(source: Arc<Source>) -> Self {
        Self {
            source,
            owner: Owner::Source,
        }
    }
    pub(crate) fn for_request(&self, message: Value) -> Self {
        Self {
            source: self.source.clone(),
            owner: Owner::Request(Arc::new(RequestOwner {
                message,
                active: AtomicBool::new(true),
            })),
        }
    }
    pub(crate) fn finish_request(&self) {
        if let Owner::Request(r) = &self.owner {
            r.active.store(false, Ordering::Release);
        }
    }
    fn require_request(&self) -> Result<&RequestOwner> {
        match &self.owner {
            Owner::Request(r) if r.active.load(Ordering::Acquire) && self.is_live() => Ok(r),
            _ => bail!("Operation has no active requesting source"),
        }
    }
    pub async fn send(&self, mut message: Value) -> Result<()> {
        if matches!(self.owner, Owner::Source)
            && self.principal()["kind"] == "hub"
            && matches!(
                message["type"].as_str(),
                Some("hub.execution.agent.update" | "hub.execution.agent.stream")
            )
        {
            return self.emit_event(message).await;
        }
        let handshake = matches!(self.owner, Owner::Source)
            && message["type"] == "status"
            && message["payload"]["status"] == "server_info"
            && self.source.handshake_pending.load(Ordering::Acquire);
        if !handshake {
            self.ensure_outbound(&message)?;
        }
        if self.source.modern {
            let allowed = match &self.owner {
                Owner::Request(r) => {
                    r.active.load(Ordering::Acquire) && is_reply(&r.message, &message)
                }
                Owner::Subscription(s) => !s.cancel.is_cancelled(),
                Owner::Operation(o) => {
                    !o.cancel.is_cancelled() && o.accepted.contains(message_type(&message)?)
                }
                Owner::Source => {
                    message["type"] == "status"
                        && message["payload"]["status"] == "server_info"
                        && self.source.handshake_pending.swap(false, Ordering::AcqRel)
                }
            };
            if !allowed {
                bail!("Outbound message lacks live source ownership");
            }
        } else {
            legacy_message(&mut message);
        }
        self.queue(OutboundFrame::Text(serde_json::to_string(
            &json!({"type":"session","message":message}),
        )?))
        .await
    }
    pub async fn send_flushed(&self, message: Value) -> Result<()> {
        self.send(message).await?;
        self.flush().await
    }
    pub async fn flush(&self) -> Result<()> {
        let (acknowledge, flushed) = tokio::sync::oneshot::channel();
        self.queue(OutboundFrame::Flush(acknowledge)).await?;
        let cancel = self.cancellation();
        tokio::select! { biased; _ = cancel.cancelled() => bail!("Physical source closed before flush"),
            result = flushed => result.map_err(|_| anyhow!("Physical writer closed before flush"))?,
        }
    }
    pub async fn send_binary(&self, bytes: Bytes) -> Result<()> {
        self.validate_binary(&bytes, false)?;
        if self.source.modern {
            let permitted = match &self.owner {
                Owner::Subscription(s) => !s.cancel.is_cancelled(),
                Owner::Request(r) => {
                    r.active.load(Ordering::Acquire)
                        && r.message["type"] == "file_explorer_request"
                        && binary_request_id(&bytes)
                            .is_some_and(|id| Some(id) == request_id(&r.message))
                }
                _ => false,
            };
            if !permitted {
                bail!("Binary frame lacks live source ownership");
            }
        }
        self.queue(OutboundFrame::Binary(bytes)).await
    }
    fn ensure_outbound(&self, message: &Value) -> Result<()> {
        if !self.is_live() {
            bail!("Physical source is closed");
        }
        if !permissions::allows_outbound(message, |p| self.allows(p)) {
            bail!("Outbound operation is not permitted");
        }
        Ok(())
    }
    pub(crate) async fn queue(&self, frame: OutboundFrame) -> Result<()> {
        self.queue_reserved(self.reserve(frame)?).await
    }
    fn reserve(&self, frame: OutboundFrame) -> Result<QueuedFrame> {
        if !self.is_live() {
            bail!("Physical source is closed");
        }
        let bytes = frame.byte_len();
        let reserved =
            self.source
                .queued
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                    current
                        .checked_add(bytes)
                        .filter(|n| *n <= MAX_BUFFERED_BYTES)
                });
        if reserved.is_err() {
            self.detach();
            bail!("Physical source backpressure limit exceeded");
        }
        Ok(QueuedFrame {
            frame,
            bytes,
            queue: self.source.queued.clone(),
        })
    }
    async fn queue_reserved(&self, item: QueuedFrame) -> Result<()> {
        let owner_cancel = self.cancellation();
        tokio::select! { biased;
            _ = self.source.cancel.cancelled() => bail!("Physical source is closed"),
            _ = owner_cancel.cancelled() => bail!("Delivery owner is closed"),
            result = self.source.sender.send(item) => result.map_err(|_| anyhow!("Physical source writer closed")),
        }
    }
    /// Register before taking a domain snapshot; updates buffer until activate().
    pub fn begin_subscription(
        &self,
        family: &str,
        requested_id: Option<&str>,
        legacy_slot: Option<&str>,
    ) -> Result<OwnedSubscription> {
        let request = self.require_request()?;
        if self.is_modern() && request_id(&request.message).is_none_or(str::is_empty) {
            bail!("Owned subscriptions require a requestId");
        }
        if self.is_modern() && requested_id.is_some() {
            bail!("Subscription IDs are assigned by the host");
        }
        let slot = legacy_slot.unwrap_or(family);
        let mut subscriptions = self.source.subscriptions.lock().unwrap();
        if !self.is_modern() {
            subscriptions.retain(|_, prior| {
                if prior.legacy_slot != slot {
                    return true;
                }
                prior.cancel.cancel();
                self.source
                    .slots
                    .lock()
                    .unwrap()
                    .retain(|_, owner| owner != &prior.id);
                self.source.events.lock().unwrap().remove(&prior.id);
                false
            });
        }
        let id = uuid::Uuid::new_v4().to_string();
        let response_id = if self.is_modern() {
            id.clone()
        } else {
            requested_id
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(&id)
                .to_owned()
        };
        let state = Arc::new(SubscriptionState {
            id: id.clone(),
            response_id,
            family: family.to_owned(),
            legacy_slot: slot.to_owned(),
            request: request.message.clone(),
            cancel: self.source.cancel.child_token(),
            delivery: AsyncMutex::new(SubscriptionDelivery {
                priming: true,
                pending: VecDeque::new(),
                bytes: 0,
            }),
        });
        subscriptions.insert(id, state.clone());
        Ok(OwnedSubscription {
            context: Self {
                source: self.source.clone(),
                owner: Owner::Subscription(state.clone()),
            },
            state,
        })
    }
    pub fn retain_operation(&self, accepted_types: &[&str]) -> Result<OwnedOperation> {
        self.require_request()?;
        let state = Arc::new(OperationState {
            id: uuid::Uuid::new_v4().to_string(),
            cancel: self.source.cancel.child_token(),
            accepted: accepted_types.iter().map(|s| (*s).to_owned()).collect(),
        });
        self.source
            .operations
            .lock()
            .unwrap()
            .insert(state.id.clone(), state.cancel.clone());
        Ok(OwnedOperation {
            context: Self {
                source: self.source.clone(),
                owner: Owner::Operation(state.clone()),
            },
            state,
        })
    }
    pub async fn release_subscription(&self, id: &str) -> Result<()> {
        self.require_request()?;
        let state = {
            let mut map = self.source.subscriptions.lock().unwrap();
            let key = if map.contains_key(id) {
                Some(id.to_owned())
            } else if !self.is_modern() {
                map.values()
                    .find(|s| s.response_id == id)
                    .map(|s| s.id.clone())
            } else {
                None
            };
            key.and_then(|k| map.remove(&k))
        };
        if let Some(s) = state {
            s.cancel.cancel();
            self.source
                .slots
                .lock()
                .unwrap()
                .retain(|_, owner| owner != &s.id);
            self.source.events.lock().unwrap().remove(&s.id);
        }
        Ok(())
    }
    pub async fn release_legacy_slot(&self, slot: &str) -> Result<()> {
        if self.is_modern() {
            bail!("Release subscriptions using the server-assigned subscription ID");
        }
        let id = self
            .source
            .subscriptions
            .lock()
            .unwrap()
            .values()
            .find(|s| s.legacy_slot == slot)
            .map(|s| s.id.clone());
        if let Some(id) = id {
            self.release_subscription(&id).await?;
        }
        Ok(())
    }
    pub fn subscription_ids(&self, family: &str) -> Vec<String> {
        self.source
            .subscriptions
            .lock()
            .unwrap()
            .values()
            .filter(|s| s.family == family && !s.cancel.is_cancelled())
            .map(|s| s.id.clone())
            .collect()
    }
    pub fn subscription_requests(&self, family: &str) -> Vec<Value> {
        self.source
            .subscriptions
            .lock()
            .unwrap()
            .values()
            .filter(|s| s.family == family && !s.cancel.is_cancelled())
            .map(|s| s.request.clone())
            .collect()
    }
    pub fn bind_file_transfer(&self, request_id: &str) -> Result<()> {
        self.require_request()?;
        if request_id.is_empty() || request_id.len() > 255 {
            bail!("Invalid file transfer request ID");
        }
        self.source
            .transfers
            .lock()
            .unwrap()
            .insert(request_id.to_owned());
        Ok(())
    }
    pub fn release_file_transfer(&self, request_id: &str) {
        self.source.transfers.lock().unwrap().remove(request_id);
    }
    pub(crate) fn validate_binary(&self, bytes: &[u8], inbound: bool) -> Result<()> {
        if !self.is_live() || bytes.is_empty() || bytes.len() > MAX_FRAME_BYTES {
            bail!("Invalid binary frame");
        }
        match bytes[0] {
            0x01..=0x05 => {
                if bytes.len() < 2
                    || !self.allows(if inbound {
                        "workspace.write"
                    } else {
                        "workspace.read"
                    })
                {
                    bail!("Unauthorized terminal frame");
                }
                if inbound && !matches!(bytes[0], 0x02 | 0x03)
                    || !inbound && !matches!(bytes[0], 0x01 | 0x04 | 0x05)
                {
                    bail!("Invalid terminal frame direction");
                }
                let slots = self.source.slots.lock().unwrap();
                let owner = slots
                    .get(&bytes[1])
                    .ok_or_else(|| anyhow!("Terminal slot is not owned by physical source"))?;
                if let Owner::Subscription(s) = &self.owner {
                    if owner != &s.id {
                        bail!("Terminal slot belongs to another subscription");
                    }
                }
            }
            0x10..=0x12 => {
                if !self.allows(if inbound {
                    "workspace.write"
                } else {
                    "workspace.read"
                }) {
                    bail!("Unauthorized file frame");
                }
                let id = binary_request_id(bytes)
                    .ok_or_else(|| anyhow!("Invalid file transfer frame"))?;
                if inbound && !self.source.transfers.lock().unwrap().contains(id) {
                    bail!("File transfer is not owned by physical source");
                }
                let body = 2 + bytes[1] as usize;
                if bytes[0] == 0x12 && bytes.len() != body {
                    bail!("File end frame has payload");
                }
                if bytes[0] == 0x10 {
                    if bytes.len() < body + 2 {
                        bail!("Truncated file metadata");
                    }
                    let length = u16::from_be_bytes([bytes[body], bytes[body + 1]]) as usize;
                    if bytes.len() != body + 2 + length {
                        bail!("Invalid file metadata length");
                    }
                    let _: Value = serde_json::from_slice(&bytes[body + 2..])?;
                }
            }
            0x30 => {
                super::decode_host_frame(bytes)?;
            }
            _ => bail!("Unknown binary opcode"),
        }
        Ok(())
    }
    pub(crate) fn detach(&self) {
        self.source.cancel.cancel();
        self.source.subscriptions.lock().unwrap().clear();
        for token in self
            .source
            .operations
            .lock()
            .unwrap()
            .drain()
            .map(|(_, t)| t)
        {
            token.cancel();
        }
        self.source.slots.lock().unwrap().clear();
        self.source.transfers.lock().unwrap().clear();
    }
    pub(crate) async fn protocol_failure(
        &self,
        message: &Value,
        code: &str,
        error: &str,
    ) -> Result<()> {
        let reply = match request_id(message) {
            Some(id) => {
                json!({"type":"rpc_error","payload":{"requestId":id,"requestType":message["type"],"code":code,"error":error}})
            }
            None => json!({"type":"status","payload":{"status":"error","message":error}}),
        };
        self.queue(OutboundFrame::Text(serde_json::to_string(
            &json!({"type":"session","message":reply}),
        )?))
        .await
    }
    pub(crate) fn register_events(
        &self,
        subscription: &OwnedSubscription,
        events: HashSet<String>,
        notifications: bool,
    ) {
        self.source
            .events
            .lock()
            .unwrap()
            .insert(subscription.id().to_owned(), (events, notifications));
    }
    pub fn wants_event(&self, event: &str) -> bool {
        if event == "workspace_setup_progress"
            && has_capability(self.capabilities(), "workspace_setup_observation")
        {
            return false;
        }
        let subscriptions = self.source.subscriptions.lock().unwrap();
        if self
            .source
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|(id, (events, _))| {
                events.contains(event)
                    && subscriptions
                        .get(id)
                        .is_some_and(|s| !s.cancel.is_cancelled())
            })
        {
            return true;
        }
        !self.is_modern() && legacy_wants_event(event, self.capabilities())
    }
    pub fn wants_notification(&self, event: &str) -> bool {
        if !self.is_live() || !self.wants_event(event) {
            return false;
        }
        if !self.is_modern() {
            return true;
        }
        let subscriptions = self.source.subscriptions.lock().unwrap();
        self.source
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|(id, (events, notifications))| {
                *notifications
                    && events.contains(event)
                    && subscriptions
                        .get(id)
                        .is_some_and(|s| !s.cancel.is_cancelled())
            })
    }
    pub(crate) async fn emit_event(&self, message: Value) -> Result<()> {
        let Some(category) = event_category(&message) else {
            bail!("Not a session event");
        };
        if !self.wants_event(&category)
            || !permissions::allows_outbound(&message, |p| self.allows(p))
        {
            return Ok(());
        }
        let owners = {
            let subscriptions = self.source.subscriptions.lock().unwrap();
            self.source
                .events
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, (events, _))| events.contains(&category))
                .filter_map(|(id, (_, notifications))| {
                    subscriptions
                        .get(id)
                        .filter(|s| !s.cancel.is_cancelled())
                        .map(|s| (s.clone(), *notifications))
                })
                .collect::<Vec<_>>()
        };
        let mut notified = false;
        for (state, notifications) in &owners {
            let subscription = OwnedSubscription {
                context: Self {
                    source: self.source.clone(),
                    owner: Owner::Subscription(state.clone()),
                },
                state: state.clone(),
            };
            let mut projected = message.clone();
            if matches!(
                category.as_str(),
                "agent_attention_required" | "terminal_attention_required"
            ) {
                let should_notify = projected["payload"]["shouldNotify"].as_bool() == Some(true)
                    && *notifications
                    && !notified;
                projected["payload"]["shouldNotify"] = Value::Bool(should_notify);
                notified |= should_notify;
            }
            subscription.emit(projected).await?;
        }
        if owners.is_empty()
            && !self.is_modern()
            && legacy_wants_event(&category, self.capabilities())
        {
            self.ensure_outbound(&message)?;
            let mut message = message;
            legacy_message(&mut message);
            self.queue(OutboundFrame::Text(serde_json::to_string(
                &json!({"type":"session","message":message}),
            )?))
            .await?;
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct OwnedSubscription {
    context: RequestContext,
    state: Arc<SubscriptionState>,
}
impl OwnedSubscription {
    pub fn id(&self) -> &str {
        &self.state.id
    }
    pub fn response_id(&self) -> &str {
        &self.state.response_id
    }
    pub fn source_id(&self) -> &str {
        self.context.source_id()
    }
    pub fn cancellation(&self) -> CancellationToken {
        self.state.cancel.clone()
    }
    pub fn context(&self) -> &RequestContext {
        &self.context
    }
    pub fn bind_terminal_slot(&self, slot: u8) -> Result<()> {
        if !self.context.is_live() {
            bail!("Subscription source is closed");
        }
        let mut slots = self.context.source.slots.lock().unwrap();
        if let Some(prior) = slots.get(&slot) {
            if prior != &self.state.id {
                bail!("Terminal slot already owned");
            }
        }
        slots.insert(slot, self.state.id.clone());
        Ok(())
    }
    pub async fn emit(&self, mut message: Value) -> Result<()> {
        self.context.ensure_outbound(&message)?;
        if self.context.is_modern()
            || matches!(
                message["type"].as_str(),
                Some("fs.file.update" | "checkout_diff_update")
            )
        {
            tag_subscription(&mut message, self.response_id())?;
        } else {
            legacy_message(&mut message);
        }
        let frame = OutboundFrame::Text(serde_json::to_string(
            &json!({"type":"session","message":message}),
        )?);
        self.deliver(frame).await
    }
    pub async fn emit_binary(&self, bytes: Bytes) -> Result<()> {
        self.context.validate_binary(&bytes, false)?;
        self.deliver(OutboundFrame::Binary(bytes)).await
    }
    async fn deliver(&self, frame: OutboundFrame) -> Result<()> {
        if !self.context.is_live() {
            bail!("Subscription source is closed");
        }
        let frame = self.context.reserve(frame)?;
        let mut d = self.state.delivery.lock().await;
        if d.priming {
            d.bytes += frame.bytes;
            d.pending.push_back(frame);
            Ok(())
        } else {
            self.context.queue_reserved(frame).await
        }
    }
    /// Call after the correlated initial snapshot response has been sent.
    pub async fn activate(&self) -> Result<()> {
        let mut d = self.state.delivery.lock().await;
        while let Some(frame) = d.pending.pop_front() {
            d.bytes -= frame.bytes;
            self.context.queue_reserved(frame).await?;
        }
        d.priming = false;
        Ok(())
    }
    pub async fn release(&self) -> Result<()> {
        self.state.cancel.cancel();
        self.context
            .source
            .subscriptions
            .lock()
            .unwrap()
            .remove(self.id());
        self.context
            .source
            .slots
            .lock()
            .unwrap()
            .retain(|_, owner| owner != self.id());
        self.context.source.events.lock().unwrap().remove(self.id());
        let mut delivery = self.state.delivery.lock().await;
        delivery.pending.clear();
        delivery.bytes = 0;
        Ok(())
    }
}

#[derive(Clone)]
pub struct OwnedOperation {
    context: RequestContext,
    state: Arc<OperationState>,
}
impl OwnedOperation {
    pub fn source_id(&self) -> &str {
        self.context.source_id()
    }
    pub fn cancellation(&self) -> CancellationToken {
        self.state.cancel.clone()
    }
    pub async fn emit(&self, message: Value) -> Result<()> {
        self.context.send(message).await
    }
    pub async fn release(&self) -> Result<()> {
        self.state.cancel.cancel();
        self.context
            .source
            .operations
            .lock()
            .unwrap()
            .remove(&self.state.id);
        Ok(())
    }
}

pub fn request_id(message: &Value) -> Option<&str> {
    message.get("requestId").and_then(Value::as_str)
}
pub fn message_type(message: &Value) -> Result<&str> {
    message
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Message type is required"))
}
pub fn has_capability(capabilities: &Value, capability: &str) -> bool {
    capabilities
        .as_array()
        .is_some_and(|a| a.iter().any(|v| v == capability))
        || capabilities.get(capability).and_then(Value::as_bool) == Some(true)
}
fn binary_request_id(bytes: &[u8]) -> Option<&str> {
    if bytes.len() < 3 || bytes[1] == 0 {
        return None;
    }
    std::str::from_utf8(bytes.get(2..2 + bytes[1] as usize)?).ok()
}
fn tag_subscription(message: &mut Value, id: &str) -> Result<()> {
    let target = if message.get("payload").is_some() {
        &mut message["payload"]
    } else {
        message
    };
    target
        .as_object_mut()
        .ok_or_else(|| anyhow!("Subscription message is not an object"))?
        .insert("subscriptionId".to_owned(), Value::String(id.to_owned()));
    Ok(())
}
fn legacy_message(message: &mut Value) {
    let kind = message["type"].as_str().unwrap_or_default().to_owned();
    if let Some(payload) = message.get_mut("payload").and_then(Value::as_object_mut) {
        if kind == "terminals_changed" {
            payload.remove("workspaceId");
        }
        if matches!(
            kind.as_str(),
            "subscribe_terminal_response"
                | "session.events.set_subscription.response"
                | "agent.timeline.set_subscription.response"
                | "workspace.label.list.response"
        ) {
            payload.remove("subscriptionId");
        }
    }
}
fn is_reply(request: &Value, reply: &Value) -> bool {
    let Some(id) = request_id(request) else {
        return false;
    };
    if reply["payload"]["requestId"].as_str() != Some(id) {
        return false;
    }
    let Some(req) = request["type"].as_str() else {
        return false;
    };
    let Some(kind) = reply["type"].as_str() else {
        return false;
    };
    match kind {
        "rpc_error" => reply["payload"]["requestType"].as_str() == Some(req),
        "pong" => req == "ping",
        "agent_archived" => req == "archive_agent_request",
        "agent_deleted" => req == "delete_agent_request",
        "agent_permission_resolved" => req == "agent_permission_response",
        "terminals_changed" => req == "subscribe_terminals_request",
        "usage.list_reports.update" => req == "usage.list_reports.request",
        "daemon.update.progress" => req == "daemon.update.request",
        "transcription_result" | "checkout_status_update" => false,
        "status" => match reply["payload"]["status"].as_str() {
            Some("agent_created") => req == "create_agent_request",
            Some("agent_create_failed") => {
                matches!(req, "create_agent_request" | "import_agent_request")
            }
            Some("agent_resumed") => matches!(req, "resume_agent_request" | "import_agent_request"),
            Some("agent_refreshed") => req == "refresh_agent_request",
            Some("restart_requested") => req == "restart_server_request",
            Some("shutdown_requested") => req == "shutdown_server_request",
            _ => false,
        },
        _ => {
            if let Some(prefix) = req.strip_suffix(".request") {
                kind.strip_suffix(".response") == Some(prefix)
            } else if let Some(prefix) = req.strip_suffix("_request") {
                kind.strip_suffix("_response") == Some(prefix)
            } else if req.contains('/') {
                kind.strip_suffix("/response") == Some(req)
            } else {
                kind.strip_suffix("_response") == Some(req)
            }
        }
    }
}

pub const SESSION_EVENTS: &[&str] = &[
    "project.update",
    "providers_snapshot_update",
    "agent_attention_required",
    "agent_permission_request",
    "agent_permission_resolved",
    "checkout_status_update",
    "script_status_update",
    "workspace_setup_progress",
    "agent.provider_subagents.update",
    "terminal_attention_required",
    "status.server_info",
    "status.daemon_config_changed",
    "status.plugin_catalog_changed",
    "status.plugin_settings_changed",
    "activity_log",
    "hub.execution.agent.update",
    "hub.execution.agent.stream",
];
pub(crate) fn event_category(message: &Value) -> Option<String> {
    let kind = message["type"].as_str()?;
    let category = if kind == "status" {
        format!("status.{}", message["payload"]["status"].as_str()?)
    } else {
        kind.to_owned()
    };
    SESSION_EVENTS
        .contains(&category.as_str())
        .then_some(category)
}
fn legacy_wants_event(event: &str, capabilities: &Value) -> bool {
    if event == "project.update" && !has_capability(capabilities, "project_updates") {
        return false;
    }
    match event {
        "project.update"
        | "providers_snapshot_update"
        | "agent_attention_required"
        | "agent_permission_request"
        | "agent_permission_resolved" => {
            !has_capability(capabilities, "explicit_event_subscriptions")
        }
        "agent.provider_subagents.update" => has_capability(capabilities, "provider_subagents"),
        _ => true,
    }
}
