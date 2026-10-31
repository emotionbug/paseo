use super::Socket;
use crate::{
    relay_crypto::{
        Frame, HANDSHAKE_TIMEOUT, KeyPair, MAX_WIRE_BYTES, SecureChannel, SecurityMode,
    },
    state::AppState,
    transport::{OutboundFrame, context::MAX_BUFFERED_BYTES},
};
use anyhow::{Context, Result, bail};
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::Instant,
};
use tokio_tungstenite::tungstenite::{Message, protocol::CloseFrame};
use tokio_util::sync::CancellationToken;

/// Frames retain their wire opcode; neither encryption nor UTF-8 guessing grants admission.
pub(super) async fn run(
    app: Weak<AppState>,
    socket: Socket,
    keypair: Arc<KeyPair>,
    policy: Arc<AtomicBool>,
    stopped: CancellationToken,
    disconnected: CancellationToken,
) -> Result<()> {
    let (sink, mut stream) = socket.split();
    let (writer, mut write_task) = Writer::start(sink);
    let mut channel = SecureChannel::responder(keypair, SecurityMode::Legacy);
    let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
    let session_cancel = CancellationToken::new();
    let (inbound, inbound_rx) = mpsc::channel(1);
    let mut inbound_rx = Some(inbound_rx);
    let (outbound_tx, mut outbound) = mpsc::channel(1);
    let mut outbound_tx = Some(outbound_tx);
    let mut application: Option<JoinHandle<Result<()>>> = None;
    let mut pending = Pending::default();
    let mut pending_send: Option<oneshot::Receiver<()>> = None;
    let result = async {
        loop {
            tokio::select! {
                _ = stopped.cancelled() => {
                    writer.close(1000, "");
                    return Ok(());
                }
                _ = disconnected.cancelled() => {
                    writer.close(1001, "Client disconnected");
                    return Ok(());
                }
                _ = writer.closed.cancelled() => bail!("Relay writer closed"),
                _ = tokio::time::sleep_until(deadline), if !channel.is_open() => {
                    // Source terminates an incomplete handshake, without inventing a close code.
                    bail!("Relay E2EE handshake timed out");
                }
                permit = inbound.reserve(), if !pending.frames.is_empty() => {
                    let permit = permit.context("Relay application closed")?;
                    permit.send(pending.pop().expect("Pending relay frame"));
                }
                acknowledged = async { pending_send.as_mut().expect("Pending relay send").await }, if pending_send.is_some() => {
                    acknowledged.context("Relay encrypted write failed")?;
                    pending_send = None;
                }
                frame = outbound.recv(), if application.is_some() && pending_send.is_none() => {
                    let Some(frame) = frame else { return Ok(()); };
                    let plaintext = match frame {
                        OutboundFrame::Text(text) => Frame::Text(text),
                        OutboundFrame::Binary(bytes) => Frame::Binary(bytes.to_vec()),
                        OutboundFrame::Ping(payload) => {
                            let (sent, acknowledged) = oneshot::channel();
                            writer.enqueue(Message::Ping(payload), Some(sent))?;
                            pending_send = Some(acknowledged);
                            continue;
                        }
                        OutboundFrame::Pong(payload) => {
                            let (sent, acknowledged) = oneshot::channel();
                            writer.enqueue(Message::Pong(payload), Some(sent))?;
                            pending_send = Some(acknowledged);
                            continue;
                        }
                        OutboundFrame::Close { code, reason } => { writer.close(code, &reason); return Ok(()); }
                        OutboundFrame::Flush(acknowledged) => {
                            // Outbound receive is gated on actual completion of the preceding
                            // sink write, including protocol ping/pong, so this has no wire form.
                            let result = if writer.closed.is_cancelled() || stopped.is_cancelled() || disconnected.is_cancelled() {
                                Err(anyhow::anyhow!("Relay socket closed"))
                            } else {
                                Ok(())
                            };
                            let _ = acknowledged.send(result);
                            continue;
                        }
                    };
                    let wire_length = channel.outbound_wire_byte_length(&plaintext)?;
                    writer.check_capacity(wire_length)?;
                    match channel.encode(plaintext) {
                        Ok(frame) => {
                            let (sent, acknowledged) = oneshot::channel();
                            writer.enqueue(wire_message(frame), Some(sent))?;
                            pending_send = Some(acknowledged);
                        }
                        Err(error) => {
                            writer.close(error.close_code, &error.reason);
                            return Err(error.into());
                        }
                    }
                }
                incoming = stream.next() => {
                    let Some(incoming) = incoming else { return Ok(()); };
                    let incoming = match incoming {
                        Err(error @ tokio_tungstenite::tungstenite::Error::Capacity(_)) => {
                            writer.close(1011, if channel.is_open() { "message exceeds relay wire limit" } else { "E2EE handshake failed" });
                            return Err(error.into());
                        }
                        incoming => incoming?,
                    };
                    let frame = match incoming {
                        Message::Text(text) => Frame::Text(text.to_string()),
                        Message::Binary(bytes) => Frame::Binary(bytes.to_vec()),
                        Message::Ping(payload) => { writer.enqueue(Message::Pong(payload), None)?; continue; }
                        Message::Pong(_) => continue,
                        Message::Close(close) => {
                            writer.enqueue(Message::Close(close), None)?;
                            return Ok(());
                        }
                        Message::Frame(_) => continue,
                    };
                    // Read on arrival, not on socket creation. Established channels keep their mode.
                    channel.set_minimum_mode(if policy.load(Ordering::Acquire) { SecurityMode::Protected } else { SecurityMode::Legacy });
                    let update = match channel.receive(frame) {
                        Ok(update) => update,
                        Err(error) => {
                            writer.close(error.close_code, &error.reason);
                            return Err(error.into());
                        }
                    };
                    if let Some(ready) = update.outbound {
                        let (sent, acknowledged) = oneshot::channel();
                        writer.enqueue(wire_message(ready), Some(sent))?;
                        // Application admission cannot overtake the plaintext security ready write.
                        tokio::select! {
                            _ = stopped.cancelled() => return Ok(()),
                            _ = disconnected.cancelled() => return Ok(()),
                            _ = tokio::time::sleep_until(deadline), if application.is_none() => bail!("Relay E2EE handshake timed out"),
                            result = acknowledged => result.context("Relay security ready write failed")?,
                        }
                        channel.acknowledge_ready();
                    }
                    if update.opened.is_some() {
                        let transport = app.upgrade().context("Daemon shutting down")?.transport.clone();
                        let incoming = inbound_rx.take().context("Relay session already attached")?;
                        let outgoing = outbound_tx.take().context("Relay session already attached")?;
                        let cancel = session_cancel.clone();
                        application = Some(tokio::spawn(async move {
                            // None is intentional: E2EE is not an authenticated owner Admission.
                            transport.run_channel(incoming, outgoing, cancel, None).await
                        }));
                    }
                    if let Some(message) = update.message {
                        pending.push(match message {
                            Frame::Text(text) => OutboundFrame::Text(text),
                            Frame::Binary(bytes) => OutboundFrame::Binary(Bytes::from(bytes)),
                        })?;
                    }
                }
            }
        }
    }.await;
    channel.close();
    session_cancel.cancel();
    if !writer.closing.load(Ordering::Acquire) {
        write_task.abort();
    }
    drop(writer);
    // Drain an explicit protocol close, but never leave shutdown held by a dead peer.
    if tokio::time::timeout(Duration::from_secs(1), &mut write_task)
        .await
        .is_err()
    {
        write_task.abort();
        let _ = write_task.await;
    }
    if let Some(mut task) = application {
        if tokio::time::timeout(Duration::from_secs(1), &mut task)
            .await
            .is_err()
        {
            task.abort();
            let _ = task.await;
        }
    }
    result
}

