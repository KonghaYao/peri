use super::*;
use peri_acp_types::tasks::{ExternalTaskRegistration, TaskRegistrationTicket};
use std::sync::atomic::{AtomicUsize, Ordering};

fn task(id: &str, kind: BgTaskKind) -> BackgroundTask {
    BackgroundTask {
        id: id.into(),
        agent_name: "retention-test".into(),
        prompt_summary: "终态摘要".repeat(256),
        status: BackgroundTaskStatus::Running,
        started_at: std::time::Instant::now(),
        chrono_started_at: chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc),
        kind,
        cancel_handle: BgCancelHandle::Kill(Some(Box::new(|| {}))),
        cancel_token: None,
        pid: None,
        output_preview: None,
        agent_inbox: None,
        initiator_session_id: None,
        owner_session_id: Some("owner".into()),
        owner_identity: Some("fixture".into()),
    }
}

fn result(id: &str) -> BackgroundTaskResult {
    BackgroundTaskResult {
        task_id: id.into(),
        agent_name: "retention-test".into(),
        prompt_summary: "summary".into(),
        success: true,
        output: "结果".repeat(500),
        tool_calls_count: 0,
        duration_ms: 1,
        timed_out: false,
        child_thread_id: None,
        subagent_failure: None,
        shell_output: None,
    }
}

fn finish(registry: &BackgroundTaskRegistry, id: &str) -> TaskRegistrationTicket {
    let ticket = registry
        .register_with_kind(task(id, BgTaskKind::Agent))
        .unwrap();
    registry.confirm_external_stopped(&ticket);
    assert!(registry.complete(&ticket, result(id)));
    ticket
}

fn pressure(registry: &BackgroundTaskRegistry, prefix: &str) {
    for index in 0..600 {
        finish(registry, &format!("{prefix}-{index}"));
    }
}

/// [回归测试] 终态数增长不能使正文投影或去重目录跟随增长。
#[test]
fn test_completed_projection_has_count_and_byte_budgets_without_tombstones() {
    let registry = BackgroundTaskRegistry::new();
    pressure(&registry, "history");
    let snapshot = registry.snapshot();
    assert!(
        snapshot.tasks.len() < projection::TERMINAL_RECORD_LIMIT,
        "此夹具应先触发 1 MiB 字节预算"
    );
    assert!(snapshot
        .tasks
        .iter()
        .all(|record| record.summary.len() <= projection::SUMMARY_BYTE_LIMIT));
    assert!(!snapshot
        .tasks
        .iter()
        .any(|record| record.task_id == "history-0"));
    assert!(snapshot
        .tasks
        .iter()
        .any(|record| record.task_id == "history-599"));
    assert!(registry.tasks.lock().is_empty());
    assert!(registry.registrations.lock().is_empty());
    assert!(registry.external_settled());
    assert_eq!(snapshot.revision, 1200);
}

/// [回归测试] 票据是 producer 的短生命周期证据，registry 不永久持有它。
#[test]
fn test_accepted_ticket_survives_eviction_and_releases_with_its_producer() {
    let registry = BackgroundTaskRegistry::new();
    let ticket = registry
        .register_with_kind(task("first", BgTaskKind::Agent))
        .unwrap();
    let receipt = Arc::downgrade(registry.registrations.lock().get("first").unwrap());
    registry.confirm_external_stopped(&ticket);
    assert!(registry.complete(&ticket, result("first")));
    pressure(&registry, "later");
    assert!(ticket.is_settled());
    assert!(receipt.upgrade().is_some());
    assert!(!registry.complete(&ticket, result("first")));
    assert!(registry.registrations.lock().is_empty());
    drop(ticket);
    assert!(
        receipt.upgrade().is_none(),
        "最后一个 producer 退出后不留墓碑"
    );
}

/// [回归测试] 即便错误 producer 重用公开 ID，旧回调也不能操作新注册。
#[test]
fn test_old_ticket_cannot_complete_or_confirm_cleanup_for_a_reused_id() {
    let registry = BackgroundTaskRegistry::new();
    let old = finish(&registry, "same-id");
    let new = registry
        .register_with_kind(task("same-id", BgTaskKind::Agent))
        .unwrap();
    assert!(!old.same_registration(&new));
    assert!(!registry.complete(&old, result("same-id")));
    registry.confirm_external_stopped(&old);
    assert_eq!(registry.active_count(), 1);
    assert!(!registry.external_settled(), "旧清理回执不能清新执行的责任");
    assert_eq!(registry.pending_handoff_tasks().len(), 1);
    registry.confirm_external_stopped(&new);
    assert!(registry.complete(&new, result("same-id")));
    assert!(registry.external_settled());
}

