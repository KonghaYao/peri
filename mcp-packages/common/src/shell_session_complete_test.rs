use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use peri_acp_types::event::BackgroundTaskResult;
use peri_acp_types::frozen::{DeregisterRuntimeFn, RegisterRuntimeFn};
use peri_acp_types::goal::GoalController;
use peri_acp_types::messages::MessageId;
use peri_acp_types::session::{
    MessageKind, MessageQueue, MessageSource, QueuedMessage, SessionAccessPort, SessionInbox,
};
use peri_acp_types::system_reminder::TrustedSystemReminder;
use peri_acp_types::tasks::{BgTaskKind, TaskManager, TaskTerminalDelivery};
use peri_agent::session::bg_complete::session_bg_complete_callback;

const SESSION_ID: &str = "session-bg-complete-test";

struct SessionDeliveryAccess {
    delivery: Arc<dyn TaskTerminalDelivery>,
}

impl SessionAccessPort for SessionDeliveryAccess {
    fn task_terminal_delivery(&self, session_id: &str) -> Option<Arc<dyn TaskTerminalDelivery>> {
        assert_eq!(
            session_id, SESSION_ID,
            "callback must resolve its own session id"
        );
        Some(Arc::clone(&self.delivery))
    }

    fn v2_message_queue(&self, _session_id: &str) -> Option<MessageQueue> {
        unimplemented!("session bg complete callback must not read v2_message_queue")
    }

    fn session_inbox(&self, _session_id: &str) -> Option<Arc<SessionInbox>> {
        unimplemented!("session bg complete callback must not read session_inbox")
    }

    fn idle_suspended_flag(&self, _session_id: &str) -> Option<Arc<AtomicBool>> {
        unimplemented!("session bg complete callback must not read idle_suspended_flag")
    }

    fn task_manager(&self, _session_id: &str) -> Option<Arc<dyn TaskManager>> {
        unimplemented!("session bg complete callback must not read task_manager")
    }

    fn goal_controller(&self, _session_id: &str) -> Option<Arc<dyn GoalController>> {
        unimplemented!("session bg complete callback must not read goal_controller")
    }

    fn register_runtime(&self, _session_id: &str) -> Option<RegisterRuntimeFn> {
        unimplemented!("session bg complete callback must not read register_runtime")
    }

    fn deregister_runtime(&self, _session_id: &str) -> Option<DeregisterRuntimeFn> {
        unimplemented!("session bg complete callback must not read deregister_runtime")
    }

    fn cancel_cascade_children(&self, _session_id: &str) {
        unimplemented!("session bg complete callback must not cascade children")
    }

    fn cron_bridge_for(&self, _session_id: &str) -> bool {
        unimplemented!("session bg complete callback must not start cron bridge")
    }
}

struct RecordingTerminalDelivery {
    queue: Arc<MessageQueue>,
    receipts: Mutex<HashSet<String>>,
}

impl TaskTerminalDelivery for RecordingTerminalDelivery {
    fn deliver<'a>(
        &'a self,
        delivery_id: MessageId,
        reminder: &'a TrustedSystemReminder,
        source: MessageSource,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            let mut receipts = self.receipts.lock().unwrap();
            if receipts.insert(delivery_id.as_uuid().to_string()) {
                self.queue
                    .push(QueuedMessage::system_reminder_with_delivery_id(
                        MessageKind::Defer,
                        source,
                        reminder.clone(),
                        delivery_id,
                    ));
            }
            Ok(())
        })
    }
}

fn queue_with_delivery() -> (Arc<MessageQueue>, Arc<RecordingTerminalDelivery>) {
    let queue = Arc::new(MessageQueue::new());
    let delivery = Arc::new(RecordingTerminalDelivery {
        queue: Arc::clone(&queue),
        receipts: Mutex::new(HashSet::new()),
    });
    (queue, delivery)
}

