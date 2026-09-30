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
