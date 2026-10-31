use super::*;
use std::collections::BTreeSet;

#[path = "wire_directory.rs"]
mod directory;
pub(crate) use directory::legacy_attention_eligible;

pub(super) async fn handle(
    manager: &AgentManager,
    context: &RequestContext,
    message: &Value,
) -> Result<bool> {
    if directory::handle(manager, context, message).await? {
        return Ok(true);
    }
    let id = text(message, "agentId");
    let kind = text(message, "type");
    match kind {
        "agent.timeline.set_subscription.request" => {
            let ids: BTreeSet<String> = strings(&message["agentIds"])
                .into_iter()
                .map(str::to_owned)
                .collect();
            let owner = context.begin_subscription("timelines", None, None)?;
            let mut events = manager.subscribe_events();
            reply(
                context,
                "agent.timeline.set_subscription.response",
                correlated(
                    message,
                    json!({"agentIds":ids,"subscriptionId":owner.response_id()}),
                ),
            )
            .await?;
            owner.activate().await?;
            let app = std::sync::Arc::downgrade(&manager.app()?);
            tokio::spawn(async move {
                let cancel = owner.cancellation();
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
                            Ok(event) => event,
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                let Some(app) = app.upgrade() else {
                                    break;
                                };
                                for id in &ids {
                                    match app.agents.timeline(id,&json!({"direction":"tail","limit":1})).await {
                                        Ok(page) => pending.push_back(json!({"type":"timeline_replacement","agentId":id,"epoch":page["epoch"]})),
                                        Err(_) => {let _ = owner.release().await; return;}
                                    }
                                }
                                continue;
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        }
                    };
                    let id = text(&event, "agentId");
                    if !ids.contains(id) {
                        continue;
                    }
                    let Some(app) = app.upgrade() else {
                        break;
                    };
                    let result: Result<()> = async {
                        if event["type"] == "agent_stream" {
                            if !owner.context().is_modern() && !supports(owner.context(),"selective_agent_timeline") { return Ok(()); }
                            if event["event"]["type"] == "attention_required" && (owner.context().is_modern() || supports(owner.context(),"selective_agent_timeline")) { return Ok(()); }
                            if event["event"]["type"] == "timeline" && !supports_item(owner.context(),&event["event"]["item"]) { return Ok(()); }
                            let Some(stream) = serialize_stream_event(&event["event"]) else {return Ok(());};
                            let mut payload = json!({"agentId":id,"event":stream,"timestamp":chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis,true)});
                            for field in ["seq","epoch","timestamp"] { if let Some(value) = event.get(field) { payload[field] = value.clone(); } }
                            owner.emit(json!({"type":"agent_stream","payload":payload})).await?;
                        } else if event["type"] == "timeline_replacement" {
                            if owner.context().is_modern() || supports(owner.context(),"timeline_replacement_invalidation") {
                                if !owner.context().is_modern() && event["initiatingSourceId"].as_str() == Some(owner.source_id()) {return Ok(());}
                                owner.emit(json!({"type":"agent.timeline.replacement","payload":{"agentId":id,"epoch":event["epoch"]}})).await?;
                            } else {
                                let timeline = app.agents.timeline(id,&json!({"direction":"tail","limit":0})).await?;
                                let snapshot = app.agents.agent_snapshot(id).await?;
                                if let Some(rows) = timeline["rows"].as_array() {
                                    for row in rows {
                                        if supports_item(owner.context(),&row["item"]) {
                                            let mut stream = json!({"type":"timeline","provider":snapshot.as_ref().map(|s|&s["provider"]),"item":row["item"]});
                                            if let Some(turn) = row.get("turnId") {stream["turnId"] = turn.clone();}
                                            owner.emit(json!({"type":"agent_stream","payload":{"agentId":id,"timestamp":row["timestamp"],"seq":row["seq"],"epoch":timeline["epoch"],"event":stream}})).await?;
                                        }
                                    }
                                }
                            }
                        }
                        Ok(())
                    }.await;
                    if result.is_err() {
                        break;
                    }
                }
                let _ = owner.release().await;
            });
        }
        "fetch_agent_timeline_request" | "agent.provider_subagents.timeline.get.request" => {
            let subagent = kind == "agent.provider_subagents.timeline.get.request";
            let mut options = message.clone();
            options["direction"] = message
                .get("direction")
                .filter(|direction| !direction.is_null())
                .cloned()
                .unwrap_or_else(|| {
                    json!(if message["cursor"].is_object() {
                        "after"
                    } else {
                        "tail"
                    })
                });
            if options.get("limit").is_none_or(Value::is_null) {
                options["limit"] = json!(if options["direction"] == "after" {
                    0
                } else {
                    200
                });
            }
            let result = if subagent {
                manager
                    .subagent_timeline(
                        text(message, "parentAgentId"),
                        text(message, "subagentId"),
                        &options,
                    )
                    .await
            } else {
                manager.timeline(id, &options).await
            };
            let mut payload = match result {
                Ok(timeline) => {
                    let mut payload = project_page(context, timeline, subagent);
                    if subagent {
                        if !supports(context, "projected_subagent_timeline") {
                            payload["hasOlder"] = json!(false);
                            payload["hasNewer"] = json!(false);
                            payload["rows"] = json!([{"seq":payload["window"]["maxSeq"],"timestamp":chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis,true),"item":{"type":"assistant_message","text":"Please upgrade the Paseo app to view this subagent conversation."}}]);
                        }
                    } else {
                        let agent = manager
                            .agent_snapshot(id)
                            .await?
                            .as_ref()
                            .map(snapshot_payload);
                        if let Some(entries) = payload["entries"].as_array_mut() {
                            for row in entries {
                                row["provider"] = agent
                                    .as_ref()
                                    .map(|a| a["provider"].clone())
                                    .unwrap_or(Value::Null);
                            }
                        }
                        payload["agent"] = json!(agent);
                    }
                    payload
                }
                Err(error) => {
                    let mut payload = json!({"epoch":"","reset":false,"staleCursor":false,"gap":false,"window":{"minSeq":0,"maxSeq":0,"nextSeq":0},"hasOlder":false,"hasNewer":false,"error":error.to_string()});
                    if subagent {
                        payload["provider"] = Value::Null;
                        payload["rows"] = json!([]);
                    } else {
                        payload["projection"] = json!("projected");
                        payload["agent"] = Value::Null;
                        payload["startCursor"] = Value::Null;
                        payload["endCursor"] = Value::Null;
                        payload["entries"] = json!([]);
                    }
                    payload
                }
            };
            payload["direction"] = options["direction"].clone();
            if subagent {
                payload["parentAgentId"] = message["parentAgentId"].clone();
                payload["subagentId"] = message["subagentId"].clone();
            } else {
                payload["agentId"] = json!(id);
                if message["mergeWindow"] == true {
                    payload["mergeWindow"] = json!(true);
                }
            }
            reply(
                context,
                if subagent {
                    "agent.provider_subagents.timeline.get.response"
                } else {
                    "fetch_agent_timeline_response"
                },
                correlated(message, payload),
            )
            .await?;
        }
        "agent.timeline.search.request" | "agent.timeline.list_prompts.request" => {
            let search = kind == "agent.timeline.search.request";
            let result = if search {
                manager.timeline_search(id, message).await
            } else {
                manager.timeline_prompts(id).await
            };
            let mut payload = match result {
                Ok(mut value) => {
                    value["error"] = Value::Null;
                    value
                }
                Err(error) => {
                    if search {
                        json!({"epoch":"","locations":[],"nextCursor":null,"error":error.to_string()})
                    } else {
                        json!({"epoch":"","prompts":[],"error":error.to_string()})
                    }
                }
            };
            payload["agentId"] = json!(id);
            reply(
                context,
                if search {
                    "agent.timeline.search.response"
                } else {
                    "agent.timeline.list_prompts.response"
                },
                correlated(message, payload),
            )
            .await?;
        }
        "agent.timeline.append.request" => {
            let plugin_id = context
                .client_id()
                .strip_prefix("plugin:")
                .filter(|id| !id.is_empty())
                .ok_or_else(|| anyhow!("Only plugin sessions can append plugin timeline items"))?;
            if crate::json::stringify(&message["item"]["data"])?.len() > 64 * 1024 {
                bail!("Plugin timeline item data exceeds 65536 bytes");
            }
            let mut item = message["item"].clone();
            item["pluginId"] = json!(plugin_id);
            let result = manager.append_timeline(id, item).await?;
            reply(
                context,
                "agent.timeline.append.response",
                correlated(message, result),
            )
            .await?;
        }
        "agent.provider_subagents.list.request" => {
            let payload = match manager.subagents(text(message, "parentAgentId")).await {
                Ok(subagents) => {
                    json!({"parentAgentId":message["parentAgentId"],"subagents":subagents,"error":null})
                }
                Err(error) => {
                    json!({"parentAgentId":message["parentAgentId"],"subagents":[],"error":error.to_string()})
                }
            };
            reply(
                context,
                "agent.provider_subagents.list.response",
                correlated(message, payload),
            )
            .await?;
        }
        "agent.fork_context.request" => {
            let mut payload = match manager.fork_context(id, message).await {
                Ok(mut value) => {
                    value["error"] = Value::Null;
                    value
                }
                Err(error) => {
                    json!({"attachment":null,"itemCount":0,"boundaryCursor":message["boundaryCursor"],"boundaryMessageId":message["boundaryMessageId"],"error":error.to_string()})
                }
            };
            payload["agentId"] = json!(id);
            reply(
                context,
                "agent.fork_context.response",
                correlated(message, payload),
            )
            .await?;
        }
        _ => return Ok(false),
    }
    Ok(true)
}
fn supports(context: &RequestContext, capability: &str) -> bool {
    context
        .capabilities()
        .as_array()
        .is_some_and(|values| values.iter().any(|v| v == capability))
        || context.capabilities()[capability] == true
}
fn supports_item(context: &RequestContext, item: &Value) -> bool {
    match text(item, "type") {
        "notification" => supports(context, "timeline_notifications"),
        "plugin" => supports(context, "plugin_timeline_items"),
        _ => true,
    }
}
fn project_page(context: &RequestContext, timeline: Value, subagent: bool) -> Value {
    let mut out = json!({"projection":"projected","error":null});
    for key in [
        "epoch",
        "direction",
        "reset",
        "staleCursor",
        "gap",
        "window",
        "hasOlder",
        "hasNewer",
        "provider",
    ] {
        if let Some(value) = timeline.get(key) {
            out[key] = value.clone();
        }
    }
    for (field, sequence) in [("startCursor", "startSeq"), ("endCursor", "endSeq")] {
        out[field] = if timeline[sequence].is_number() {
            json!({"epoch":timeline["epoch"],"seq":timeline[sequence]})
        } else {
            Value::Null
        };
    }
    let mut rows = Vec::new();
    if let Some(entries) = timeline["rows"].as_array() {
        for row in entries {
            if !supports_item(context, &row["item"]) {
                continue;
            }
            let mut projected = json!({"item":row["item"],"timestamp":row["timestamp"],"seqStart":row["seqStart"],"seqEnd":row["seqEnd"],"sourceSeqRanges":row["sourceSeqRanges"]});
            if subagent {
                projected["seq"] = row["seqEnd"].clone();
            } else {
                if let Some(turn) = row.get("turnId") {
                    projected["turnId"] = turn.clone();
                }
                projected["collapsed"] = json!(
                    row["collapsed"]
                        .as_array()
                        .map(|v| v
                            .iter()
                            .filter(|value| supports(context, "reasoning_merge_enum")
                                || **value != "reasoning_merge")
                            .cloned()
                            .collect::<Vec<_>>())
                        .unwrap_or_default()
                );
            }
            rows.push(projected);
        }
    }
    out[if subagent { "rows" } else { "entries" }] = json!(rows);
    out
}
