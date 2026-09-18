use super::*;

fn make_task(id: &str) -> BackgroundTask {
    BackgroundTask {
        id: id.to_string(),
        agent_name: "test-agent".to_string(),
        prompt_summary: "registry wake test".to_string(),
        status: BackgroundTaskStatus::Running,
        started_at: std::time::Instant::now(),
        chrono_started_at: chrono::Utc::now(),
        kind: BgTaskKind::Agent,
        cancel_handle: BgCancelHandle::Abort(tokio::spawn(async {})),
        cancel_token: None,
        pid: None,
        output_preview: None,
        agent_inbox: None,
    }
}

fn result(task_id: &str) -> BackgroundTaskResult {
    BackgroundTaskResult {
        task_id: task_id.to_string(),
        agent_name: "test-agent".to_string(),
        prompt_summary: "registry wake test".to_string(),
        success: true,
        output: "done".to_string(),
        tool_calls_count: 0,
        duration_ms: 1,
        child_thread_id: None,
        timed_out: false,
        subagent_failure: None,
        shell_output: None,
    }
}

#[tokio::test]
async fn test_registry_activity_signal_reports_register_and_complete_once() {
    let registry = BackgroundTaskRegistry::new();
    let mut activity = registry.subscribe_activity();
    assert_eq!(*activity.borrow(), 0);

    registry.register_with_kind(make_task("bg-1")).unwrap();
    assert!(activity.has_changed().unwrap());
    assert_eq!(*activity.borrow_and_update(), 1);
    assert_eq!(registry.active_count(), 1);

    assert!(registry.complete("bg-1", result("bg-1")));
    assert!(activity.has_changed().unwrap());
    assert_eq!(*activity.borrow_and_update(), 2);
    assert_eq!(registry.active_count(), 0);
    assert!(!activity.has_changed().unwrap());
}

#[tokio::test]
async fn test_registry_activity_signal_is_observable_after_wait_registration_race() {
    let registry = BackgroundTaskRegistry::new();
    registry.register_with_kind(make_task("bg-1")).unwrap();
    let mut activity = registry.subscribe_activity();
    assert_eq!(*activity.borrow(), 1);

    // A terminal transition may happen before the waiter reaches changed().
    // watch retains the new version, so this cannot become a lost wakeup.
    assert!(registry.complete("bg-1", result("bg-1")));
    activity.changed().await.unwrap();
    assert_eq!(*activity.borrow(), 2);
    assert_eq!(registry.active_count(), 0);
}

#[tokio::test]
async fn test_registry_activity_signal_wakes_when_cancel_removes_task() {
    let registry = BackgroundTaskRegistry::new();
    let mut activity = registry.subscribe_activity();
    registry.register_with_kind(make_task("bg-1")).unwrap();
    activity.borrow_and_update();

    registry.cancel("bg-1").unwrap();
    activity.changed().await.unwrap();
    assert_eq!(*activity.borrow(), 2);
    assert_eq!(registry.active_count(), 0);
}

#[tokio::test]
async fn test_registry_completion_claim_wins_cancel_and_duplicate_claim() {
    let registry = BackgroundTaskRegistry::new();
    let (tx, mut events) = tokio::sync::mpsc::unbounded_channel();
    registry.set_event_sender(tx, "claim-test".to_string());
    registry.register_with_kind(make_task("bg-claim")).unwrap();
    assert!(matches!(
        events.try_recv().unwrap(),
        BgRegistryEvent::Started { .. }
    ));

    assert!(registry.claim_completion("bg-claim"));
    assert!(!registry.claim_completion("bg-claim"));
    assert_eq!(
        registry.active_count(),
        1,
        "Completing must retain active ownership"
    );
    assert!(matches!(
        registry.cancel("bg-claim"),
        Err(BackgroundRegistryError::TaskCompleting(id)) if id == "bg-claim"
    ));
    assert!(
        events.try_recv().is_err(),
        "claiming must not emit Cancelled"
    );

    // Completing still consumes the per-kind slot until final settlement.
    registry.register_with_kind(make_task("bg-2")).unwrap();
    registry.register_with_kind(make_task("bg-3")).unwrap();
    assert!(registry.register_with_kind(make_task("bg-4")).is_err());
    assert!(registry.complete("bg-claim", result("bg-claim")));
    assert_eq!(registry.active_count(), 2);
}

#[tokio::test]
async fn test_registry_cancel_wins_completion_claim_and_suppresses_late_complete() {
    let registry = BackgroundTaskRegistry::new();
    let (tx, mut events) = tokio::sync::mpsc::unbounded_channel();
    registry.set_event_sender(tx, "cancel-test".to_string());
    registry.register_with_kind(make_task("bg-cancel")).unwrap();
    let _ = events.try_recv();

    registry.cancel("bg-cancel").unwrap();
    assert!(!registry.claim_completion("bg-cancel"));
    assert!(!registry.complete("bg-cancel", result("bg-cancel")));
    assert_eq!(registry.active_count(), 0);
    let mut saw_cancelled = false;
    let mut saw_completed = false;
    while let Ok(event) = events.try_recv() {
        saw_cancelled |= matches!(event, BgRegistryEvent::Cancelled { .. });
        saw_completed |= matches!(event, BgRegistryEvent::Completed { .. });
    }
    assert!(saw_cancelled);
    assert!(!saw_completed);
}
