use crate::agent::async_tasks::registry::BackgroundRegistryError;
use crate::agent::async_tasks::registry::BackgroundTaskRegistry;
use crate::agent::async_tasks::{BackgroundTask, BackgroundTaskStatus, BgCancelHandle};
use crate::agent::events::BackgroundTaskResult;
use peri_acp_types::session::{
    MessageKind, MessageQueue, MessageSource, QueuedMessage, SessionInbox,
};
use peri_acp_types::tasks::BgRegistryEvent;
use peri_acp_types::tasks::BgTaskKind;
use peri_acp_types::tasks::OnBgCompleteFn;
use peri_acp_types::tasks::TaskRegistrationTicket;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::Arc;

fn registered(kind: BgTaskKind) -> (Arc<BackgroundTaskRegistry>, TaskRegistrationTicket) {
    let registry = Arc::new(BackgroundTaskRegistry::new());
    let ticket = registry
        .register_with_kind(BackgroundTask {
            id: "task".into(),
            agent_name: "fixture".into(),
            prompt_summary: "task".into(),
            status: BackgroundTaskStatus::Running,
            started_at: std::time::Instant::now(),
            chrono_started_at: chrono::Utc::now(),
            kind,
            cancel_handle: BgCancelHandle::Kill(Some(Box::new(|| {}))),
            cancel_token: None,
            pid: None,
            output_preview: None,
            agent_inbox: None,
            initiator_session_id: None,
            owner_session_id: None,
            owner_identity: None,
        })
        .expect("the register with kind fixture result is required for this assertion");
    // 此夹具没有外部进程；真实资源证明由专门生命周期用例覆盖。
    registry.confirm_external_stopped(&ticket);
    (registry, ticket)
}

fn result() -> BackgroundTaskResult {
    BackgroundTaskResult {
        task_id: "task".into(),
        agent_name: "fixture".into(),
        prompt_summary: "task".into(),
        success: true,
        output: "retained terminal result".into(),
        tool_calls_count: 0,
        duration_ms: 1,
        child_thread_id: None,
        timed_out: false,
        subagent_failure: None,
        shell_output: None,
    }
}

