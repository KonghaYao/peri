use super::*;
use crate::agent::async_tasks::delivery::SessionTerminalDelivery;
use peri_acp_types::session::MessageQueue;
use peri_acp_types::tasks::{BgTaskRegistration, TaskManager as _, TaskTerminalDelivery};
use std::sync::atomic::{AtomicUsize, Ordering};

fn shell_result(task_id: &str) -> BackgroundTaskResult {
    BackgroundTaskResult {
        task_id: task_id.into(),
        agent_name: "Bash".into(),
        prompt_summary: "shell".into(),
        success: true,
        output: "terminal output".into(),
        tool_calls_count: 0,
        duration_ms: 1200,
        child_thread_id: None,
        timed_out: false,
        subagent_failure: None,
        shell_output: None,
    }
}

struct FailedDelivery;
impl peri_acp_types::tasks::TaskTerminalDelivery for FailedDelivery {
    fn accept(
        &self,
        _: peri_acp_types::messages::MessageId,
        _: &peri_acp_types::system_reminder::TrustedSystemReminder,
        _: peri_acp_types::session::MessageSource,
    ) -> Result<(), String> {
        Err("current terminal delivery unavailable".into())
    }

    fn deliver<'a>(
        &'a self,
        _: peri_acp_types::messages::MessageId,
        _: &'a peri_acp_types::system_reminder::TrustedSystemReminder,
        _: peri_acp_types::session::MessageSource,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async { Err("current terminal delivery unavailable".into()) })
    }
}
fn failed_terminal_callback() -> OnBgCompleteFn {
    task_bg_complete_callback(Arc::new(FailedDelivery))
}

#[test]
fn current_terminal_delivery_is_accepted_once_without_a_ledger() {
    let queue = MessageQueue::new();
    let callback = task_bg_complete_callback(SessionTerminalDelivery::for_queue(queue.clone()));
    let result = shell_result("shell-current");
    callback(&result, BgTaskKind::Shell).unwrap();
    callback(&result, BgTaskKind::Shell).unwrap();
    assert_eq!(queue.drain_batch(64).len(), 1);
}

#[test]
fn same_terminal_identity_cannot_acknowledge_changed_payload() {
    let queue = MessageQueue::new();
    let callback = task_bg_complete_callback(SessionTerminalDelivery::for_queue(queue.clone()));
    let mut result = shell_result("shell-conflict");
    callback(&result, BgTaskKind::Shell).unwrap();
    result.output = "changed".into();
    assert!(callback(&result, BgTaskKind::Shell)
        .unwrap_err()
        .contains("conflicting"));
    assert_eq!(queue.drain_batch(64).len(), 1);
}

fn registered_manager(task_id: &str) -> crate::agent::async_tasks::TaskManager {
    let manager = crate::agent::async_tasks::TaskManager::new();
    manager
        .register(BgTaskRegistration {
            task_id: task_id.into(),
            kind: BgTaskKind::Agent,
            summary: "terminal acceptance".into(),
            pid: None,
            kill: Some(Box::new(|| {})),
        })
        .unwrap();
    manager
}

#[test]
fn accepted_queue_delivery_settles_task_without_another_run() {
    let queue = MessageQueue::new();
    let callback = task_bg_complete_callback(SessionTerminalDelivery::for_queue(queue.clone()));
    let result = shell_result("accepted-task");
    let manager = registered_manager(&result.task_id);
    assert!(manager
        .settle_completed(&result.task_id, result.clone(), callback.clone())
        .unwrap());
    assert_eq!(manager.active_count(), 0);
    assert!(manager.is_execution_idle());
    assert_eq!(manager.snapshot().tasks[0].status, "completed");
    assert!(queue.has_wake_up());
    assert_eq!(queue.len(), 1);
    callback(&result, BgTaskKind::Agent).unwrap();
    assert!(!manager
        .settle_completed(&result.task_id, result.clone(), callback)
        .unwrap());
    assert_eq!(manager.retry_pending_deliveries(), 0);
    assert_eq!(queue.len(), 1);
}

struct RetryDelivery {
    route: Arc<dyn TaskTerminalDelivery>,
    attempts: AtomicUsize,
}

impl TaskTerminalDelivery for RetryDelivery {
    fn accept(
        &self,
        delivery_id: peri_acp_types::messages::MessageId,
        reminder: &peri_acp_types::system_reminder::TrustedSystemReminder,
        source: peri_acp_types::session::MessageSource,
    ) -> Result<(), String> {
        if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err("queue acceptance refused".into());
        }
        self.route.accept(delivery_id, reminder, source)
    }

    fn deliver<'a>(
        &'a self,
        delivery_id: peri_acp_types::messages::MessageId,
        reminder: &'a peri_acp_types::system_reminder::TrustedSystemReminder,
        source: peri_acp_types::session::MessageSource,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move { self.accept(delivery_id, reminder, source) })
    }
}

#[test]
fn refused_queue_delivery_retains_task_and_retries_original_once() {
    let queue = MessageQueue::new();
    let route = Arc::new(RetryDelivery {
        route: SessionTerminalDelivery::for_queue(queue.clone()),
        attempts: AtomicUsize::new(0),
    });
    let callback = task_bg_complete_callback(route.clone());
    let result = shell_result("refused-task");
    let manager = registered_manager(&result.task_id);
    let error = manager
        .settle_completed(&result.task_id, result.clone(), callback.clone())
        .unwrap_err();
    assert!(error.to_string().contains("queue acceptance refused"));
    assert_eq!(manager.active_count(), 1);
    assert_eq!(manager.snapshot().tasks[0].status, "delivery_pending");
    assert!(queue.is_empty());
    assert_eq!(manager.retry_pending_deliveries(), 1);
    assert_eq!(manager.active_count(), 0);
    assert!(manager.is_execution_idle());
    assert_eq!(manager.snapshot().tasks[0].status, "completed");
    callback(&result, BgTaskKind::Agent).unwrap();
    assert_eq!(manager.retry_pending_deliveries(), 0);
    assert_eq!(route.attempts.load(Ordering::SeqCst), 2);
    let accepted = queue.drain_all();
    assert_eq!(accepted.len(), 1);
    assert_eq!(
        accepted[0].source,
        peri_acp_types::session::MessageSource::SubAgentComplete
    );
}

