use super::*;

fn task(id: &str) -> BackgroundTask {
    BackgroundTask {
        id: id.into(),
        agent_name: "worker".into(),
        prompt_summary: "pending start".into(),
        status: BackgroundTaskStatus::Running,
        started_at: peri_time::monotonic_now(),
        chrono_started_at: peri_time::now_wall().into(),
        kind: BgTaskKind::Agent,
        cancel_handle: BgCancelHandle::Kill(None),
        cancel_token: None,
        pid: None,
        output_preview: None,
        agent_inbox: None,
        initiator_session_id: None,
        owner_session_id: None,
        owner_identity: None,
    }
}

#[test]
fn unpolled_background_future_withdraws_only_its_unstarted_receipt() {
    let manager = Arc::new(TaskManager::new());
    let ticket = manager.register_pending_start(task("unpolled")).unwrap();
    let guard = UnstartedRegistrationGuard {
        manager: manager.clone(),
        ticket: Arc::new(parking_lot::Mutex::new(Some(ticket.clone()))),
        armed: true,
    };
    let never_polled = async move {
        let _guard = guard;
        std::future::pending::<()>().await;
    };
    drop(never_polled);
    assert_eq!(manager.active_count(), 0);
    assert!(ticket.is_settled());
    assert!(!manager.has_unsettled_tasks());
}

#[test]
fn successful_start_disarms_unstarted_withdrawal_without_claiming_completion() {
    let manager = Arc::new(TaskManager::new());
    let ticket = manager.register_pending_start(task("started")).unwrap();
    let mut guard = UnstartedRegistrationGuard {
        manager: manager.clone(),
        ticket: Arc::new(parking_lot::Mutex::new(Some(ticket.clone()))),
        armed: true,
    };
    manager.mark_execution_started(&ticket).unwrap();
    guard.armed = false;
    drop(guard);
    assert_eq!(manager.active_count(), 1);
    assert!(!ticket.is_settled());
    assert!(manager.has_unsettled_tasks());
}
