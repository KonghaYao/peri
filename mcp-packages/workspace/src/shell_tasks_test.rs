use std::{sync::Arc, time::Duration};

use peri_acp_types::tasks::TaskShutdownReport;
use peri_agent::agent::async_tasks::{BackgroundTaskRegistry, TaskManager};
use rmcp::model::{TaskPayload, TaskStatus};

use super::ShellTasks;

struct FailedCreationExecutor {
    panics: bool,
}

impl peri_agent::agent::async_tasks::ShellExecutor for FailedCreationExecutor {
    fn cancel_callback(
        &self,
        _: u32,
        _: Arc<peri_agent::agent::async_tasks::BackgroundTaskRegistry>,
    ) -> Option<Box<dyn FnOnce() + Send + Sync>> {
        None
    }

    fn spawn(
        &self,
        _: Arc<peri_agent::agent::async_tasks::BackgroundTaskRegistry>,
        _: Box<dyn peri_acp_types::tasks::ExternalExecutionGuard>,
        _: String,
        _: String,
        _: Option<u64>,
        _: Option<peri_acp_types::tasks::OnBgCompleteFn>,
    ) -> Result<peri_acp_types::tasks::BgShellHandle, Box<dyn std::error::Error + Send + Sync>>
    {
        if self.panics {
            panic!("fixture: accepted shell creation panicked before registration");
        }
        Err("fixture: accepted shell creation failed before registration".into())
    }
}

async fn assert_failed_creation_keeps_scope_unconfirmed(panics: bool) {
    let mut owner = ShellTasks::new();
    owner.manager = Arc::new(
        peri_agent::agent::async_tasks::TaskManager::with_shell_executor(Arc::new(
            FailedCreationExecutor { panics },
        )),
    );
    assert!(owner
        .spawn_scoped("command".into(), "/tmp".into(), None, Some("child"))
        .await
        .is_err());
    let cursor = owner.close_scope("child", 0).await.unwrap();
    let snapshot = owner.snapshot("child");
    assert!(snapshot.closing);
    assert_eq!(snapshot.epoch, 0);
    assert!(snapshot.cursor >= cursor);
    assert!(snapshot.tasks.is_empty());
    assert!(!snapshot.resources_settled);
    assert!(owner
        .open_scope("child", 0)
        .err()
        .unwrap()
        .message
        .contains("Incomplete"));
    assert!(owner.snapshot("other-child").resources_settled);
    assert!(!owner.snapshot("other-child").closing);
}

#[tokio::test]
async fn failed_creation_without_task_row_cannot_report_scope_settled() {
    assert_failed_creation_keeps_scope_unconfirmed(false).await;
}

#[tokio::test]
async fn panicked_creation_without_task_row_cannot_report_scope_settled() {
    assert_failed_creation_keeps_scope_unconfirmed(true).await;
}

