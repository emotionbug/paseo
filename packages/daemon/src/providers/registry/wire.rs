mod codec;
mod compat;
#[cfg(test)]
mod tests;

use super::catalog::{CatalogService, Snapshot};
use crate::transport::RequestContext;
use anyhow::{Context, Result};
use compat::Compatibility;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub(super) fn snapshot_payload(
    snapshot: &Snapshot,
    context: &RequestContext,
    request: Option<&Value>,
) -> Value {
    codec::payload(
        snapshot,
        Compatibility::for_context(context),
        request,
        now(),
    )
}

pub(super) fn same_visible_records(
    previous: &Snapshot,
    current: &Snapshot,
    context: &RequestContext,
) -> bool {
    codec::same_records(previous, current, Compatibility::for_context(context))
}

pub(super) async fn handle(
    service: &CatalogService,
    context: &RequestContext,
    message: &Value,
) -> Result<bool> {
    let kind = message["type"].as_str().unwrap_or("");
    match kind {
        "get_providers_snapshot_request" => {
            // A pull reads current state, including loading placeholders. Only the
            // legacy model/mode reads below wait for an already-started warmup.
            let app = service.app()?;
            let cwd = message["cwd"]
                .as_str()
                .filter(|cwd| !cwd.trim().is_empty())
                .map(|cwd| expand_tilde(cwd, &app.user_home));
            let _delivery = service.delivery(context).await;
            let snapshot = service.snapshot(cwd.as_deref(), None, false).await?;
            let payload = snapshot_payload(&snapshot, context, Some(message));
            reply(context, "get_providers_snapshot_response", message, payload).await?;
        }
        "refresh_providers_snapshot_request" => {
            let app = service.app()?;
            // Any non-empty cwd selects a workspace refresh; resolveSnapshotCwd
            // maps a blank one to the home workspace, never the global target.
            let cwd = message["cwd"]
                .as_str()
                .filter(|cwd| !cwd.is_empty())
                .map(|cwd| {
                    if cwd.trim().is_empty() {
                        app.user_home.clone()
                    } else {
                        expand_tilde(cwd, &app.user_home)
                    }
                });
            let providers = message
                .get("providers")
                .and_then(Value::as_array)
                .map(|providers| {
                    providers
                        .iter()
                        .map(|provider| {
                            provider
                                .as_str()
                                .context("Provider must be a string")
                                .map(str::to_owned)
                        })
                        .collect::<Result<Vec<_>>>()
                })
                .transpose()?;
            service
                .refresh(cwd.as_deref(), providers.as_deref())
                .await?;
            reply(
                context,
                "refresh_providers_snapshot_response",
                message,
                json!({"acknowledged": true}),
            )
            .await?;
        }
        "list_provider_models_request" | "list_provider_modes_request" => {
            let provider = message["provider"].as_str().context("Missing provider")?;
            let fetched_at = now();
            let app = service.app()?;
            let cwd = message["cwd"]
                .as_str()
                .map(str::trim)
                .filter(|cwd| !cwd.is_empty())
                .map(|cwd| expand_tilde(cwd, &app.user_home));
            let snapshot = service.snapshot(cwd.as_deref(), None, false).await?;
            let mut record = snapshot
                .records
                .into_iter()
                .find(|record| record.entry["provider"] == provider);
            if record.as_ref().is_none_or(|record| {
                record.entry["enabled"] != false && record.entry["status"] == "loading"
            }) {
                let providers = [provider.to_owned()];
                let snapshot = service
                    .snapshot(cwd.as_deref(), Some(&providers), true)
                    .await?;
                record = snapshot
                    .records
                    .into_iter()
                    .find(|record| record.entry["provider"] == provider);
            }
            let models = kind == "list_provider_models_request";
            let field = if models { "models" } else { "modes" };
            let mut payload = json!({"provider": provider, "fetchedAt": fetched_at});
            match record.as_ref().map(|record| &record.entry) {
                None => payload["error"] = json!(format!("Unknown provider: {provider}")),
                Some(entry) if entry["enabled"] == false => {
                    payload["error"] = json!(format!("Provider {provider} is disabled"))
                }
                Some(entry) if entry["status"] == "ready" => {
                    let mut values = entry
                        .get(field)
                        .filter(|value| !value.is_null())
                        .cloned()
                        .unwrap_or_else(|| json!([]));
                    if models {
                        if let Some(models) = values.as_array_mut() {
                            models.retain(|model| model["isSelectable"] != false);
                        }
                    } else {
                        Compatibility::for_context(context).modes(&mut values);
                    }
                    payload[field] = values;
                    payload["error"] = Value::Null;
                    if let Some(fetched_at) =
                        entry.get("fetchedAt").filter(|value| !value.is_null())
                    {
                        payload["fetchedAt"] = fetched_at.clone();
                    }
                }
                Some(entry) => {
                    payload["error"] = if entry["status"] == "error" {
                        entry
                            .get("error")
                            .filter(|error| !error.is_null())
                            .cloned()
                            .unwrap_or_else(|| {
                                json!(format!("Failed to list {field} for {provider}"))
                            })
                    } else {
                        json!(format!("Provider {provider} is not available"))
                    };
                }
            }
            reply(
                context,
                if models {
                    "list_provider_models_response"
                } else {
                    "list_provider_modes_response"
                },
                message,
                payload,
            )
            .await?;
        }
        "list_provider_features_request" => {
            let fetched_at = now();
            let draft = &message["draftConfig"];
            let mut payload = json!({"provider": draft["provider"], "fetchedAt": fetched_at});
            let result = async {
                let app = service.app()?;
                let cwd = draft["cwd"].as_str().context("Missing draft cwd")?;
                let mut config = json!({"provider": draft["provider"], "cwd": expand_tilde(cwd, &app.user_home)});
                for field in ["modeId", "model", "thinkingOptionId"] {
                    if let Some(value) = draft.get(field).filter(|value| value.as_str().is_some_and(|value| !value.is_empty())) { config[field] = value.clone(); }
                }
                if let Some(value) = draft.get("featureValues").filter(|value| !value.is_null()) { config["featureValues"] = value.clone(); }
                app.agents.list_draft_features(&config).await
            }.await;
            match result {
                Ok(features) => {
                    payload["features"] = json!(features);
                    payload["error"] = Value::Null;
                }
                Err(error) => {
                    tracing::error!(provider = ?draft["provider"], %error, "Failed to list provider features");
                    payload["error"] = json!(error.to_string());
                }
            }
            reply(context, "list_provider_features_response", message, payload).await?;
        }
        "list_available_providers_request" => {
            let fetched_at = now();
            let result = async {
                let app = service.app()?;
                let entries = app.providers.entries();
                let results = futures_util::future::join_all(entries.iter().filter(|entry| entry.enabled).map(|entry| {
                    let cancel = app.cancel.child_token();
                    async move {
                        match entry.client.is_available(json!({}), cancel).await {
                            Ok(available) => json!({"provider": entry.spec.id, "available": available, "error": null}),
                            Err(error) => {
                                tracing::warn!(provider = %entry.spec.id, %error, "Failed to check provider availability");
                                json!({"provider": entry.spec.id, "available": false, "error": error.to_string()})
                            }
                        }
                    }
                })).await;
                let compatibility = Compatibility::for_context(context);
                Ok::<_, anyhow::Error>(results.into_iter().filter(|entry| compatibility.visible(entry["provider"].as_str().unwrap_or(""))).collect::<Vec<_>>())
            }.await;
            let payload = match result {
                Ok(providers) => {
                    json!({"providers": providers, "error": null, "fetchedAt": fetched_at})
                }
                Err(error) => {
                    json!({"providers": [], "error": error.to_string(), "fetchedAt": fetched_at})
                }
            };
            reply(
                context,
                "list_available_providers_response",
                message,
                payload,
            )
            .await?;
        }
        "provider_diagnostic_request" => {
            let provider = message["provider"].as_str().context("Missing provider")?;
            match service.diagnostic(provider).await {
                Ok(diagnostic) => {
                    reply(
                        context,
                        "provider_diagnostic_response",
                        message,
                        json!({"provider": provider, "diagnostic": diagnostic}),
                    )
                    .await?
                }
                Err(error) => {
                    tracing::error!(provider, %error, "Failed to get provider diagnostic");
                    reply(context, "rpc_error", message, json!({"requestType": kind, "error": format!("Failed to get provider diagnostic: {error}"), "code": "provider_diagnostic_failed"})).await?;
                }
            }
        }
        // Provider changes use the central session.events subscription family;
        // there is no separate provider subscription or default-mode request.
        _ => return Ok(false),
    }
    Ok(true)
}

async fn reply(
    context: &RequestContext,
    kind: &str,
    request: &Value,
    mut payload: Value,
) -> Result<()> {
    if let Some(request_id) = request.get("requestId") {
        payload["requestId"] = request_id.clone();
    }
    context
        .send(json!({"type": kind, "payload": payload}))
        .await
}

fn expand_tilde(value: &str, home: &Path) -> PathBuf {
    if value == "~" {
        home.to_owned()
    } else if let Some(suffix) = value.strip_prefix("~/") {
        home.join(suffix)
    } else {
        PathBuf::from(value)
    }
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
