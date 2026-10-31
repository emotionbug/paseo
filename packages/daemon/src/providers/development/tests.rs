use super::*;

fn session(features: Value) -> (Arc<dyn AgentSession>, tokio::sync::mpsc::Receiver<Value>) {
    let (sink, events) = EventSink::channel(128, CancellationToken::new());
    let session = DevelopmentProvider::new("mock")
        .unwrap()
        .session(
            json!({"featureValues":features}),
            "test-session".into(),
            sink,
        )
        .unwrap();
    (session, events)
}
async fn event(events: &mut tokio::sync::mpsc::Receiver<Value>) -> Value {
    events.recv().await.unwrap()
}

#[tokio::test(start_paused = true)]
async fn delayed_echo_precedes_response_and_completion() {
    let (session, mut events) = session(json!({"mockAssistantResponse":"settled"}));
    let accepted = session
        .start_turn(
            json!("delay synthetic user message by 100ms"),
            json!({"clientMessageId":"client"}),
        )
        .await
        .unwrap();
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(99)).await;
    assert!(events.try_recv().is_err());
    tokio::time::advance(Duration::from_millis(1)).await;
    assert_eq!(event(&mut events).await["type"], "turn_started");
    let user = event(&mut events).await;
    assert_eq!(user["item"]["clientMessageId"], "client");
    assert_eq!(user["turnId"], accepted["turnId"]);
    assert_eq!(event(&mut events).await["item"]["text"], "settled");
    let completion = event(&mut events).await;
    assert_eq!(completion["type"], "turn_completed");
    assert!(completion.get("usage").is_none());
}

#[tokio::test(start_paused = true)]
async fn streaming_tokens_wait_for_configured_interval_and_interrupt_fences_future_tokens() {
    let (session, mut events) = session(
        json!({"mockStreamingAssistantResponse":"abcdef ghij","mockStreamingAssistantIntervalMs":80}),
    );
    session
        .start_turn(json!("stream"), json!({}))
        .await
        .unwrap();
    assert_eq!(event(&mut events).await["type"], "turn_started");
    assert_eq!(event(&mut events).await["item"]["type"], "user_message");
    assert_eq!(event(&mut events).await["item"]["text"], "abcd");
    tokio::time::advance(Duration::from_millis(79)).await;
    assert!(events.try_recv().is_err());
    tokio::time::advance(Duration::from_millis(1)).await;
    assert_eq!(event(&mut events).await["item"]["text"], "ef");
    session.interrupt().await.unwrap();
    assert_eq!(event(&mut events).await["type"], "turn_canceled");
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn rejected_prompts_and_ambiguous_steers_do_not_silently_accept() {
    let (session, mut events) =
        session(json!({"mockPromptRejections":1,"mockSteerAmbiguousFailures":1}));
    assert_eq!(
        session
            .start_turn(json!("first"), json!({}))
            .await
            .unwrap_err()
            .to_string(),
        "Requested mock prompt rejection"
    );
    assert!(events.try_recv().is_err());
    let accepted = session
        .start_turn(
            json!("withhold synthetic user message until interrupted"),
            json!({}),
        )
        .await
        .unwrap();
    assert_eq!(
        session
            .steer_active_turn(json!("steer"), json!({"expectedTurnId":"wrong"}))
            .await
            .unwrap()["status"],
        "unavailable"
    );
    let options = json!({"expectedTurnId":accepted["turnId"]});
    assert_eq!(
        session
            .steer_active_turn(json!("steer"), options.clone())
            .await
            .unwrap_err()
            .to_string(),
        "Requested mock steer transport failure"
    );
    assert_eq!(
        session
            .steer_active_turn(json!("steer"), options)
            .await
            .unwrap()["status"],
        "accepted"
    );
    session.interrupt().await.unwrap();
    assert_eq!(event(&mut events).await["type"], "turn_canceled");
    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn permissions_resolve_once_and_rewind_keeps_first_user_message() {
    let (session, mut events) = session(json!({}));
    session
        .start_turn(json!("emit synthetic questions freeform"), json!({}))
        .await
        .unwrap();
    event(&mut events).await;
    let user = event(&mut events).await;
    let permission = event(&mut events).await;
    assert_eq!(
        permission["request"]["input"]["questions"][0]["header"],
        "repoUrl"
    );
    let request_id = permission["request"]["id"].as_str().unwrap();
    assert_eq!(session.pending_permissions().len(), 1);
    session
        .respond_to_permission(request_id, json!({"behavior":"allow"}))
        .await
        .unwrap();
    assert_eq!(event(&mut events).await["type"], "permission_resolved");
    assert_eq!(event(&mut events).await["type"], "turn_completed");
    session
        .respond_to_permission(request_id, json!({"behavior":"deny"}))
        .await
        .unwrap();
    assert!(session.pending_permissions().is_empty());
    assert!(events.try_recv().is_err());
    assert!(
        session
            .rewind(RewindTarget::Conversation, "missing")
            .await
            .is_err()
    );
    session
        .rewind(
            RewindTarget::Both,
            user["item"]["messageId"].as_str().unwrap(),
        )
        .await
        .unwrap();
    let (sink, mut history) = EventSink::channel(16, CancellationToken::new());
    session.stream_history(sink).await.unwrap();
    assert_eq!(event(&mut history).await, user);
    assert!(history.try_recv().is_err());
}

#[tokio::test(start_paused = true)]
async fn slow_catalog_remains_pending_until_owner_cancels() {
    let provider = DevelopmentProvider::new("mock-slow").unwrap();
    let cancel = CancellationToken::new();
    let catalog = provider.fetch_catalog(json!({}), cancel.clone());
    tokio::pin!(catalog);
    tokio::select! {
        result=&mut catalog=>panic!("slow catalog unexpectedly finished: {result:?}"),
        _=tokio::time::sleep(Duration::from_secs(60))=>{}
    }
    cancel.cancel();
    assert!(catalog.await.is_err());
}
