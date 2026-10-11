use super::*;

fn external_request(
    initiator: Option<&str>,
    owner_task_id: &str,
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
) -> ExternalTaskRegistration {
    ExternalTaskRegistration {
        session_id: "root-session".into(),
        initiator_session_id: initiator.map(str::to_owned),
        owner_identity: "mcp:workspace:Some(Builtin { instance: \"workspace\" })".into(),
        owner_task_id: owner_task_id.into(),
        kind: BgTaskKind::Shell,
        summary: "bg shell".into(),
        started_at: None,
        cancel: std::sync::Arc::new(|| Box::pin(async { Ok(()) })),
        on_terminal: std::sync::Arc::new(move |_, _| {
            calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }),
    }
}

#[tokio::test]
async fn scope_reconciliation_keeps_recorded_initiator_route() {
    let manager = TaskManager::new();
    let child_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let root_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let task_id = manager
        .register_external(external_request(
            Some("child-session"),
            "shell-raw-1",
            child_calls.clone(),
        ))
        .unwrap();
    let reconciled = manager
        .register_external(external_request(None, "shell-raw-1", root_calls.clone()))
        .unwrap();
    assert_eq!(reconciled, task_id, "两条路径必须落在同一公共任务 id");
    assert_eq!(
        manager.snapshot().tasks[0].initiator_session_id.as_deref(),
        Some("child-session"),
        "对账注册不得抹掉已记录的投递归属"
    );
    assert!(manager
        .settle_external(&task_id, "terminal-1", make_result(&task_id, true))
        .await
        .unwrap());
    assert_eq!(
        child_calls.load(Ordering::SeqCst),
        1,
        "终态提醒必须投递给直接发起会话"
    );
    assert_eq!(
        root_calls.load(Ordering::SeqCst),
        0,
        "未知发起者的路由不得替换已记录发起者的路由"
    );
}

