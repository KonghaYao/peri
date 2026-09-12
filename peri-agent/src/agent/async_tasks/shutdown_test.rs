use super::*;
use peri_acp_types::tasks::{TaskManager as TaskManagerPort, TaskShutdownReport};
use std::sync::Arc;

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

#[cfg(unix)]
#[tokio::test]
async fn test_shutdown_joins_cancelled_background_shell() {
    let manager = TaskManager::new();
    let cwd = tempfile::tempdir().unwrap();
    let (events, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    manager.set_event_sender(events, "session".into());
    let shell = manager
        .spawn_shell(
            "sleep 60".into(),
            cwd.path().to_str().unwrap().into(),
            None,
            None,
        )
        .unwrap();
    assert!(shell.pid.is_some());
    assert!(matches!(
        receiver.recv().await,
        Some(BgRegistryEvent::Started { .. })
    ));
    assert_eq!(manager.shutdown().await, TaskShutdownReport::Complete);
    assert!(manager
        .spawn_shell(
            "true".into(),
            cwd.path().to_str().unwrap().into(),
            None,
            None
        )
        .is_err());
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

#[cfg(unix)]
#[tokio::test]
async fn test_shutdown_immediately_after_shell_spawn_keeps_cleanup_owned() {
    let manager = TaskManager::new();
    let cwd = tempfile::tempdir().unwrap();
    manager
        .spawn_shell(
            "sleep 60".into(),
            cwd.path().to_str().unwrap().into(),
            None,
            None,
        )
        .unwrap();
    assert_eq!(manager.shutdown().await, TaskShutdownReport::Complete);
}

#[cfg(unix)]
#[tokio::test]
async fn test_timed_out_shell_can_close_cleanly() {
    let manager = TaskManager::new();
    let cwd = tempfile::tempdir().unwrap();
    let (complete, completed) = tokio::sync::oneshot::channel();
    let complete = std::sync::Mutex::new(Some(complete));
    manager
        .spawn_shell(
            "sleep 60".into(),
            cwd.path().to_str().unwrap().into(),
            Some(20),
            Some(Arc::new(move |result, _| {
                assert!(result.timed_out);
                if let Some(tx) = complete.lock().unwrap().take() {
                    let _ = tx.send(());
                }
            })),
        )
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), completed)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(manager.shutdown().await, TaskShutdownReport::Complete);
}

#[cfg(unix)]
#[tokio::test]
async fn test_rejected_promoted_task_settles_registered_process_after_cleanup() {
    let manager = Arc::new(TaskManager::new());
    let mut execution = ShellExecutionGuard::new(Some(manager.begin_external_execution().unwrap()));
    let mut command = shell_command("sleep 60", &[]);
    command.process_group(0).kill_on_drop(true);
    execution.prepare(&mut command).unwrap();
    let mut child = command.spawn().unwrap();
    execution.attach(&child).unwrap();
    manager
        .register(BgTaskRegistration {
            task_id: "promoted".into(),
            kind: BgTaskKind::Shell,
            summary: "shell".into(),
            pid: child.id(),
            kill: None,
        })
        .unwrap();
    execution.track_registration(manager.clone(), "promoted".into());
    let mut shutdown = manager.shutdown();
    assert!(futures::poll!(&mut shutdown).is_pending());
    assert!(manager
        .spawn_owned(Box::pin(async move {
            let _ = child.wait().await;
            execution.confirm_stopped();
        }))
        .is_err());
    assert_eq!(shutdown.await, TaskShutdownReport::Complete);
}
