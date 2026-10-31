use super::fs;
use anyhow::Result;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde_json::Value;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Weak},
    time::Duration,
};
use tokio::sync::{Mutex, mpsc, watch};
use tokio_util::sync::CancellationToken;

pub struct Observers {
    targets: Mutex<HashMap<PathBuf, Weak<Target>>>,
    cancel: CancellationToken,
}
struct Target {
    sender: watch::Sender<Value>,
}
pub struct Observation {
    _target: Arc<Target>,
    pub receiver: watch::Receiver<Value>,
}
impl Observers {
    pub fn new() -> Self {
        Self {
            targets: Mutex::new(HashMap::new()),
            cancel: CancellationToken::new(),
        }
    }
    pub fn shutdown(&self) {
        self.cancel.cancel();
    }
    pub async fn subscribe(&self, cwd: String, path: String, home: PathBuf) -> Result<Observation> {
        let (c, p, h) = (cwd.clone(), path.clone(), home.clone());
        let target =
            tokio::task::spawn_blocking(move || fs::scoped(&c, &p, &h).map(|s| s.resolved))
                .await??;
        let mut targets = self.targets.lock().await;
        targets.retain(|_, value| value.strong_count() > 0);
        if let Some(shared) = targets.get(&target).and_then(Weak::upgrade) {
            let initial =
                tokio::task::spawn_blocking(move || fs::version(&cwd, &path, &home)).await?;
            // The source refreshes the shared fingerprint for a joining subscriber
            // without notifying the subscribers that are already listening.
            shared.sender.send_if_modified(|current| {
                *current = initial;
                false
            });
            return Ok(Observation {
                _target: shared.clone(),
                receiver: shared.sender.subscribe(),
            });
        }
        let (events, mut event_rx) = mpsc::channel::<bool>(1);
        let watched = target.clone();
        let failed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let failure = failed.clone();
        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                let relevant = match &event {
                    Ok(event) => {
                        event.paths.is_empty() || event.paths.iter().any(|path| path == &watched)
                    }
                    Err(_) => true,
                };
                if event.is_err() {
                    failure.store(true, std::sync::atomic::Ordering::Release);
                }
                if relevant {
                    let _ = events.try_send(event.is_err());
                }
            })
            .ok();
        if watcher.as_mut().is_some_and(|watcher| {
            watcher
                .watch(
                    target.parent().unwrap_or(&target),
                    RecursiveMode::NonRecursive,
                )
                .is_err()
        }) {
            watcher = None;
        }
        let (c, p, h) = (cwd.clone(), path.clone(), home.clone());
        let initial = tokio::task::spawn_blocking(move || fs::version(&c, &p, &h)).await?;
        let (sender, receiver) = watch::channel(initial);
        let shared = Arc::new(Target {
            sender: sender.clone(),
        });
        targets.insert(target, Arc::downgrade(&shared));
        let cancel = self.cancel.child_token();
        tokio::spawn(async move {
            let mut watcher: Option<RecommendedWatcher> = watcher;
            let mut fallback = tokio::time::interval(Duration::from_secs(5));
            fallback.tick().await;
            loop {
                tokio::select! {biased;
                    _=cancel.cancelled()=>break,
                    _=sender.closed()=>break,
                    event=event_rx.recv(),if watcher.is_some()=>{
                        if failed.load(std::sync::atomic::Ordering::Acquire){watcher=None;continue;}
                        match event {Some(true)|None=>{watcher=None;continue;},Some(false)=>{}}
                        loop {tokio::select!{biased;
                            _=cancel.cancelled()=>return,
                            _=sender.closed()=>return,
                            event=event_rx.recv()=>{if event!=Some(false){watcher=None;break;}},
                            _=tokio::time::sleep(Duration::from_millis(50))=>break,
                        }}
                    },
                    _=fallback.tick(),if watcher.is_none()=>{},
                }
                let (c, p, h) = (cwd.clone(), path.clone(), home.clone());
                let version =
                    match tokio::task::spawn_blocking(move || fs::version(&c, &p, &h)).await {
                        Ok(value) => value,
                        Err(_) => break,
                    };
                let changed = {
                    let prior = sender.borrow();
                    fingerprint(&prior) != fingerprint(&version)
                };
                if changed && sender.send(version).is_err() {
                    break;
                }
            }
        });
        Ok(Observation {
            _target: shared,
            receiver,
        })
    }
}
fn fingerprint(value: &Value) -> String {
    if value["status"] == "ready" {
        format!("ready:{}", value["revision"])
    } else {
        value["status"].to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn replacement_missing_and_last_release() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.md");
        tokio::fs::write(&path, "first").await.unwrap();
        let observers = Observers::new();
        let mut observed = observers
            .subscribe(
                dir.path().to_string_lossy().into_owned(),
                "notes.md".into(),
                dir.path().to_owned(),
            )
            .await
            .unwrap();
        let original = observed.receiver.borrow_and_update()["revision"].clone();
        let replacement = dir.path().join("replacement");
        tokio::fs::write(&replacement, "second version")
            .await
            .unwrap();
        tokio::fs::rename(&replacement, &path).await.unwrap();
        tokio::time::timeout(Duration::from_secs(8), observed.receiver.changed())
            .await
            .unwrap()
            .unwrap();
        assert_ne!(observed.receiver.borrow_and_update()["revision"], original);
        tokio::fs::remove_file(&path).await.unwrap();
        tokio::time::timeout(Duration::from_secs(8), observed.receiver.changed())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(observed.receiver.borrow_and_update()["status"], "missing");
        drop(observed);
        assert!(
            observers
                .targets
                .lock()
                .await
                .values()
                .all(|target| target.strong_count() == 0)
        );
        observers.shutdown();
    }
}
