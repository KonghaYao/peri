use super::*;
use peri_acp_types::tasks::{ExternalNotifyFn, TaskManager as TaskManagerPort};

fn make_result() -> BackgroundTaskResult {
    BackgroundTaskResult {
        task_id: "owner-task".into(),
        agent_name: "mcp".into(),
        prompt_summary: "task".into(),
        success: true,
        output: "done".into(),
        tool_calls_count: 0,
        duration_ms: 0,
        timed_out: false,
        child_thread_id: None,
        subagent_failure: None,
        shell_output: None,
    }
}

fn ok_notify() -> ExternalNotifyFn {
    Arc::new(|_, _| Box::pin(async { Ok(()) }))
}

fn make_registration(on_terminal: ExternalNotifyFn) -> ExternalTaskRegistration {
    ExternalTaskRegistration {
        session_id: "session".into(),
        initiator_session_id: Some("session".into()),
        owner_identity: "owner".into(),
        owner_task_id: "owner-task".into(),
        kind: BgTaskKind::Mcp,
        summary: "task".into(),
        started_at: Some("2026-01-01T00:00:00Z".into()),
        cancel: Arc::new(|| Box::pin(async { Ok(()) })),
        on_terminal,
    }
}

// 回归：未知任务不能以 Ok(false) 让订阅误认为完成投递。
#[tokio::test]
async fn test_missing_external_task_reports_unconfirmed_delivery() {
    let manager = TaskManager::new();
    let error = manager
        .settle_external("missing", "terminal", make_result())
        .await
        .unwrap_err();
    assert_eq!(
        error,
        "external task missing has no terminal delivery route"
    );
    assert!(manager.snapshot().tasks.is_empty());
}

// 回归：竞争中的结算不代表已送达；首个回调失败后仍能重试。
#[tokio::test]
async fn test_competing_external_settlement_does_not_acknowledge_failed_delivery() {
    let manager = Arc::new(TaskManager::new());
    let weak = Arc::downgrade(&manager);
    let request = make_registration(Arc::new(move |result, _| {
        let manager = weak.upgrade().unwrap();
        let result = result.clone();
        Box::pin(async move {
            let error = manager
                .settle_external(&result.task_id, "terminal", result.clone())
                .await
                .unwrap_err();
            assert!(error.contains("settlement is still pending"), "{error}");
            Err("提交失败".into())
        })
    }));
    let task_id = manager.register_external(request).unwrap();
    assert_eq!(
        manager
            .settle_external(&task_id, "terminal", make_result())
            .await,
        Err("提交失败".into())
    );
    assert_eq!(manager.active_count(), 1);
    manager
        .register_external(make_registration(ok_notify()))
        .unwrap();
    assert!(manager
        .settle_external(&task_id, "terminal", make_result())
        .await
        .unwrap());
    assert_eq!(manager.active_count(), 0);
    assert!(!manager
        .settle_external(&task_id, "terminal", make_result())
        .await
        .unwrap());
    assert_eq!(manager.snapshot().tasks[0].status, "completed");
}

// 回归：恢复遇到正在提交的任务也必须返回待重试，而非报告恢复成功。
#[tokio::test]
async fn test_restoration_preserves_pending_settlement() {
    let manager = TaskManager::new();
    let task_id = manager
        .register_external(make_registration(ok_notify()))
        .unwrap();
    assert!(manager.registry.claim_completion(&task_id));
    let error = manager
        .restore_external_terminal(make_registration(ok_notify()), "terminal", make_result())
        .await
        .unwrap_err();
    assert!(error.contains("settlement is still pending"), "{error}");
    assert_eq!(manager.active_count(), 1);
    assert_eq!(manager.snapshot().tasks[0].status, "running");
}

// 回归：有界放弃必须产出终态，不能把任务留在 active。
#[tokio::test]
async fn test_abandon_external_settles_with_terminal_record() {
    let manager = TaskManager::new();
    let task_id = manager
        .register_external(make_registration(ok_notify()))
        .unwrap();
    assert!(manager
        .abandon_external(&task_id, "owner unreachable")
        .await
        .unwrap());
    assert_eq!(manager.active_count(), 0);
    let snapshot = manager.snapshot();
    assert_eq!(snapshot.tasks[0].status, "failed");
    assert!(!TaskManagerPort::has_unsettled_external(&manager));
}