/// [回归测试] Subagent、Shell、Workflow 共用 owner 的交付后结算规则。
#[tokio::test]
async fn all_owned_kinds_publish_before_terminal_and_wake_idle_consumer() {
    for kind in [BgTaskKind::Agent, BgTaskKind::Shell, BgTaskKind::Workflow] {
        let (registry, ticket) = registered(kind);
        let queue = Arc::new(MessageQueue::new());
        let inbox = SessionInbox::new(Arc::clone(&queue));
        let waiting = inbox.await_wake();
        tokio::pin!(waiting);
        futures::future::poll_fn(|context| {
            use std::future::Future;
            assert!(waiting.as_mut().poll(context).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        let owner = Arc::downgrade(&registry);
        let output = Arc::clone(&queue);
        let delivery: OnBgCompleteFn = Arc::new(move |result, actual_kind| {
            assert_eq!(actual_kind, kind);
            assert_eq!(
                owner
                    .upgrade()
                    .expect("the upgrade fixture result is required for this assertion")
                    .active_count(),
                1
            );
            output.push_sync(QueuedMessage::new(
                MessageKind::Defer,
                MessageSource::SystemInjected,
                crate::messages::BaseMessage::human(result.output.clone()),
            ));
            Ok(())
        });
        assert!(registry
            .settle_completed(&ticket, result(), delivery)
            .expect("the settle completed fixture result is required for this assertion"));
        tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
            .await
            .expect("the timeout fixture result is required for this assertion");
        assert_eq!(registry.active_count(), 0);
        assert_eq!(queue.len_sync(), 1);
    }
}

/// [回归测试] 回调拒绝交付时保留结果，不允许 active count 提前归零。
#[test]
fn rejected_delivery_is_visible_retained_and_retried_once() {
    let (registry, ticket) = registered(BgTaskKind::Agent);
    let attempts = Arc::new(AtomicUsize::new(0));
    let called = Arc::clone(&attempts);
    let delivery: OnBgCompleteFn = Arc::new(move |result, kind| {
        assert_eq!(kind, BgTaskKind::Agent);
        assert_eq!(result.output, "retained terminal result");
        if called.fetch_add(1, Ordering::SeqCst) == 0 {
            Err("inbox not yet registered".into())
        } else {
            Ok(())
        }
    });
    assert!(matches!(
        registry.settle_completed(&ticket, result(), Arc::clone(&delivery)),
        Err(BackgroundRegistryError::DeliveryFailed { .. })
    ));
    assert_eq!(registry.active_count(), 1);
    assert_eq!(registry.snapshot().tasks[0].status, "delivery_pending");
    assert!(matches!(
        registry.cancel("task"),
        Err(BackgroundRegistryError::TaskCompleting(_))
    ));
    assert_eq!(registry.retry_pending_deliveries(), 1);
    assert_eq!(registry.active_count(), 0);
    assert_eq!(registry.snapshot().tasks[0].status, "completed");
    assert!(!registry
        .settle_completed(&ticket, result(), delivery)
        .expect("the settle completed fixture result is required for this assertion"));
    assert_eq!(registry.retry_pending_deliveries(), 0);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

#[test]
fn delivery_panic_keeps_terminal_unpublished_until_retry() {
    let (registry, ticket) = registered(BgTaskKind::Workflow);
    let attempts = Arc::new(AtomicUsize::new(0));
    let called = Arc::clone(&attempts);
    let delivery: OnBgCompleteFn = Arc::new(move |_, _| {
        if called.fetch_add(1, Ordering::SeqCst) == 0 {
            panic!("delivery failed");
        }
        Ok(())
    });
    assert!(registry
        .settle_completed(&ticket, result(), delivery)
        .is_err());
    assert_eq!(registry.active_count(), 1);
    assert_eq!(registry.retry_pending_deliveries(), 1);
    assert_eq!(registry.active_count(), 0);
}

#[test]
fn cancelled_shell_cleanup_is_delivered_without_ghost_completion() {
    let (registry, ticket) = registered(BgTaskKind::Shell);
    registry
        .cancel("task")
        .expect("the cancel fixture result is required for this assertion");
    let attempts = Arc::new(AtomicUsize::new(0));
    let called = Arc::clone(&attempts);
    let delivery: OnBgCompleteFn = Arc::new(move |result, kind| {
        assert_eq!(kind, BgTaskKind::Shell);
        assert!(!result.success);
        called.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });
    assert!(!registry
        .settle_completed(&ticket, result(), Arc::clone(&delivery))
        .expect("the clone fixture result is required for this assertion"));
    assert!(!registry
        .settle_completed(&ticket, result(), delivery)
        .expect("the settle completed fixture result is required for this assertion"));
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    assert_eq!(registry.snapshot().tasks[0].status, "cancelled");
}

#[test]
fn unknown_task_does_not_call_delivery() {
    let registry = Arc::new(BackgroundTaskRegistry::new());
    let (_, ticket) = registered(BgTaskKind::Agent);
    let delivery: OnBgCompleteFn = Arc::new(|_, _| panic!("unknown task cannot publish"));
    assert!(matches!(
        registry.settle_completed(&ticket, result(), delivery),
        Err(BackgroundRegistryError::InvalidRegistration(_))
    ));
}

#[test]
fn cancelled_cleanup_retry_cannot_report_idle_while_delivery_is_in_flight() {
    let (registry, ticket) = registered(BgTaskKind::Shell);
    registry
        .cancel("task")
        .expect("the cancel fixture result is required for this assertion");
    registry.confirm_external_stopped(&ticket);
    let owner = Arc::downgrade(&registry);
    let attempts = Arc::new(AtomicUsize::new(0));
    let called = Arc::clone(&attempts);
    let delivery: OnBgCompleteFn = Arc::new(move |_, _| {
        assert!(!owner
            .upgrade()
            .expect("the upgrade fixture result is required for this assertion")
            .external_settled());
        if called.fetch_add(1, Ordering::SeqCst) == 0 {
            Err("temporarily unavailable".into())
        } else {
            Ok(())
        }
    });
    assert!(registry
        .settle_completed(&ticket, result(), delivery)
        .is_err());
    assert!(!registry.external_settled());
    assert_eq!(registry.retry_pending_deliveries(), 1);
    assert!(registry.external_settled());
}

/// [回归测试] 当前进程的暂时投递失败不能依赖下一次用户输入才能恢复。
#[tokio::test(start_paused = true)]
async fn pending_delivery_recovers_without_new_input() {
    let (registry, ticket) = registered(BgTaskKind::Agent);
    let attempts = Arc::new(AtomicUsize::new(0));
    let called = Arc::clone(&attempts);
    let delivery: OnBgCompleteFn = Arc::new(move |_, _| {
        if called.fetch_add(1, Ordering::SeqCst) == 0 {
            Err("temporarily unavailable".into())
        } else {
            Ok(())
        }
    });
    assert!(registry
        .settle_completed(&ticket, result(), delivery)
        .is_err());
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    assert_eq!(registry.active_count(), 0);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert_eq!(registry.snapshot().tasks[0].status, "completed");
}

#[tokio::test(start_paused = true)]
async fn permanent_delivery_failure_backs_off_and_close_drains_worker() {
    let (registry, ticket) = registered(BgTaskKind::Agent);
    let attempts = Arc::new(AtomicUsize::new(0));
    let called = attempts.clone();
    assert!(registry
        .settle_completed(
            &ticket,
            result(),
            Arc::new(move |_, _| {
                called.fetch_add(1, Ordering::SeqCst);
                Err("permanently unavailable".into())
            })
        )
        .is_err());
    tokio::task::yield_now().await;
    // 重试在阻塞池执行：退避计时可能晚于推进才登记，按固定步长推进到预算以内。
    // 预算 = 八次退避总和 + 3/4 次退避余量：覆盖登记漂移，又不让第 10 次重试落地。
    let retries = 8_u32;
    let retry_budget: std::time::Duration =
        (0..retries).map(super::delivery_retry::retry_delay).sum();
    let step = std::time::Duration::from_millis(100);
    let steps = (retry_budget + super::delivery_retry::retry_delay(retries) * 3 / 4).as_millis()
        / step.as_millis();
    for _ in 0..steps {
        if attempts.load(Ordering::SeqCst) >= 9 {
            break;
        }
        tokio::time::advance(step).await;
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert_eq!(attempts.load(Ordering::SeqCst), 9);
    assert_eq!(registry.active_count(), 1);
    assert_eq!(registry.snapshot().tasks[0].status, "delivery_pending");
    registry.scope.close();
    assert!(registry.scope.wait().await);
    assert!(!registry.external_settled());
    assert!(registry.scope.is_idle());
    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    assert_eq!(attempts.load(Ordering::SeqCst), 9);
}

#[tokio::test]
async fn concurrent_manual_retry_does_not_reenter_or_strand_delivery() {
    let (registry, ticket) = registered(BgTaskKind::Agent);
    let attempts = Arc::new(AtomicUsize::new(0));
    let called = attempts.clone();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let entered_tx = parking_lot::Mutex::new(Some(entered_tx));
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release_rx = parking_lot::Mutex::new(release_rx);
    let delivery: OnBgCompleteFn =
        Arc::new(move |_, _| match called.fetch_add(1, Ordering::SeqCst) {
            0 => Err("first failure".into()),
            1 => {
                entered_tx
                    .lock()
                    .take()
                    .expect("entered tx lock fixture must provide its pending value exactly once")
                    .send(())
                    .expect(
                        "entered tx lock test receiver must remain available for this delivery",
                    );
                release_rx.lock().recv().expect(
                    "release rx lock producer must deliver the expected test event before closing",
                );
                Err("manual retry also failed".into())
            }
            2 => Ok(()),
            _ => panic!("duplicate delivery"),
        });
    assert!(registry
        .settle_completed(&ticket, result(), delivery)
        .is_err());
    let retry_owner = registry.clone();
    let manual = std::thread::spawn(move || retry_owner.retry_pending_deliveries());
    entered_rx
        .await
        .expect("the entered rx fixture result is required for this assertion");
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert_eq!(registry.active_count(), 1);
    release_tx
        .send(())
        .expect("release tx send test receiver must remain available for this delivery");
    assert_eq!(
        manual
            .join()
            .expect("manual join test worker must complete without panicking"),
        0
    );
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while registry.active_count() != 0 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the timeout fixture result is required for this assertion");
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
    registry.scope.close();
    assert!(registry.scope.wait().await);
    assert!(registry.external_settled());
}

#[tokio::test(start_paused = true)]
async fn shutdown_retries_pending_delivery_once_and_reports_remaining_failure() {
    use peri_acp_types::tasks::{TaskManager as _, TaskShutdownReport};
    for recover in [true, false] {
        let manager = crate::agent::async_tasks::manager::TaskManager::new();
        let (fixture, _) = registered(BgTaskKind::Agent);
        let task = fixture
            .tasks
            .lock()
            .remove("task")
            .expect("the remove fixture result is required for this assertion");
        let ticket = manager
            .register_with_kind(task)
            .expect("the register with kind fixture result is required for this assertion");
        // Moving this no-resource fixture creates a new cleanup receipt.
        manager.confirm_external_execution_stopped(&ticket);
        let attempts = Arc::new(AtomicUsize::new(0));
        let called = attempts.clone();
        assert!(manager
            .settle_completed(
                &ticket,
                result(),
                Arc::new(move |_, _| {
                    let prior = called.fetch_add(1, Ordering::SeqCst);
                    if recover && prior != 0 {
                        Ok(())
                    } else {
                        Err("delivery failed".into())
                    }
                })
            )
            .is_err());
        assert_eq!(
            manager.shutdown().await,
            if recover {
                TaskShutdownReport::Complete
            } else {
                TaskShutdownReport::Incomplete
            }
        );
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }
}

#[tokio::test(start_paused = true)]
async fn pending_delivery_worker_does_not_keep_registry_alive() {
    let (registry, ticket) = registered(BgTaskKind::Agent);
    let owner = Arc::downgrade(&registry);
    assert!(registry
        .settle_completed(
            &ticket,
            result(),
            Arc::new(|_, _| Err("unavailable".into()))
        )
        .is_err());
    tokio::task::yield_now().await;
    drop(registry);
    assert!(owner.upgrade().is_none());
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    assert!(owner.upgrade().is_none());
}

struct AmbiguousQueueAcceptance {
    route: Arc<dyn peri_acp_types::tasks::TaskTerminalDelivery>,
    ids: parking_lot::Mutex<Vec<peri_acp_types::messages::MessageId>>,
}

impl peri_acp_types::tasks::TaskTerminalDelivery for AmbiguousQueueAcceptance {
    fn accept(
        &self,
        delivery_id: peri_acp_types::messages::MessageId,
        reminder: &peri_acp_types::system_reminder::TrustedSystemReminder,
        source: MessageSource,
    ) -> Result<(), String> {
        self.route.accept(delivery_id, reminder, source)?;
        let mut ids = self.ids.lock();
        ids.push(delivery_id);
        if ids.len() == 1 {
            Err("queue accepted but acknowledgment failed".into())
        } else {
            Ok(())
        }
    }

    fn deliver<'delivery>(
        &'delivery self,
        delivery_id: peri_acp_types::messages::MessageId,
        reminder: &'delivery peri_acp_types::system_reminder::TrustedSystemReminder,
        source: MessageSource,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'delivery>>
    {
        Box::pin(async move { self.accept(delivery_id, reminder, source) })
    }
}

#[tokio::test(start_paused = true)]
async fn automatic_retry_uses_original_delivery_identity_without_duplicate_queue_message() {
    let (registry, ticket) = registered(BgTaskKind::Agent);
    let queue = MessageQueue::new();
    let mut wake = queue.subscribe_wake();
    let route = Arc::new(AmbiguousQueueAcceptance {
        route: crate::agent::async_tasks::delivery::SessionTerminalDelivery::for_queue(
            queue.clone(),
        ),
        ids: parking_lot::Mutex::new(Vec::new()),
    });
    let delivery = crate::session::bg_complete::task_bg_complete_callback(route.clone());
    assert!(registry
        .settle_completed(&ticket, result(), delivery)
        .is_err());
    assert_eq!(queue.len_sync(), 1);
    assert!(wake
        .has_changed()
        .expect("the has changed fixture result is required for this assertion"));
    wake.borrow_and_update();
    assert_eq!(queue.drain_batch_sync(64).len(), 1);
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    assert_eq!(registry.active_count(), 0);
    assert_eq!(queue.len_sync(), 0);
    assert!(!wake
        .has_changed()
        .expect("the has changed fixture result is required for this assertion"));
    let ids = route.ids.lock();
    assert_eq!(ids.len(), 2);
    assert_eq!(ids[0], ids[1]);
    assert_eq!(
        ids[0],
        crate::agent::async_tasks::delivery::terminal_delivery_id("task", "terminal")
    );
}

#[test]
fn frozen_queue_route_rejects_conflicting_payload_or_source_without_waking() {
    let queue = MessageQueue::new();
    let mut wake = queue.subscribe_wake();
    let route =
        crate::agent::async_tasks::delivery::SessionTerminalDelivery::for_queue(queue.clone());
    let delivery_id = crate::agent::async_tasks::delivery::terminal_delivery_id("task", "terminal");
    let original = result();
    let reminder =
        crate::session::async_router::background_result_reminder(&original, BgTaskKind::Agent);
    route
        .accept(delivery_id, &reminder, MessageSource::SubAgentComplete)
        .expect("the accept fixture result is required for this assertion");
    wake.borrow_and_update();
    assert_eq!(queue.drain_batch_sync(64).len(), 1);
    route
        .accept(delivery_id, &reminder, MessageSource::SubAgentComplete)
        .expect("the accept fixture result is required for this assertion");
    let mut changed = original;
    changed.output = "changed payload".into();
    let changed_reminder =
        crate::session::async_router::background_result_reminder(&changed, BgTaskKind::Agent);
    assert!(route
        .accept(
            delivery_id,
            &changed_reminder,
            MessageSource::SubAgentComplete
        )
        .unwrap_err()
        .contains("conflicting"));
    assert!(route
        .accept(delivery_id, &reminder, MessageSource::ShellComplete)
        .unwrap_err()
        .contains("conflicting"));
    assert_eq!(queue.len_sync(), 0);
    assert!(!wake
        .has_changed()
        .expect("the has changed fixture result is required for this assertion"));
}

#[tokio::test(start_paused = true)]
async fn cooperative_agent_cancellation_retries_original_delivery_without_new_terminal() {
    let (registry, ticket) = registered(BgTaskKind::Agent);
    let cancel = tokio_util::sync::CancellationToken::new();
    let execution_cancel = cancel.clone();
    let owner = Arc::downgrade(&registry);
    let attempts = Arc::new(AtomicUsize::new(0));
    let called = attempts.clone();
    let delivery: OnBgCompleteFn = Arc::new(move |result, kind| {
        assert_eq!(result.task_id, "task");
        assert_eq!(result.child_thread_id.as_deref(), Some("original-child"));
        assert!(!result.success);
        assert_eq!(kind, BgTaskKind::Agent);
        if called.fetch_add(1, Ordering::SeqCst) == 0 {
            Err("temporarily unavailable".into())
        } else {
            Ok(())
        }
    });
    let task_delivery = delivery.clone();
    let task_ticket = ticket.clone();
    let handle = registry
        .scope
        .spawn(async move {
            execution_cancel.cancelled().await;
            let registry = owner
                .upgrade()
                .expect("the upgrade fixture result is required for this assertion");
            let mut cancelled = result();
            cancelled.success = false;
            cancelled.output = "Background sub-agent was interrupted".into();
            cancelled.child_thread_id = Some("original-child".into());
            assert!(registry
                .settle_completed(&task_ticket, cancelled.clone(), task_delivery.clone())
                .is_err());
            assert!(matches!(
                registry.settle_completed(&task_ticket, cancelled, task_delivery),
                Err(BackgroundRegistryError::TaskCompleting(_))
            ));
            assert!(
                !task_ticket.is_settled(),
                "pending delivery is not acceptance"
            );
        })
        .expect("the spawn fixture result is required for this assertion");
    {
        let mut tasks = registry.tasks.lock();
        let task = tasks
            .get_mut("task")
            .expect("the fixture must contain task");
        task.cancel_handle = BgCancelHandle::Abort(handle);
        task.cancel_token = Some(cancel);
    }
    registry.confirm_external_stopped(&ticket);
    let mut events = registry.subscribe_events();
    registry
        .cancel("task")
        .expect("the cancel fixture result is required for this assertion");
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert_eq!(registry.snapshot().tasks[0].status, "cancelled");
    assert!(!registry
        .settle_completed(&ticket, result(), delivery)
        .expect("the settle completed fixture result is required for this assertion"));
    assert_eq!(registry.retry_pending_deliveries(), 0);
    assert!(matches!(
        events
            .try_recv()
            .expect("events try recv producer must deliver the expected test event before closing")
            .event,
        BgRegistryEvent::Cancelled { .. }
    ));
    assert!(events.try_recv().is_err());
    registry.scope.close();
    assert!(registry.scope.wait().await);
    assert!(registry.external_settled());
}

#[tokio::test(start_paused = true)]
async fn aborted_agent_does_not_fabricate_cleanup_delivery() {
    let (registry, ticket) = registered(BgTaskKind::Agent);
    let delivered = Arc::new(AtomicUsize::new(0));
    let called = delivered.clone();
    let owner = Arc::downgrade(&registry);
    let task_ticket = ticket.clone();
    let handle = registry
        .scope
        .spawn(async move {
            futures::future::pending::<()>().await;
            if let Some(registry) = owner.upgrade() {
                registry
                    .settle_completed(
                        &task_ticket,
                        result(),
                        Arc::new(move |_, _| {
                            called.fetch_add(1, Ordering::SeqCst);
                            Ok(())
                        }),
                    )
                    .expect("the settle completed fixture result is required for this assertion");
            }
        })
        .expect("the spawn fixture result is required for this assertion");
    let abort = handle.abort_handle();
    let cancel = tokio_util::sync::CancellationToken::new();
    {
        let mut tasks = registry.tasks.lock();
        let task = tasks
            .get_mut("task")
            .expect("the fixture must contain task");
        task.cancel_handle = BgCancelHandle::Abort(handle);
        task.cancel_token = Some(cancel.clone());
    }
    registry.confirm_external_stopped(&ticket);
    registry
        .cancel("task")
        .expect("the cancel fixture result is required for this assertion");
    assert!(cancel.is_cancelled());
    tokio::task::yield_now().await;
    tokio::time::advance(std::time::Duration::from_millis(2999)).await;
    assert!(!abort.is_finished());
    tokio::time::advance(std::time::Duration::from_millis(1)).await;
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;
    assert!(abort.is_finished());
    assert_eq!(delivered.load(Ordering::SeqCst), 0);
    assert_eq!(registry.snapshot().tasks[0].status, "cancelled");
    registry.scope.close();
    assert!(!registry.scope.wait().await);
    assert!(
        !registry.scope.is_idle(),
        "abort lost execution cleanup evidence"
    );
}

fn manager_fixture() -> (
    Arc<crate::agent::async_tasks::TaskManager>,
    TaskRegistrationTicket,
) {
    use peri_acp_types::tasks::TaskManager as _;
    let manager = Arc::new(crate::agent::async_tasks::TaskManager::new());
    let ticket = manager
        .register_with_kind(BackgroundTask {
            id: "task".into(),
            agent_name: "fixture".into(),
            prompt_summary: "task".into(),
            status: BackgroundTaskStatus::Running,
            started_at: std::time::Instant::now(),
            chrono_started_at: chrono::Utc::now(),
            kind: BgTaskKind::Agent,
            cancel_handle: BgCancelHandle::Kill(Some(Box::new(|| {}))),
            cancel_token: None,
            pid: None,
            output_preview: None,
            agent_inbox: None,
            initiator_session_id: None,
            owner_session_id: None,
            owner_identity: None,
        })
        .expect("the register fixture result is required for this assertion");
    manager.confirm_external_execution_stopped(&ticket);
    (manager, ticket)
}

/// [回归] 单 worker 上等待持久 MQ 提交时，终态结算不能占住 worker。
///
/// 存储门控期间心跳任务必须仍能推进；提交前不得结算，提交后必须能看到 MQ 行。
#[test]
fn terminal_settlement_must_not_block_the_only_runtime_worker() {
    use std::sync::mpsc;
    use std::time::Duration;

    let queue = MessageQueue::new();
    let (entered, release) = queue.gate_next_enqueue();
    let delivery = crate::session::bg_complete::task_bg_complete_callback(
        crate::agent::async_tasks::delivery::SessionTerminalDelivery::for_queue(queue.clone()),
    );
    let (manager, ticket) = manager_fixture();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("the single-worker runtime fixture must build");
    let (progress_tx, progress_rx) = mpsc::channel();
    let settlement = {
        let manager = Arc::clone(&manager);
        let ticket = ticket.clone();
        runtime.spawn(async move {
            manager
                .settle_completed_awaited(&ticket, result(), delivery)
                .await
        })
    };
    runtime
        .block_on(entered)
        .expect("the enqueue gate must report the pending commit");
    runtime.spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let _ = progress_tx.send(());
    });
    let progressed = progress_rx.recv_timeout(Duration::from_secs(2)).is_ok();
    assert!(
        !ticket.is_settled(),
        "terminal must not be acknowledged before the queue commit"
    );
    release
        .send(())
        .expect("the enqueue gate release must remain available");
    let settled = runtime
        .block_on(settlement)
        .expect("the settlement task must join")
        .expect("the settlement fixture must succeed");
    assert!(settled);
    assert!(ticket.is_settled());
    assert_eq!(queue.len_sync(), 1);
    assert!(
        progressed,
        "waiting for the persistent queue must not block the only runtime worker"
    );
}

/// [回归] awaited 结算仍以存储事实为准：首次入队被存储拒绝保持 delivery_pending，
/// 重试成功后同一结果才结算并落库。
#[tokio::test]
async fn awaited_settlement_retries_a_failed_commit_before_acknowledging() {
    use crate::session::async_result_mailbox::AsyncResultMailbox;
    use crate::session::test_resources::TestSession;
    use peri_acp_types::session::MessageSource;
    use peri_acp_types::system_reminder::TrustedSystemReminder;
    use peri_acp_types::tasks::TaskTerminalDelivery;

    /// 首次投递绑定只读门面，让失败发生在真实 `queue.enqueue`；重试换可写门面。
    struct FirstCommitReadOnly {
        failing: Arc<dyn TaskTerminalDelivery>,
        writable: Arc<dyn TaskTerminalDelivery>,
        attempts: AtomicUsize,
    }

    impl FirstCommitReadOnly {
        fn route(&self) -> &Arc<dyn TaskTerminalDelivery> {
            if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                &self.failing
            } else {
                &self.writable
            }
        }
    }

    impl TaskTerminalDelivery for FirstCommitReadOnly {
        fn accept(
            &self,
            delivery_id: peri_acp_types::messages::MessageId,
            reminder: &TrustedSystemReminder,
            source: MessageSource,
        ) -> Result<(), String> {
            self.route().accept(delivery_id, reminder, source)
        }

        fn deliver<'a>(
            &'a self,
            delivery_id: peri_acp_types::messages::MessageId,
            reminder: &'a TrustedSystemReminder,
            source: MessageSource,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>>
        {
            Box::pin(async move { self.route().deliver(delivery_id, reminder, source).await })
        }
    }

    let session = TestSession::open().await;
    let mailbox = Arc::new(AsyncResultMailbox::new("session", "thread"));
    let route = Arc::new(FirstCommitReadOnly {
        failing:
            crate::agent::async_tasks::delivery::SessionTerminalDelivery::for_queue_with_mailbox(
                MessageQueue::bind(session.read_only_resources().await, session.thread_id()),
                Arc::clone(&mailbox),
            ),
        writable:
            crate::agent::async_tasks::delivery::SessionTerminalDelivery::for_queue_with_mailbox(
                session.queue(),
                Arc::clone(&mailbox),
            ),
        attempts: AtomicUsize::new(0),
    });
    let delivery = crate::session::bg_complete::task_bg_complete_callback(route.clone());
    let (manager, ticket) = manager_fixture();
    let error = manager
        .settle_completed_awaited(&ticket, result(), delivery)
        .await
        .expect_err("a refused commit must not acknowledge the terminal");
    assert!(!error.is_empty(), "存储拒绝必须原样返回给生产者");
    assert!(!ticket.is_settled());
    assert_eq!(manager.snapshot().tasks[0].status, "delivery_pending");
    assert_eq!(
        session
            .queue()
            .snapshot()
            .await
            .expect("the snapshot fixture result is required for this assertion")
            .len(),
        0,
        "提交失败不得留下 MQ 行"
    );

    assert_eq!(manager.retry_pending_deliveries(), 1);
    assert!(ticket.is_settled());
    assert_eq!(manager.snapshot().tasks[0].status, "completed");
    assert_eq!(
        session
            .queue()
            .snapshot()
            .await
            .expect("the snapshot fixture result is required for this assertion")
            .len(),
        1,
        "重试后结果必须落库且只落一条"
    );
    assert_eq!(route.attempts.load(Ordering::SeqCst), 2);
}
