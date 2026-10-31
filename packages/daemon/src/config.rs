//! Persisted daemon configuration and its serialized live-settings transaction.
mod resolve;
mod schema;
mod storage;
#[cfg(test)]
mod tests;
mod transactions;

use crate::{state::AppState, transport::RequestContext};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{RwLock, Weak},
};
use tokio::sync::Mutex;
use transactions::{get, set};

pub use schema::validate_persisted_path;
pub use storage::parse_document;

#[derive(Clone)]
struct Snapshot {
    resolved: Value,
    mutable: Value,
    startup_persisted: Value,
    known_persisted: Value,
}

pub struct ConfigService {
    app: Weak<AppState>,
    environment: BTreeMap<String, String>,
    current: RwLock<Option<Snapshot>>,
    writer: Mutex<()>,
}

impl ConfigService {
    pub fn new(app: Weak<AppState>) -> Self {
        Self {
            app,
            environment: std::env::vars().collect(),
            current: RwLock::new(None),
            writer: Mutex::new(()),
        }
    }

    pub async fn initialize(&self) -> Result<()> {
        self.initialize_with_overrides(&json!({})).await
    }

    pub async fn initialize_with_overrides(&self, cli: &Value) -> Result<()> {
        let _writer = self.writer.lock().await;
        if self
            .current
            .read()
            .map_err(|_| anyhow!("Configuration state poisoned"))?
            .is_some()
        {
            bail!("Configuration already initialized");
        }
        let app = self.app.upgrade().context("Daemon shutting down")?;
        let home = app.home.clone();
        let user_home = app.user_home.clone();
        let environment = self.environment.clone();
        let cli = cli.clone();
        let (persisted, resolved) = tokio::task::spawn_blocking(move || -> Result<_> {
            let persisted = storage::read(&home, true, true)?;
            let resolved = resolve::resolve_with_options(
                &home,
                &user_home,
                &persisted,
                &environment,
                None,
                &cli,
            )?;
            Ok((persisted, resolved))
        })
        .await??;
        let mutable = resolve::mutable(&resolved);
        *self
            .current
            .write()
            .map_err(|_| anyhow!("Configuration state poisoned"))? = Some(Snapshot {
            resolved,
            mutable,
            startup_persisted: persisted.clone(),
            known_persisted: persisted,
        });
        Ok(())
    }

    /// Internal resolved server configuration. Contains secrets; never put this
    /// snapshot on a wire response. mutable_snapshot is the protocol projection.
    pub fn snapshot(&self) -> Value {
        self.current
            .read()
            .expect("Configuration state poisoned")
            .as_ref()
            .expect("Configuration must initialize before use")
            .resolved
            .clone()
    }

    pub fn mutable_snapshot(&self) -> Value {
        self.current
            .read()
            .expect("Configuration state poisoned")
            .as_ref()
            .expect("Configuration must initialize before use")
            .mutable
            .clone()
    }

    pub fn environment(&self) -> BTreeMap<String, String> {
        self.environment.clone()
    }

    fn state(&self) -> Result<Snapshot> {
        self.current
            .read()
            .map_err(|_| anyhow!("Configuration state poisoned"))?
            .clone()
            .context("Configuration is not initialized")
    }

    pub async fn handle(&self, context: &RequestContext, message: &Value) -> Result<bool> {
        let kind = message.get("type").and_then(Value::as_str).unwrap_or("");
        if !matches!(
            kind,
            "get_daemon_config_request"
                | "set_daemon_config_request"
                | "daemon.config.reload.request"
        ) {
            return Ok(false);
        }
        let request_id = message
            .get("requestId")
            .and_then(Value::as_str)
            .context("requestId is required")?;
        let result = match kind {
            "get_daemon_config_request" => Ok(json!({"type":"get_daemon_config_response","payload":{"requestId":request_id,"config":self.mutable_snapshot()}})),
            "set_daemon_config_request" => self.patch(message.get("config").context("config is required")?.clone()).await
                .map(|config| json!({"type":"set_daemon_config_response","payload":{"requestId":request_id,"config":config}})),
            _ => self.reload().await.map(|mut payload| {
                payload["requestId"] = json!(request_id);
                json!({"type":"daemon.config.reload.response","payload":payload})
            }),
        };
        match result {
            Ok(response) => context.send(response).await?,
            Err(error) => context.send(json!({"type":"rpc_error","payload":{"requestId":request_id,"requestType":kind,"error":error.to_string(),"code":"handler_error"}})).await?,
        }
        Ok(true)
    }

