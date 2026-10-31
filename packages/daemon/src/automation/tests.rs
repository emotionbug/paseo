use super::*;

async fn service() -> (tempfile::TempDir, AutomationService) {
    let dir = tempfile::tempdir().unwrap();
    let now = date("2026-10-01T12:00:00Z").unwrap();
    let service = AutomationService::with_clock(Weak::new(), Arc::new(move || now));
    assert!(
        service
            .store
            .set(Store::open(dir.path().join("schedules")).await.unwrap())
            .is_ok()
    );
    (dir, service)
}
fn input() -> Value {
    json!({"name":" heartbeat ","prompt":" check status ","cadence":{"type":"every","everyMs":60000},"target":{"type":"agent","agentId":"00000000-0000-4000-8000-000000000001"}})
}
#[tokio::test]
async fn named_upsert_keeps_identity_and_history_but_rpc_create_does_not() {
    let (_dir, service) = service().await;
    let created = service.create_or_replace(&input()).await.unwrap();
    let id = created["id"].as_str().unwrap();
    let (_, run_id) = service
        .claim(id, true, service.now())
        .await
        .unwrap()
        .unwrap();
    service
        .finish(id, &run_id, true, Err(anyhow!("fixture failure")))
        .await
        .unwrap();
    let mut refreshed = input();
    refreshed["prompt"] = json!("new prompt");
    let updated = service.create_or_replace(&refreshed).await.unwrap();
    assert_eq!(updated["id"], created["id"]);
    assert_eq!(updated["runs"].as_array().unwrap().len(), 1);
    assert_eq!(updated["runs"][0]["error"], "fixture failure");
    assert_ne!(
        service.create(&refreshed).await.unwrap()["id"],
        created["id"]
    );
}
#[tokio::test]
async fn claim_is_durable_and_cannot_replay_after_uncertain_finalization() {
    let (dir, service) = service().await;
    let created = service.create(&input()).await.unwrap();
    let id = created["id"].as_str().unwrap();
    let (first, second) = tokio::join!(
        service.claim(id, false, service.now()),
        service.claim(id, false, service.now())
    );
    assert_eq!(
        usize::from(first.unwrap().is_some()) + usize::from(second.unwrap().is_some()),
        1
    );
    assert!(service.claim(id, true, service.now()).await.is_err());
    let now = date("2026-10-02T12:00:00Z").unwrap();
    let restarted = AutomationService::with_clock(Weak::new(), Arc::new(move || now));
    assert!(
        restarted
            .store
            .set(Store::open(dir.path().join("schedules")).await.unwrap())
            .is_ok()
    );
    restarted.recover().await.unwrap();
    let schedule = restarted.inspect(id).await.unwrap();
    assert_eq!(schedule["runs"][0]["status"], "failed");
    assert_eq!(
        schedule["runs"][0]["error"],
        "Daemon restarted before the scheduled run completed"
    );
    assert_eq!(schedule["nextRunAt"], "2026-10-02T12:01:00.000Z");
    assert!(restarted.claim(id, false, now).await.unwrap().is_none());
}
#[tokio::test]
async fn manual_paused_runs_do_not_advance_or_complete_cadence() {
    let (_dir, service) = service().await;
    let mut value = input();
    value["maxRuns"] = json!(1);
    let created = service.create(&value).await.unwrap();
    let id = created["id"].as_str().unwrap();
    service.pause(id).await.unwrap();
    let (_, run_id) = service
        .claim(id, true, service.now())
        .await
        .unwrap()
        .unwrap();
    service
        .finish(id, &run_id, true, Err(anyhow!("manual")))
        .await
        .unwrap();
    let paused = service.inspect(id).await.unwrap();
    assert_eq!(paused["status"], "paused");
    assert!(paused["nextRunAt"].is_null());
    service.resume(id).await.unwrap();
    service.tick().await.unwrap();
    assert_eq!(service.inspect(id).await.unwrap()["status"], "completed");
    assert!(service.resume(id).await.is_err());
    assert!(service.claim(id, true, service.now()).await.is_err());
}
#[tokio::test]
async fn update_and_upsert_preserve_timezone_and_clear_nullable_launch_fields() {
    let (_dir, service) = service().await;
    let mut value = input();
    value["cadence"] =
        json!({"type":"cron","expression":"0 9 * * *","timezone":"America/New_York"});
    value["target"] = json!({"type":"new-agent","config":{"provider":"codex","cwd":"/isolated","model":"model-a"}});
    let created = service.create(&value).await.unwrap();
    let id = created["id"].as_str().unwrap();
    let updated=service.update(&json!({"id":id,"cadence":{"type":"cron","expression":"0 10 * * *"},"newAgentConfig":{"model":null}})).await.unwrap();
    assert_eq!(updated["cadence"]["timezone"], "America/New_York");
    assert!(updated["target"]["config"].get("model").is_none());
    assert_eq!(updated["nextRunAt"], "2026-10-01T14:00:00.000Z");
}
