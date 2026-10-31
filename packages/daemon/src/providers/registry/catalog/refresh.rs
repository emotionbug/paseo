use super::state::{GLOBAL, ProviderState, SnapshotRecord, identify};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::{
    future::Future,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct Activities {
    pending: Arc<Mutex<Vec<(String, usize)>>>,
    cancel: CancellationToken,
}

tokio::task_local! { static ACTIVITIES: Activities; }

struct ActivityGuard {
    activities: Activities,
    name: String,
}
impl Drop for ActivityGuard {
    fn drop(&mut self) {
        let mut pending = self.activities.pending.lock().unwrap();
        if let Some(index) = pending.iter().position(|(name, _)| name == &self.name) {
            if pending[index].1 == 1 {
                pending.remove(index);
            } else {
                pending[index].1 -= 1;
            }
        }
    }
}

/// Provider adapters mark real discovery stages without coupling their public
/// cancellation API to the registry. Guards clear counts even when a future drops.
pub(crate) async fn refresh_activity<T>(
    name: &str,
    operation: impl Future<Output = Result<T>>,
) -> Result<T> {
    let Some(activities) = ACTIVITIES.try_with(Clone::clone).ok() else {
        return operation.await;
    };
    if activities.cancel.is_cancelled() {
        bail!("Provider refresh cancelled");
    }
    {
        let mut pending = activities.pending.lock().unwrap();
        if let Some((_, count)) = pending.iter_mut().find(|(stage, _)| stage == name) {
            *count += 1;
        } else {
            pending.push((name.to_owned(), 1));
        }
    }
    let _guard = ActivityGuard {
        activities: activities.clone(),
        name: name.to_owned(),
    };
    let result = operation.await?;
    if activities.cancel.is_cancelled() {
        bail!("Provider refresh cancelled");
    }
    Ok(result)
}

pub(super) async fn discover(
    state: &ProviderState,
    options: Value,
    timeout_ms: u64,
    cancel: CancellationToken,
) -> Arc<SnapshotRecord> {
    let activities = Activities {
        pending: Arc::new(Mutex::new(Vec::new())),
        cancel: cancel.clone(),
    };
    let operation = async {
        let available = refresh_activity("availability", async {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => bail!("Provider refresh cancelled"),
                result = state.entry.client.is_available(options.clone(), cancel.clone()) => result,
            }
        })
        .await?;
        if !available {
            return Ok(None);
        }
        assemble(state, options, cancel.clone()).await.map(Some)
    };
    let scoped = ACTIVITIES.scope(activities.clone(), operation);
    tokio::pin!(scoped);
    let outcome: Result<Option<Value>> = tokio::select! {
        result = &mut scoped => result,
        _ = tokio::time::sleep(Duration::from_millis(timeout_ms)) => {
            let pending = activities.pending.lock().unwrap().iter().map(|(name, _)| name.clone()).collect::<Vec<_>>();
            let suffix = if pending.is_empty() { String::new() } else { format!("; pending: {}", pending.join(", ")) };
            let label = state.entry.definition["label"].as_str().unwrap_or(&state.entry.spec.id);
            let error = anyhow!("Timed out refreshing {label} after {timeout_ms}ms{suffix}");
            cancel.cancel();
            // Cooperative provider cleanup is part of a completed refresh. A
            // following discovery may not reuse its process until this returns.
            let _ = scoped.await;
            Err(error)
        }
    };
    let mut entry = state.initial.entry.clone();
    match outcome {
        Ok(None) => {
            entry["status"] = json!("unavailable");
        }
        Ok(Some(mut catalog)) => {
            let catalog = catalog
                .as_object_mut()
                .expect("assembled catalog is an object");
            if let Some(default_mode) = catalog.shift_remove("defaultModeId") {
                entry["defaultModeId"] = default_mode;
            }
            entry["status"] = json!("ready");
            let mut seen = std::collections::HashSet::new();
            let Value::Array(mut models) = catalog
                .shift_remove("models")
                .expect("assembled catalog has models")
            else {
                unreachable!()
            };
            let original_count = models.len();
            models.retain(|model| seen.insert(model["id"].clone()));
            if models.len() != original_count {
                tracing::warn!(provider = %state.entry.spec.id, discarded_rows = original_count - models.len(), "Provider catalog contains repeated model IDs; retaining the first definition");
            }
            entry["models"] = Value::Array(models);
            entry["modes"] = catalog
                .shift_remove("modes")
                .expect("assembled catalog has modes");
            entry["fetchedAt"] =
                json!(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
        }
        Err(error) => {
            // toErrorMessage: an empty message reports "Unknown error".
            let message = error.to_string();
            entry["status"] = json!("error");
            entry["error"] = json!(if message.is_empty() {
                "Unknown error".to_owned()
            } else {
                message
            });
        }
    }
    identify(entry)
}

async fn assemble(
    state: &ProviderState,
    options: Value,
    cancel: CancellationToken,
) -> Result<Value> {
    let entry = &state.entry;
    let static_modes = entry.definition["modes"]
        .as_array()
        .filter(|modes| !modes.is_empty());
    let mut catalog = if let (Some(replacement), Some(modes)) =
        (&state.replacement_models, static_modes)
    {
        let models = replacement.as_ref().map_err(|error| anyhow!("{error}"))?;
        let cwd = if options["scope"] == "workspace" {
            options["cwd"].clone()
        } else {
            json!(std::env::current_dir()?)
        };
        let default_mode = refresh_activity(
            "default-mode",
            entry.client.resolve_default_mode(
                json!({"config": {"provider": entry.spec.id, "cwd": cwd}}),
                cancel,
            ),
        )
        .await?;
        let mut catalog = json!({"models": models, "modes": modes});
        if let Some(mode) = default_mode {
            catalog["defaultModeId"] = json!(mode);
        }
        catalog
    } else {
        let mut catalog = entry.client.fetch_catalog(options, cancel).await?;
        if let Some(replacement) = &state.replacement_models {
            catalog["models"] = json!(replacement.as_ref().map_err(|error| anyhow!("{error}"))?);
        }
        catalog
    };
    catalog
        .get("models")
        .and_then(Value::as_array)
        .context("Provider catalog models must be an array")?;
    catalog
        .get("modes")
        .and_then(Value::as_array)
        .context("Provider catalog modes must be an array")?;
    if let Some(modes) = catalog["modes"].as_array_mut() {
        for mode in modes {
            let complete = mode["icon"].as_str().is_some_and(|value| !value.is_empty())
                && mode["colorTier"]
                    .as_str()
                    .is_some_and(|value| !value.is_empty());
            if complete {
                continue;
            }
            let definition = entry.definition["modes"].as_array().and_then(|modes| {
                modes
                    .iter()
                    .find(|definition| definition["id"] == mode["id"])
            });
            if let Some(definition) = definition {
                for field in ["icon", "colorTier"] {
                    if mode.get(field).is_none_or(Value::is_null) {
                        if let Some(value) = definition.get(field) {
                            mode[field] = value.clone();
                        }
                    }
                }
            }
        }
    }
    Ok(catalog)
}

pub(super) fn fetch_options(cwd: &str, force: bool) -> Value {
    if cwd == GLOBAL {
        json!({"scope": "global", "force": force})
    } else {
        json!({"scope": "workspace", "cwd": cwd, "force": force})
    }
}

pub(super) fn target(cwd: Option<&Path>) -> Result<String> {
    let Some(cwd) = cwd.filter(|path| !path.as_os_str().is_empty()) else {
        return Ok(GLOBAL.to_owned());
    };
    let text = cwd.to_string_lossy();
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(GLOBAL.to_owned());
    }
    let mut path = if trimmed == "~" {
        dirs::home_dir().context("User home directory is unavailable")?
    } else if let Some(rest) = trimmed.strip_prefix("~/") {
        dirs::home_dir()
            .context("User home directory is unavailable")?
            .join(rest)
    } else {
        PathBuf::from(trimmed)
    };
    #[cfg(windows)]
    if trimmed.len() == 2 && trimmed.as_bytes()[0].is_ascii_alphabetic() && trimmed.ends_with(':') {
        path.push("\\");
    }
    if !path.is_absolute() {
        path = std::env::current_dir()?.join(path);
    }
    let mut resolved = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            other => resolved.push(other.as_os_str()),
        }
    }
    Ok(resolved.to_string_lossy().into_owned())
}