fn child_registration(on_terminal: ExternalNotifyFn) -> ExternalTaskRegistration {
    let mut request = make_registration(on_terminal);
    request.initiator_session_id = Some("child-session".into());
    request
}

fn assert_snapshot_unchanged(manager: &TaskManager, before: &TaskSnapshot) {
    assert_eq!(
        serde_json::to_value(manager.snapshot()).unwrap(),
        serde_json::to_value(before).unwrap()
    );
}

#[tokio::test]
async fn test_register_external_rejects_conflicting_initiator_before_mutation() {
    let manager = TaskManager::new();
    let original_notify = ok_notify();
    let task_id = manager
        .register_external(child_registration(Arc::clone(&original_notify)))
        .unwrap();
    let before = manager.snapshot();
    let mut conflict = make_registration(ok_notify());
    conflict.summary = "conflicting metadata".into();
    conflict.started_at = Some("2026-02-01T00:00:00Z".into());
    let error = manager.register_external(conflict).unwrap_err();
    assert!(matches!(
        error,
        BackgroundRegistryError::ExternalInitiatorConflict {
            task_id: ref rejected_task,
            ref recorded,
            ref requested,
        } if rejected_task == &task_id && recorded == "child-session" && requested == "session"
    ));
    assert_snapshot_unchanged(&manager, &before);
    assert!(Arc::ptr_eq(
        &manager.registry.external_notify(&task_id).unwrap(),
        &original_notify
    ));
    assert!(manager
        .settle_external(&task_id, "terminal", make_result())
        .await
        .unwrap());
    assert_eq!(
        manager.snapshot().tasks[0].output_preview.as_deref(),
        Some("done")
    );
}

#[tokio::test]
async fn test_register_external_matching_initiator_refreshes_callbacks() {
    let manager = TaskManager::new();
    let task_id = manager
        .register_external(child_registration(ok_notify()))
        .unwrap();
    let before = manager.snapshot();
    let replay_notify = ok_notify();
    assert_eq!(
        manager
            .register_external(child_registration(Arc::clone(&replay_notify)))
            .unwrap(),
        task_id
    );
    assert_snapshot_unchanged(&manager, &before);
    assert!(Arc::ptr_eq(
        &manager.registry.external_notify(&task_id).unwrap(),
        &replay_notify
    ));
}

#[tokio::test]
async fn test_register_external_discovery_preserves_known_child_route() {
    let manager = TaskManager::new();
    let original_notify = ok_notify();
    let task_id = manager
        .register_external(child_registration(Arc::clone(&original_notify)))
        .unwrap();
    let before = manager.snapshot();
    let mut discovery = make_registration(ok_notify());
    discovery.initiator_session_id = None;
    assert_eq!(manager.register_external(discovery).unwrap(), task_id);
    assert_snapshot_unchanged(&manager, &before);
    assert!(Arc::ptr_eq(
        &manager.registry.external_notify(&task_id).unwrap(),
        &original_notify
    ));
}

#[tokio::test]
async fn test_restore_external_rejects_conflicting_initiator_before_settlement() {
    let manager = TaskManager::new();
    let original_notify = ok_notify();
    let task_id = manager
        .register_external(child_registration(Arc::clone(&original_notify)))
        .unwrap();
    let before = manager.snapshot();
    let conflicting_notify: ExternalNotifyFn =
        Arc::new(|_, _| panic!("conflicting initiator must not receive a terminal result"));
    let error = manager
        .restore_external_terminal(
            make_registration(conflicting_notify),
            "conflicting-terminal",
            make_result(),
        )
        .await
        .unwrap_err();
    assert!(error.contains("initiator conflict"), "{error}");
    assert_snapshot_unchanged(&manager, &before);
    assert!(Arc::ptr_eq(
        &manager.registry.external_notify(&task_id).unwrap(),
        &original_notify
    ));
    assert!(manager
        .settle_external(&task_id, "terminal", make_result())
        .await
        .unwrap());
}

