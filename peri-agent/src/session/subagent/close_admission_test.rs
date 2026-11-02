use super::*;
use peri_acp_types::tasks::{BgTaskRegistration, TaskRegistrationTicket};

fn register(manager: &TaskManager, id: &str) -> Result<TaskRegistrationTicket, String> {
    manager.register(BgTaskRegistration {
        task_id: id.into(),
        kind: BgTaskKind::Agent,
        summary: "sync owner".into(),
        kill: None,
        pid: None,
    })
}
fn result(id: &str) -> crate::agent::events::BackgroundTaskResult {
    crate::agent::events::BackgroundTaskResult {
        task_id: id.into(),
        agent_name: "worker".into(),
        prompt_summary: String::new(),
        success: true,
        output: "done".into(),
        tool_calls_count: 0,
        duration_ms: 0,
        child_thread_id: Some("child".into()),
        timed_out: false,
        subagent_failure: None,
        shell_output: None,
    }
}

#[tokio::test]
async fn close_before_admission_keeps_execution_with_caller_and_creates_no_receipt() {
    let parent = Arc::new(TaskManager::new());
    let session = child(Arc::new(TaskManager::new()), None);
    let state = session.subagent_host().unwrap().close_state.clone();
    close_subagent_session_scope(session).await.unwrap();
    let (release, wait) = tokio::sync::oneshot::channel();
    let mut execution = Some(tokio::spawn(async move {
        wait.await.unwrap();
        7
    }));
    let called = AtomicUsize::new(0);
    let worker_parent = parent.clone();
    let rejected = state.spawn_sync_worker("late".into(), || {
        called.fetch_add(1, Ordering::SeqCst);
        let ticket = register(&parent, "late")?;
        let execution = execution.take().unwrap();
        Ok(async move {
            execution.await.unwrap();
            worker_parent.confirm_external_execution_stopped(&ticket);
            worker_parent.complete(&ticket, result("late"));
        })
    });
    assert!(rejected.is_err());
    assert_eq!(called.load(Ordering::SeqCst), 0);
    assert_eq!(parent.active_count(), 0);
    assert!(!parent.has_unsettled_tasks());
    release.send(()).unwrap();
    assert_eq!(
        execution.unwrap().await.unwrap(),
        7,
        "caller still owns the same child completion"
    );
}

#[tokio::test]
async fn admission_before_close_tracks_completion_and_settles_original_receipt_after_caller_drop() {
    let parent = Arc::new(TaskManager::new());
    let session = child(Arc::new(TaskManager::new()), None);
    let state = session.subagent_host().unwrap().close_state.clone();
    let (release, wait) = tokio::sync::oneshot::channel();
    let execution = tokio::spawn(async move {
        wait.await.unwrap();
    });
    let receipt = Arc::new(parking_lot::Mutex::new(None));
    let receipt_capture = receipt.clone();
    let worker_parent = parent.clone();
    let worker_state = state.clone();
    let caller_handle = state
        .spawn_sync_worker("owned".into(), || {
            let ticket = register(&parent, "owned")?;
            *receipt_capture.lock() = Some(ticket.clone());
            Ok(async move {
                execution.await.unwrap();
                worker_parent.confirm_external_execution_stopped(&ticket);
                assert!(worker_parent.complete(&ticket, result("owned")));
                worker_state.settle_sync_delivery("owned");
            })
        })
        .unwrap();
    assert_eq!(parent.active_count(), 1);
    drop(caller_handle); // The close state's tracker remains the completion owner.
    let closing = tokio::spawn(close_subagent_session_scope(session));
    tokio::task::yield_now().await;
    assert!(!closing.is_finished());
    release.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), closing)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(parent.active_count(), 0);
    assert!(!parent.has_unsettled_tasks());
    assert!(receipt.lock().as_ref().unwrap().is_settled());
}
