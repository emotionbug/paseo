use super::*;
use crate::directory_sync::{agent_directory_includes, matches_agent_updates_filter};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use std::{cmp::Ordering, collections::HashMap};

#[path = "wire_search.rs"]
mod search;

pub(super) async fn handle(
    manager: &AgentManager,
    context: &RequestContext,
    request: &Value,
) -> Result<bool> {
    match text(request, "type") {
        "fetch_agents_request" | "fetch_agent_history_request" => {
            let history = request["type"] == "fetch_agent_history_request";
            let owner = if !history && request["subscribe"].is_object() {
                Some(context.begin_subscription(
                    "agents",
                    request["subscribe"]["subscriptionId"].as_str(),
                    None,
                )?)
            } else {
                None
            };
            let events = owner.as_ref().map(|_| manager.subscribe_events());
            match list(manager, context, request).await {
                Ok(mut payload) => {
                    let stamps: HashMap<String, String> = if owner.is_some() {
                        payload["entries"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .map(|e| {
                                (
                                    text(&e["agent"], "id").to_owned(),
                                    text(&e["agent"], "updatedAt").to_owned(),
                                )
                            })
                            .collect()
                    } else {
                        HashMap::new()
                    };
                    if let Some(owner) = &owner {
                        payload["subscriptionId"] = json!(owner.response_id());
                    }
                    reply(
                        context,
                        if history {
                            "fetch_agent_history_response"
                        } else {
                            "fetch_agents_response"
                        },
                        correlated(request, payload),
                    )
                    .await?;
                    if let Some(owner) = owner {
                        owner.activate().await?;
                        follow(
                            manager,
                            owner,
                            events.expect("subscription receiver"),
                            request.clone(),
                            stamps,
                        )?;
                    }
                }
                Err(error) => {
                    if let Some(owner) = owner {
                        owner.release().await?;
                    }
                    let code = if error.to_string().contains("cursor") {
                        "invalid_cursor"
                    } else if request.get("sync").is_some()
                        && (request["scope"] != "active" || request.get("filter").is_some())
                    {
                        "invalid_request"
                    } else if history {
                        "fetch_agent_history_failed"
                    } else {
                        "fetch_agents_failed"
                    };
                    rpc_error(context, request, code, &error).await?;
                }
            }
        }
        "fetch_agent_request" => {
            let result = async {
                let Some(id) = resolve(manager, text(request, "agentId")).await? else {
                    return Ok(json!({"agent":null,"project":null,"error":null}));
                };
                let Some(agent) = manager.agent_snapshot(&id).await? else {
                    return Ok(json!({"agent":null,"project":null,"error":null}));
                };
                if !provider_visible(context, text(&agent, "provider")) {
                    return Ok(json!({"agent":null,"project":null,"error":null}));
                }
                let project = manager
                    .app()?
                    .projects
                    .project_placement_for_workspace(text(&agent, "workspaceId"))
                    .await?;
                Ok::<_, anyhow::Error>(
                    json!({"agent":snapshot_payload(&agent),"project":project,"error":null}),
                )
            }
            .await;
            let payload = result.unwrap_or_else(
                |error| json!({"agent":null,"project":null,"error":error.to_string()}),
            );
            reply(
                context,
                "fetch_agent_response",
                correlated(request, payload),
            )
            .await?;
        }
        "fetch_recent_provider_sessions_request" => match manager.import_sessions(request).await {
            Ok(value) => {
                let entries: Vec<Value> = value["entries"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|entry| {
                        Value::Object(
                            [
                                "providerId",
                                "providerLabel",
                                "providerHandleId",
                                "cwd",
                                "title",
                                "firstPromptPreview",
                                "lastPromptPreview",
                                "lastActivityAt",
                            ]
                            .into_iter()
                            .filter_map(|key| {
                                entry.get(key).map(|value| (key.to_owned(), value.clone()))
                            })
                            .collect(),
                        )
                    })
                    .collect();
                let mut payload = json!({"entries":entries});
                if value["filteredAlreadyImportedCount"]
                    .as_u64()
                    .is_some_and(|count| count > 0)
                {
                    payload["filteredAlreadyImportedCount"] =
                        value["filteredAlreadyImportedCount"].clone();
                }
                if value["providerErrors"]
                    .as_array()
                    .is_some_and(|errors| !errors.is_empty())
                {
                    payload["providerErrors"] = value["providerErrors"].clone();
                }
                reply(
                    context,
                    "fetch_recent_provider_sessions_response",
                    correlated(request, payload),
                )
                .await?;
            }
            Err(error) => {
                rpc_error(
                    context,
                    request,
                    if error.to_string() == "Invalid recent provider sessions since" {
                        "invalid_since"
                    } else {
                        "fetch_recent_provider_sessions_failed"
                    },
                    &error,
                )
                .await?
            }
        },
        _ => return Ok(false),
    }
    Ok(true)
}
fn provider_visible(context: &RequestContext, provider: &str) -> bool {
    if supports(context, "all_providers") || matches!(provider, "claude" | "codex" | "opencode") {
        return true;
    }
    let Some(version) = context.app_version().filter(|v| !v.is_empty()) else {
        return false;
    };
    let mut parts = version.split('-').next().unwrap_or("").split('.');
    for minimum in [0.0, 1.0, 45.0] {
        let value = parts
            .next()
            .map(|part| {
                if part.trim().is_empty() {
                    0.0
                } else {
                    part.parse::<f64>().unwrap_or(f64::NAN)
                }
            })
            .unwrap_or(0.0);
        if value > minimum {
            return true;
        }
        if value < minimum {
            return false;
        }
    }
    true
}
async fn list(manager: &AgentManager, context: &RequestContext, request: &Value) -> Result<Value> {
    let sync = request.get("sync").is_some();
    if sync && (request["scope"] != "active" || request.get("filter").is_some()) {
        bail!("Sequenced agent directory reads require scope=active and no filter.");
    }
    let history = request["type"] == "fetch_agent_history_request";
    let mut filter = request.get("filter").cloned().unwrap_or_else(|| json!({}));
    if history && filter.get("includeArchived").is_none() {
        filter["includeArchived"] = json!(true);
    }
    let background = request["includeBackground"] == true || filter["includeBackground"] == true;
    let app = manager.app()?;
    let mut workspace_cache = HashMap::new();
    let mut placement_cache = HashMap::new();
    let active_projects = if request["scope"] == "active" {
        Some(
            app.projects
                .list_projects()
                .await?
                .into_iter()
                .filter(|p| p["archivedAt"].is_null())
                .filter_map(|p| p["projectId"].as_str().map(str::to_owned))
                .collect::<std::collections::HashSet<_>>(),
        )
    } else {
        None
    };
    let mut entries = Vec::new();
    for raw in manager.list_snapshots().await? {
        if (!sync && !provider_visible(context, text(&raw, "provider")))
            || (!history
                && (raw["providerUnavailable"] == true || raw["_providerAvailable"] == false))
        {
            continue;
        }
        let agent = snapshot_payload(&raw);
        let workspace_id = text(&agent, "workspaceId");
        if workspace_id.is_empty() {
            continue;
        }
        if !workspace_cache.contains_key(workspace_id) {
            workspace_cache.insert(
                workspace_id.to_owned(),
                app.projects.workspace(workspace_id).await?,
            );
        }
        let workspace = workspace_cache.get(workspace_id).and_then(Option::as_ref);
        if !agent_directory_includes(&agent, workspace, background || sync) {
            continue;
        }
        if !placement_cache.contains_key(workspace_id) {
            placement_cache.insert(
                workspace_id.to_owned(),
                app.projects
                    .project_placement_for_workspace(workspace_id)
                    .await?,
            );
        }
        let Some(project) = placement_cache.get(workspace_id).and_then(Option::as_ref) else {
            continue;
        };
        if request["scope"] == "active"
            && (!agent["archivedAt"].is_null()
                || workspace.is_none_or(|w| {
                    !w["archivedAt"].is_null()
                        || !active_projects
                            .as_ref()
                            .unwrap()
                            .contains(text(w, "projectId"))
                }))
        {
            continue;
        }
        if !matches_agent_updates_filter(&agent, project, Some(&filter)) {
            continue;
        }
        if history && !search::matches(text(request, "search"), &agent, project) {
            continue;
        }
        entries.push(json!({"agent":agent,"project":project}));
    }
    if sync {
        let entries = ordered(entries, &normalize_sort(request))
            .into_iter()
            .map(|(entry, _)| entry)
            .collect();
        let mut result =
            manager
                .directory_sync
                .synchronize_agents(entries, &request["sync"], background)?;
        if let Some(entries) = result["entries"].as_array_mut() {
            entries.retain(|entry| {
                provider_visible(context, text(&entry["agent"], "provider"))
                    && agent_directory_includes(
                        &entry["agent"],
                        workspace_cache
                            .get(text(&entry["agent"], "workspaceId"))
                            .and_then(Option::as_ref),
                        background,
                    )
            });
        }
        return Ok(result);
    }
    paginate(entries, request)
}
fn follow(
    manager: &AgentManager,
    owner: crate::transport::OwnedSubscription,
    mut events: tokio::sync::broadcast::Receiver<Value>,
    request: Value,
    mut stamps: HashMap<String, String>,
) -> Result<()> {
    let app = std::sync::Arc::downgrade(&manager.app()?);
    tokio::spawn(async move {
        let cancel = owner.cancellation();
        let mut filter = request.get("filter").cloned().unwrap_or_else(|| json!({}));
        if request["includeBackground"] == true {
            filter["includeBackground"] = json!(true);
        }
        let mut pending = std::collections::VecDeque::new();
        loop {
            if cancel.is_cancelled() {
                break;
            }
            let event = if let Some(event) = pending.pop_front() {
                event
            } else {
                let event = tokio::select! { biased; _ = cancel.cancelled() => break, event = events.recv() => event };
                match event {
                    Ok(value) => value,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        let Some(app) = app.upgrade() else {
                            break;
                        };
                        let snapshots = match app.agents.list_snapshots().await {
                            Ok(snapshots) => snapshots,
                            Err(_) => break,
                        };
                        let present: std::collections::HashSet<_> =
                            snapshots.iter().map(|agent| text(agent, "id")).collect();
                        for id in stamps.keys().filter(|id| !present.contains(id.as_str())) {
                            pending.push_back(json!({"type":"agent_deleted","agentId":id}));
                        }
                        pending.extend(
                            snapshots
                                .into_iter()
                                .map(|agent| json!({"type":"agent_state","agent":agent})),
                        );
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            };
            let Some(app) = app.upgrade() else {
                break;
            };
            let result: Result<()> = async {
                let (id, entry, matches) = if event["type"] == "agent_deleted" {
                    (text(&event, "agentId").to_owned(), None, false)
                } else if event["type"] == "agent_state" {
                    let agent = snapshot_payload(&event["agent"]);
                    let id = text(&agent, "id").to_owned();
                    if stamps
                        .get(&id)
                        .is_some_and(|stamp| text(&agent, "updatedAt") < stamp.as_str())
                    {
                        return Ok(());
                    }
                    stamps.insert(id.clone(), text(&agent, "updatedAt").to_owned());
                    let project = app
                        .projects
                        .project_placement_for_workspace(text(&agent, "workspaceId"))
                        .await?;
                    let workspace = app.projects.workspace(text(&agent, "workspaceId")).await?;
                    let matches = project
                        .as_ref()
                        .is_some_and(|p| matches_agent_updates_filter(&agent, p, Some(&filter)))
                        && agent_directory_includes(
                            &agent,
                            workspace.as_ref(),
                            filter["includeBackground"] == true,
                        );
                    let entry = project.map(|project| json!({"agent":agent,"project":project}));
                    (id, entry, matches)
                } else {
                    return Ok(());
                };
                let payload = if matches {
                    let entry = entry.as_ref().unwrap();
                    json!({"kind":"upsert","agent":entry["agent"],"project":entry["project"]})
                } else {
                    json!({"kind":"remove","agentId":id})
                };
                let visible = entry.as_ref().is_none_or(|entry| {
                    provider_visible(owner.context(), text(&entry["agent"], "provider"))
                });
                let payload = app.agents.directory_sync.sequence_agent_update(
                    payload,
                    entry,
                    &id,
                    request.get("sync").is_some(),
                    filter["includeBackground"] == true,
                )?;
                if visible {
                    owner
                        .emit(json!({"type":"agent_update","payload":payload}))
                        .await
                } else {
                    Ok(())
                }
            }
            .await;
            if result.is_err() {
                break;
            }
        }
        let _ = owner.release().await;
    });
    Ok(())
}

fn sort_value(agent: &Value, key: &str) -> Value {
    match key {
        "title" => json!(text(agent, "title").to_lowercase()),
        "created_at" | "updated_at" => chrono::DateTime::parse_from_rfc3339(text(
            agent,
            if key == "created_at" {
                "createdAt"
            } else {
                "updatedAt"
            },
        ))
        .ok()
        .map(|d| json!(d.timestamp_millis()))
        .unwrap_or(Value::Null),
        "status_priority" => json!(if agent["pendingPermissions"]
            .as_array()
            .is_some_and(|a| !a.is_empty())
            || agent["attentionReason"] == "permission"
        {
            0
        } else if agent["status"] == "error" || agent["attentionReason"] == "error" {
            1
        } else if agent["status"] == "running" {
            2
        } else if agent["status"] == "initializing" {
            3
        } else {
            4
        }),
        _ => Value::Null,
    }
}
fn compare_value(left: &Value, right: &Value) -> Ordering {
    if left == right {
        Ordering::Equal
    } else if left.is_null() {
        Ordering::Less
    } else if right.is_null() {
        Ordering::Greater
    } else if let (Some(a), Some(b)) = (left.as_f64(), right.as_f64()) {
        a.total_cmp(&b)
    } else {
        crate::json::locale_compare(left.as_str().unwrap_or(""), right.as_str().unwrap_or(""))
    }
}
fn compare(left: &Value, left_id: &str, right: &Value, right_id: &str, sort: &[Value]) -> Ordering {
    for spec in sort {
        let order = compare_value(&left[text(spec, "key")], &right[text(spec, "key")]);
        if order != Ordering::Equal {
            return if spec["direction"] == "asc" {
                order
            } else {
                order.reverse()
            };
        }
    }
    crate::json::locale_compare(left_id, right_id)
}
fn values(agent: &Value, sort: &[Value]) -> Value {
    Value::Object(
        sort.iter()
            .map(|s| (text(s, "key").to_owned(), sort_value(agent, text(s, "key"))))
            .collect(),
    )
}
fn paginate(entries: Vec<Value>, request: &Value) -> Result<Value> {
    let sort = normalize_sort(request);
    let mut entries = ordered(entries, &sort);
    if let Some(token) = request["page"]["cursor"].as_str().filter(|s| !s.is_empty()) {
        let decoded: Value = URL_SAFE_NO_PAD
            .decode(token.trim_end_matches('='))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .ok_or_else(|| anyhow!("Invalid fetch_agents cursor"))?;
        let valid = decoded["sort"].as_array().is_some_and(|s| {
            s.iter().all(|spec| {
                matches!(
                    text(spec, "key"),
                    "status_priority" | "created_at" | "updated_at" | "title"
                ) && matches!(text(spec, "direction"), "asc" | "desc")
            })
        });
        if !valid || !decoded["values"].is_object() || !decoded["id"].is_string() {
            bail!("Invalid fetch_agents cursor");
        }
        if decoded["sort"] != json!(sort) {
            bail!("fetch_agents cursor does not match current sort");
        }
        entries.retain(|(entry, keys)| {
            compare(
                keys,
                text(&entry["agent"], "id"),
                &decoded["values"],
                text(&decoded, "id"),
                &sort,
            ) == Ordering::Greater
        });
    }
    let limit = request["page"]["limit"].as_u64().unwrap_or(200) as usize;
    let has_more = entries.len() > limit;
    entries.truncate(limit);
    let cursor = if has_more {
        entries.last().map(|(entry, keys)| {
            URL_SAFE_NO_PAD.encode(
                serde_json::to_vec(&json!({"sort":sort,"values":keys,"id":entry["agent"]["id"]}))
                    .expect("JSON cursor"),
            )
        })
    } else {
        None
    };
    let entries: Vec<_> = entries.into_iter().map(|(entry, _)| entry).collect();
    Ok(
        json!({"entries":entries,"pageInfo":{"nextCursor":cursor,"prevCursor":request["page"]["cursor"],"hasMore":has_more}}),
    )
}

fn normalize_sort(request: &Value) -> Vec<Value> {
    let mut sort = Vec::new();
    if let Some(specs) = request["sort"].as_array() {
        for spec in specs {
            if !sort.iter().any(|s: &Value| s["key"] == spec["key"]) {
                sort.push(spec.clone());
            }
        }
    }
    if sort.is_empty() {
        sort.push(json!({"key":"updated_at","direction":"desc"}));
    }
    sort
}
fn ordered(entries: Vec<Value>, sort: &[Value]) -> Vec<(Value, Value)> {
    let mut entries: Vec<_> = entries
        .into_iter()
        .map(|entry| {
            let keys = values(&entry["agent"], sort);
            (entry, keys)
        })
        .collect();
    entries.sort_by(|(a, ak), (b, bk)| {
        compare(
            ak,
            text(&a["agent"], "id"),
            bk,
            text(&b["agent"], "id"),
            sort,
        )
    });
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cursor_tiebreaker_avoids_losing_equal_timestamps() {
        let entries = vec![
            json!({"agent":{"id":"b","updatedAt":"2026-01-01T00:00:00Z"}}),
            json!({"agent":{"id":"a","updatedAt":"2026-01-01T00:00:00Z"}}),
        ];
        let first = paginate(entries.clone(), &json!({"page":{"limit":1}})).unwrap();
        assert_eq!(first["entries"][0]["agent"]["id"], "a");
        let second = paginate(
            entries,
            &json!({"page":{"cursor":first["pageInfo"]["nextCursor"],"limit":1}}),
        )
        .unwrap();
        assert_eq!(second["entries"][0]["agent"]["id"], "b");
        assert_eq!(second["pageInfo"]["hasMore"], false);
    }
    #[test]
    fn cursor_rejects_changed_sort() {
        let token = URL_SAFE_NO_PAD.encode(
            br#"{"sort":[{"key":"title","direction":"asc"}],"values":{"title":"a"},"id":"a"}"#,
        );
        assert_eq!(
            paginate(vec![], &json!({"page":{"cursor":token}}))
                .unwrap_err()
                .to_string(),
            "fetch_agents cursor does not match current sort"
        );
    }
}

pub(crate) async fn legacy_attention_eligible(
    manager: &AgentManager,
    context: &RequestContext,
    snapshot: &Value,
) -> Result<bool> {
    if context.is_modern() {
        return Ok(false);
    }
    let requests = context.subscription_requests("agents");
    if requests.is_empty() {
        return Ok(false);
    }
    let agent = snapshot_payload(snapshot);
    if !provider_visible(context, text(&agent, "provider")) {
        return Ok(false);
    }
    let app = manager.app()?;
    let Some(project) = app
        .projects
        .project_placement_for_workspace(text(&agent, "workspaceId"))
        .await?
    else {
        return Ok(false);
    };
    let workspace = app.projects.workspace(text(&agent, "workspaceId")).await?;
    Ok(requests.iter().any(|request| {
        let background =
            request["includeBackground"] == true || request["filter"]["includeBackground"] == true;
        agent_directory_includes(&agent, workspace.as_ref(), background)
            && matches_agent_updates_filter(&agent, &project, request.get("filter"))
    }))
}