#[tokio::test]
async fn test_restore_external_matching_initiator_uses_replayed_route() {
    let manager = TaskManager::new();
    let original_notify: ExternalNotifyFn =
        Arc::new(|_, _| panic!("matching initiator replay should refresh the callback"));
    let task_id = manager
        .register_external(child_registration(original_notify))
        .unwrap();
    let delivered = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let received = Arc::clone(&delivered);
    let replay_notify: ExternalNotifyFn = Arc::new(move |result, _| {
        received.lock().push(result.output.clone());
        Box::pin(async { Ok(()) })
    });
    assert_eq!(
        manager
            .restore_external_terminal(child_registration(replay_notify), "terminal", make_result())
            .await
            .unwrap(),
        task_id
    );
    assert_eq!(*delivered.lock(), vec!["done"]);
    let snapshot = manager.snapshot();
    assert_eq!(snapshot.tasks[0].status, "completed");
    assert_eq!(
        snapshot.tasks[0].initiator_session_id.as_deref(),
        Some("child-session")
    );
}

#[tokio::test]
async fn test_restore_external_discovery_delivers_to_known_child() {
    let manager = TaskManager::new();
    let delivered = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let received = Arc::clone(&delivered);
    let child_notify: ExternalNotifyFn = Arc::new(move |result, _| {
        received.lock().push(result.output.clone());
        Box::pin(async { Ok(()) })
    });
    let task_id = manager
        .register_external(child_registration(child_notify))
        .unwrap();
    let fallback_notify: ExternalNotifyFn =
        Arc::new(|_, _| panic!("discovery must not reroute a known child's terminal result"));
    let mut discovery = make_registration(fallback_notify);
    discovery.initiator_session_id = None;
    assert_eq!(
        manager
            .restore_external_terminal(discovery, "terminal", make_result())
            .await
            .unwrap(),
        task_id
    );
    assert_eq!(*delivered.lock(), vec!["done"]);
    let snapshot = manager.snapshot();
    assert_eq!(snapshot.tasks[0].status, "completed");
    assert_eq!(
        snapshot.tasks[0].initiator_session_id.as_deref(),
        Some("child-session")
    );
}

#[tokio::test]
async fn test_register_external_rejects_conflict_with_restored_terminal_record() {
    let manager = TaskManager::new();
    manager
        .restore_external_terminal(child_registration(ok_notify()), "terminal", make_result())
        .await
        .unwrap();
    let before = manager.snapshot();
    assert!(matches!(
        manager.register_external(make_registration(ok_notify())),
        Err(BackgroundRegistryError::ExternalInitiatorConflict { .. })
    ));
    assert_snapshot_unchanged(&manager, &before);
    assert_eq!(manager.active_count(), 0);
}

#[tokio::test]
async fn test_restore_external_rejects_conflict_with_terminal_record() {
    let manager = TaskManager::new();
    manager
        .restore_external_terminal(child_registration(ok_notify()), "terminal", make_result())
        .await
        .unwrap();
    let before = manager.snapshot();
    let mut conflicting_result = make_result();
    conflicting_result.success = false;
    conflicting_result.output = "conflicting result".into();
    let error = manager
        .restore_external_terminal(
            make_registration(ok_notify()),
            "other-terminal",
            conflicting_result,
        )
        .await
        .unwrap_err();
    assert!(error.contains("initiator conflict"), "{error}");
    assert_snapshot_unchanged(&manager, &before);
}

#[tokio::test]
async fn test_restore_external_matching_terminal_replay_is_idempotent() {
    let manager = TaskManager::new();
    let task_id = manager
        .restore_external_terminal(child_registration(ok_notify()), "terminal", make_result())
        .await
        .unwrap();
    let before = manager.snapshot();
    let replay_notify: ExternalNotifyFn =
        Arc::new(|_, _| panic!("terminal replay must not redeliver"));
    assert_eq!(
        manager
            .restore_external_terminal(child_registration(replay_notify), "terminal", make_result())
            .await
            .unwrap(),
        task_id
    );
    assert_snapshot_unchanged(&manager, &before);
}

fn paused_notify(
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
) -> ExternalNotifyFn {
    Arc::new(move |_, _| {
        let entered = Arc::clone(&entered);
        let release = Arc::clone(&release);
        Box::pin(async move {
            entered.notify_one();
            release.notified().await;
            Ok(())
        })
    })
}

