use anyhow::{Result, bail};
use std::{
    collections::VecDeque,
    sync::{LazyLock, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
struct State {
    active: usize,
    concurrency: usize,
    rate: usize,
    starts: VecDeque<Instant>,
}
static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| {
    Mutex::new(State {
        active: 0,
        concurrency: 8,
        rate: 64,
        starts: VecDeque::new(),
    })
});
static CHANGED: Notify = Notify::const_new();
pub struct Permit;
impl Drop for Permit {
    fn drop(&mut self) {
        if let Ok(mut state) = STATE.lock() {
            state.active = state.active.saturating_sub(1);
        }
        CHANGED.notify_waiters();
    }
}
pub fn configure(rate: usize, concurrency: usize) -> Result<()> {
    if rate == 0 || concurrency == 0 {
        bail!("Git process limits must be positive");
    }
    let mut state = STATE
        .lock()
        .map_err(|_| anyhow::anyhow!("Git scheduler poisoned"))?;
    state.rate = rate;
    state.concurrency = concurrency;
    drop(state);
    CHANGED.notify_waiters();
    Ok(())
}
pub async fn acquire(cancel: &CancellationToken) -> Result<Permit> {
    loop {
        if cancel.is_cancelled() {
            bail!("Git command cancelled before admission");
        }
        let changed = CHANGED.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        let delay = {
            let mut state = STATE
                .lock()
                .map_err(|_| anyhow::anyhow!("Git scheduler poisoned"))?;
            let now = Instant::now();
            while state
                .starts
                .front()
                .is_some_and(|s| now.duration_since(*s) >= Duration::from_secs(1))
            {
                state.starts.pop_front();
            }
            if state.active < state.concurrency && state.starts.len() < state.rate {
                state.active += 1;
                state.starts.push_back(now);
                return Ok(Permit);
            }
            state
                .starts
                .front()
                .map(|s| Duration::from_secs(1).saturating_sub(now.duration_since(*s)))
                .unwrap_or(Duration::from_secs(1))
        };
        tokio::select! {
            _ = cancel.cancelled() => bail!("Git command cancelled before admission"),
            _ = &mut changed => {},
            _ = tokio::time::sleep(delay) => {}
        }
    }
}