/// [回归测试] 接纳结果与实际资源清理是两个边界，展示回收不能代替后者。
#[test]
fn test_accepted_terminal_stays_owned_until_execution_cleanup_is_confirmed() {
    let manager = super::super::TaskManager::new();
    let registry = manager.registry();
    let ticket = registry
        .register_with_kind(task("cleanup", BgTaskKind::Workflow))
        .unwrap();
    assert!(registry.complete(&ticket, result("cleanup")));
    pressure(registry, "later");
    assert!(ticket.is_settled());
    assert!(!registry.external_settled());
    assert_eq!(manager.active_count(), 0);
    assert!(!manager.has_pending_deliveries());
    assert!(manager.has_unsettled_tasks());
    let pending = registry.pending_handoff_tasks();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].task_id, "cleanup");
    assert!(registry
        .snapshot()
        .tasks
        .iter()
        .any(|record| record.task_id == "cleanup"));
    registry.confirm_external_stopped(&ticket);
    assert!(registry.registrations.lock().is_empty());
    assert!(registry.external_settled());
    assert!(!manager.has_unsettled_tasks());
}

/// [回归测试] cancelled 的协作清理结果在历史压力与投递失败后仍能送达一次。
#[test]
fn test_cancelled_cleanup_and_pending_delivery_are_not_evicted() {
    let registry = Arc::new(BackgroundTaskRegistry::new());
    let ticket = registry
        .register_with_kind(task("cancelled", BgTaskKind::Shell))
        .unwrap();
    registry.cancel(ticket.task_id()).unwrap();
    pressure(&registry, "later");
    assert!(registry
        .snapshot()
        .tasks
        .iter()
        .any(|record| record.task_id == "cancelled"));
    let attempts = Arc::new(AtomicUsize::new(0));
    let calls = attempts.clone();
    let delivery: peri_acp_types::tasks::OnBgCompleteFn = Arc::new(move |result, kind| {
        assert_eq!(kind, BgTaskKind::Shell);
        assert!(!result.success);
        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            Err("retry".into())
        } else {
            Ok(())
        }
    });
    assert!(matches!(
        registry.settle_completed(&ticket, result("cancelled"), delivery.clone()),
        Err(BackgroundRegistryError::DeliveryFailed { .. })
    ));
    registry.confirm_external_stopped(&ticket);
    pressure(&registry, "more");
    assert!(!ticket.is_settled());
    assert!(!registry.external_settled());
    assert_eq!(registry.retry_pending_deliveries(), 1);
    assert!(ticket.is_settled());
    assert!(registry.external_settled());
    assert!(!registry
        .settle_completed(&ticket, result("cancelled"), delivery)
        .unwrap());
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert!(registry.registrations.lock().is_empty());
}

fn external(deliveries: Arc<AtomicUsize>) -> ExternalTaskRegistration {
    ExternalTaskRegistration {
        session_id: "owner".into(),
        initiator_session_id: Some("owner".into()),
        owner_identity: "fixture".into(),
        owner_task_id: "opaque-source-id".into(),
        kind: BgTaskKind::Mcp,
        summary: "remote".into(),
        started_at: Some("2026-01-01T00:00:00Z".into()),
        cancel: Arc::new(|| Box::pin(async { Ok(()) })),
        on_terminal: Arc::new(move |_, _| {
            deliveries.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }),
    }
}

/// [回归测试] 外部重复投递的确认不能依赖会淘汰的快照行。
#[tokio::test]
async fn test_external_ticket_acknowledges_duplicate_after_projection_eviction() {
    let manager = super::super::TaskManager::new();
    let deliveries = Arc::new(AtomicUsize::new(0));
    let ticket = manager
        .register_external(external(deliveries.clone()))
        .unwrap();
    assert!(manager
        .settle_external(&ticket, "terminal", result(ticket.task_id()))
        .await
        .unwrap());
    pressure(manager.registry(), "later");
    assert!(!manager
        .snapshot()
        .tasks
        .iter()
        .any(|record| record.task_id == ticket.task_id()));
    manager
        .refresh_external(&ticket, external(deliveries.clone()))
        .unwrap();
    assert!(!manager
        .settle_external(&ticket, "terminal", result(ticket.task_id()))
        .await
        .unwrap());
    assert!(ticket.is_settled());
    assert_eq!(deliveries.load(Ordering::SeqCst), 1);
    assert!(manager.registry().registrations.lock().is_empty());
}