async fn shutdown_after_receipt(manager: &peri_agent::agent::async_tasks::TaskManager) {
    use peri_acp_types::tasks::TaskShutdownReport;

    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if manager.shutdown().await == TaskShutdownReport::Complete {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("终态回执和进程收尾必须最终使 shutdown 返回 Complete");
}

#[cfg(unix)]
#[tokio::test]
async fn test_cancelled_background_shell_delivers_cleanup_once_without_second_terminal() {
    let (queue, delivery) = queue_with_delivery();
    let port = Arc::new(SessionDeliveryAccess {
        delivery: delivery.clone(),
    });
    let delivered = session_bg_complete_callback(port, SESSION_ID.to_string());
    let calls = Arc::new(AtomicUsize::new(0));
    let counting: peri_acp_types::tasks::OnBgCompleteFn = {
        let calls = Arc::clone(&calls);
        let delivered = Arc::clone(&delivered);
        Arc::new(move |result: &BackgroundTaskResult, kind: BgTaskKind| {
            assert!(!result.success);
            assert_eq!(kind, BgTaskKind::Shell);
            assert_eq!(
                result.output,
                "Shell command cancelled; read the output files as needed."
            );
            calls.fetch_add(1, Ordering::SeqCst);
            delivered(result, kind)
        })
    };

    let manager = crate::create_local_task_manager();
    let cwd = tempfile::tempdir().expect("临时目录夹具必须可创建");
    let shell = manager
        .spawn_shell(
            "sleep 60".into(),
            cwd.path().to_string_lossy().into_owned(),
            None,
            Some(counting),
        )
        .expect("spawn_shell 必须成功（真进程 + 真注册）");
    assert_eq!(manager.active_count(), 1, "任务必须先登记（取消才有对象）");
    let mut changes = manager.subscribe_events();

    manager
        .cancel(&shell.task_id)
        .expect("取消已登记的后台 shell 必须成功");
    assert_eq!(
        manager.active_count(),
        0,
        "取消即移除条目：登记表里不得再有该任务"
    );

    tokio::time::timeout(std::time::Duration::from_secs(5), queue.await_wake())
        .await
        .expect("取消清理必须取得终态投递回执");
    shutdown_after_receipt(&manager).await;
    let callback_calls = calls.load(Ordering::SeqCst);
    assert!(callback_calls >= 2, "取消清理必须在 Pending 后重试确认回执");
    assert_eq!(
        manager.shutdown().await,
        peri_acp_types::tasks::TaskShutdownReport::Complete
    );
    assert_eq!(calls.load(Ordering::SeqCst), callback_calls);
    assert_eq!(delivery.receipts.lock().unwrap().len(), 1);
    assert_eq!(queue.len(), 1);
    assert!(queue.has_wake_up());
    let messages = queue.drain_all();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].kind, MessageKind::Defer);
    assert_eq!(messages[0].source, MessageSource::ShellComplete);
    assert!(messages[0].delivery_id.is_some());
    assert!(matches!(
        changes.try_recv().unwrap().event,
        peri_acp_types::tasks::BgRegistryEvent::Cancelled { task_id, .. } if task_id == shell.task_id
    ));
    assert!(changes.try_recv().is_err());
}

/// U7 正向对照（同一夹具）：**自然完成**的同一路径必须调用回调并把结果投成 `Defer`。
///
/// 与 `test_shell_completion_is_delivered_as_defer_with_shell_source` 的分工：那条用
/// **手工构造**的 `BackgroundTaskResult` 直接调闭包（锁投递形态），本条的 result 由真
/// `TaskManager::spawn_shell` 的收尾链（`finalize_bg_shell`）产出
/// （锁「真收尾路径确实会调到这个闭包」）。两者都不能替代对方。
///
/// 区分力（破坏后必红）：自然完成也提前返回 ⇒
/// 回调计数为 0、队列为空 ⇒ 本用例红。
#[cfg(unix)]
#[tokio::test]
async fn test_natural_background_shell_completion_delivers_defer_through_the_manager() {
    let (queue, delivery) = queue_with_delivery();
    let port = Arc::new(SessionDeliveryAccess {
        delivery: delivery.clone(),
    });
    let delivered = session_bg_complete_callback(port, SESSION_ID.to_string());
    let calls = Arc::new(AtomicUsize::new(0));
    let counting: peri_acp_types::tasks::OnBgCompleteFn = {
        let calls = Arc::clone(&calls);
        let delivered = Arc::clone(&delivered);
        Arc::new(move |result: &BackgroundTaskResult, kind: BgTaskKind| {
            assert!(result.success, "自然完成必须保留成功结果");
            assert_eq!(kind, BgTaskKind::Shell);
            calls.fetch_add(1, Ordering::SeqCst);
            delivered(result, kind)
        })
    };

    let manager = crate::create_local_task_manager();
    let cwd = tempfile::tempdir().expect("临时目录夹具必须可创建");
    manager
        .spawn_shell(
            "true".into(),
            cwd.path().to_string_lossy().into_owned(),
            None,
            Some(counting),
        )
        .expect("spawn_shell 必须成功");

    tokio::time::timeout(std::time::Duration::from_secs(5), queue.await_wake())
        .await
        .expect("自然完成必须取得终态投递回执");
    shutdown_after_receipt(&manager).await;
    let callback_calls = calls.load(Ordering::SeqCst);
    assert!(callback_calls >= 2, "自然完成必须在 Pending 后重试确认回执");
    assert_eq!(
        manager.shutdown().await,
        peri_acp_types::tasks::TaskShutdownReport::Complete
    );
    assert_eq!(calls.load(Ordering::SeqCst), callback_calls);
    assert_eq!(delivery.receipts.lock().unwrap().len(), 1);
    assert_eq!(queue.len(), 1, "自然完成必须投递 1 条消息");
    assert!(queue.has_wake_up(), "Defer 必须唤醒 idle 会话循环");

    let messages = queue.drain_all();
    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0].kind,
        MessageKind::Defer,
        "自然完成以 Defer 投递"
    );
    assert_eq!(messages[0].source, MessageSource::ShellComplete);
    assert!(messages[0].delivery_id.is_some());
}