async fn wait_for_delivery(entered: &tokio::sync::Notify) {
    tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
        .await
        .expect("cold restore did not enter delivery");
}

#[tokio::test]
async fn test_cold_restore_reserves_initiator_before_concurrent_registration() {
    let manager = Arc::new(TaskManager::new());
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let request = child_registration(paused_notify(Arc::clone(&entered), Arc::clone(&release)));
    let task_id = external_task_id(&request);
    let restoring = Arc::clone(&manager);
    let restore = tokio::spawn(async move {
        restoring
            .restore_external_terminal(request, "terminal", make_result())
            .await
    });
    wait_for_delivery(&entered).await;
    let before = manager.snapshot();
    let error = manager
        .register_external(make_registration(ok_notify()))
        .unwrap_err();
    assert!(matches!(
        error,
        BackgroundRegistryError::ExternalInitiatorConflict { .. }
    ));
    assert_snapshot_unchanged(&manager, &before);
    release.notify_one();
    assert_eq!(restore.await.unwrap().unwrap(), task_id);
    let snapshot = manager.snapshot();
    assert_eq!(snapshot.tasks.len(), 1);
    assert_eq!(
        snapshot.tasks[0].initiator_session_id.as_deref(),
        Some("child-session")
    );
    assert_eq!(snapshot.tasks[0].status, "completed");
    assert_eq!(snapshot.tasks[0].output_preview.as_deref(), Some("done"));
    assert_eq!(manager.active_count(), 0);
}

#[tokio::test]
async fn test_cold_restore_rejects_concurrent_conflicting_restoration() {
    let manager = Arc::new(TaskManager::new());
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let request = child_registration(paused_notify(Arc::clone(&entered), Arc::clone(&release)));
    let restoring = Arc::clone(&manager);
    let restore = tokio::spawn(async move {
        restoring
            .restore_external_terminal(request, "terminal", make_result())
            .await
    });
    wait_for_delivery(&entered).await;
    let before = manager.snapshot();
    let conflict_notify: ExternalNotifyFn =
        Arc::new(|_, _| panic!("conflicting restore must not deliver"));
    let mut conflicting_result = make_result();
    conflicting_result.success = false;
    conflicting_result.output = "wrong result".into();
    let error = manager
        .restore_external_terminal(
            make_registration(conflict_notify),
            "conflicting-terminal",
            conflicting_result,
        )
        .await
        .unwrap_err();
    assert!(error.contains("initiator conflict"), "{error}");
    assert_snapshot_unchanged(&manager, &before);
    release.notify_one();
    restore.await.unwrap().unwrap();
    assert_eq!(
        manager.snapshot().tasks[0].output_preview.as_deref(),
        Some("done")
    );
}

#[tokio::test]
async fn test_cold_restore_matching_and_discovery_replays_preserve_inflight_delivery() {
    let manager = Arc::new(TaskManager::new());
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let original_notify = paused_notify(Arc::clone(&entered), Arc::clone(&release));
    let request = child_registration(Arc::clone(&original_notify));
    let task_id = external_task_id(&request);
    let restoring = Arc::clone(&manager);
    let restore = tokio::spawn(async move {
        restoring
            .restore_external_terminal(request, "terminal", make_result())
            .await
    });
    wait_for_delivery(&entered).await;
    let before = manager.snapshot();
    assert_eq!(
        manager
            .register_external(child_registration(ok_notify()))
            .unwrap(),
        task_id
    );
    let mut discovery = make_registration(ok_notify());
    discovery.initiator_session_id = None;
    assert_eq!(manager.register_external(discovery).unwrap(), task_id);
    let error = manager
        .restore_external_terminal(child_registration(ok_notify()), "terminal", make_result())
        .await
        .unwrap_err();
    assert!(error.contains("settlement is still pending"), "{error}");
    assert!(Arc::ptr_eq(
        &manager.registry.external_notify(&task_id).unwrap(),
        &original_notify
    ));
    assert_snapshot_unchanged(&manager, &before);
    release.notify_one();
    restore.await.unwrap().unwrap();
    assert_eq!(manager.snapshot().tasks[0].status, "completed");
}

