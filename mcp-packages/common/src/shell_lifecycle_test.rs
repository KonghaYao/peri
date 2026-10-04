use super::*;
use peri_acp_types::tasks::{TaskManager as TaskManagerPort, TaskShutdownReport};
use peri_agent::agent::async_tasks::*;
#[cfg(unix)]
use std::sync::Arc;

#[cfg(unix)]
#[tokio::test]
async fn cancellation_preserves_term_handler_and_cleanup_evidence() {
    let manager = crate::create_local_task_manager();
    let fixture = tempfile::tempdir().unwrap();
    let shell = manager.spawn_shell(
        "trap 'printf handled > terminated; exit 0' TERM; printf ready > ready; while :; do sleep 1; done".into(),
        fixture.path().to_str().unwrap().into(), None, None,
    ).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !fixture.path().join("ready").exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    manager.cancel(&shell.task_id).unwrap();
    assert_eq!(manager.active_count(), 0);
    assert!(!manager.is_execution_idle());
    assert_eq!(manager.shutdown().await, TaskShutdownReport::Complete);
    assert_eq!(
        std::fs::read_to_string(fixture.path().join("terminated")).unwrap(),
        "handled"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn cancellation_escalation_stays_owned_until_process_and_pipes_stop() {
    let manager = crate::create_local_task_manager();
    let fixture = tempfile::tempdir().unwrap();
    let shell = manager
        .spawn_shell(
            "trap '' TERM; printf ready > ready; while :; do sleep 1; done".into(),
            fixture.path().to_str().unwrap().into(),
            None,
            None,
        )
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !fixture.path().join("ready").exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    manager.cancel(&shell.task_id).unwrap();
    assert!(!manager.is_execution_idle());
    assert_eq!(manager.shutdown().await, TaskShutdownReport::Complete);
    assert!(manager.is_execution_idle());
    assert!(std::fs::read_to_string(shell.stdout_log.unwrap()).is_ok());
    assert!(std::fs::read_to_string(shell.stderr_log.unwrap()).is_ok());
}

#[tokio::test]
async fn test_failed_shell_spawn_with_full_registry_returns_error() {
    let manager = crate::create_local_task_manager();
    for index in 0..BackgroundTaskRegistry::SHELL_LIMIT {
        manager
            .register(BgTaskRegistration {
                task_id: format!("occupied-{index}"),
                kind: BgTaskKind::Shell,
                summary: "capacity fixture".into(),
                pid: None,
                kill: Some(Box::new(|| {})),
            })
            .unwrap();
    }
    let fixture = tempfile::tempdir().unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let result = manager.spawn_shell(
        "echo never-started".into(),
        fixture
            .path()
            .join("missing-cwd")
            .to_string_lossy()
            .into_owned(),
        None,
        Some(std::sync::Arc::new(move |result, _| {
            tx.send(result.clone()).unwrap();
        })),
    );
    assert_eq!(manager.active_count(), BackgroundTaskRegistry::SHELL_LIMIT);
    for index in 0..BackgroundTaskRegistry::SHELL_LIMIT {
        let id = format!("occupied-{index}");
        manager.cancel(&id).unwrap();
        manager.confirm_external_execution_stopped(&id);
    }
    assert_eq!(manager.shutdown().await, TaskShutdownReport::Complete);
    assert!(
        rx.try_recv().is_err(),
        "unregistered failure has no callback"
    );
    let error = result.expect_err("spawn failure must be returned synchronously");
    assert!(error.to_string().contains("Failed to spawn"));
}

#[cfg(unix)]
#[tokio::test]
async fn test_shutdown_joins_cancelled_background_shell() {
    let manager = crate::create_local_task_manager();
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

#[cfg(unix)]
#[tokio::test]
async fn test_shutdown_immediately_after_shell_spawn_keeps_cleanup_owned() {
    let manager = crate::create_local_task_manager();
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
    let manager = crate::create_local_task_manager();
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
async fn test_shutdown_reaps_child_owned_by_dropped_shell_guard() {
    let manager = crate::create_local_task_manager();
    let mut execution =
        ShellExecutionGuard::new(Some(manager.begin_external_execution("workspace").unwrap()));
    let mut command = shell_command("exec sleep 60", &[]);
    execution.prepare(&mut command).unwrap();
    let child = command.spawn().unwrap();
    let pid = i32::try_from(child.id().unwrap()).unwrap();
    execution.attach_owned(child).unwrap();

    let mut shutdown = manager.shutdown();
    assert!(futures::poll!(&mut shutdown).is_pending());
    drop(execution);
    assert_eq!(shutdown.await, TaskShutdownReport::Complete);
    // Complete 必须证明进程组已消失，而且 Child 已被回收而非仅收到 kill。
    assert_eq!(unsafe { libc::kill(-pid, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    assert_eq!(
        unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) },
        -1
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
}

#[cfg(unix)]
#[tokio::test]
async fn test_rejected_promoted_task_settles_registered_process_after_cleanup() {
    let manager = Arc::new(crate::create_local_task_manager());
    let mut execution =
        ShellExecutionGuard::new(Some(manager.begin_external_execution("workspace").unwrap()));
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
