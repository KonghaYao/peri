use std::{sync::Arc, time::Duration};

use peri_acp_types::tasks::TaskShutdownReport;
use peri_agent::agent::async_tasks::{BackgroundTaskRegistry, TaskManager};
use rmcp::model::{Task, TaskPayload, TaskStatus};

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

#[tokio::test]
async fn task_owner_isolation_covers_get_cancel_and_membership() {
    let dir = tempfile::tempdir().expect("workspace");
    let owner = ShellTasks::new();
    let task = owner
        .spawn_owned(
            "sleep 30".into(),
            dir.path().to_string_lossy().into_owned(),
            None,
            Some("session-a"),
        )
        .await
        .expect("start task");
    assert!(owner.belongs_to(&task.task_id, "session-a"));
    assert!(!owner.belongs_to(&task.task_id, "session-b"));
    owner
        .get_owned(&task.task_id, "session-a")
        .expect("owner can read its task");
    assert!(owner.get_owned(&task.task_id, "session-b").is_err());
    assert!(owner.cancel_owned(&task.task_id, "session-b").is_err());
    assert!(
        owner.get(&task.task_id).is_ok(),
        "generic reads stay global"
    );
    owner
        .cancel_owned(&task.task_id, "session-a")
        .expect("owner cancels");
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
    assert_eq!(owner.shutdown().await, TaskShutdownReport::Complete);
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
async fn cancellation_targets_known_task_id() {
    let dir = tempfile::tempdir().expect("workspace");
    let owner = ShellTasks::new();
    let task = owner
        .spawn_owned(
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

/// 占满 Shell 并发位（真实进程），返回占位任务；调用方负责 `settle_shell_capacity`。
async fn fill_shell_capacity(owner: &ShellTasks, cwd: &str) -> Vec<Task> {
    let mut tasks = Vec::new();
    for _ in 0..BackgroundTaskRegistry::SHELL_LIMIT {
        tasks.push(
            owner
                .spawn_owned("sleep 30".into(), cwd.to_owned(), None, Some("capacity"))
                .await
                .expect("capacity shell starts"),
        );
    }
    tasks
}

/// 取消占位任务并等待终态与进程清理。
async fn settle_shell_capacity(owner: &ShellTasks, tasks: &[Task]) {
    for task in tasks {
        owner.cancel(&task.task_id).expect("cancel capacity shell");
    }
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if tasks.iter().all(|task| {
                owner
                    .get(&task.task_id)
                    .expect("capacity task")
                    .task
                    .status()
                    .is_terminal()
            }) {
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
/// 故障与真实启动失败；修复后模型面和此处 tracing 只投影分类，不重复记录自由文本。
#[tokio::test]
async fn panicked_launch_worker_reports_classification_without_raw_payload() {
    let mut owner = ShellTasks::new();
    owner.manager = Arc::new(TaskManager::with_shell_executor(Arc::new(
        FailedCreationExecutor { panics: true },
    )));
    let error = owner
        .spawn_owned("fixture-command".into(), "/tmp".into(), None, Some("child"))
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
/// 稳定兜底文案；tracing 同样不记录原始文本。
#[tokio::test]
async fn unclassified_launch_failure_does_not_echo_underlying_error_text() {
    let mut owner = ShellTasks::new();
    owner.manager = Arc::new(TaskManager::with_shell_executor(Arc::new(
        FailedCreationExecutor { panics: false },
    )));
    let error = owner
        .spawn_owned("fixture-command".into(), "/tmp".into(), None, Some("child"))
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
    let capacity = fill_shell_capacity(&owner, &cwd).await;
    let error = owner
        .spawn_owned("sleep 30".into(), cwd.clone(), None, Some("capacity"))
        .await
        .expect_err("over-limit launch must be rejected");
    let limit = BackgroundTaskRegistry::SHELL_LIMIT;
    assert!(
        error.message.contains("concurrency limit reached")
            && error.message.contains(&format!("shell {limit}/{limit}")),
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
        20,
        "shell 并发上限按容量决策固定为 20"
    );
    settle_shell_capacity(&owner, &capacity).await;
}

/// [行为边界] spawn 失败与注册失败叠加时底层类型已丢失（执行环境把 io 错误与
/// 注册错误拼成单条 String），只能走兜底文案；该文本同样不进入模型面。
#[tokio::test]
async fn composite_spawn_and_registration_failure_stays_unclassified() {
    let directory = tempfile::tempdir().unwrap();
    let owner = ShellTasks::new();
    let cwd = directory.path().to_string_lossy().into_owned();
    let capacity = fill_shell_capacity(&owner, &cwd).await;
    let error = owner
        .spawn_owned(
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
    settle_shell_capacity(&owner, &capacity).await;
}
