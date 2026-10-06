use super::*;
use peri_acp_types::tasks::{TaskManager as TaskManagerPort, TaskShutdownReport};

#[derive(Clone, Default)]
struct LogBuffer(std::sync::Arc<parking_lot::Mutex<Vec<u8>>>);

impl std::io::Write for LogBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test(flavor = "current_thread")]
async fn async_tasks_owner_drop_and_cancel_join_panic_are_logged() {
    use crate::agent::async_tasks::{BackgroundTask, BackgroundTaskStatus, BgCancelHandle};
    let logs = LogBuffer::default();
    let writer = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .without_time()
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let _capture = tracing::subscriber::set_default(subscriber);
    let manager = TaskManager::new();
    drop(manager.begin_external_execution("test-owner").unwrap());
    assert!(!manager.is_execution_idle());
    manager.resolve_external_execution_evidence("test-owner");

    // 直接注册未捕获 panic 的 handle，验证取消等待层，而非后台包装层。
    let handle = tokio::spawn(async { panic!("取消等待测试 panic") });
    manager
        .register_with_kind(BackgroundTask {
            id: "cancel-panic-task".into(),
            agent_name: "fixture".into(),
            prompt_summary: "task".into(),
            status: BackgroundTaskStatus::Running,
            started_at: peri_time::monotonic_now(),
            chrono_started_at: peri_time::now_wall().into(),
            kind: BgTaskKind::Agent,
            cancel_handle: BgCancelHandle::Abort(handle),
            cancel_token: None,
            pid: None,
            output_preview: None,
            agent_inbox: None,
            initiator_session_id: None,
            owner_session_id: None,
            owner_identity: None,
        })
        .unwrap();
    manager.cancel("cancel-panic-task").unwrap();
    assert_eq!(manager.shutdown().await, TaskShutdownReport::Complete);
    assert!(manager.is_execution_idle());
    let text = String::from_utf8(logs.0.lock().clone()).unwrap();
    for (message, field) in [
        (
            "external execution scope still uncertain after owner drop",
            "scope=test-owner id=1",
        ),
        (
            "bg task cancel: execution join failed",
            "task_id=cancel-panic-task is_panic=true",
        ),
    ] {
        let line = text
            .lines()
            .find(|line| line.contains(message))
            .expect(&text);
        assert!(line.contains("WARN"), "{line}");
        assert!(line.contains(field), "{line}");
        std::io::Write::write_all(&mut std::io::stdout(), format!("{line}\n").as_bytes()).unwrap();
    }
}

#[derive(Default)]
struct RecordingShellExecutor {
    request: std::sync::Mutex<Option<(String, String, Option<u64>)>>,
}

impl ShellExecutor for RecordingShellExecutor {
    fn cancel_callback(
        &self,
        _pid: u32,
        _registry: std::sync::Arc<BackgroundTaskRegistry>,
    ) -> Option<Box<dyn FnOnce() + Send + Sync>> {
        None
    }

    fn spawn(
        &self,
        _registry: std::sync::Arc<BackgroundTaskRegistry>,
        mut ownership: Box<dyn peri_acp_types::tasks::ExternalExecutionGuard>,
        command: String,
        cwd: String,
        timeout_ms: Option<u64>,
        _on_bg_complete: Option<peri_acp_types::tasks::OnBgCompleteFn>,
    ) -> Result<BgShellHandle, Box<dyn std::error::Error + Send + Sync>> {
        *self.request.lock().unwrap() = Some((command, cwd, timeout_ms));
        ownership.confirm_stopped();
        Err("execution environment rejected request".into())
    }
}

#[tokio::test]
async fn injected_shell_environment_receives_request_without_local_execution() {
    let executor = std::sync::Arc::new(RecordingShellExecutor::default());
    let manager = TaskManager::with_shell_executor(executor.clone());
    let error = manager
        .spawn_shell(
            "remote-command".into(),
            "remote-workspace".into(),
            Some(37),
            None,
        )
        .unwrap_err();
    assert_eq!(error.to_string(), "execution environment rejected request");
    assert_eq!(
        executor.request.lock().unwrap().as_ref(),
        Some(&("remote-command".into(), "remote-workspace".into(), Some(37)))
    );
    assert_eq!(manager.shutdown().await, TaskShutdownReport::Complete);
}

#[tokio::test]
async fn missing_shell_environment_is_rejected_without_admitting_work() {
    let manager = TaskManager::new();
    let error = manager
        .spawn_shell("true".into(), "unused".into(), None, None)
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("execution environment is not configured"));
    assert_eq!(manager.active_count(), 0);
    assert_eq!(manager.shutdown().await, TaskShutdownReport::Complete);
}