#[tokio::test]
async fn test_cold_restore_delivery_failure_retains_initiator_and_allows_retry() {
    let manager = TaskManager::new();
    let deliveries = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let rejected = Arc::clone(&deliveries);
    let failed_notify: ExternalNotifyFn = Arc::new(move |_, delivery_id| {
        rejected.lock().push(delivery_id);
        Box::pin(async { Err("delivery rejected".into()) })
    });
    let request = child_registration(failed_notify);
    let task_id = external_task_id(&request);
    assert_eq!(
        manager
            .restore_external_terminal(request, "terminal", make_result())
            .await,
        Err("delivery rejected".into())
    );
    let before = manager.snapshot();
    assert_eq!(before.tasks[0].status, "pending_delivery");
    assert_eq!(manager.active_count(), 1);
    assert!(matches!(
        manager.register_external(make_registration(ok_notify())),
        Err(BackgroundRegistryError::ExternalInitiatorConflict { .. })
    ));
    assert_snapshot_unchanged(&manager, &before);
    let accepted = Arc::clone(&deliveries);
    let retry_notify: ExternalNotifyFn = Arc::new(move |_, delivery_id| {
        accepted.lock().push(delivery_id);
        Box::pin(async { Ok(()) })
    });
    assert_eq!(
        manager
            .restore_external_terminal(child_registration(retry_notify), "terminal", make_result())
            .await
            .unwrap(),
        task_id
    );
    let deliveries = deliveries.lock();
    assert_eq!(deliveries.len(), 2);
    assert_eq!(deliveries[0], deliveries[1]);
    assert_eq!(manager.snapshot().tasks[0].status, "completed");
    assert_eq!(manager.active_count(), 0);
}

#[tokio::test]
async fn test_cold_restore_aborted_delivery_can_retry_without_losing_initiator() {
    let manager = Arc::new(TaskManager::new());
    let entered = Arc::new(tokio::sync::Notify::new());
    let request = child_registration(paused_notify(
        Arc::clone(&entered),
        Arc::new(tokio::sync::Notify::new()),
    ));
    let restoring = Arc::clone(&manager);
    let restore = tokio::spawn(async move {
        restoring
            .restore_external_terminal(request, "terminal", make_result())
            .await
    });
    wait_for_delivery(&entered).await;
    restore.abort();
    assert!(restore.await.unwrap_err().is_cancelled());
    assert_eq!(manager.snapshot().tasks[0].status, "pending_delivery");
    assert!(matches!(
        manager.register_external(make_registration(ok_notify())),
        Err(BackgroundRegistryError::ExternalInitiatorConflict { .. })
    ));
    manager
        .restore_external_terminal(child_registration(ok_notify()), "terminal", make_result())
        .await
        .unwrap();
    assert_eq!(manager.snapshot().tasks[0].status, "completed");
}

#[tokio::test]
async fn test_cold_restore_can_reconcile_after_execution_scope_closes() {
    let manager = TaskManager::new();
    manager.registry.scope.close();
    manager
        .restore_external_terminal(child_registration(ok_notify()), "terminal", make_result())
        .await
        .unwrap();
    assert_eq!(manager.snapshot().tasks[0].status, "completed");
    assert_eq!(manager.active_count(), 0);
}

#[tokio::test]
async fn test_cold_restore_is_not_blocked_by_execution_concurrency_limit() {
    let manager = TaskManager::new();
    for task_index in 0..BackgroundTaskRegistry::SHELL_LIMIT {
        let mut running = child_registration(ok_notify());
        running.kind = BgTaskKind::Shell;
        running.owner_task_id = format!("running-{task_index}");
        manager.register_external(running).unwrap();
    }
    let mut restored = child_registration(ok_notify());
    restored.kind = BgTaskKind::Shell;
    manager
        .restore_external_terminal(restored, "terminal", make_result())
        .await
        .unwrap();
    let snapshot = manager.snapshot();
    assert_eq!(
        snapshot.tasks.len(),
        BackgroundTaskRegistry::SHELL_LIMIT + 1
    );
    assert_eq!(
        snapshot
            .tasks
            .iter()
            .filter(|task| task.status == "completed")
            .count(),
        1
    );
    assert_eq!(manager.active_count(), BackgroundTaskRegistry::SHELL_LIMIT);
}