#[test]
fn test_duplicate_source_admission_requires_the_original_ticket() {
    let manager = super::super::TaskManager::new();
    let deliveries = Arc::new(AtomicUsize::new(0));
    let ticket = manager
        .register_external(external(deliveries.clone()))
        .unwrap();
    assert!(
        matches!(manager.register_external(external(deliveries.clone())),
        Err(BackgroundRegistryError::DuplicateTask(id)) if id == ticket.task_id())
    );
    manager
        .refresh_external(&ticket, external(deliveries))
        .unwrap();
    assert_eq!(manager.active_count(), 1);
}

/// [回归测试] gate 未放行的失败不遗留不存在的结果/清理责任。
#[test]
fn test_failed_start_gate_withdraws_registration_without_fabricating_a_result() {
    let registry = BackgroundTaskRegistry::new();
    let mut changes = registry.subscribe_events();
    let ticket = registry
        .register_pending_start(task("unstarted", BgTaskKind::Agent))
        .unwrap();
    assert!(registry.withdraw_unstarted_registration(&ticket).unwrap());
    assert!(!registry.withdraw_unstarted_registration(&ticket).unwrap());
    assert!(ticket.is_settled());
    assert!(registry.external_settled());
    assert!(registry.registrations.lock().is_empty());
    assert!(matches!(
        changes.try_recv().unwrap().event,
        BgRegistryEvent::Started { .. }
    ));
    assert!(matches!(
        changes.try_recv().unwrap().event,
        BgRegistryEvent::Cancelled { .. }
    ));
    assert!(changes.try_recv().is_err());
}

#[tokio::test]
async fn test_cancel_before_gate_activation_can_withdraw_its_cleanup_reservation() {
    let registry = BackgroundTaskRegistry::new();
    let mut task = task("before-gate", BgTaskKind::Agent);
    let handle = tokio::spawn(async {});
    task.cancel_handle = BgCancelHandle::Abort(handle);
    let ticket = registry.register_pending_start(task).unwrap();
    registry.cancel(ticket.task_id()).unwrap();
    assert!(!registry.external_settled());
    assert!(matches!(
        registry.mark_execution_started(&ticket),
        Err(BackgroundRegistryError::TaskNotFound(_))
    ));
    assert!(registry.withdraw_unstarted_registration(&ticket).unwrap());
    assert!(registry.external_settled());
    assert!(registry.cancelled_tasks_waiting_cleanup.lock().is_empty());
    registry.scope.close();
    assert!(registry.scope.wait().await);
}

#[test]
fn test_started_execution_cannot_use_the_unstarted_withdrawal_path() {
    let registry = BackgroundTaskRegistry::new();
    let ticket = registry
        .register_pending_start(task("started", BgTaskKind::Agent))
        .unwrap();
    registry.mark_execution_started(&ticket).unwrap();
    assert!(
        matches!(registry.withdraw_unstarted_registration(&ticket), Err(BackgroundRegistryError::TaskAlreadyStarted(id)) if id == "started")
    );
    assert_eq!(registry.active_count(), 1);
    assert!(!ticket.is_settled());
    assert!(!registry.external_settled());
    registry.confirm_external_stopped(&ticket);
    assert!(registry.complete(&ticket, result(ticket.task_id())));
    assert!(registry.external_settled());
}

/// [回归测试] 不需要通知父会话的 producer 也必须结清真实协作取消结果。
#[tokio::test]
async fn test_execution_only_agent_cancellation_releases_its_original_ticket() {
    let registry = Arc::new(BackgroundTaskRegistry::new());
    let cancel = CancellationToken::new();
    let execution_cancel = cancel.clone();
    let owner = Arc::downgrade(&registry);
    let (send, receive) = tokio::sync::oneshot::channel::<TaskRegistrationTicket>();
    let handle = registry
        .scope
        .spawn(async move {
            let ticket = receive.await.unwrap();
            execution_cancel.cancelled().await;
            assert!(!owner
                .upgrade()
                .unwrap()
                .complete(&ticket, result(ticket.task_id())));
        })
        .unwrap();
    let mut task = task("execution-only", BgTaskKind::Agent);
    task.cancel_handle = BgCancelHandle::Abort(handle);
    task.cancel_token = Some(cancel);
    let ticket = registry.register_with_kind(task).unwrap();
    send.send(ticket.clone()).unwrap();
    registry.cancel(ticket.task_id()).unwrap();
    registry.scope.close();
    assert!(registry.scope.wait().await);
    assert!(ticket.is_settled());
    assert!(registry.external_settled());
    assert!(registry.registrations.lock().is_empty());
    assert_eq!(registry.snapshot().tasks[0].status, "cancelled");
}