#[tokio::test]
async fn test_shutdown_signals_owned_work_and_waits_for_its_cleanup() {
    let manager = TaskManager::new();
    let token = manager.execution_cancel_token().unwrap();
    let other = manager.execution_cancel_token().unwrap();
    other.cancel();
    assert!(!token.is_cancelled());
    let (cleanup_started, started) = tokio::sync::oneshot::channel();
    let (finish_cleanup, finished) = tokio::sync::oneshot::channel();
    manager
        .spawn_owned(Box::pin(async move {
            token.cancelled().await;
            cleanup_started.send(()).unwrap();
            finished.await.unwrap();
        }))
        .unwrap();
    let mut shutdown = manager.shutdown();
    assert!(futures::poll!(&mut shutdown).is_pending());
    started.await.unwrap();
    assert!(futures::poll!(&mut shutdown).is_pending());
    finish_cleanup.send(()).unwrap();
    assert_eq!(shutdown.await, TaskShutdownReport::Complete);
    assert!(manager.execution_cancel_token().unwrap().is_cancelled());
}

#[tokio::test]
async fn test_shutdown_waits_for_owned_completion_and_closes_admission() {
    let manager = TaskManager::new();
    let (release, waiting) = tokio::sync::oneshot::channel();
    manager
        .spawn_owned(Box::pin(async move {
            let _ = waiting.await;
        }))
        .unwrap();
    let mut closing = manager.shutdown();
    assert!(futures::poll!(&mut closing).is_pending());
    assert!(manager.spawn_owned(Box::pin(async {})).is_err());
    release.send(()).unwrap();
    assert_eq!(closing.await, TaskShutdownReport::Complete);
}

#[tokio::test]
async fn test_shutdown_cannot_report_clean_after_abandoned_external_execution() {
    let manager = TaskManager::new();
    let owner = manager.begin_external_execution("workspace").unwrap();
    drop(owner);
    assert_eq!(manager.shutdown().await, TaskShutdownReport::Incomplete);
    assert_eq!(manager.shutdown().await, TaskShutdownReport::Incomplete);
}

// [回归测试] 取消/超时的 MCP 调用只污染它自己的执行 scope；按 scope 的对账证据
// 可以恢复，其他 owner 的未结清证据不得被顺带清除（历史故障：单一 uncertain
// 布尔永久置位，会话无法 close / fork 永久被拒）。
#[tokio::test]
async fn test_external_uncertainty_clears_only_with_scope_evidence() {
    let manager = TaskManager::new();
    drop(TaskManagerPort::begin_external_execution(&manager, "workspace").unwrap());
    drop(TaskManagerPort::begin_external_execution(&manager, "web").unwrap());
    assert!(!TaskManagerPort::is_execution_idle(&manager));
    assert_eq!(
        TaskManagerPort::resolve_external_execution_evidence(&manager, "workspace"),
        1
    );
    assert!(
        !TaskManagerPort::is_execution_idle(&manager),
        "非 workspace 的证据不能清除其他 owner 的不确定"
    );
    assert_eq!(
        TaskManagerPort::resolve_external_execution_evidence(&manager, "web"),
        1
    );
    assert!(TaskManagerPort::is_execution_idle(&manager));
}

// 证据清除后，关闭必须能重新走到 Complete（不能永久 Incomplete）。
#[tokio::test]
async fn test_evidenced_reconciliation_restores_clean_shutdown() {
    let manager = TaskManager::new();
    drop(TaskManagerPort::begin_external_execution(&manager, "workspace").unwrap());
    assert_eq!(manager.shutdown().await, TaskShutdownReport::Incomplete);
    assert_eq!(
        TaskManagerPort::resolve_external_execution_evidence(&manager, "workspace"),
        1
    );
    assert_eq!(manager.shutdown().await, TaskShutdownReport::Complete);
}

#[tokio::test]
async fn test_shutdown_accepts_confirmed_external_cleanup() {
    let manager = TaskManager::new();
    let mut owner = manager.begin_external_execution("workspace").unwrap();
    owner.confirm_stopped();
    drop(owner);
    assert_eq!(manager.shutdown().await, TaskShutdownReport::Complete);
}

#[tokio::test]
async fn test_shutdown_does_not_treat_kill_request_as_external_completion() {
    let manager = TaskManager::new();
    TaskManagerPort::register(
        &manager,
        BgTaskRegistration {
            task_id: "workflow".into(),
            kind: BgTaskKind::Workflow,
            summary: "workflow".into(),
            pid: None,
            kill: Some(Box::new(|| {})),
        },
    )
    .unwrap();
    assert_eq!(manager.shutdown().await, TaskShutdownReport::Incomplete);
}