fn wire_message(frame: Frame) -> Message {
    match frame {
        Frame::Text(text) => Message::Text(text.into()),
        Frame::Binary(bytes) => Message::Binary(bytes.into()),
    }
}

#[derive(Default)]
pub(super) struct Pending {
    frames: VecDeque<OutboundFrame>,
    bytes: usize,
}
impl Pending {
    pub(super) fn push(&mut self, frame: OutboundFrame) -> Result<()> {
        let next = self
            .bytes
            .checked_add(frame.byte_len())
            .context("Relay plaintext queue overflow")?;
        // The count bound also covers zero-byte malicious frames.
        if next > MAX_BUFFERED_BYTES || self.frames.len() >= 4096 {
            bail!("Relay plaintext queue high water");
        }
        self.bytes = next;
        self.frames.push_back(frame);
        Ok(())
    }
    pub(super) fn pop(&mut self) -> Option<OutboundFrame> {
        let frame = self.frames.pop_front()?;
        self.bytes -= frame.byte_len();
        Some(frame)
    }
}

struct Queued {
    message: Option<Message>,
    bytes: usize,
    buffered: Arc<AtomicUsize>,
    acknowledged: Option<oneshot::Sender<()>>,
}
impl Drop for Queued {
    fn drop(&mut self) {
        self.buffered.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}
struct Writer {
    tx: mpsc::Sender<Queued>,
    buffered: Arc<AtomicUsize>,
    closed: CancellationToken,
    closing: AtomicBool,
}
impl Writer {
    fn start(mut sink: futures_util::stream::SplitSink<Socket, Message>) -> (Self, JoinHandle<()>) {
        let (tx, mut rx) = mpsc::channel::<Queued>(256);
        let buffered = Arc::new(AtomicUsize::new(0));
        let closed = CancellationToken::new();
        let finished = closed.clone();
        let task = tokio::spawn(async move {
            while let Some(mut queued) = rx.recv().await {
                let message = queued.message.take().expect("Queued relay wire message");
                let closing = message.is_close();
                if let Err(error) = sink.send(message).await {
                    tracing::debug!(%error, "Relay wire send failed");
                    break;
                }
                if let Some(sent) = queued.acknowledged.take() {
                    let _ = sent.send(());
                }
                if closing {
                    break;
                }
            }
            finished.cancel();
        });
        (
            Self {
                tx,
                buffered,
                closed,
                closing: AtomicBool::new(false),
            },
            task,
        )
    }

