use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use peri_acp_types::event::BackgroundTaskResult;
use peri_acp_types::frozen::{DeregisterRuntimeFn, RegisterRuntimeFn};
use peri_acp_types::goal::GoalController;
use peri_acp_types::session::{
    MessageKind, MessageQueue, MessageSource, SessionAccessPort, SessionInbox,
};
use peri_acp_types::tasks::{BgTaskKind, TaskManager};
use peri_agent::session::bg_complete::session_bg_complete_callback;

const SESSION_ID: &str = "session-bg-complete-test";

struct SessionInboxAccess {
    inbox: Arc<SessionInbox>,
}

impl SessionAccessPort for SessionInboxAccess {
    fn v2_message_queue(&self, _session_id: &str) -> Option<MessageQueue> {
        unimplemented!("session bg complete callback must not read v2_message_queue")
    }

    fn session_inbox(&self, session_id: &str) -> Option<Arc<SessionInbox>> {
        assert_eq!(
            session_id, SESSION_ID,
            "callback must resolve its own session id"
        );
        Some(Arc::clone(&self.inbox))
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

fn queue_with_inbox() -> (Arc<MessageQueue>, Arc<SessionInbox>) {
    let queue = Arc::new(MessageQueue::new());
    let inbox = Arc::new(SessionInbox::new(Arc::clone(&queue)));
    (queue, inbox)
}

#[cfg(unix)]
#[tokio::test]
async fn test_cancelled_background_shell_delivers_cleanup_once_without_second_terminal() {
    use peri_acp_types::tasks::TaskShutdownReport;

    let (queue, inbox) = queue_with_inbox();
    let port = Arc::new(SessionInboxAccess { inbox });
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

    // 确定性屏障：`shutdown` 返回 `Complete` 之前，owned 执行（含 worker 走到
    // `finalize_bg_shell` 的那一步）必须已收尾——否则下面的计数 / 队列断言可能只是
    // 「回调还没跑到」的假绿。`Complete` 同时证明进程组确已消失（`scope.wait()` 等的是
    // 真实执行结束，不只是登记表条目被删）。
    assert_eq!(
        manager.shutdown().await,
        TaskShutdownReport::Complete,
        "取消后的收尾必须是 Complete（进程组真正退出）"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "取消清理完成后必须恰好调用一次 on_bg_complete"
    );
    assert_eq!(queue.len(), 1);
    assert!(queue.has_wake_up());
    let messages = queue.drain_all();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].kind, MessageKind::Defer);
    assert_eq!(messages[0].source, MessageSource::ShellComplete);
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
/// `TaskManager::spawn_shell` 的收尾链（`manager.rs:547` → `finalize_bg_shell`）产出
/// （锁「真收尾路径确实会调到这个闭包」）。两者都不能替代对方。
///
/// 区分力（破坏后必红）：自然完成也提前返回 ⇒
/// 回调计数为 0、队列为空 ⇒ 本用例红。
#[cfg(unix)]
#[tokio::test]
async fn test_natural_background_shell_completion_delivers_defer_through_the_manager() {
    use peri_acp_types::tasks::TaskShutdownReport;

    let (queue, inbox) = queue_with_inbox();
    let port = Arc::new(SessionInboxAccess { inbox });
    let delivered = session_bg_complete_callback(port, SESSION_ID.to_string());
    let calls = Arc::new(AtomicUsize::new(0));
    let counting: peri_acp_types::tasks::OnBgCompleteFn = {
        let calls = Arc::clone(&calls);
        let delivered = Arc::clone(&delivered);
        Arc::new(move |result: &BackgroundTaskResult, kind: BgTaskKind| {
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

    // 有界等待回调（收尾在 worker 里异步发生），随后用 shutdown 作确定性屏障再终判。
    for _ in 0..200 {
        if calls.load(Ordering::SeqCst) >= 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(
        manager.shutdown().await,
        TaskShutdownReport::Complete,
        "自然完成后的收尾必须是 Complete"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "自然完成必须**恰好**调用一次 on_bg_complete"
    );
    assert_eq!(queue.len(), 1, "自然完成必须投递 1 条消息");
    assert!(queue.has_wake_up(), "Defer 必须唤醒 idle 会话循环");

    let messages = queue.drain_all();
    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0].kind,
        MessageKind::Defer,
        "自然完成以 Defer 投递（与取消路径的「无消息」互斥）"
    );
    assert_eq!(messages[0].source, MessageSource::ShellComplete);
}