struct AsyncDelivery(AtomicUsize);

impl TaskTerminalDelivery for AsyncDelivery {
    fn deliver<'a>(
        &'a self,
        _: peri_acp_types::messages::MessageId,
        _: &'a peri_acp_types::system_reminder::TrustedSystemReminder,
        _: peri_acp_types::session::MessageSource,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            tokio::task::yield_now().await;
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
}

#[tokio::test]
async fn asynchronous_route_requires_its_owner_to_await_delivery() {
    let route = Arc::new(AsyncDelivery(AtomicUsize::new(0)));
    let callback = task_bg_complete_callback(route.clone());
    let result = shell_result("async-task");
    assert!(callback(&result, BgTaskKind::Shell)
        .unwrap_err()
        .contains("owner-awaited asynchronous delivery"));
    tokio::task::yield_now().await;
    assert_eq!(route.0.load(Ordering::SeqCst), 0);
    let reminder = background_result_reminder(&result, BgTaskKind::Shell);
    route
        .deliver(
            crate::agent::async_tasks::delivery::terminal_delivery_id(&result.task_id, "terminal"),
            &reminder,
            peri_acp_types::session::MessageSource::ShellComplete,
        )
        .await
        .unwrap();
    assert_eq!(route.0.load(Ordering::SeqCst), 1);
}

#[derive(Clone, Default)]
struct CapturedLogs(std::sync::Arc<parking_lot::Mutex<Vec<u8>>>);

impl CapturedLogs {
    fn append(&self, buffer: &[u8]) {
        self.0.lock().extend_from_slice(buffer);
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock()).into_owned()
    }

    fn count_lines(&self, needle: &str) -> usize {
        self.text()
            .lines()
            .filter(|line| line.contains(needle))
            .count()
    }

    fn line_with(&self, needle: &str) -> String {
        self.text()
            .lines()
            .find(|line| line.contains(needle))
            .unwrap_or_else(|| {
                panic!(
                    "no log line contains {needle:?}; captured:\n{}",
                    self.text()
                )
            })
            .to_string()
    }
}

struct CapturedLogWriter(CapturedLogs);

impl std::io::Write for CapturedLogWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0.append(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for CapturedLogs {
    type Writer = CapturedLogWriter;

    fn make_writer(&'writer self) -> Self::Writer {
        CapturedLogWriter(self.clone())
    }
}

/// 只放行生产默认级别（`info` 及以上），把事件写进内存缓冲。
///
/// `set_default` 只装线程局部订阅，不会重建 tracing 的全局 callsite 兴趣缓存：
/// 并行测试若先在无订阅者的线程上命中同一 callsite，该 callsite 会被缓存为
/// `never`，本测试要断言的 `warn!`/`info!` 会被静默丢弃（重试路径断言就会超时）。
/// 因此安装订阅后显式重建兴趣缓存，让当前订阅者对已注册 callsite 生效。
fn capture_logs(logs: &CapturedLogs) -> tracing::subscriber::DefaultGuard {
    let guard = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::INFO)
            .finish(),
    );
    tracing::callsite::rebuild_interest_cache();
    guard
}

#[test]
fn terminal_publication_failure_reason_is_visible_at_default_log_level() {
    let logs = CapturedLogs::default();
    let _capture = capture_logs(&logs);
    let callback = failed_terminal_callback();
    let result = shell_result("unbound-shell");

    assert_eq!(
        callback(&result, BgTaskKind::Shell).unwrap_err(),
        "current terminal delivery unavailable"
    );

    let failure_line = logs.line_with("terminal publication failed");
    assert!(
        failure_line.contains("task_id=unbound-shell"),
        "captured:\n{}",
        logs.text()
    );
    assert!(
        failure_line.contains("delivery_id="),
        "captured:\n{}",
        logs.text()
    );
    assert!(
        failure_line.contains("current terminal delivery unavailable"),
        "captured:\n{}",
        logs.text()
    );
}

#[test]
fn terminal_publication_retry_keeps_reason_visible_without_log_flooding() {
    let logs = CapturedLogs::default();
    let _capture = capture_logs(&logs);
    let callback = failed_terminal_callback();
    let result = shell_result("unbound-shell");

    assert!(callback(&result, BgTaskKind::Shell).is_err());
    assert_eq!(logs.count_lines("terminal publication failed"), 1);

    assert!(callback(&result, BgTaskKind::Shell).is_err());

    let retry_line = logs.line_with("retrying original terminal publication");
    assert!(
        retry_line.contains("task_id=unbound-shell"),
        "captured:\n{}",
        logs.text()
    );
    assert!(
        retry_line.contains("delivery_id="),
        "captured:\n{}",
        logs.text()
    );
    assert!(
        retry_line.contains("current terminal delivery unavailable"),
        "captured:\n{}",
        logs.text()
    );

    assert_eq!(
        logs.count_lines("retrying original terminal publication"),
        1
    );
    assert_eq!(logs.count_lines("terminal publication failed"), 2);
}