    pub async fn patch(&self, patch: Value) -> Result<Value> {
        self.apply_patch(patch, false).await
    }

    pub async fn set_agent_skill_selection(&self, selection: Value) -> Result<()> {
        self.apply_patch(json!({"skills":{"selection":selection}}), true)
            .await?;
        Ok(())
    }

    async fn apply_patch(&self, patch: Value, internal_skills: bool) -> Result<Value> {
        let patch = transactions::supported_patch(patch, internal_skills)?;
        let _writer = self.writer.lock().await;
        let before = self.state()?;
        if get(&patch, "relay.enabled").is_some()
            && before.resolved.get("relayEnabledMutable") == Some(&Value::Bool(false))
        {
            bail!(
                "Relay is controlled by a daemon launch override. Remove PASEO_RELAY_ENABLED or the relay CLI flag before changing it here."
            );
        }
        let removed = transactions::removed(&patch);
        let next_mutable = transactions::next_mutable(&before.mutable, &patch, &removed);
        if next_mutable == before.mutable && removed.is_empty() {
            return Ok(before.mutable);
        }
        let app = self.app.upgrade().context("Daemon shutting down")?;
        let home = app.home.clone();
        let disk_before =
            tokio::task::spawn_blocking(move || storage::read(&home, true, true)).await??;
        let disk_after = transactions::persist_patch(&disk_before, &patch, &removed)?;
        let known_after = transactions::persist_patch(&before.known_persisted, &patch, &removed)?;
        save_persisted(&app.home, &disk_after).await?;
        // Like the source, a removal that leaves the live configuration unchanged
        // only records the persisted edit; no owner is reapplied or notified.
        if next_mutable == before.mutable {
            *self
                .current
                .write()
                .map_err(|_| anyhow!("Configuration state poisoned"))? = Some(Snapshot {
                known_persisted: known_after,
                ..before
            });
            return Ok(next_mutable);
        }
        let next_resolved = resolve::apply_mutable(&before.resolved, &next_mutable);
        if let Err(error) = self
            .apply_live(&app, &next_resolved, &before.resolved)
            .await
        {
            if let Err(rollback) = save_persisted(&app.home, &disk_before).await {
                return Err(anyhow!(
                    "Configuration apply failed: {error}; persisted configuration rollback failed: {rollback}"
                ));
            }
            return Err(error);
        }
        *self
            .current
            .write()
            .map_err(|_| anyhow!("Configuration state poisoned"))? = Some(Snapshot {
            resolved: next_resolved,
            mutable: next_mutable.clone(),
            startup_persisted: before.startup_persisted,
            known_persisted: known_after,
        });
        // Notification failure cannot turn a committed mutation into a rejection.
        if let Err(error) = app
            .transport
            .publish_daemon_config_changed(next_mutable.clone())
            .await
        {
            tracing::warn!(%error,"Daemon configuration committed but notification failed");
        }
        Ok(next_mutable)
    }

