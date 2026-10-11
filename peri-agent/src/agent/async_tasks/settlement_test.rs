use super::*;
use crate::agent::async_tasks::{BackgroundTask, BackgroundTaskStatus, BgCancelHandle};
use peri_acp_types::session::{
    MessageKind, MessageQueue, MessageSource, QueuedMessage, SessionInbox,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

fn registered(kind: BgTaskKind) -> Arc<BackgroundTaskRegistry> {
    let registry = Arc::new(BackgroundTaskRegistry::new());
    registry
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
        .unwrap();
    registry
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
        let registry = registered(kind);
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
            assert_eq!(owner.upgrade().unwrap().active_count(), 1);
            output.push(QueuedMessage::new(
                MessageKind::Defer,
                MessageSource::SystemInjected,
                crate::messages::BaseMessage::human(result.output.clone()),
            ));
            Ok(())
        });
        assert!(registry
            .settle_completed("task", result(), delivery)
            .unwrap());
        tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
            .await
            .unwrap();
        assert_eq!(registry.active_count(), 0);
        assert_eq!(queue.len(), 1);
    }
}

/// [回归测试] 回调拒绝交付时保留结果，不允许 active count 提前归零。
#[test]
fn rejected_delivery_is_visible_retained_and_retried_once() {
    let registry = registered(BgTaskKind::Agent);
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
        registry.settle_completed("task", result(), Arc::clone(&delivery)),
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
        .settle_completed("task", result(), delivery)
        .unwrap());
    assert_eq!(registry.retry_pending_deliveries(), 0);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

#[test]
fn delivery_panic_keeps_terminal_unpublished_until_retry() {
    let registry = registered(BgTaskKind::Workflow);
    let attempts = Arc::new(AtomicUsize::new(0));
    let called = Arc::clone(&attempts);
    let delivery: OnBgCompleteFn = Arc::new(move |_, _| {
        if called.fetch_add(1, Ordering::SeqCst) == 0 {
            panic!("delivery failed");
        }
        Ok(())
    });
    assert!(registry
        .settle_completed("task", result(), delivery)
        .is_err());
    assert_eq!(registry.active_count(), 1);
    assert_eq!(registry.retry_pending_deliveries(), 1);
    assert_eq!(registry.active_count(), 0);
}

#[test]
fn cancelled_shell_cleanup_is_delivered_without_ghost_completion() {
    let registry = registered(BgTaskKind::Shell);
    registry.cancel("task").unwrap();
    let attempts = Arc::new(AtomicUsize::new(0));
    let called = Arc::clone(&attempts);
    let delivery: OnBgCompleteFn = Arc::new(move |result, kind| {
        assert_eq!(kind, BgTaskKind::Shell);
        assert!(!result.success);
        called.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });
    assert!(!registry
        .settle_completed("task", result(), Arc::clone(&delivery))
        .unwrap());
    assert!(!registry
        .settle_completed("task", result(), delivery)
        .unwrap());
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    assert_eq!(registry.snapshot().tasks[0].status, "cancelled");
}

#[test]
fn unknown_task_does_not_call_delivery() {
    let registry = Arc::new(BackgroundTaskRegistry::new());
    let delivery: OnBgCompleteFn = Arc::new(|_, _| panic!("unknown task cannot publish"));
    assert!(matches!(
        registry.settle_completed("task", result(), delivery),
        Err(BackgroundRegistryError::TaskNotFound(_))
    ));
}

#[test]
fn cancelled_cleanup_retry_cannot_report_idle_while_delivery_is_in_flight() {
    let registry = registered(BgTaskKind::Shell);
    registry.cancel("task").unwrap();
    registry.confirm_external_stopped("task");
    let owner = Arc::downgrade(&registry);
    let attempts = Arc::new(AtomicUsize::new(0));
    let called = Arc::clone(&attempts);
    let delivery: OnBgCompleteFn = Arc::new(move |_, _| {
        assert!(!owner.upgrade().unwrap().external_settled());
        if called.fetch_add(1, Ordering::SeqCst) == 0 {
            Err("temporarily unavailable".into())
        } else {
            Ok(())
        }
    });
    assert!(registry
        .settle_completed("task", result(), delivery)
        .is_err());
    assert!(!registry.external_settled());
    assert_eq!(registry.retry_pending_deliveries(), 1);
    assert!(registry.external_settled());
}

/// [回归测试] 当前进程的暂时投递失败不能依赖下一次用户输入才能恢复。
#[tokio::test(start_paused = true)]
async fn pending_delivery_recovers_without_new_input() {
    let registry = registered(BgTaskKind::Agent);
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
        .settle_completed("task", result(), delivery)
        .is_err());
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    assert_eq!(registry.active_count(), 0);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert_eq!(registry.snapshot().tasks[0].status, "completed");
}