#[tokio::test]
async fn scoped_resource_settlement_waits_for_shell_cleanup_not_cancel_acceptance() {
    let owner = ShellTasks::new();
    let directory = tempfile::tempdir().unwrap();
    let task = owner
        .spawn_scoped(
            "sleep 30".into(),
            directory.path().to_string_lossy().into_owned(),
            None,
            Some("closing-child"),
        )
        .await
        .unwrap();
    let other = owner
        .spawn_scoped(
            "sleep 30".into(),
            directory.path().to_string_lossy().into_owned(),
            None,
            Some("independent-child"),
        )
        .await
        .unwrap();
    let cursor = owner.close_scope("closing-child", 0).await.unwrap();
    assert!(!owner.snapshot("closing-child").resources_settled);
    owner.cancel_scoped(&task.task_id, "closing-child").unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = owner.snapshot("closing-child");
            assert!(snapshot.closing);
            assert_eq!(snapshot.epoch, 0);
            assert!(snapshot.cursor >= cursor);
            if snapshot.resources_settled {
                assert!(snapshot
                    .tasks
                    .iter()
                    .all(|task| task.task.status().is_terminal()));
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("shell cleanup must settle before close succeeds");
    assert!(!owner.snapshot("independent-child").resources_settled);
    assert!(!owner.snapshot("independent-child").closing);
    owner
        .cancel_scoped(&other.task_id, "independent-child")
        .unwrap();
    owner.manager().shutdown().await;
}
#[test]
fn session_admissions_do_not_require_an_execution_owner() {
    let owner = ShellTasks::new();
    let first = owner.admit("session-a").expect("first request");
    let second = owner.admit("session-a").expect("second request");
    assert_eq!(owner.state.lock().inflight["session-a"], 2);
    drop(first);
    drop(second);
    assert!(!owner.state.lock().inflight.contains_key("session-a"));
}

#[tokio::test]
async fn close_gate_waits_for_admitted_creation_and_rejects_later_work() {
    let owner = ShellTasks::new();
    let admitted = owner.admit("session-a").expect("admission");
    let closing_owner = owner.clone();
    let close = tokio::spawn(async move { closing_owner.close_scope("session-a", 0).await });
    tokio::time::timeout(Duration::from_secs(1), async {
        while !owner.state.lock().closing.contains("session-a") {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("close gate activated");
    assert!(owner.admit("session-a").is_err());
    assert!(!close.is_finished());
    drop(admitted);
    let cursor = tokio::time::timeout(Duration::from_secs(1), close)
        .await
        .expect("close barrier")
        .expect("join close")
        .expect("close accepted");
    assert_eq!(cursor, owner.snapshot("session-a").cursor);
    assert!(owner.admit("session-a").is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn delayed_close_cannot_report_success_after_scope_reopens() {
    let owner = ShellTasks::new();
    let admitted = owner.admit("session-a").expect("admission");
    let closing_owner = owner.clone();
    let close = tokio::spawn(async move { closing_owner.close_scope("session-a", 0).await });
    tokio::time::timeout(Duration::from_secs(1), async {
        while !owner.snapshot("session-a").closing {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("close gate activated");
    drop(admitted);
    let reopened = owner
        .open_scope("session-a", 0)
        .expect("settled scope reopens");
    assert_eq!(reopened.epoch, 1);
    assert!(
        close.await.expect("join close").is_err(),
        "old close is stale"
    );
    assert!(!owner.snapshot("session-a").closing);
}

#[tokio::test]
async fn cancelled_request_cannot_escape_scope_close_barrier() {
    let directory = tempfile::tempdir().unwrap();
    let gate = Arc::new(tokio::sync::Notify::new());
    let mut owner = ShellTasks::new();
    owner.spawn_gate = Some(Arc::clone(&gate));
    let launch_owner = owner.clone();
    let cwd = directory.path().to_string_lossy().into_owned();
    let caller = tokio::spawn(async move {
        launch_owner
            .spawn_scoped("sleep 30".into(), cwd, None, Some("session-a"))
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        while owner
            .state
            .lock()
            .inflight
            .get("session-a")
            .copied()
            .unwrap_or(0)
            == 0
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("owner admitted spawn");
    caller.abort();
    let closing_owner = owner.clone();
    let close = tokio::spawn(async move { closing_owner.close_scope("session-a", 0).await });
    tokio::task::yield_now().await;
    assert!(
        !close.is_finished(),
        "barrier must wait for detached registration"
    );
    gate.notify_one();
    tokio::time::timeout(Duration::from_secs(5), close)
        .await
        .expect("barrier")
        .expect("join")
        .expect("epoch");
    let snapshot = owner.snapshot("session-a");
    assert_eq!(
        snapshot.tasks.len(),
        1,
        "cancelled request must leave a discoverable task"
    );
    owner.cancel(&snapshot.tasks[0].task.task.task_id).unwrap();
    tokio::time::timeout(Duration::from_secs(5), owner.shutdown())
        .await
        .expect("cleanup");
}

#[tokio::test]
async fn shell_completion_is_queryable_after_client_like_owner_clone_is_dropped() {
    let dir = tempfile::tempdir().expect("workspace");
    let owner = ShellTasks::new();
    let second_connection = owner.clone();
    let task = owner
        .spawn(
            "printf 'workspace-task-ok'".into(),
            dir.path().to_string_lossy().into_owned(),
            None,
        )
        .await
        .expect("start shell task");
    drop(owner);

    let result = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let result = second_connection
                .get(&task.task_id)
                .expect("task remains owned");
            if result.task.status().is_terminal() {
                break result;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("shell must complete");
    assert_eq!(result.task.status(), TaskStatus::Completed);
    let TaskPayload::Completed { result } = result.task.payload else {
        panic!("terminal task must carry CallToolResult")
    };
    assert_eq!(result.get("isError").and_then(|v| v.as_bool()), Some(false));
    assert!(result.get("structuredContent").is_some());
    assert!(second_connection.get("unknown-task").is_err());
    assert_eq!(
        second_connection.shutdown().await,
        peri_acp_types::tasks::TaskShutdownReport::Complete
    );
}

#[tokio::test]
async fn cancellation_is_scoped_to_known_task_id() {
    let dir = tempfile::tempdir().expect("workspace");
    let owner = ShellTasks::new();
    let task = owner
        .spawn_scoped(
            "trap '' TERM; printf ready > ready; while :; do sleep 1; done".into(),
            dir.path().to_string_lossy().into_owned(),
            None,
            Some("session-a"),
        )
        .await
        .expect("start shell task");
    tokio::time::timeout(Duration::from_secs(2), async {
        while !dir.path().join("ready").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("shell trap ready");
    assert!(owner.cancel("not-this-task").is_err());
    owner.cancel(&task.task_id).expect("cancel task");
    assert_eq!(
        owner.get(&task.task_id).expect("task").task.status(),
        TaskStatus::Working
    );
    assert!(owner.snapshot("session-a").tasks[0]
        .terminal_transition_id
        .is_none());
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if owner.get(&task.task_id).expect("task").task.status() == TaskStatus::Cancelled {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("cancelled after process cleanup");
    assert!(owner.snapshot("session-a").tasks[0]
        .terminal_transition_id
        .is_some());
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), owner.shutdown())
            .await
            .expect("shutdown must await process cleanup"),
        peri_acp_types::tasks::TaskShutdownReport::Complete
    );
}

#[tokio::test]
async fn cancelling_after_completion_preserves_terminal_result() {
    let dir = tempfile::tempdir().expect("workspace");
    let owner = ShellTasks::new();
    let task = owner
        .spawn(
            "printf done".into(),
            dir.path().to_string_lossy().into_owned(),
            None,
        )
        .await
        .expect("start task");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if owner
                .get(&task.task_id)
                .expect("task")
                .task
                .status()
                .is_terminal()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("task deadline");
    owner
        .cancel(&task.task_id)
        .expect("terminal cancel is idempotent");
    assert_eq!(
        owner.get(&task.task_id).expect("task result").task.status(),
        TaskStatus::Completed
    );
    assert_eq!(
        owner.shutdown().await,
        peri_acp_types::tasks::TaskShutdownReport::Complete
    );
}

/// 占满 Shell 并发位（真实进程），用于制造注册表拒绝；调用方负责 `settle_shell_capacity`。
async fn fill_shell_capacity(owner: &ShellTasks, cwd: &str) {
    for _ in 0..BackgroundTaskRegistry::SHELL_LIMIT {
        owner
            .spawn_scoped("sleep 30".into(), cwd.to_owned(), None, Some("capacity"))
            .await
            .expect("capacity shell starts");
    }
}

/// 取消占位任务并等待终态，再等待进程清理。失败创建按既有语义让 scope 保持
/// 未确认结算，所以清理证据取任务终态与 `shutdown` 报告，而不是 scope settled。
async fn settle_shell_capacity(owner: &ShellTasks) {
    for task in owner.snapshot("capacity").tasks {
        owner
            .cancel(&task.task.task.task_id)
            .expect("cancel capacity shell");
    }
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if owner
                .snapshot("capacity")
                .tasks
                .iter()
                .all(|task| task.task.status().is_terminal())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("capacity shells must reach terminal state");
    let report = tokio::time::timeout(Duration::from_secs(10), owner.shutdown())
        .await
        .expect("shutdown must not hang");
    assert_eq!(report, TaskShutdownReport::Complete);
}

/// [回归测试] 启动 worker 崩溃与真实启动失败必须在模型面可区分。
///
/// 历史背景：两处 `map_err(|_| ...)` 曾把 `JoinError`（worker panic）与执行环境
/// 返回的 boxed error 折叠成同一条 `shell task launch failed`，模型无法区分内部
/// 故障与真实启动失败；折叠同时丢弃了原因。修复后模型面只投影分类，原始诊断
/// 走 tracing，panic payload 由进程级 hook 记录。
#[tokio::test]
async fn panicked_launch_worker_reports_classification_without_raw_payload() {
    let mut owner = ShellTasks::new();
    owner.manager = Arc::new(TaskManager::with_shell_executor(Arc::new(
        FailedCreationExecutor { panics: true },
    )));
    let error = owner
        .spawn_scoped("fixture-command".into(), "/tmp".into(), None, Some("child"))
        .await
        .expect_err("panicked worker must fail the launch");
    assert!(
        error.message.contains("panicked before registration"),
        "worker panic must be classified, got: {}",
        error.message
    );
    assert!(
        !error.message.contains("fixture:"),
        "model-facing text must not carry the panic payload: {}",
        error.message
    );
}

/// [回归测试] 不可分类的 boxed error 不得原样回显到模型面。
///
/// fixture 的 `String` 错误不可 downcast（std 私有 `StringError`），因此只能走
/// 稳定兜底文案；原始文本保留在 tracing。
#[tokio::test]
async fn unclassified_launch_failure_does_not_echo_underlying_error_text() {
    let mut owner = ShellTasks::new();
    owner.manager = Arc::new(TaskManager::with_shell_executor(Arc::new(
        FailedCreationExecutor { panics: false },
    )));
    let error = owner
        .spawn_scoped("fixture-command".into(), "/tmp".into(), None, Some("child"))
        .await
        .expect_err("failing executor must fail the launch");
    assert!(
        error.message.contains("could not be started"),
        "unclassified failure must use the stable fallback, got: {}",
        error.message
    );
    assert!(
        !error.message.contains("fixture"),
        "model-facing text must not echo the boxed error: {}",
        error.message
    );
}

/// [回归测试] `KindConcurrentLimit` 是正常背压：拒绝必须报告结构化上限事实，
/// 不改变上限，也不影响已注册任务。
#[tokio::test]
async fn concurrent_limit_rejection_is_reported_as_backpressure_not_launch_failure() {
    let directory = tempfile::tempdir().unwrap();
    let owner = ShellTasks::new();
    let cwd = directory.path().to_string_lossy().into_owned();
    fill_shell_capacity(&owner, &cwd).await;
    let error = owner
        .spawn_scoped("sleep 30".into(), cwd.clone(), None, Some("capacity"))
        .await
        .expect_err("over-limit launch must be rejected");
    assert!(
        error.message.contains("concurrency limit reached") && error.message.contains("5/5"),
        "limit rejection must keep kind/current/limit facts, got: {}",
        error.message
    );
    assert!(
        !error.message.contains("sleep 30"),
        "model-facing text must not carry the command: {}",
        error.message
    );
    assert_eq!(
        owner.manager().active_count(),
        BackgroundTaskRegistry::SHELL_LIMIT,
        "rejection must not change the registered task set"
    );
    assert_eq!(
        BackgroundTaskRegistry::SHELL_LIMIT,
        5,
        "shell 并发上限是背压契约，本项不改变它"
    );
    settle_shell_capacity(&owner).await;
    assert!(
        !owner.snapshot("capacity").resources_settled,
        "被拒绝的创建必须保持未确认结算，不能静默变为已结算"
    );
}

/// [行为边界] spawn 失败与注册失败叠加时底层类型已丢失（执行环境把 io 错误与
/// 注册错误拼成单条 String），只能走兜底文案；该文本同样不进入模型面。
#[tokio::test]
async fn composite_spawn_and_registration_failure_stays_unclassified() {
    let directory = tempfile::tempdir().unwrap();
    let owner = ShellTasks::new();
    let cwd = directory.path().to_string_lossy().into_owned();
    fill_shell_capacity(&owner, &cwd).await;
    let error = owner
        .spawn_scoped(
            "sleep 30".into(),
            directory
                .path()
                .join("missing-cwd")
                .to_string_lossy()
                .into_owned(),
            None,
            Some("capacity"),
        )
        .await
        .expect_err("missing cwd against a full registry must fail");
    assert!(
        error.message.contains("could not be started"),
        "composite failure must use the stable fallback, got: {}",
        error.message
    );
    assert!(
        !error.message.contains("Failed to spawn"),
        "composite underlying text must not reach the model: {}",
        error.message
    );
    settle_shell_capacity(&owner).await;
}
