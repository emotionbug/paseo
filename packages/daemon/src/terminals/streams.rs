use super::{Terminal, TerminalEvent, TerminalService, dimensions, required};
use crate::transport::{OwnedSubscription, RequestContext};
use anyhow::{Result, anyhow};
use bytes::{BufMut, Bytes, BytesMut};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub(super) struct Registration {
    pub terminal_id: String,
    pub subscription_id: String,
    pub session_id: String,
    pub cancel: CancellationToken,
}
type Registrations = Arc<Mutex<HashMap<(String, u8), Registration>>>;

pub(super) async fn subscribe(
    service: &TerminalService,
    context: &RequestContext,
    message: &Value,
) -> Result<()> {
    let id = required(message, "terminalId")?;
    let terminal = match service.get(id).await {
        Ok(terminal) => terminal,
        Err(error) => {
            context.send(json!({"type":"subscribe_terminal_response","payload":{"terminalId":id,"requestId":message["requestId"],"error":error.to_string()}})).await?;
            return Ok(());
        }
    };
    let owner = context.begin_subscription(
        "terminal-output",
        None,
        Some(&format!("terminal-output:{id}")),
    )?;
    let slot = {
        let mut registrations = service.streams.lock().unwrap();
        registrations.retain(|_, v| !v.cancel.is_cancelled());
        let slot = (0..=255u8).find(|slot| {
            !registrations.iter().any(|((_, used), registration)| {
                used == slot && registration.session_id == context.session_id()
            })
        });
        if let Some(slot) = slot {
            registrations.insert(
                (context.source_id().into(), slot),
                Registration {
                    terminal_id: id.into(),
                    subscription_id: owner.id().into(),
                    session_id: context.session_id().into(),
                    cancel: owner.cancellation(),
                },
            );
        }
        slot
    };
    let Some(slot) = slot else {
        owner.release().await?;
        context.send(json!({"type":"subscribe_terminal_response","payload":{"terminalId":id,"requestId":message["requestId"],"error":"No terminal stream slots available"}})).await?;
        return Ok(());
    };
    let result=async {
        owner.bind_terminal_slot(slot)?;
        service.track_source(context);
        if !context.is_modern() && message["restore"].get("size").is_some(){let(rows,cols)=dimensions(&message["restore"]["size"])?;terminal.resize(context.source_id(),rows,cols,true).await?;}
        let receiver=terminal.subscribe();
        let mut payload=json!({"terminalId":id,"slot":slot,"error":null,"requestId":message["requestId"]});
        if context.is_modern(){payload["subscriptionId"]=json!(owner.response_id());}
        context.send(json!({"type":"subscribe_terminal_response","payload":payload})).await?;
        owner.activate().await?;
        let mode=message["restore"]["mode"].as_str().map(str::to_owned);
        let scrollback=message["restore"]["scrollbackLines"].as_u64().unwrap_or(200).min(500) as usize;
        let wrapped=has_wrap(context.capabilities());let registrations=service.streams.clone();
        let runner_owner=owner.clone();let runner_terminal=terminal.clone();
        tokio::spawn(async move {
            let result=run_stream(&runner_terminal,&runner_owner,slot,receiver,mode,scrollback,wrapped).await;
            if let Err(error)=result{if !runner_owner.cancellation().is_cancelled(){let _=runner_owner.emit(json!({"type":"terminal_stream_error","payload":{"terminalId":runner_terminal.id,"error":error.to_string()}})).await;}}
            cleanup(&registrations,&runner_owner,slot);
            let _=runner_owner.release().await;
        });
        Ok::<(),anyhow::Error>(())
    }.await;
    if result.is_err() {
        cleanup(&service.streams, &owner, slot);
        let _ = owner.release().await;
    }
    result
}
fn cleanup(registrations: &Registrations, owner: &OwnedSubscription, slot: u8) {
    let mut registrations = registrations.lock().unwrap();
    let key = (owner.source_id().to_owned(), slot);
    if registrations
        .get(&key)
        .is_some_and(|r| r.subscription_id == owner.id())
    {
        registrations.remove(&key);
    }
}
async fn snapshot(
    terminal: &Terminal,
    owner: &OwnedSubscription,
    slot: u8,
    mode: Option<&str>,
    scrollback: usize,
    wrapped: bool,
) -> Result<u64> {
    if mode == Some("live") {
        let preamble = terminal.replay_preamble().await?;
        if !preamble.is_empty() {
            owner
                .emit_binary(frame(1, slot, preamble.as_bytes()))
                .await?;
        }
        return Ok(0);
    }
    let (revision, opcode, data) = terminal.restore_frame(mode, scrollback, wrapped)?;
    owner.emit_binary(frame(opcode, slot, &data)).await?;
    if mode.is_none() {
        let preamble = terminal.replay_preamble().await?;
        if !preamble.is_empty() {
            owner
                .emit_binary(frame(1, slot, preamble.as_bytes()))
                .await?;
        }
    }
    Ok(revision)
}
async fn run_stream(
    terminal: &Terminal,
    owner: &OwnedSubscription,
    slot: u8,
    mut receiver: broadcast::Receiver<TerminalEvent>,
    mut mode: Option<String>,
    scrollback: usize,
    wrapped: bool,
) -> Result<()> {
    let cancel = owner.cancellation();
    let mut revision =
        snapshot(terminal, owner, slot, mode.as_deref(), scrollback, wrapped).await?;
    if terminal.is_exited() && receiver.is_empty() {
        terminal.wait().await?;
        owner
            .emit(json!({"type":"terminal_stream_exit","payload":{"terminalId":terminal.id}}))
            .await?;
        return Ok(());
    }
    let mut pending = BytesMut::new();
    let mut produced = 0usize;
    let mut timer = tokio::time::interval(Duration::from_millis(8));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let mut flush_due = false;
        tokio::select! {biased;
            _=cancel.cancelled()=>return Ok(()),
            event=receiver.recv()=>match event {
                Ok(TerminalEvent::Output{revision:next,data})=>{if next>revision{produced=produced.saturating_add(data.len());pending.extend_from_slice(&data);}},
                Ok(TerminalEvent::Exit(_))=>{flush(owner,slot,&mut pending).await?;owner.emit(json!({"type":"terminal_stream_exit","payload":{"terminalId":terminal.id}})).await?;return Ok(());},
                Ok(TerminalEvent::Error(error))=>return Err(anyhow!(error)),
                Ok(_)=>{},
                Err(broadcast::error::RecvError::Lagged(_))=>{
                    pending.clear();if mode.as_deref()==Some("live"){mode=Some("visible-snapshot".into());}
                    revision=snapshot(terminal,owner,slot,mode.as_deref(),scrollback,wrapped).await?;produced=0;
                },
                Err(broadcast::error::RecvError::Closed)=>return Err(anyhow!("Terminal event owner closed")),
            },
            _=timer.tick()=>{flush_due=true;}
        }
        if !pending.is_empty() {
            if produced > 256 * 1024
                && (owner.context().is_routed()
                    || owner.context().buffered_amount() > 4 * 1024 * 1024)
            {
                pending.clear();
                if mode.as_deref() == Some("live") {
                    mode = Some("visible-snapshot".into());
                }
                revision =
                    snapshot(terminal, owner, slot, mode.as_deref(), scrollback, wrapped).await?;
                produced = 0;
            } else if pending.len() >= 16 * 1024 || flush_due {
                flush(owner, slot, &mut pending).await?;
            }
        }
        if terminal.is_exited() && receiver.is_empty() {
            flush(owner, slot, &mut pending).await?;
            terminal.wait().await?;
            owner
                .emit(json!({"type":"terminal_stream_exit","payload":{"terminalId":terminal.id}}))
                .await?;
            return Ok(());
        }
    }
}
async fn flush(owner: &OwnedSubscription, slot: u8, pending: &mut BytesMut) -> Result<()> {
    if !pending.is_empty() {
        let mut bytes = BytesMut::with_capacity(pending.len() + 2);
        bytes.put_u8(1);
        bytes.put_u8(slot);
        bytes.extend_from_slice(pending);
        pending.clear();
        owner.emit_binary(bytes.freeze()).await?;
    }
    Ok(())
}
pub(super) fn frame(opcode: u8, slot: u8, data: &[u8]) -> Bytes {
    let mut bytes = BytesMut::with_capacity(data.len() + 2);
    bytes.put_u8(opcode);
    bytes.put_u8(slot);
    bytes.extend_from_slice(data);
    bytes.freeze()
}
fn has_wrap(capabilities: &Value) -> bool {
    capabilities
        .get("terminal_reflowable_snapshot")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || capabilities
            .as_array()
            .is_some_and(|caps| caps.iter().any(|cap| cap == "terminal_reflowable_snapshot"))
}
pub(super) fn directory_slot(message: &Value) -> String {
    let cwd = message["cwd"].as_str().unwrap_or("");
    match message["workspaceId"].as_str() {
        Some(id) => format!("terminal-directory:{id}::{cwd}"),
        None => format!("terminal-directory:{cwd}"),
    }
}
pub(super) async fn subscribe_directory(
    service: &TerminalService,
    context: &RequestContext,
    message: &Value,
) -> Result<()> {
    let cwd = required(message, "cwd")?.to_owned();
    let workspace = message["workspaceId"].as_str().map(str::to_owned);
    let owner =
        context.begin_subscription("terminal-directories", None, Some(&directory_slot(message)))?;
    let mut changed = service.changed.subscribe();
    let terminals = service.list(workspace.as_deref(), Some(&cwd)).await;
    let mut payload = directory_payload(&cwd, workspace.as_deref(), terminals);
    if let Some(request) = message.get("requestId") {
        payload["requestId"] = request.clone();
    }
    // terminals_changed is the initial directory snapshot, not a separate response.
    owner
        .emit(json!({"type":"terminals_changed","payload":payload}))
        .await?;
    owner.activate().await?;
    let weak = service.app.clone();
    tokio::spawn(async move {
        let cancel = owner.cancellation();
        loop {
            tokio::select! {biased;_=cancel.cancelled()=>break,event=changed.recv()=>{if matches!(event,Err(broadcast::error::RecvError::Closed)){break;}let Some(app)=weak.upgrade()else{break};let terminals=app.terminals.list(workspace.as_deref(),Some(&cwd)).await;drop(app);if owner.emit(json!({"type":"terminals_changed","payload":directory_payload(&cwd,workspace.as_deref(),terminals)})).await.is_err(){break;}}}
        }
        let _ = owner.release().await;
    });
    Ok(())
}
fn directory_payload(cwd: &str, workspace: Option<&str>, mut terminals: Vec<Value>) -> Value {
    for terminal in &mut terminals {
        if let Some(object) = terminal.as_object_mut() {
            object.remove("cwd");
        }
    }
    let mut payload = json!({"cwd":cwd,"terminals":terminals});
    if let Some(workspace) = workspace {
        payload["workspaceId"] = json!(workspace);
    }
    payload
}