    async fn apply_live(&self, app: &AppState, next: &Value, previous: &Value) -> Result<()> {
        app.plugins.apply_config(next, previous).await?;
        if let Err(error) = app.providers.apply_config(next, previous).await {
            // Restored workers expose new handles; rebuild the prior registry
            // only after the plugin owner restores its registrations.
            let plugin_rollback = app.plugins.apply_config(previous, next).await;
            let provider_rollback = app.providers.apply_config(previous, next).await;
            if plugin_rollback.is_err() || provider_rollback.is_err() {
                return Err(anyhow!(
                    "Daemon config apply failed: {error}; live rollback errors: plugins={}, providers={}",
                    plugin_rollback
                        .err()
                        .map(|error| error.to_string())
                        .unwrap_or_default(),
                    provider_rollback
                        .err()
                        .map(|error| error.to_string())
                        .unwrap_or_default()
                ));
            }
            return Err(error);
        }
        if let Err(error) = app.git.apply_config(next, previous).await {
            let plugin_rollback = app.plugins.apply_config(previous, next).await;
            let provider_rollback = app.providers.apply_config(previous, next).await;
            if provider_rollback.is_err() || plugin_rollback.is_err() {
                return Err(anyhow!(
                    "Daemon config apply failed: {error}; live rollback errors: providers={}, plugins={}",
                    provider_rollback
                        .err()
                        .map(|error| error.to_string())
                        .unwrap_or_default(),
                    plugin_rollback
                        .err()
                        .map(|error| error.to_string())
                        .unwrap_or_default()
                ));
            }
            return Err(error);
        }
        if let Err(error) = app.transport.apply_config(next, previous).await {
            let git_rollback = app.git.apply_config(previous, next).await;
            let plugin_rollback = app.plugins.apply_config(previous, next).await;
            let provider_rollback = app.providers.apply_config(previous, next).await;
            if git_rollback.is_err() || provider_rollback.is_err() || plugin_rollback.is_err() {
                return Err(anyhow!(
                    "Daemon config apply failed: {error}; live rollback errors: git={}, providers={}, plugins={}",
                    git_rollback
                        .err()
                        .map(|error| error.to_string())
                        .unwrap_or_default(),
                    provider_rollback
                        .err()
                        .map(|error| error.to_string())
                        .unwrap_or_default(),
                    plugin_rollback
                        .err()
                        .map(|error| error.to_string())
                        .unwrap_or_default()
                ));
            }
            return Err(error);
        }
        // Hook installers mutate user-owned provider configuration and retain
        // the source's warning-only failure semantics, not fictitious rollback.
        // Run only once all rejecting live owners have accepted the candidate.
        if next.get("enableTerminalAgentHooks") != previous.get("enableTerminalAgentHooks") {
            if let Err(error) = app.terminals.apply_config(next, previous).await {
                tracing::warn!(%error, "Daemon configuration applied but terminal hook installation failed");
            }
        }
        Ok(())
    }

