use crate::{state::AppState, transport::RequestContext};
use anyhow::{Context, Result, bail};
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex as StdMutex, Weak},
};
use tokio::{
    sync::{Mutex, OnceCell, Semaphore},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

const LEASE_MS: i64 = 48 * 60 * 60 * 1000;
const EXPO_URL: &str = "https://exp.host/--/api/v2/push/send";
#[derive(Clone)]
struct Lease {
    token: String,
    expires_at: i64,
}
#[derive(Default)]
struct Store {
    initialized: bool,
    path: PathBuf,
    leases: Vec<Lease>,
}
struct SourceToken {
    token: String,
    lifetime: CancellationToken,
}
struct Inner {
    app: Weak<AppState>,
    store: Mutex<Store>,
    sources: StdMutex<HashMap<String, SourceToken>>,
    http: OnceCell<reqwest::Client>,
    capacity: Arc<Semaphore>,
    tasks: Mutex<JoinSet<()>>,
    cancel: CancellationToken,
}
#[derive(Clone)]
pub(super) struct PushNotifications {
    inner: Arc<Inner>,
}
impl PushNotifications {
    pub fn new(app: Weak<AppState>) -> Self {
        Self {
            inner: Arc::new(Inner {
                app,
                store: Mutex::new(Store::default()),
                sources: StdMutex::new(HashMap::new()),
                http: OnceCell::new(),
                capacity: Arc::new(Semaphore::new(64)),
                tasks: Mutex::new(JoinSet::new()),
                cancel: CancellationToken::new(),
            }),
        }
    }
    pub async fn initialize(&self) -> Result<()> {
        let mut store = self.inner.store.lock().await;
        if store.initialized {
            return Ok(());
        }
        let path = self
            .inner
            .app
            .upgrade()
            .context("Daemon is shutting down")?
            .home
            .join("push-tokens.json");
        let path = match tokio::fs::canonicalize(&path).await {
            Ok(path) => path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => path,
            Err(error) => return Err(error.into()),
        };
        let parsed = match super::super::storage::read_optional(&path).await? {
            Some(bytes) => {
                private_mode(&path, 0o600).await;
                serde_json::from_slice::<Value>(&bytes).context("Invalid push token store")?
            }
            None => json!({}),
        };
        let (leases, migrated) = load_leases(&parsed, Utc::now().timestamp_millis());
        if migrated {
            persist(&path, &leases).await?;
        }
        self.inner
            .http
            .get_or_try_init(|| async {
                reqwest::Client::builder()
                    .build()
                    .context("Creating push HTTP client")
            })
            .await?;
        store.path = path;
        store.leases = leases;
        store.initialized = true;
        Ok(())
    }
    pub async fn heartbeat(&self, context: &RequestContext) -> Result<()> {
        if !context.is_live() {
            return Ok(());
        }
        let token = {
            let mut sources = self.inner.sources.lock().unwrap_or_else(|e| e.into_inner());
            sources.retain(|_, entry| !entry.lifetime.is_cancelled());
            sources
                .get(context.source_id())
                .map(|entry| entry.token.clone())
        };
        if let Some(token) = token {
            self.renew(&token).await?;
        }
        Ok(())
    }
    pub async fn handle(&self, context: &RequestContext, message: &Value) -> Result<bool> {
        match message["type"].as_str() {
            Some("register_push_token") => {
                if !context.is_live() {
                    bail!("Physical source is closed");
                }
                let token = message["token"]
                    .as_str()
                    .context("Push token must be a string")?;
                {
                    let mut sources = self.inner.sources.lock().unwrap_or_else(|e| e.into_inner());
                    sources.retain(|_, entry| !entry.lifetime.is_cancelled());
                    sources.insert(
                        context.source_id().to_owned(),
                        SourceToken {
                            token: token.to_owned(),
                            lifetime: context.cancellation(),
                        },
                    );
                }
                self.renew(token).await?;
                Ok(true)
            }
            Some("push.unregister.request") => {
                let token = message["token"]
                    .as_str()
                    .context("Push token must be a string")?;
                self.revoke(token).await?;
                {
                    let mut sources = self.inner.sources.lock().unwrap_or_else(|e| e.into_inner());
                    if sources
                        .get(context.source_id())
                        .is_some_and(|entry| entry.token.trim() == token.trim())
                    {
                        sources.remove(context.source_id());
                    }
                }
                context.send(json!({"type":"push.unregister.response","payload":{"requestId":message["requestId"]}})).await?;
                Ok(true)
            }
            Some("client_heartbeat") => {
                self.heartbeat(context).await?;
                Ok(false)
            }
            _ => Ok(false),
        }
    }
    async fn renew(&self, token: &str) -> Result<()> {
        let token = token.trim();
        if token.is_empty() {
            return Ok(());
        }
        self.initialize().await?;
        let now = Utc::now().timestamp_millis();
        let mut store = self.inner.store.lock().await;
        if store.leases.iter().any(|lease| {
            lease.token == token && lease.expires_at.saturating_sub(now) > LEASE_MS / 2
        }) {
            return Ok(());
        }
        let mut next = store.leases.clone();
        put_lease(&mut next, token.to_owned(), now + LEASE_MS);
        persist(&store.path, &next).await?;
        store.leases = next;
        Ok(())
    }
    async fn revoke(&self, token: &str) -> Result<()> {
        let token = token.trim();
        if token.is_empty() {
            return Ok(());
        }
        self.initialize().await?;
        let mut store = self.inner.store.lock().await;
        if !store.leases.iter().any(|lease| lease.token == token) {
            return Ok(());
        }
        let next = store
            .leases
            .iter()
            .filter(|lease| lease.token != token)
            .cloned()
            .collect::<Vec<_>>();
        persist(&store.path, &next).await?;
        store.leases = next;
        Ok(())
    }
    async fn active_tokens(&self) -> Result<Vec<String>> {
        self.initialize().await?;
        let now = Utc::now().timestamp_millis();
        let mut store = self.inner.store.lock().await;
        let active = store
            .leases
            .iter()
            .filter(|lease| lease.expires_at > now)
            .cloned()
            .collect::<Vec<_>>();
        let tokens = active.iter().map(|lease| lease.token.clone()).collect();
        if active.len() != store.leases.len() {
            match persist(&store.path, &active).await {
                Ok(()) => store.leases = active,
                Err(error) => tracing::warn!(?error, "Failed to prune expired push tokens"),
            }
        }
        Ok(tokens)
    }
    pub async fn enqueue(&self, payload: Value) -> Result<()> {
        let tokens = self.active_tokens().await?;
        if tokens.is_empty() {
            return Ok(());
        }
        let permit = tokio::select! {
            _ = self.inner.cancel.cancelled() => bail!("Push notifications are closed"),
            permit = self.inner.capacity.clone().acquire_owned() => permit.context("Push notifications are closed")?,
        };
        let service = self.clone();
        let mut tasks = self.inner.tasks.lock().await;
        if self.inner.cancel.is_cancelled() {
            bail!("Push notifications are closed");
        }
        while tasks.try_join_next().is_some() {}
        tasks.spawn(async move {
            let _permit = permit;
            // A source event is never replayed after an ambiguous HTTP outcome.
            for batch in tokens.chunks(100) {
                let result = tokio::select! {
                    _ = service.inner.cancel.cancelled() => return,
                    result = service.send_batch(batch, &payload) => result,
                };
                if let Err(error) = result {
                    tracing::warn!(?error, "Failed to send push notifications");
                }
            }
        });
        Ok(())
    }
    async fn send_batch(&self, tokens: &[String], payload: &Value) -> Result<()> {
        let messages = tokens.iter().map(|token| {
            let mut message = json!({"to":token,"title":payload["title"],"body":payload["body"],"sound":"default"});
            if let Some(data) = payload.get("data") { message["data"] = data.clone(); }
            message
        }).collect::<Vec<_>>();
        let response = self
            .inner
            .http
            .get()
            .context("Push service is not initialized")?
            .post(EXPO_URL)
            .header(reqwest::header::ACCEPT, "application/json")
            .json(&messages)
            .send()
            .await?;
        if !response.status().is_success() {
            bail!("Expo push API returned {}", response.status());
        }
        let result: Value = response.json().await?;
        let tickets = result["data"]
            .as_array()
            .context("Invalid Expo push ticket response")?;
        for (token, ticket) in tokens.iter().zip(tickets) {
            if ticket["status"] == "error" {
                tracing::warn!(message = ?ticket["message"], details = ?ticket["details"], "Push notification rejected");
                if matches!(
                    ticket["details"]["error"].as_str(),
                    Some("DeviceNotRegistered" | "InvalidCredentials")
                ) {
                    self.revoke(token).await?;
                }
            }
        }
        Ok(())
    }
    pub async fn shutdown(&self) {
        self.inner.cancel.cancel();
        self.inner.capacity.close();
        let mut tasks = self.inner.tasks.lock().await;
        while tasks.join_next().await.is_some() {}
        self.inner
            .sources
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }
}
fn put_lease(leases: &mut Vec<Lease>, token: String, expires_at: i64) {
    if let Some(lease) = leases.iter_mut().find(|lease| lease.token == token) {
        lease.expires_at = expires_at;
    } else {
        leases.push(Lease { token, expires_at });
    }
}
fn load_leases(value: &Value, now: i64) -> (Vec<Lease>, bool) {
    let mut leases = Vec::new();
    for value in value["subscriptions"].as_array().into_iter().flatten() {
        if let (Some(token), Some(expiry)) = (
            value["token"]
                .as_str()
                .map(str::trim)
                .filter(|t| !t.is_empty()),
            value["expiresAt"]
                .as_str()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok()),
        ) {
            put_lease(&mut leases, token.to_owned(), expiry.timestamp_millis());
        }
    }
    let mut migrated = false;
    for token in value["tokens"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        migrated = true;
        let token = token.trim();
        if !token.is_empty() {
            put_lease(&mut leases, token.to_owned(), now + LEASE_MS);
        }
    }
    (leases, migrated)
}
async fn persist(path: &Path, leases: &[Lease]) -> Result<()> {
    let subscriptions = leases.iter().map(|lease| json!({"token":lease.token,"expiresAt":DateTime::<Utc>::from_timestamp_millis(lease.expires_at).expect("Lease timestamp in range").to_rfc3339_opts(SecondsFormat::Millis, true)})).collect::<Vec<_>>();
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
        private_mode(parent, 0o700).await;
    }
    let mut bytes = serde_json::to_vec_pretty(&json!({"subscriptions":subscriptions}))?;
    bytes.push(b'\n');
    super::super::storage::atomic_bytes(path, &bytes).await
}
async fn private_mode(path: &Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).await;
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn failed_revoke_keeps_lease_and_heartbeat_uses_physical_source() {
        let home = tempfile::tempdir().unwrap();
        let service = PushNotifications::new(Weak::new());
        let path = home.path().join("push-tokens.json");
        {
            let mut store = service.inner.store.lock().await;
            store.initialized = true;
            store.path = path.clone();
            store.leases = vec![Lease {
                token: "token".into(),
                expires_at: 1,
            }];
        }
        let source = |id: &str| {
            RequestContext::attach(
                id.into(),
                "session".into(),
                "same-client".into(),
                json!({}),
                json!({"kind":"owner"}),
                Arc::new(std::sync::RwLock::new(std::collections::HashSet::new())),
                false,
                None,
                CancellationToken::new(),
            )
        };
        let (first, _first_output) = source("physical-one");
        let (replacement, _replacement_output) = source("physical-two");
        service.inner.sources.lock().unwrap().insert(
            first.source_id().into(),
            SourceToken {
                token: "token".into(),
                lifetime: first.cancellation(),
            },
        );
        service.heartbeat(&replacement).await.unwrap();
        assert_eq!(service.inner.store.lock().await.leases[0].expires_at, 1);
        service.heartbeat(&first).await.unwrap();
        assert!(service.inner.store.lock().await.leases[0].expires_at > 1);
        let failure_path = home.path().join("directory-not-file");
        tokio::fs::create_dir(&failure_path).await.unwrap();
        service.inner.store.lock().await.path = failure_path;
        assert!(service.revoke("token").await.is_err());
        assert_eq!(service.inner.store.lock().await.leases.len(), 1);
        first.cancellation().cancel();
        service.inner.store.lock().await.leases[0].expires_at = 1;
        service.heartbeat(&replacement).await.unwrap();
        assert_eq!(service.inner.store.lock().await.leases[0].expires_at, 1);
        assert!(service.inner.sources.lock().unwrap().is_empty());
    }
    #[test]
    fn legacy_tokens_migrate_with_a_finite_lease_and_stable_deduplication() {
        let now = 1_700_000_000_000;
        let (leases, migrated) = load_leases(
            &json!({"subscriptions":[{"token":"a","expiresAt":"2020-01-01T00:00:00Z"}],"tokens":[" a ","b","",null]}),
            now,
        );
        assert!(migrated);
        assert_eq!(
            leases
                .iter()
                .map(|lease| lease.token.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert!(
            leases
                .iter()
                .all(|lease| lease.expires_at == now + LEASE_MS)
        );
    }
    #[tokio::test]
    async fn leases_are_private_and_reloaded_without_extending_expiry() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("push-tokens.json");
        let leases = vec![Lease {
            token: "token".into(),
            expires_at: 1_700_000_000_000,
        }];
        persist(&path, &leases).await.unwrap();
        let parsed: Value = serde_json::from_slice(&tokio::fs::read(&path).await.unwrap()).unwrap();
        let (loaded, migrated) = load_leases(&parsed, 1_800_000_000_000);
        assert!(!migrated);
        assert_eq!(loaded[0].expires_at, leases[0].expires_at);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                tokio::fs::metadata(path)
                    .await
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
}