#[tokio::test(start_paused = true)]
async fn permanent_delivery_failure_backs_off_and_close_drains_worker() {
    let registry = registered(BgTaskKind::Agent);
    let attempts = Arc::new(AtomicUsize::new(0));
    let called = attempts.clone();
    assert!(registry
        .settle_completed(
            "task",
            result(),
            Arc::new(move |_, _| {
                called.fetch_add(1, Ordering::SeqCst);
                Err("permanently unavailable".into())
            })
        )
        .is_err());
    tokio::task::yield_now().await;
    for millis in [100, 200, 400, 800, 1600, 3200, 5000, 5000] {
        tokio::time::advance(std::time::Duration::from_millis(millis)).await;
        tokio::task::yield_now().await;
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
    let registry = registered(BgTaskKind::Agent);
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
                entered_tx.lock().take().unwrap().send(()).unwrap();
                release_rx.lock().recv().unwrap();
                Err("manual retry also failed".into())
            }
            2 => Ok(()),
            _ => panic!("duplicate delivery"),
        });
    assert!(registry
        .settle_completed("task", result(), delivery)
        .is_err());
    let retry_owner = registry.clone();
    let manual = std::thread::spawn(move || retry_owner.retry_pending_deliveries());
    entered_rx.await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert_eq!(registry.active_count(), 1);
    release_tx.send(()).unwrap();
    assert_eq!(manual.join().unwrap(), 0);
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while registry.active_count() != 0 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
    registry.scope.close();
    assert!(registry.scope.wait().await);
    assert!(registry.external_settled());
}

#[tokio::test(start_paused = true)]
async fn shutdown_retries_pending_delivery_once_and_reports_remaining_failure() {
    use peri_acp_types::tasks::{TaskManager as _, TaskShutdownReport};
    for recover in [true, false] {
        let manager = super::super::super::TaskManager::new();
        let fixture = registered(BgTaskKind::Agent);
        let task = fixture.tasks.lock().remove("task").unwrap();
        manager.register_with_kind(task).unwrap();
        let attempts = Arc::new(AtomicUsize::new(0));
        let called = attempts.clone();
        assert!(manager
            .settle_completed(
                "task",
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
    let registry = registered(BgTaskKind::Agent);
    let owner = Arc::downgrade(&registry);
    assert!(registry
        .settle_completed("task", result(), Arc::new(|_, _| Err("unavailable".into())))
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
    let registry = registered(BgTaskKind::Agent);
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
        .settle_completed("task", result(), delivery)
        .is_err());
    assert_eq!(queue.len(), 1);
    assert!(wake.has_changed().unwrap());
    wake.borrow_and_update();
    assert_eq!(queue.drain_batch(64).len(), 1);
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    assert_eq!(registry.active_count(), 0);
    assert_eq!(queue.len(), 0);
    assert!(!wake.has_changed().unwrap());
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
        .unwrap();
    wake.borrow_and_update();
    assert_eq!(queue.drain_batch(64).len(), 1);
    route
        .accept(delivery_id, &reminder, MessageSource::SubAgentComplete)
        .unwrap();
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
    assert_eq!(queue.len(), 0);
    assert!(!wake.has_changed().unwrap());
}

#[tokio::test(start_paused = true)]
async fn cooperative_agent_cancellation_retries_original_delivery_without_new_terminal() {
    let registry = registered(BgTaskKind::Agent);
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
    let handle = registry
        .scope
        .spawn(async move {
            execution_cancel.cancelled().await;
            let registry = owner.upgrade().unwrap();
            let mut cancelled = result();
            cancelled.success = false;
            cancelled.output = "Background sub-agent was interrupted".into();
            cancelled.child_thread_id = Some("original-child".into());
            assert!(registry
                .settle_completed("task", cancelled.clone(), task_delivery.clone())
                .is_err());
            assert!(!registry
                .settle_completed("task", cancelled, task_delivery)
                .unwrap());
        })
        .unwrap();
    {
        let mut tasks = registry.tasks.lock();
        let task = tasks.get_mut("task").unwrap();
        task.cancel_handle = BgCancelHandle::Abort(handle);
        task.cancel_token = Some(cancel);
    }
    registry.confirm_external_stopped("task");
    let mut events = registry.subscribe_events();
    registry.cancel("task").unwrap();
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert_eq!(registry.snapshot().tasks[0].status, "cancelled");
    assert!(!registry
        .settle_completed("task", result(), delivery)
        .unwrap());
    assert_eq!(registry.retry_pending_deliveries(), 0);
    assert!(matches!(
        events.try_recv().unwrap().event,
        BgRegistryEvent::Cancelled { .. }
    ));
    assert!(events.try_recv().is_err());
    registry.scope.close();
    assert!(registry.scope.wait().await);
    assert!(registry.external_settled());
}

#[tokio::test(start_paused = true)]
async fn aborted_agent_does_not_fabricate_cleanup_delivery() {
    let registry = registered(BgTaskKind::Agent);
    let delivered = Arc::new(AtomicUsize::new(0));
    let called = delivered.clone();
    let owner = Arc::downgrade(&registry);
    let handle = registry
        .scope
        .spawn(async move {
            futures::future::pending::<()>().await;
            if let Some(registry) = owner.upgrade() {
                registry
                    .settle_completed(
                        "task",
                        result(),
                        Arc::new(move |_, _| {
                            called.fetch_add(1, Ordering::SeqCst);
                            Ok(())
                        }),
                    )
                    .unwrap();
            }
        })
        .unwrap();
    let abort = handle.abort_handle();
    let cancel = tokio_util::sync::CancellationToken::new();
    {
        let mut tasks = registry.tasks.lock();
        let task = tasks.get_mut("task").unwrap();
        task.cancel_handle = BgCancelHandle::Abort(handle);
        task.cancel_token = Some(cancel.clone());
    }
    registry.confirm_external_stopped("task");
    registry.cancel("task").unwrap();
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