    pub async fn reload(&self) -> Result<Value> {
        let _writer = self.writer.lock().await;
        let before = self.state()?;
        let app = self.app.upgrade().context("Daemon shutting down")?;
        let home = app.home.clone();
        let user_home = app.user_home.clone();
        let environment = self.environment.clone();
        let fallback = before
            .resolved
            .pointer("/configReload/relayEnabledFallback")
            .and_then(Value::as_bool);
        let cli = before
            .resolved
            .pointer("/configReload/cli")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let (persisted, resolved) = tokio::task::spawn_blocking(move || -> Result<_> {
            let persisted = storage::read(&home, true, true)?;
            let resolved = resolve::resolve_with_options(
                &home,
                &user_home,
                &persisted,
                &environment,
                fallback,
                &cli,
            )?;
            Ok((persisted, resolved))
        })
        .await??;
        let mut desired = resolve::mutable(&resolved);
        set(&mut desired, "plugins", before.mutable.get("plugins"));
        let overrides: Vec<String> = resolved
            .pointer("/configReload/overrideControlledPaths")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        let mut changed = Vec::new();
        transactions::diff(
            Some(&before.known_persisted),
            Some(&persisted),
            "",
            &mut changed,
        );
        let override_paths = transactions::compact(
            changed.into_iter().filter(|path| {
                overrides
                    .iter()
                    .any(|owner| transactions::belongs(path, owner))
            }),
            &overrides,
        );
        let mut applied: Vec<String> = transactions::RELOADABLE
            .iter()
            .filter(|(stored, mutable)| {
                !overrides
                    .iter()
                    .any(|owner| transactions::belongs(stored, owner))
                    && get(&before.mutable, mutable) != get(&desired, mutable)
            })
            .map(|(path, _)| (*path).to_owned())
            .collect();
        applied.sort();
        let mut restart = Vec::new();
        transactions::diff(
            Some(&before.startup_persisted),
            Some(&persisted),
            "",
            &mut restart,
        );
        let restart = transactions::compact(
            restart.into_iter().filter(|path| {
                path != "$schema"
                    && path != "version"
                    && !transactions::RELOADABLE
                        .iter()
                        .any(|(owner, _)| transactions::belongs(path, owner))
                    && !overrides
                        .iter()
                        .any(|owner| transactions::belongs(path, owner))
            }),
            &[],
        );
        let next = resolve::apply_mutable(&before.resolved, &desired);
        if desired != before.mutable {
            self.apply_live(&app, &next, &before.resolved).await?;
        }
        let changed = desired != before.mutable;
        *self
            .current
            .write()
            .map_err(|_| anyhow!("Configuration state poisoned"))? = Some(Snapshot {
            resolved: next,
            mutable: desired.clone(),
            startup_persisted: before.startup_persisted,
            known_persisted: persisted,
        });
        if changed {
            if let Err(error) = app.transport.publish_daemon_config_changed(desired).await {
                tracing::warn!(%error,"Daemon configuration reloaded but notification failed");
            }
        }
        Ok(
            json!({"appliedPaths":applied,"restartRequiredPaths":restart,"overrideControlledPaths":override_paths}),
        )
    }
}

/// Read-only CLI/configuration helpers never initialize a daemon identity.
pub async fn read_persisted(home: &Path, defaults_if_missing: bool) -> Result<Value> {
    let home = home.to_path_buf();
    tokio::task::spawn_blocking(move || storage::read(&home, false, defaults_if_missing)).await?
}

pub async fn save_persisted(home: &Path, value: &Value) -> Result<()> {
    let home = home.to_path_buf();
    let value = value.clone();
    tokio::task::spawn_blocking(move || storage::save(&home, &value)).await?
}

/// Offline pairing observes persisted settings without inheriting daemon launch
/// overrides or initializing a configuration file or daemon identity.
pub async fn resolve_persisted_for_pairing(home: &Path) -> Result<Value> {
    let home = home.to_path_buf();
    let user_home = dirs::home_dir().context("Cannot resolve user home directory")?;
    tokio::task::spawn_blocking(move || {
        let persisted = storage::read(&home, false, true)?;
        resolve::resolve(&home, &user_home, &persisted, &BTreeMap::new())
    })
    .await?
}

/// Offline source-compatible field editing; daemon authentication has its own
/// password command and cannot be changed indirectly by replacing daemon.
pub async fn edit_persisted(home: &Path, field: &str, value: Option<Value>) -> Result<()> {
    validate_persisted_path(field)?;
    if field == "daemon.auth" || field.starts_with("daemon.auth.") {
        bail!("Use daemon set-password to change the daemon password.");
    }
    let home = home.to_path_buf();
    let field = field.to_owned();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let mut config = storage::read(&home, false, true)?;
        if field == "daemon"
            && config.pointer("/daemon/auth") != value.as_ref().and_then(|value| value.get("auth"))
        {
            bail!("Use daemon set-password to change the daemon password.");
        }
        let mut parts = field.split('.').peekable();
        let mut cursor = &mut config;
        while let Some(part) = parts.next() {
            let object = cursor
                .as_object_mut()
                .context("Configuration parent must be an object")?;
            if parts.peek().is_none() {
                if let Some(value) = value {
                    object.insert(part.to_owned(), value);
                } else {
                    object.remove(part);
                }
                break;
            }
            cursor = object.entry(part).or_insert_with(|| json!({}));
        }
        storage::save(&home, &config)
    })
    .await?
}