    fn check_capacity(&self, bytes: usize) -> Result<()> {
        if bytes > MAX_WIRE_BYTES {
            bail!("Relay ciphertext exceeds wire limit");
        }
        if self
            .buffered
            .load(Ordering::Acquire)
            .checked_add(bytes)
            .is_none_or(|n| n > MAX_BUFFERED_BYTES)
        {
            bail!("Relay ciphertext queue high water");
        }
        Ok(())
    }

    fn enqueue(&self, message: Message, acknowledged: Option<oneshot::Sender<()>>) -> Result<()> {
        let bytes = message.len();
        let closing = message.is_close();
        self.check_capacity(bytes)?;
        self.buffered.fetch_add(bytes, Ordering::AcqRel);
        let queued = Queued {
            message: Some(message),
            bytes,
            buffered: self.buffered.clone(),
            acknowledged,
        };
        self.tx
            .try_send(queued)
            .map_err(|_| anyhow::anyhow!("Relay ciphertext queue unavailable"))?;
        if closing {
            self.closing.store(true, Ordering::Release);
        }
        Ok(())
    }

    fn close(&self, code: u16, reason: &str) {
        // Reasons come from protocol constants or the application, not peer-controlled payloads.
        let mut end = reason.len().min(123);
        while !reason.is_char_boundary(end) {
            end -= 1;
        }
        let _ = self.enqueue(
            Message::Close(Some(CloseFrame {
                code: code.into(),
                reason: reason[..end].to_owned().into(),
            })),
            None,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn writer() -> (Writer, mpsc::Receiver<Queued>) {
        let (tx, rx) = mpsc::channel(1);
        (
            Writer {
                tx,
                buffered: Arc::new(AtomicUsize::new(0)),
                closed: CancellationToken::new(),
                closing: AtomicBool::new(false),
            },
            rx,
        )
    }

    #[test]
    fn wire_high_water_includes_ciphertext_expansion() {
        let (writer, _rx) = writer();
        writer
            .buffered
            .store(MAX_BUFFERED_BYTES - 100, Ordering::Release);
        assert!(writer.check_capacity(100).is_ok());
        assert!(writer.check_capacity(101).is_err());
        writer.buffered.store(0, Ordering::Release);
        assert!(writer.check_capacity(MAX_WIRE_BYTES).is_ok());
        assert!(writer.check_capacity(MAX_WIRE_BYTES + 1).is_err());
    }

    #[test]
    fn failed_queue_reservation_is_released_and_inflight_stays_accounted() {
        let (writer, mut rx) = writer();
        writer
            .enqueue(Message::Binary(Bytes::from_static(b"abc")), None)
            .unwrap();
        assert!(
            writer
                .enqueue(Message::Binary(Bytes::from_static(b"defg")), None)
                .is_err()
        );
        assert_eq!(writer.buffered.load(Ordering::Acquire), 3);
        let mut queued = rx.try_recv().unwrap();
        let message = queued.message.take().unwrap();
        assert_eq!(writer.buffered.load(Ordering::Acquire), 3);
        drop(message);
        drop(queued);
        assert_eq!(writer.buffered.load(Ordering::Acquire), 0);
    }

    #[test]
    fn close_reasons_are_utf8_bounded_and_keep_policy_code() {
        let (writer, mut rx) = writer();
        writer.close(1008, &"é".repeat(100));
        assert!(writer.closing.load(Ordering::Acquire));
        let mut queued = rx.try_recv().unwrap();
        let Message::Close(Some(close)) = queued.message.take().unwrap() else {
            panic!("Expected protocol close");
        };
        assert_eq!(u16::from(close.code), 1008);
        assert!(close.reason.len() <= 123);
        assert_eq!(close.reason.len(), 122);
    }
}
