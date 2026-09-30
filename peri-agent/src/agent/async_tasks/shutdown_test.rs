use super::*;
use peri_acp_types::tasks::{TaskManager as TaskManagerPort, TaskShutdownReport};

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
    let owner = manager.begin_external_execution().unwrap();
    drop(owner);
    assert_eq!(manager.shutdown().await, TaskShutdownReport::Incomplete);
    assert_eq!(manager.shutdown().await, TaskShutdownReport::Incomplete);
}

#[tokio::test]
async fn test_shutdown_accepts_confirmed_external_cleanup() {
    let manager = TaskManager::new();
    let mut owner = manager.begin_external_execution().unwrap();
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