#[tokio::test]
async fn terminal_reconciliation_keeps_recorded_initiator_route() {
    let manager = TaskManager::new();
    let child_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let root_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let task_id = manager
        .register_external(external_request(
            Some("child-session"),
            "shell-raw-2",
            child_calls.clone(),
        ))
        .unwrap();
    let restored = manager
        .restore_external_terminal(
            external_request(None, "shell-raw-2", root_calls.clone()),
            "transition-1",
            make_result(&task_id, true),
        )
        .await
        .unwrap();
    assert_eq!(restored, task_id);
    assert_eq!(manager.snapshot().tasks[0].status, "completed");
    assert_eq!(child_calls.load(Ordering::SeqCst), 1);
    assert_eq!(root_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn reconciliation_with_rebuilt_initiator_may_take_over_route() {
    let manager = TaskManager::new();
    let cold_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let rebuilt_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let task_id = manager
        .register_external(external_request(None, "shell-raw-3", cold_calls.clone()))
        .unwrap();
    assert_eq!(
        manager
            .register_external(external_request(
                Some("child-session"),
                "shell-raw-3",
                rebuilt_calls.clone(),
            ))
            .unwrap(),
        task_id
    );
    assert_eq!(
        manager.snapshot().tasks[0].initiator_session_id.as_deref(),
        Some("child-session")
    );
    let before = serde_json::to_value(manager.snapshot()).unwrap();
    let conflicting_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    assert!(matches!(
        manager.register_external(external_request(
            Some("other-session"),
            "shell-raw-3",
            conflicting_calls.clone(),
        )),
        Err(BackgroundRegistryError::ExternalInitiatorConflict { .. })
    ));
    assert_eq!(serde_json::to_value(manager.snapshot()).unwrap(), before);
    assert!(manager
        .settle_external(&task_id, "terminal-1", make_result(&task_id, true))
        .await
        .unwrap());
    assert_eq!(rebuilt_calls.load(Ordering::SeqCst), 1);
    assert_eq!(cold_calls.load(Ordering::SeqCst), 0);
    assert_eq!(conflicting_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn terminal_reconciliation_completes_missing_initiator_identity() {
    let manager = TaskManager::new();
    let unknown_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let child_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let task_id = manager
        .register_external(external_request(None, "shell-raw-4", unknown_calls.clone()))
        .unwrap();
    manager
        .restore_external_terminal(
            external_request(Some("child-session"), "shell-raw-4", child_calls.clone()),
            "terminal-1",
            make_result(&task_id, true),
        )
        .await
        .unwrap();
    assert_eq!(
        manager.snapshot().tasks[0].initiator_session_id.as_deref(),
        Some("child-session")
    );
    let before = serde_json::to_value(manager.snapshot()).unwrap();
    let error = manager
        .restore_external_terminal(
            external_request(Some("other-session"), "shell-raw-4", unknown_calls.clone()),
            "terminal-1",
            make_result(&task_id, true),
        )
        .await
        .unwrap_err();
    assert!(error.contains("initiator conflict"), "{error}");
    assert_eq!(serde_json::to_value(manager.snapshot()).unwrap(), before);
    assert_eq!(child_calls.load(Ordering::SeqCst), 1);
    assert_eq!(unknown_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn unknown_initiator_cannot_settle_until_identity_is_completed() {
    let manager = TaskManager::new();
    let unknown_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let child_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let task_id = manager
        .register_external(external_request(None, "shell-raw-5", unknown_calls.clone()))
        .unwrap();
    let before = serde_json::to_value(manager.snapshot()).unwrap();
    let error = manager
        .settle_external(&task_id, "terminal-1", make_result(&task_id, true))
        .await
        .unwrap_err();
    assert!(error.contains("Unroutable"), "{error}");
    assert_eq!(serde_json::to_value(manager.snapshot()).unwrap(), before);
    assert_eq!(manager.active_count(), 1);
    assert_eq!(unknown_calls.load(Ordering::SeqCst), 0);
    manager
        .register_external(external_request(
            Some("child-session"),
            "shell-raw-5",
            child_calls.clone(),
        ))
        .unwrap();
    let mut discovery = external_request(None, "shell-raw-5", unknown_calls.clone());
    discovery.summary = "discovery metadata must not replace the original".into();
    manager.register_external(discovery).unwrap();
    assert!(manager
        .settle_external(&task_id, "terminal-1", make_result(&task_id, true))
        .await
        .unwrap());
    assert_eq!(child_calls.load(Ordering::SeqCst), 1);
    assert_eq!(unknown_calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        manager.snapshot().tasks[0].initiator_session_id.as_deref(),
        Some("child-session")
    );
}

#[tokio::test]
async fn cold_restore_unknown_initiator_remains_unsettled_without_delivery() {
    let manager = TaskManager::new();
    let unknown_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let child_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let error = manager
        .restore_external_terminal(
            external_request(None, "shell-raw-6", unknown_calls.clone()),
            "terminal-1",
            make_result("owner-task", true),
        )
        .await
        .unwrap_err();
    assert!(error.contains("Unroutable"), "{error}");
    assert_eq!(unknown_calls.load(Ordering::SeqCst), 0);
    assert_eq!(manager.snapshot().tasks[0].status, "pending_delivery");
    assert_eq!(manager.active_count(), 1);
    manager
        .restore_external_terminal(
            external_request(Some("child-session"), "shell-raw-6", child_calls.clone()),
            "terminal-1",
            make_result("owner-task", true),
        )
        .await
        .unwrap();
    assert_eq!(child_calls.load(Ordering::SeqCst), 1);
    assert_eq!(unknown_calls.load(Ordering::SeqCst), 0);
    assert_eq!(manager.snapshot().tasks[0].status, "completed");
    assert_eq!(
        manager.snapshot().tasks[0].initiator_session_id.as_deref(),
        Some("child-session")
    );
    assert_eq!(manager.active_count(), 0);
}

#[tokio::test]
async fn unknown_initiator_cannot_abandon_through_untrusted_route() {
    let manager = TaskManager::new();
    let unknown_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let task_id = manager
        .register_external(external_request(None, "shell-raw-7", unknown_calls.clone()))
        .unwrap();
    let before = serde_json::to_value(manager.snapshot()).unwrap();
    let error = manager
        .abandon_external(&task_id, "owner unreachable")
        .await
        .unwrap_err();
    assert!(error.contains("Unroutable"), "{error}");
    assert_eq!(serde_json::to_value(manager.snapshot()).unwrap(), before);
    assert_eq!(unknown_calls.load(Ordering::SeqCst), 0);
    assert_eq!(manager.active_count(), 1);
}
