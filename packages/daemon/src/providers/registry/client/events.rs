use anyhow::Result;
use serde_json::Value;
use tokio::{
    sync::{Mutex, mpsc},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use super::with_provider;
use crate::providers::api::{AgentSession, EventSink};

/// Owns the forwarding task for exactly one public session. Dropping a canceled
/// create/resume/import future also drops this guard, never detaching the pump.
pub(super) struct EventBridge {
    cancel: CancellationToken,
    stop: CancellationToken,
    task: Mutex<Option<JoinHandle<Result<()>>>>,
}

impl EventBridge {
    pub(super) fn new(provider: String, destination: EventSink) -> (Self, EventSink) {
        let cancel = destination.cancellation().child_token();
        let stop = CancellationToken::new();
        let (sink, mut receiver) = EventSink::channel(256, cancel.clone());
        let task_cancel = cancel.clone();
        let task_stop = stop.clone();
        let task = tokio::spawn(async move {
            let _cancel_on_exit = task_cancel.clone().drop_guard();
            loop {
                let event = tokio::select! {
                    biased;
                    _ = task_cancel.cancelled() => return Ok(()),
                    _ = task_stop.cancelled() => break,
                    event = receiver.recv() => event,
                };
                let Some(event) = event else {
                    return Ok(());
                };
                destination.emit(with_provider(event, &provider)).await?;
            }
            // close() stops new sends but retains every already accepted event.
            receiver.close();
            forward(&provider, &destination, &mut receiver).await
        });
        (
            Self {
                cancel,
                stop,
                task: Mutex::new(Some(task)),
            },
            sink,
        )
    }

    pub(super) async fn finish(&self) -> Result<()> {
        let mut task = self.task.lock().await;
        self.stop.cancel();
        let result = match task.as_mut() {
            Some(task) => task.await,
            None => Ok(Ok(())),
        };
        task.take();
        result?
    }
}

impl Drop for EventBridge {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(task) = self.task.get_mut().take() {
            task.abort();
        }
    }
}

async fn forward(
    provider: &str,
    destination: &EventSink,
    receiver: &mut mpsc::Receiver<Value>,
) -> Result<()> {
    while let Some(event) = receiver.recv().await {
        destination.emit(with_provider(event, provider)).await?;
    }
    Ok(())
}

pub(super) async fn stream_history(
    inner: &dyn AgentSession,
    provider: &str,
    destination: EventSink,
) -> Result<()> {
    let cancel = destination.cancellation().child_token();
    let _cancel_on_exit = cancel.clone().drop_guard();
    let (sink, mut receiver) = EventSink::channel(256, cancel);
    // Join the producer and consumer in this future: success means the complete
    // history has reached its owner, including the final buffered events.
    tokio::try_join!(
        inner.stream_history(sink),
        forward(provider, &destination, &mut receiver)
    )?;
    Ok(())
}
