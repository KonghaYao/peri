//! Tests for [`session_bg_complete_callback`](super::session_bg_complete_callback)
//! —— lazy resolve、Shell 完成投递语义、别名口径。

use super::*;
use peri_acp_types::event::ShellOutput;
use peri_acp_types::frozen::{DeregisterRuntimeFn, RegisterRuntimeFn};
use peri_acp_types::goal::GoalController;
use peri_acp_types::session::{
    MessageKind, MessageQueue, MessageSource, QueuedPayload, SessionInbox,
};
use peri_acp_types::system_reminder::{ReminderCategory, ReminderSource};
use peri_acp_types::tasks::{BgTaskKind, TaskManager};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const SESSION_ID: &str = "session-bg-complete-test";

/// 手写测试端口：只有 `session_inbox` 是真实路径。
///
/// 其余方法一律 `unimplemented!()` —— helper 一旦触碰 inbox 之外的会话状态，
/// 测试立即失败（而不是静默通过）。
struct FakeAccessPort {
    inbox: parking_lot::Mutex<Option<Arc<SessionInbox>>>,
    inbox_calls: AtomicUsize,
}

impl FakeAccessPort {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            inbox: parking_lot::Mutex::new(None),
            inbox_calls: AtomicUsize::new(0),
        })
    }

    fn with_inbox(inbox: Arc<SessionInbox>) -> Arc<Self> {
        let port = Self::new();
        port.set_inbox(inbox);
        port
    }

    /// 模拟 session 注册完成后 inbox 变为可见（lazy-init 写回同一槽位）。
    fn set_inbox(&self, inbox: Arc<SessionInbox>) {
        *self.inbox.lock() = Some(inbox);
    }

    fn inbox_calls(&self) -> usize {
        self.inbox_calls.load(Ordering::SeqCst)
    }
}

impl SessionAccessPort for FakeAccessPort {
    fn v2_message_queue(&self, _session_id: &str) -> Option<MessageQueue> {
        unimplemented!("session bg complete callback must not read v2_message_queue")
    }

    fn session_inbox(&self, session_id: &str) -> Option<Arc<SessionInbox>> {
        assert_eq!(
            session_id, SESSION_ID,
            "callback must resolve its own session id"
        );
        self.inbox_calls.fetch_add(1, Ordering::SeqCst);
        self.inbox.lock().clone()
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

/// Shell 类 bg 结果（走 `to_notification` 的 shell 分支，与生产路径同形）。
fn shell_result(task_id: &str) -> BackgroundTaskResult {
    BackgroundTaskResult {
        task_id: task_id.to_string(),
        agent_name: "Bash".to_string(),
        prompt_summary: "cargo test -p peri-agent".to_string(),
        success: true,
        output: String::new(),
        tool_calls_count: 0,
        duration_ms: 1200,
        child_thread_id: None,
        timed_out: false,
        subagent_failure: None,
        shell_output: Some(Box::new(ShellOutput {
            stdout_path: Some("/tmp/peri-bg/stdout.log".to_string()),
            stderr_path: None,
            complete: true,
            error: None,
            exit_code: Some(0),
        })),
    }
}

fn queue_with_inbox() -> (Arc<MessageQueue>, Arc<SessionInbox>) {
    let queue = Arc::new(MessageQueue::new());
    let inbox = Arc::new(SessionInbox::new(Arc::clone(&queue)));
    (queue, inbox)
}

#[test]
fn test_missing_inbox_is_silent_noop() {
    // 装配点形态：session 尚未注册（`session_inbox` 恒 None）。
    let port = FakeAccessPort::new();
    let callback = session_bg_complete_callback(port.clone(), SESSION_ID.to_string());
    let result = shell_result("shell-noop");

    callback(&result, BgTaskKind::Shell);
    callback(&result, BgTaskKind::Shell);

    assert_eq!(
        port.inbox_calls(),
        2,
        "callback must probe the port on every call (no cached/panicking path)"
    );
}

#[test]
fn test_inbox_is_resolved_lazily_at_call_time() {
    // 构造回调时 inbox 不存在（装配点早于 session 注册）。
    let port = FakeAccessPort::new();
    let callback = session_bg_complete_callback(port.clone(), SESSION_ID.to_string());
    let result = shell_result("shell-lazy");

    // 注册前调用：静默 no-op，不得 panic、不得投递。
    callback(&result, BgTaskKind::Shell);

    // session 注册之后：同一闭包（无需重建）必须能投递。
    let (queue, inbox) = queue_with_inbox();
    port.set_inbox(inbox);
    callback(&result, BgTaskKind::Shell);

    assert_eq!(
        queue.len(),
        1,
        "lazily resolved inbox must receive the Defer"
    );
    assert!(queue.has_wake_up(), "Defer must wake the idle session loop");
}

#[test]
fn test_shell_completion_is_delivered_as_defer_with_shell_source() {
    let (queue, inbox) = queue_with_inbox();
    let port = FakeAccessPort::with_inbox(inbox);
    let callback = session_bg_complete_callback(port, SESSION_ID.to_string());
    // task_id 恰好 8 字符，`to_notification` 的 short_id 截断对其为恒等。
    let result = shell_result("shell-ab");

    callback(&result, BgTaskKind::Shell);

    assert_eq!(queue.len(), 1, "exactly one message per completion");
    assert!(queue.has_wake_up(), "Defer must wake the idle session loop");

    let messages = queue.drain_all();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].kind, MessageKind::Defer);
    assert_eq!(messages[0].source, MessageSource::ShellComplete);

