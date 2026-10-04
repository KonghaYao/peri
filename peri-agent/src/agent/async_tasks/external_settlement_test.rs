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
