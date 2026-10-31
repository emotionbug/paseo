use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[derive(Default)]
struct Hooks {
    agent: StdMutex<Option<Value>>,
    exists: AtomicBool,
    validations: AtomicUsize,
    reject_completed: AtomicBool,
}
#[async_trait]
impl CreationHooks for Hooks {
    async fn exists(&self, _: &str, _: &str) -> Result<bool> {
        Ok(self.exists.load(Ordering::SeqCst))
    }
    async fn read_agent(&self, _: &str) -> Result<Option<Value>> {
        Ok(self.agent.lock().unwrap().clone())
    }
    async fn validate_completed(&self, _: &Value) -> Result<()> {
        self.validations.fetch_add(1, Ordering::SeqCst);
        if self.reject_completed.load(Ordering::SeqCst) {
            bail!("Previously created agent no longer exists");
        }
        Ok(())
    }
}
fn input(kind: &str) -> CreationInput {
    CreationInput {
        kind: kind.to_owned(),
        key: "intent".to_owned(),
        request: json!({"prompt":"hello","config":{"cwd":"/project","provider":"custom"}}),
        workspace_id: Some("wks_0123456789abcdef".to_owned()),
        agent_id: Some("agent".to_owned()),
        has_agent: true,
        has_prompt: true,
    }
}
async fn run(
    store: &CreationReceipts,
    input: &CreationInput,
    hooks: &Hooks,
) -> Result<CreationGuard> {
    match store.begin(input, hooks).await? {
        CreationAdmission::Run(guard) => Ok(guard),
        _ => bail!("Expected creation runner"),
    }
}

