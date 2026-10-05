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
    let registry = BackgroundTaskRegistry::new();
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
