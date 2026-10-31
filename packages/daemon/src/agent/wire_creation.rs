use super::*;

pub(super) async fn handle(
    manager: &AgentManager,
    context: &RequestContext,
    message: &Value,
) -> Result<bool> {
    match text(message, "type") {
        "creation.subscribe.request" => {
            let kind = text(message, "kind").to_owned();
            let key = text(message, "idempotencyKey").to_owned();
            let owner = if message["subscribe"] == false {
                None
            } else {
                Some(context.begin_subscription(
                    "creation",
                    None,
                    Some(&serde_json::to_string(&json!([kind, key]))?),
                )?)
            };
            let mut events = manager.subscribe_creation_events();
            match manager.creation_snapshot(&kind, &key).await {
                Ok(snapshot) => {
                    let mut revision = snapshot.as_ref().and_then(|s| s["revision"].as_u64());
                    let mut payload =
                        correlated(message, json!({"snapshot":snapshot,"error":null}));
                    if let Some(owner) = &owner {
                        payload["subscriptionId"] = json!(owner.response_id());
                    }
                    reply(context, "creation.subscribe.response", payload).await?;
                    if let Some(owner) = owner {
                        let app = std::sync::Arc::downgrade(&manager.app()?);
                        owner.activate().await?;
                        tokio::spawn(async move {
                            let cancel = owner.cancellation();
                            loop {
                                let event = tokio::select! { biased; _ = cancel.cancelled() => break, event = events.recv() => event };
                                let snapshot = match event {
                                    Ok(value) => value,
                                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                        let Some(app) = app.upgrade() else {
                                            break;
                                        };
                                        match app.agents.creation_snapshot(&kind, &key).await {
                                            Ok(Some(value)) => value,
                                            _ => break,
                                        }
                                    }
                                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                                };
                                if snapshot["kind"] == kind
                                    && snapshot["idempotencyKey"] == key
                                    && snapshot["revision"].as_u64().is_some_and(|next| {
                                        revision.is_none_or(|previous| next > previous)
                                    })
                                {
                                    revision = snapshot["revision"].as_u64();
                                    if owner.emit(update(snapshot)).await.is_err() {
                                        break;
                                    }
                                }
                            }
                            let _ = owner.release().await;
                        });
                    }
                }
                Err(error) => {
                    if let Some(owner) = owner {
                        owner.release().await?;
                    }
                    reply(
                        context,
                        "creation.subscribe.response",
                        correlated(message, json!({"snapshot":null,"error":error.to_string()})),
                    )
                    .await?;
                }
            }
        }
        "agent.create.request" => {
            let progress = if message["subscribe"] == true {
                Some(
                    context
                        .retain_operation(&["agent.create.update", "workspace.create.update"])?,
                )
            } else {
                None
            };
            let mut events = manager.subscribe_creation_events();
            let key = message
                .get("idempotencyKey")
                .unwrap_or(&message["requestId"])
                .clone();
            let creation = manager.create_with_receipt(context.principal(), message.clone());
            tokio::pin!(creation);
            let result = loop {
                tokio::select! {
                    result = &mut creation => break result,
                    event = events.recv(), if progress.is_some() => {
                        match event {
                            Ok(snapshot) if snapshot["kind"] == "agent" && snapshot["idempotencyKey"] == key => {
                                // A disconnected requester does not cancel durable creation.
                                let _ = progress.as_ref().unwrap().emit(update(snapshot)).await;
                            }
                            Ok(_) => {}
                            Err(_) => break creation.await,
                        }
                    }
                }
            };
            if let Some(progress) = progress {
                while let Ok(snapshot) = events.try_recv() {
                    if snapshot["kind"] == "agent" && snapshot["idempotencyKey"] == key {
                        let _ = progress.emit(update(snapshot)).await;
                    }
                }
                progress.release().await?;
            }
            if context.cancellation().is_cancelled() {
                return Ok(true);
            }
            let payload = match result {
                Ok(creation) => {
                    json!({"agent":creation["agent"],"error":creation["error"],"creation":creation})
                }
                Err(error) => json!({"agent":null,"error":error.to_string()}),
            };
            reply(
                context,
                "agent.create.response",
                correlated(message, payload),
            )
            .await?;
        }
        "create_agent_request" => {
            let result = async {
                if message.get("idempotencyKey").is_some() {
                    if message.get("initialPrompt").is_some() {
                        bail!("Idempotent creation requires sending the initial prompt separately");
                    }
                    let creation = manager
                        .create_with_receipt(context.principal(), message.clone())
                        .await?;
                    if !creation["error"].is_null() {
                        return Err(anyhow!(crate::agent::creation::AgentCreationError {
                            code: creation["errorCode"]
                                .as_str()
                                .unwrap_or("unknown")
                                .to_owned(),
                            message: text(&creation, "error").to_owned(),
                            details: Value::Null
                        }));
                    }
                    let id = text(&creation["agent"], "id");
                    return manager
                        .agent_snapshot(id)
                        .await?
                        .ok_or_else(|| anyhow!("Previously created agent no longer exists"));
                }
                let result = manager.create(context.principal(), message.clone()).await?;
                Ok(result["liveSnapshot"].clone())
            }
            .await;
            match result {
                Ok(agent) => reply(context,"status",correlated(message,json!({"status":"agent_created","agentId":agent["id"],"agent":snapshot_payload(&agent)}))).await?,
                Err(error) => {
                    reply(context,"status",correlated(message,json!({"status":"agent_create_failed","error":error.to_string(),"errorCode":error_code(&error,"unknown")}))).await?;
                    activity_error(manager,format!("Failed to create agent: {error}")).await?;
                }
            }
        }
        "resume_agent_request" | "refresh_agent_request" | "import_agent_request" => {
            let kind = text(message, "type");
            // Request-shape failures are reported without the "Failed to ..." activity
            // wrapper; an import missing its identity logs no activity at all.
            if kind == "resume_agent_request" && message["handle"].is_null() {
                let error = anyhow!("Unable to resume agent: missing persistence handle");
                if message.get("requestId").is_some() {
                    rpc_error(context, message, "agent_resume_failed", &error).await?;
                }
                activity_error(manager, error.to_string()).await?;
                return Ok(true);
            }
            if kind == "import_agent_request"
                && (text(message, "providerId").is_empty() && text(message, "provider").is_empty()
                    || text(message, "providerHandleId").is_empty()
                        && text(message, "sessionId").is_empty())
            {
                reply(
                    context,
                    "status",
                    correlated(
                        message,
                        json!({"status":"agent_create_failed","error":"Import requires providerId and providerHandleId"}),
                    ),
                )
                .await?;
                return Ok(true);
            }
            let result = async {
                if kind == "import_agent_request" {
                    let mut input = message.clone();
                    input["provider"] = message
                        .get("providerId")
                        .or_else(|| message.get("provider"))
                        .cloned()
                        .unwrap_or(Value::Null);
                    input["providerHandleId"] = message
                        .get("providerHandleId")
                        .or_else(|| message.get("sessionId"))
                        .cloned()
                        .unwrap_or(Value::Null);
                    if text(&input, "provider").is_empty()
                        || text(&input, "providerHandleId").is_empty()
                    {
                        bail!("Import requires providerId and providerHandleId");
                    }
                    if text(&input, "cwd").is_empty() {
                        bail!("Import requires cwd from the selected provider session");
                    }
                    manager
                        .import_provider_session(context.principal(), input)
                        .await
                } else {
                    manager
                        .control(
                            context.principal(),
                            text(message, "agentId"),
                            if kind == "resume_agent_request" {
                                "resume"
                            } else {
                                "refresh"
                            },
                            message.clone(),
                        )
                        .await
                }
            }
            .await;
            match result {
                Ok(result) => {
                    if message.get("requestId").is_some() {
                        let snapshot = result.get("snapshot").unwrap_or(&result);
                        let mut payload = json!({"status":if kind == "refresh_agent_request" {"agent_refreshed"} else {"agent_resumed"},"agentId":snapshot["id"],"timelineSize":result["timelineSize"]});
                        if kind != "refresh_agent_request" {
                            payload["agent"] = snapshot_payload(snapshot);
                        }
                        reply(context, "status", correlated(message, payload)).await?;
                    }
                }
                Err(error) => {
                    if kind == "import_agent_request" {
                        reply(
                            context,
                            "status",
                            correlated(
                                message,
                                json!({"status":"agent_create_failed","error":error.to_string()}),
                            ),
                        )
                        .await?;
                    } else if message.get("requestId").is_some() {
                        rpc_error(
                            context,
                            message,
                            if kind == "resume_agent_request" {
                                "agent_resume_failed"
                            } else {
                                "agent_refresh_failed"
                            },
                            &error,
                        )
                        .await?;
                    }
                    let label = match kind {
                        "import_agent_request" => "import",
                        "resume_agent_request" => "resume",
                        _ => "refresh",
                    };
                    activity_error(manager, format!("Failed to {label} agent: {error}")).await?;
                }
            }
        }
        _ => return Ok(false),
    }
    Ok(true)
}

fn update(snapshot: Value) -> Value {
    json!({"type":if snapshot["kind"] == "workspace" {"workspace.create.update"} else {"agent.create.update"},"payload":snapshot})
}