#[tokio::test]
async fn send_serializes_retries_and_reconstruction() -> Result<()> {
    let home = tempfile::tempdir()?;
    let receipts = MessageReceipts::new(home.path().to_owned());
    let sends = AtomicUsize::new(0);
    let request = json!({"text":"hello"});
    let delivery = || async {
        sends.fetch_add(1, Ordering::SeqCst);
        Ok(())
    };
    let (first, second) = tokio::join!(
        receipts.send(
            "agent",
            "message",
            &request,
            || async { Ok(()) },
            |_| delivery()
        ),
        receipts.send(
            "agent",
            "message",
            &request,
            || async { Ok(()) },
            |_| delivery()
        )
    );
    first?;
    second?;
    MessageReceipts::new(home.path().to_owned())
        .send(
            "agent",
            "message",
            &request,
            || async { bail!("Completed replay must not prepare") },
            |_: ()| delivery(),
        )
        .await?;
    assert_eq!(sends.load(Ordering::SeqCst), 1);
    let conflict = receipts
        .send(
            "agent",
            "message",
            &json!({"text":"changed"}),
            || async { Ok(()) },
            |_| delivery(),
        )
        .await
        .unwrap_err();
    assert_eq!(conflict.to_string(), "agent_request_key_conflict");
    Ok(())
}
#[tokio::test]
async fn pending_send_is_not_replayed_but_preparation_can_retry() -> Result<()> {
    let home = tempfile::tempdir()?;
    let receipts = MessageReceipts::new(home.path().to_owned());
    let request = json!({});
    let prepare_error = receipts
        .send(
            "agent",
            "message",
            &request,
            || async { bail!("load failed") },
            |_: ()| async { Ok(()) },
        )
        .await
        .unwrap_err();
    assert_eq!(prepare_error.to_string(), "load failed");
    let delivery_error = receipts
        .send(
            "agent",
            "message",
            &request,
            || async { Ok(()) },
            |_| async { bail!("connection lost") },
        )
        .await
        .unwrap_err();
    assert_eq!(delivery_error.to_string(), "connection lost");
    let replay_error = MessageReceipts::new(home.path().to_owned())
        .send(
            "agent",
            "message",
            &request,
            || async { Ok(()) },
            |_| async { panic!("must not deliver pending receipt") },
        )
        .await
        .unwrap_err();
    assert_eq!(replay_error.to_string(), "agent_request_outcome_unknown");
    Ok(())
}
#[tokio::test]
async fn duplicate_joins_and_conflict_is_rejected_before_runner_finishes() -> Result<()> {
    let home = tempfile::tempdir()?;
    let receipts = CreationReceipts::new(home.path().to_owned());
    let hooks = Hooks::default();
    let input = input("workspace");
    let mut runner = run(&receipts, &input, &hooks).await?;
    runner.begin_stage("workspace").await?;
    runner
        .workspace_ready(json!({"id":"wks_0123456789abcdef"}), None)
        .await?;
    let CreationAdmission::Running(mut duplicate) = receipts.begin(&input, &hooks).await? else {
        panic!("duplicate must join");
    };
    assert_eq!(
        duplicate.snapshot.as_ref().unwrap()["phase"],
        "workspace_ready"
    );
    let mut conflict = input.clone();
    conflict.request["prompt"] = json!("different");
    assert_eq!(
        receipts
            .begin(&conflict, &hooks)
            .await
            .err()
            .unwrap()
            .to_string(),
        "workspace_request_key_conflict"
    );
    runner.begin_stage("agent").await?;
    runner.agent_ready(json!({"id":"agent"}), true).await?;
    runner.prompt_started(json!({"id":"agent"})).await?;
    runner.complete(None).await?;
    assert_eq!(duplicate.completion().await?["phase"], "completed");
    drop(runner);
    let CreationAdmission::Replay(replayed) = CreationReceipts::new(home.path().to_owned())
        .begin(&input, &hooks)
        .await?
    else {
        panic!("must replay");
    };
    assert_eq!(replayed["phase"], "completed");
    assert_eq!(hooks.validations.load(Ordering::SeqCst), 1);
    Ok(())
}
#[tokio::test]
async fn claims_reject_different_intents_and_survive_restart() -> Result<()> {
    let home = tempfile::tempdir()?;
    let receipts = CreationReceipts::new(home.path().to_owned());
    let hooks = Hooks::default();
    let first = input("workspace");
    let runner = run(&receipts, &first, &hooks).await?;
    let mut other = first.clone();
    other.key = "other".to_owned();
    assert_eq!(
        receipts
            .begin(&other, &hooks)
            .await
            .err()
            .unwrap()
            .to_string(),
        "workspace_id_conflict"
    );
    drop(runner);
    let mut restarted = run(
        &CreationReceipts::new(home.path().to_owned()),
        &first,
        &hooks,
    )
    .await?;
    assert_eq!(
        restarted.snapshot()["workspaceId"],
        first.workspace_id.unwrap()
    );
    restarted.complete(None).await?;
    Ok(())
}
#[tokio::test]
async fn retryable_agent_failure_preserves_workspace_and_generated_ids() -> Result<()> {
    let home = tempfile::tempdir()?;
    let receipts = CreationReceipts::new(home.path().to_owned());
    let hooks = Hooks::default();
    let mut input = input("workspace");
    input.agent_id = None;
    input.workspace_id = None;
    let mut runner = run(&receipts, &input, &hooks).await?;
    let agent_id = runner.snapshot()["agentId"].clone();
    let workspace_id = runner.snapshot()["workspaceId"].clone();
    assert_eq!(workspace_id.as_str().unwrap().len(), 20);
    runner.begin_stage("workspace").await?;
    runner
        .workspace_ready(json!({"id":workspace_id}), None)
        .await?;
    runner.begin_stage("agent").await?;
    runner.fail("provider unavailable", None, &hooks).await?;
    assert_eq!(runner.snapshot()["outcomeUnknown"], false);
    drop(runner);
    let retry = run(
        &CreationReceipts::new(home.path().to_owned()),
        &input,
        &hooks,
    )
    .await?;
    assert_eq!(retry.snapshot()["phase"], "workspace_ready");
    assert_eq!(retry.snapshot()["agentId"], agent_id);
    assert_eq!(retry.snapshot()["workspaceId"], workspace_id);
    assert!(retry.snapshot().get("failedStage").is_none());
    assert!(retry.snapshot().get("outcomeUnknown").is_none());
    Ok(())
}
#[tokio::test]
async fn interrupted_prompt_and_partial_workspace_remain_unknown() -> Result<()> {
    for stage in ["workspace", "prompt"] {
        let home = tempfile::tempdir()?;
        let receipts = CreationReceipts::new(home.path().to_owned());
        let hooks = Hooks::default();
        let input = input("workspace");
        let mut runner = run(&receipts, &input, &hooks).await?;
        runner.begin_stage(stage).await?;
        drop(runner);
        let recovered = CreationReceipts::new(home.path().to_owned());
        let subscription = recovered.subscribe(&input.kind, &input.key, &hooks).await?;
        let snapshot = subscription.snapshot.unwrap();
        assert_eq!(snapshot["phase"], "failed");
        assert_eq!(snapshot["failedStage"], stage);
        assert_eq!(snapshot["outcomeUnknown"], true);
        let CreationAdmission::Replay(replayed) = recovered.begin(&input, &hooks).await? else {
            panic!("ambiguous work must not run");
        };
        assert_eq!(replayed, snapshot);
    }
    Ok(())
}
#[tokio::test]
async fn ready_agent_without_prompt_can_complete_after_restart() -> Result<()> {
    let home = tempfile::tempdir()?;
    let receipts = CreationReceipts::new(home.path().to_owned());
    let hooks = Hooks::default();
    let mut input = input("agent");
    input.has_prompt = false;
    let mut runner = run(&receipts, &input, &hooks).await?;
    runner.begin_stage("agent").await?;
    runner.agent_ready(json!({"id":"agent"}), false).await?;
    drop(runner);
    let mut recovered = run(
        &CreationReceipts::new(home.path().to_owned()),
        &input,
        &hooks,
    )
    .await?;
    assert_eq!(recovered.snapshot()["agent"]["id"], "agent");
    recovered.complete(None).await?;
    hooks.reject_completed.store(true, Ordering::SeqCst);
    drop(recovered);
    assert_eq!(
        receipts
            .begin(&input, &hooks)
            .await
            .err()
            .unwrap()
            .to_string(),
        "Previously created agent no longer exists"
    );
    assert!(
        receipts
            .subscribe(&input.kind, &input.key, &hooks)
            .await
            .is_err()
    );
    Ok(())
}
#[tokio::test]
async fn imports_legacy_completed_or_pending_with_resource_without_reexecution() -> Result<()> {
    for state in ["pending", "completed"] {
        let home = tempfile::tempdir()?;
        let input = input("agent");
        let mut request = input.request.clone();
        request["type"] = json!("create_agent_request");
        let receipt =
            json!({"fingerprint":digest(&request)?,"state":state,"agentId":"legacy-agent"});
        atomic_json(
            &home
                .path()
                .join("agent-requests")
                .join(format!("{}.json", digest(&json!(["create", input.key]))?)),
            &receipt,
        )
        .await?;
        let hooks = Hooks::default();
        *hooks.agent.lock().unwrap() =
            Some(json!({"id":"legacy-agent","workspaceId":"legacy-workspace"}));
        let receipts = CreationReceipts::new(home.path().to_owned());
        let CreationAdmission::Replay(snapshot) = receipts.begin(&input, &hooks).await? else {
            panic!("legacy resource must be imported");
        };
        assert_eq!(snapshot["phase"], "completed");
        assert_eq!(snapshot["agentId"], "legacy-agent");
        assert_eq!(snapshot["workspaceId"], "legacy-workspace");
    }
    Ok(())
}
#[tokio::test]
async fn pending_legacy_without_resource_never_executes() -> Result<()> {
    let home = tempfile::tempdir()?;
    let input = input("agent");
    let mut request = input.request.clone();
    request["type"] = json!("create_agent_request");
    let path = home
        .path()
        .join("agent-requests")
        .join(format!("{}.json", digest(&json!(["create", input.key]))?));
    atomic_json(
        &path,
        &json!({"fingerprint":digest(&request)?,"state":"pending","agentId":"missing"}),
    )
    .await?;
    let CreationAdmission::Replay(snapshot) = CreationReceipts::new(home.path().to_owned())
        .begin(&input, &Hooks::default())
        .await?
    else {
        panic!("pending legacy receipt must not run");
    };
    assert_eq!(snapshot["outcomeUnknown"], true);
    Ok(())
}
#[tokio::test]
async fn corrupt_receipts_are_errors_not_missing_receipts() -> Result<()> {
    let home = tempfile::tempdir()?;
    let input = input("agent");
    let path = home
        .path()
        .join("creations")
        .join(format!("{}.json", identity_for(&input.kind, &input.key)?));
    atomic_json(&path, &json!({"snapshot":{}})).await?;
    assert!(
        CreationReceipts::new(home.path().to_owned())
            .begin(&input, &Hooks::default())
            .await
            .is_err()
    );
    Ok(())
}
#[cfg(unix)]
#[tokio::test]
async fn receipts_and_claims_are_private() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir()?;
    let receipts = CreationReceipts::new(home.path().to_owned());
    let input = input("workspace");
    let _runner = run(&receipts, &input, &Hooks::default()).await?;
    let mut entries = tokio::fs::read_dir(home.path().join("creations")).await?;
    while let Some(entry) = entries.next_entry().await? {
        assert_eq!(entry.metadata().await?.permissions().mode() & 0o777, 0o600);
    }
    Ok(())
}

#[tokio::test]
async fn failed_milestone_commit_does_not_publish_or_mutate_snapshot() -> Result<()> {
    let home = tempfile::tempdir()?;
    let receipts = CreationReceipts::new(home.path().to_owned());
    let input = input("agent");
    let mut runner = run(&receipts, &input, &Hooks::default()).await?;
    let mut events = receipts.subscribe_events();
    let before = runner.snapshot().clone();
    let receipt_path = home
        .path()
        .join("creations")
        .join(format!("{}.json", identity_for(&input.kind, &input.key)?));
    tokio::fs::remove_file(&receipt_path).await?;
    tokio::fs::create_dir(&receipt_path).await?;
    assert!(
        runner
            .agent_ready(json!({"id":"agent"}), false)
            .await
            .is_err()
    );
    assert_eq!(runner.snapshot(), &before);
    assert!(matches!(
        events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    Ok(())
}
