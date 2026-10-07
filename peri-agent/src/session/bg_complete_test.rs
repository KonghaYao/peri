use super::*;
use crate::agent::async_tasks::delivery::SessionTerminalDelivery;
use peri_acp_types::session::MessageQueue;

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

async fn wait_for_ack(callback: &OnBgCompleteFn, result: &BackgroundTaskResult) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while callback(result, BgTaskKind::Shell).is_err() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

struct FailedDelivery;
impl peri_acp_types::tasks::TaskTerminalDelivery for FailedDelivery {
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

#[tokio::test]
async fn current_terminal_delivery_is_accepted_once_without_a_ledger() {
    let queue = MessageQueue::new();
    let callback = task_bg_complete_callback(SessionTerminalDelivery::for_queue(queue.clone()));
    let result = shell_result("shell-current");
    wait_for_ack(&callback, &result).await;
    callback(&result, BgTaskKind::Shell).unwrap();
    assert_eq!(queue.drain_batch(64).len(), 1);
}

#[tokio::test]
async fn same_terminal_identity_cannot_acknowledge_changed_payload() {
    let queue = MessageQueue::new();
    let callback = task_bg_complete_callback(SessionTerminalDelivery::for_queue(queue.clone()));
    let mut result = shell_result("shell-conflict");
    wait_for_ack(&callback, &result).await;
    result.output = "changed".into();
    assert!(callback(&result, BgTaskKind::Shell)
        .unwrap_err()
        .contains("conflicting"));
    assert_eq!(queue.drain_batch(64).len(), 1);
}

// ─── Inline log capture ──────────────────────────────────────────────────────
//
// 仓库没有 tracing 捕获测试工具，这里内联最小实现：自定义 `MakeWriter` 把
// 默认过滤器（`INFO` 起）下的事件写进内存缓冲，供断言检查。
// `#[tokio::test]` 默认 current_thread runtime，被 spawn 的投递任务与测试在
// 同一线程上被轮询，因此线程局部 subscriber 能捕获异步投递结果日志。

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

async fn wait_for_log_line(logs: &CapturedLogs, needle: &str) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !logs.text().contains(needle) {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "log line {needle:?} never appeared; captured:\n{}",
            logs.text()
        )
    });
}

async fn wait_for_log_lines(logs: &CapturedLogs, needle: &str, expected: usize) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while logs.count_lines(needle) < expected {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "expected {expected} log lines containing {needle:?}; captured:\n{}",
            logs.text()
        )
    });
}

#[tokio::test]
async fn terminal_publication_failure_reason_is_visible_at_default_log_level() {
    let logs = CapturedLogs::default();
    let _capture = capture_logs(&logs);
    let callback = failed_terminal_callback();
    let result = shell_result("unbound-shell");

    // 同步返回值仍是 Pending 契约；真实原因只能由异步结果回写路径记录。
    assert!(callback(&result, BgTaskKind::Shell).is_err());
    wait_for_log_line(&logs, "terminal publication failed").await;

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

#[tokio::test]
async fn terminal_publication_retry_keeps_reason_visible_without_log_flooding() {
    let logs = CapturedLogs::default();
    let _capture = capture_logs(&logs);
    let callback = failed_terminal_callback();
    let result = shell_result("unbound-shell");

    assert!(callback(&result, BgTaskKind::Shell).is_err());
    wait_for_log_line(&logs, "terminal publication failed").await;
    assert_eq!(logs.count_lines("terminal publication failed"), 1);

    // 第二次调用进入 Failed 重试路径；若异步状态尚未回写会落到 Pending 分支，
    // 重试调用即可，直到重试日志出现。
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            assert!(callback(&result, BgTaskKind::Shell).is_err());
            // 该 callsite 与并行用例共享，全局兴趣缓存可能把它置为 never：重建后再重试，
            // 避免把“记录被抑制”误判成“重试路径没走到”。
            tracing::callsite::rebuild_interest_cache();
            if logs.count_lines("retrying original terminal publication") > 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("retry path never taken; captured:\n{}", logs.text()));

    wait_for_log_lines(&logs, "terminal publication failed", 2).await;

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

    // 每次重试只新增 1 行 info（重试决策）+ 1 行 warn（本次失败原因）。
    assert_eq!(
        logs.count_lines("retrying original terminal publication"),
        1
    );
    assert_eq!(logs.count_lines("terminal publication failed"), 2);
}