    let QueuedPayload::SystemReminder(reminder) = &messages[0].payload else {
        panic!("bg completion must be delivered as a trusted system reminder");
    };
    let reminder = reminder.as_reminder();
    assert_eq!(reminder.category, ReminderCategory::Task);
    assert_eq!(reminder.source, ReminderSource("shell".to_string()));
    assert_eq!(
        reminder.body,
        result.to_notification(),
        "reminder body must be the canonical notification text"
    );
    assert!(reminder
        .body
        .starts_with("[后台任务 shell-ab 已完成] Agent: Bash | 退出码 0"));
    assert!(reminder
        .body
        .contains("stdout 输出文件：/tmp/peri-bg/stdout.log"));
}

#[test]
fn test_callback_result_is_assignable_to_acp_types_alias() {
    // 口径证据：两处 `OnBgCompleteFn` 是同一底层类型的别名，本 helper 的返回值
    // 可直接作为 seam 字段（acp-types 口径）使用。
    let (queue, inbox) = queue_with_inbox();
    let port = FakeAccessPort::with_inbox(inbox);
    let callback: peri_acp_types::tasks::OnBgCompleteFn =
        session_bg_complete_callback(port, SESSION_ID.to_string());
    let result = shell_result("shell-alias");

    callback(&result, BgTaskKind::Shell);

    assert_eq!(queue.len(), 1);
    assert_eq!(queue.drain_all()[0].source, MessageSource::ShellComplete);
}

// ─── U7：取消路径上的 session 级回调 ────────────────────────────────────────────
//
// 口径（U7 原文）：`shell.rs` 的 `claim_completion` 短路使取消路径**按设计跳过**回调
// ⇒ 后台任务被取消时不产生 `Defer` / 提醒（与迁移前链上行为一致，非本波引入）。
// 本节把「自然完成必送达」与「取消必不送达」做成**同一夹具下的对照**：两侧都走真
// `TaskManager::spawn_shell` 的收尾路径 + 真 session 级回调（真 `SessionInbox`），
// 差异只在「取消」这一步。

/// U7 主体：取消路径**不得**调用 `on_bg_complete`，队列里**不得**出现 `Defer`。
///
/// 观测面（每一层都是真的，无替身）：真 `TaskManager::spawn_shell` 起 `sleep 60` +
/// **真 session 级回调**（[`session_bg_complete_callback`] → 真 `SessionInbox` /
/// 真 `MessageQueue`）。计数壳只做「记一次调用 + 原样转交真回调」，因此「回调未被调用」
/// 与「队列里没有 Defer」是同一次观测的两面。
///
/// 区分力（破坏后必红）：把 `peri-agent/src/agent/async_tasks/shell.rs:588-590` 的
/// `if !registry.claim_completion(&task_id) { return; }` 去掉（取消路径照常回调）⇒
/// 本轮计数变 1、队列多 1 条 `Defer` ⇒ 本用例红。
#[cfg(unix)]
#[tokio::test]
async fn test_cancelled_background_shell_skips_completion_callback_and_defer() {
    use crate::agent::async_tasks::TaskManager as ConcreteTaskManager;
    use peri_acp_types::tasks::TaskShutdownReport;

    let (queue, inbox) = queue_with_inbox();
    let port = FakeAccessPort::with_inbox(inbox);
    let delivered = session_bg_complete_callback(port, SESSION_ID.to_string());
    let calls = Arc::new(AtomicUsize::new(0));
    let counting: peri_acp_types::tasks::OnBgCompleteFn = {
        let calls = Arc::clone(&calls);
        let delivered = Arc::clone(&delivered);
        Arc::new(move |result: &BackgroundTaskResult, kind: BgTaskKind| {
            calls.fetch_add(1, Ordering::SeqCst);
            delivered(result, kind);
        })
    };

    let manager = ConcreteTaskManager::new();
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
        0,
        "取消路径不得调用 on_bg_complete（claim_completion 短路，shell.rs:588-590）"
    );
    assert_eq!(queue.len(), 0, "取消路径不得投递任何消息（含 Defer 提醒）");
    assert!(!queue.has_wake_up(), "取消不产生唤醒信号");
}

/// U7 正向对照（同一夹具）：**自然完成**的同一路径必须调用回调并把结果投成 `Defer`。
///
/// 它证明上一条用例的绿不是「夹具本身不投递」造成的：同一对 `(manager, 回调, 队列)`，
/// 只把「取消」换成「跑完」。
///
/// 与 `test_shell_completion_is_delivered_as_defer_with_shell_source` 的分工：那条用
/// **手工构造**的 `BackgroundTaskResult` 直接调闭包（锁投递形态），本条的 result 由真
/// `TaskManager::spawn_shell` 的收尾链（`manager.rs:547` → `finalize_bg_shell`）产出
/// （锁「真收尾路径确实会调到这个闭包」）。两者都不能替代对方。
///
/// 区分力（破坏后必红）：把 `shell.rs:588-590` 的短路条件反转（自然完成也提前返回）⇒
/// 回调计数为 0、队列为空 ⇒ 本用例红。
#[cfg(unix)]
#[tokio::test]
async fn test_natural_background_shell_completion_delivers_defer_through_the_manager() {
    use crate::agent::async_tasks::TaskManager as ConcreteTaskManager;
    use peri_acp_types::tasks::TaskShutdownReport;

    let (queue, inbox) = queue_with_inbox();
    let port = FakeAccessPort::with_inbox(inbox);
    let delivered = session_bg_complete_callback(port, SESSION_ID.to_string());
    let calls = Arc::new(AtomicUsize::new(0));
    let counting: peri_acp_types::tasks::OnBgCompleteFn = {
        let calls = Arc::clone(&calls);
        let delivered = Arc::clone(&delivered);
        Arc::new(move |result: &BackgroundTaskResult, kind: BgTaskKind| {
            calls.fetch_add(1, Ordering::SeqCst);
            delivered(result, kind);
        })
    };

    let manager = ConcreteTaskManager::new();
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
