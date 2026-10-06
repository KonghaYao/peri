use super::*;
use crate::agent::async_tasks::durable_task_terminal_delivery;
use crate::session::test_resources::{mock::work::bind_fixture_task, TestSession};
use peri_acp_types::session::MessageQueue;
use peri_acp_types::session_resources::work::WorkQuery;

fn shell_result(task_id: &str) -> BackgroundTaskResult {
    BackgroundTaskResult {
        task_id: task_id.into(),
        agent_name: "Bash".into(),
        prompt_summary: "durable shell".into(),
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
        loop {
            if callback(result, BgTaskKind::Shell).is_ok() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("durable terminal publication never acknowledged");
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
fn capture_logs(logs: &CapturedLogs) -> tracing::subscriber::DefaultGuard {
    tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::INFO)
            .finish(),
    )
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

/// 未绑定 task binding 的投递：必然在 `delivery.rs` 的 binding 查找处失败。
fn unbound_terminal_callback(bound: &TestSession) -> OnBgCompleteFn {
    durable_bg_complete_callback(durable_task_terminal_delivery(
        bound.resources(),
        bound.thread_id(),
        1,
        MessageQueue::new(),
    ))
}

#[tokio::test]
async fn owner_ack_requires_confirmed_reliable_inbox_not_queue_or_transcript() {
    let bound = TestSession::open().await;
    bind_fixture_task(bound.resources(), &bound.thread_id(), 1, "shell-durable").await;
    let queue = MessageQueue::new();
    let delivery =
        durable_task_terminal_delivery(bound.resources(), bound.thread_id(), 1, queue.clone());
    let callback = durable_bg_complete_callback(delivery);
    let result = shell_result("shell-durable");
    assert!(callback(&result, BgTaskKind::Shell).is_err());
    wait_for_ack(&callback, &result).await;
    assert!(callback(&result, BgTaskKind::Shell).is_ok());
    let snapshot = bound
        .resources
        .load_session_work(&WorkQuery {
            session_id: bound.thread_id(),
            limit: 1,
        })
        .await
        .unwrap();
    assert_eq!(snapshot.state.deliveries.len(), 1);
    assert_eq!(snapshot.state.task_bindings.len(), 1);
    assert!(bound
        .resources
        .load_session_history(&bound.thread_id())
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn missing_immutable_task_binding_never_acknowledges_or_falls_back_to_queue() {
    let bound = TestSession::open().await;
    let queue = MessageQueue::new();
    let callback = durable_bg_complete_callback(durable_task_terminal_delivery(
        bound.resources(),
        bound.thread_id(),
        1,
        queue.clone(),
    ));
    let result = shell_result("unbound-shell");
    for _ in 0..10 {
        assert!(callback(&result, BgTaskKind::Shell).is_err());
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(queue.is_empty());
    let snapshot = bound
        .resources
        .load_session_work(&WorkQuery {
            session_id: bound.thread_id(),
            limit: 1,
        })
        .await
        .unwrap();
    assert!(snapshot.state.deliveries.is_empty());
    assert!(bound
        .resources
        .load_session_history(&bound.thread_id())
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn same_terminal_identity_cannot_acknowledge_changed_payload() {
    let bound = TestSession::open().await;
    bind_fixture_task(bound.resources(), &bound.thread_id(), 1, "shell-conflict").await;
    let callback = durable_bg_complete_callback(durable_task_terminal_delivery(
        bound.resources(),
        bound.thread_id(),
        1,
        MessageQueue::new(),
    ));
    let mut result = shell_result("shell-conflict");
    assert!(callback(&result, BgTaskKind::Shell).is_err());
    wait_for_ack(&callback, &result).await;
    result.output = "different terminal output".into();
    assert!(callback(&result, BgTaskKind::Shell)
        .unwrap_err()
        .contains("conflicting"));
    let snapshot = bound
        .resources
        .load_session_work(&WorkQuery {
            session_id: bound.thread_id(),
            limit: 1,
        })
        .await
        .unwrap();
    assert_eq!(snapshot.state.deliveries.len(), 1);
}

#[tokio::test]
async fn terminal_publication_failure_reason_is_visible_at_default_log_level() {
    let bound = TestSession::open().await;
    let logs = CapturedLogs::default();
    let _capture = capture_logs(&logs);
    let callback = unbound_terminal_callback(&bound);
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
        failure_line.contains("terminal publication has no durable invocation/task binding"),
        "captured:\n{}",
        logs.text()
    );
}

#[tokio::test]
async fn terminal_publication_retry_keeps_reason_visible_without_log_flooding() {
    let bound = TestSession::open().await;
    let logs = CapturedLogs::default();
    let _capture = capture_logs(&logs);
    let callback = unbound_terminal_callback(&bound);
    let result = shell_result("unbound-shell");

    assert!(callback(&result, BgTaskKind::Shell).is_err());
    wait_for_log_line(&logs, "terminal publication failed").await;
    assert_eq!(logs.count_lines("terminal publication failed"), 1);

    // 第二次调用进入 Failed 重试路径；若异步状态尚未回写会落到 Pending 分支，
    // 重试调用即可，直到重试日志出现。
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            assert!(callback(&result, BgTaskKind::Shell).is_err());
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
        retry_line.contains("terminal publication has no durable invocation/task binding"),
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