pub(super) fn deadline(configured: Option<&Value>) -> u64 {
    deadline_with_env(
        configured,
        std::env::var("PASEO_PROVIDER_REFRESH_TIMEOUT_MS")
            .ok()
            .as_deref(),
    )
}

pub(super) fn deadline_with_env(configured: Option<&Value>, environment: Option<&str>) -> u64 {
    fn valid(value: f64) -> Option<u64> {
        (value.is_finite() && value.fract() == 0.0 && value > 0.0 && value <= 2_147_483_647.0)
            .then_some(value as u64)
    }
    fn number(value: &str) -> Option<f64> {
        let value = value.trim();
        for (prefixes, radix) in [(["0x", "0X"], 16), (["0o", "0O"], 8), (["0b", "0B"], 2)] {
            if let Some(digits) = prefixes
                .iter()
                .find_map(|prefix| value.strip_prefix(prefix))
            {
                return u64::from_str_radix(digits, radix)
                    .ok()
                    .map(|value| value as f64);
            }
        }
        value.parse::<f64>().ok()
    }
    configured
        .and_then(Value::as_f64)
        .and_then(valid)
        .or_else(|| environment.and_then(number).and_then(valid))
        .unwrap_or(120_000)
}

#[cfg(test)]
mod activity_tests {
    use super::*;
    use std::task::Poll;

    #[tokio::test]
    async fn dropping_overlapping_activities_releases_only_their_own_count() {
        let activities = Activities {
            pending: Arc::new(Mutex::new(Vec::new())),
            cancel: CancellationToken::new(),
        };
        let mut first = Box::pin(ACTIVITIES.scope(
            activities.clone(),
            refresh_activity("models", std::future::pending::<Result<()>>()),
        ));
        let mut second = Box::pin(ACTIVITIES.scope(
            activities.clone(),
            refresh_activity("models", std::future::pending::<Result<()>>()),
        ));
        futures_util::future::poll_fn(|context| {
            assert!(first.as_mut().poll(context).is_pending());
            assert!(second.as_mut().poll(context).is_pending());
            Poll::Ready(())
        })
        .await;
        assert_eq!(
            activities.pending.lock().unwrap().as_slice(),
            &[("models".to_owned(), 2)]
        );
        drop(first);
        assert_eq!(
            activities.pending.lock().unwrap().as_slice(),
            &[("models".to_owned(), 1)]
        );
        drop(second);
        assert!(activities.pending.lock().unwrap().is_empty());
    }
}
