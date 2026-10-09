use super::*;

fn stop(harness: &ActivationHarness, sessions: &mut HashMap<String, SessionState>) {
    if let Some(request) = crate::host::notify::handle_notification(
        "session/cancel",
        &json!({"sessionId": harness.session_id}),
        sessions,
        &harness.cfg,
    ) {
        harness.sender.send(request).unwrap();
    }
}

#[tokio::test]
#[serial]
async fn explicit_stop_after_failure_does_not_release_a_new_child_error() {
    let mut harness = ActivationHarness::new().await;
    harness._mock.remove_async().await;
    harness.start_scheduler();
    let failure = harness
        ._server
        .mock("POST", mockito::Matcher::Any)
        .expect(1)
        .with_status(400)
        .with_body(r#"{"error":{"message":"invalid request","type":"invalid_request_error"}}"#)
        .create_async()
        .await;
    let result = crate::host::dispatch_prompt_turn(
        json!({"sessionId": harness.session_id, "message":{"role":"user", "content":"trigger failure"}}),
        crate::host::PromptOrigin::User, None, &harness.sessions, &harness.locks,
        &harness.transport, &harness.cfg, &harness.sender,
    ).await;
    assert!(result.unwrap_err().message.contains("400"));
    stop(&harness, &mut *harness.sessions.lock().await);
    harness.complete_task("after-stop", BgTaskKind::Agent, false);
    tokio::time::sleep(Duration::from_millis(100)).await;
    failure.assert_async().await;
    assert!(!crate::host::activation::mq_allowed(
        &harness.cfg,
        &harness.session_id
    ));
    assert!(harness
        .cfg
        .session_manager
        .v2_queue_for(&harness.session_id)
        .unwrap()
        .has_ensure_processing());
    let sessions = harness.sessions.lock().await;
    let state = &sessions[&harness.session_id];
    assert!(!state.continuation_in_flight);
    assert!(!state.continuation_armed);
    assert!(!state.continuation_mq_steering_pending);
    assert!(state.cancel_token.is_none());
    assert_eq!(
        harness
            .recorded
            .notifications()
            .iter()
            .filter(|(method, _)| method == "peri/agent_event_done")
            .count(),
        1
    );
    drop(sessions);
    harness
        .cfg
        .session_manager
        .pre_close_session(&harness.session_id);
}

#[tokio::test]
#[serial]
async fn stopped_user_turn_allows_exactly_one_automatic_independent_child_turn() {
    let mut harness = ActivationHarness::new().await;
    harness.start_scheduler();
    harness
        .cfg
        .session_manager
        .get_session(&harness.session_id)
        .unwrap()
        .task_manager
        .register(BgTaskRegistration {
            task_id: "independent-child".into(),
            kind: BgTaskKind::Agent,
            summary: "child".into(),
            pid: None,
            kill: None,
        })
        .unwrap();
    let cfg = harness.cfg.clone();
    let sessions = harness.sessions.clone();
    let locks = harness.locks.clone();
    let transport = harness.transport.clone();
    let sender = harness.sender.clone();
    let session_id = harness.session_id.clone();
    let prompt =
        tokio::spawn(async move {
            crate::host::dispatch_prompt_turn(
            json!({"sessionId": session_id, "message":{"role":"user", "content":"start parent"}}),
            crate::host::PromptOrigin::User, None, &sessions, &locks, &transport, &cfg, &sender,
        ).await
        });
    tokio::time::timeout(Duration::from_secs(15), async {
        while !harness
            .cfg
            .session_manager
            .is_idle_suspended(&harness.session_id)
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("real parent must be suspended waiting for its independent child");
    stop(&harness, &mut *harness.sessions.lock().await);
    let response = tokio::time::timeout(Duration::from_secs(10), prompt)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(response["stopReason"], "cancelled");
    harness.complete_child("independent-child");
    harness.wait_for_terminals(2).await;
    let history = harness
        .cfg
        .session_resources
        .load_session_history(&harness.session_id)
        .await
        .unwrap();
    assert_eq!(
        history
            .iter()
            .filter(|payload| {
                peri_acp_types::store::serialize_persisted_payload(payload)
                    .unwrap()
                    .contains("late child result")
            })
            .count(),
        1
    );
    let queue = harness
        .cfg
        .session_manager
        .v2_queue_for(&harness.session_id)
        .unwrap();
    assert!(queue.is_empty());
    harness.complete_task("second-child", BgTaskKind::Agent, false);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(queue.has_ensure_processing());
    assert!(!crate::host::activation::mq_allowed(
        &harness.cfg,
        &harness.session_id
    ));
    assert_eq!(
        harness
            .recorded
            .notifications()
            .iter()
            .filter(|(method, _)| method == "peri/agent_event_done")
            .count(),
        2
    );
    let sessions = harness.sessions.lock().await;
    let state = &sessions[&harness.session_id];
    assert!(!state.continuation_armed);
    assert!(!state.continuation_in_flight);
    assert!(state.cancel_token.is_none());
    drop(sessions);
    harness
        .cfg
        .session_manager
        .pre_close_session(&harness.session_id);
}
