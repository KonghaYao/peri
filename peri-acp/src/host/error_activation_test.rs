use super::*;

async fn consume_error_notification(harness: &mut ActivationHarness) {
    let request = tokio::time::timeout(
        Duration::from_secs(2),
        harness.receiver.as_mut().unwrap().recv(),
    )
    .await
    .expect("a fresh required error must wake the session")
    .unwrap();
    assert!(request.mq_steering);
    let epoch = {
        let mut sessions = harness.sessions.lock().await;
        crate::host::continuation::take_continuation_for_request(
            sessions.get_mut(&harness.session_id).unwrap(),
            &request,
        )
        .unwrap()
    };
    let response = crate::host::dispatch_prompt_turn(
        json!({"sessionId": harness.session_id, "message":{"role":"user", "content":[]}}),
        crate::host::PromptOrigin::Continuation { mq_steering: true },
        Some(epoch),
        &harness.sessions,
        &harness.locks,
        &harness.transport,
        &harness.cfg,
        &harness.sender,
    )
    .await
    .unwrap();
    assert_eq!(response["stopReason"], "end_turn");
    let history = harness
        .cfg
        .session_resources
        .load_session_history(&harness.session_id)
        .await
        .unwrap();
    assert!(history.iter().any(|payload| {
        peri_acp_types::store::serialize_persisted_payload(payload)
            .unwrap()
            .contains("late task error")
    }));
}

#[tokio::test]
#[serial]
async fn fresh_task_error_after_model_failure_reactivates_the_loop() {
    let mut harness = ActivationHarness::new().await;
    harness._mock.remove_async().await;
    let failure = harness
        ._server
        .mock("POST", mockito::Matcher::Any)
        .with_status(400)
        .with_body(r#"{"error":{"message":"invalid request","type":"invalid_request_error"}}"#)
        .create_async()
        .await;
    let result = crate::host::dispatch_prompt_turn(
        json!({"sessionId": harness.session_id, "message":{"role":"user", "content":[{"type":"text", "text":"trigger failure"}]}}),
        crate::host::PromptOrigin::User,
        None,
        &harness.sessions,
        &harness.locks,
        &harness.transport,
        &harness.cfg,
        &harness.sender,
    )
    .await;
    let error = result.unwrap_err();
    assert!(error.message.contains("400"), "{}", error.message);
    failure.remove_async().await;
    let _success = harness._server.mock("POST", mockito::Matcher::Any)
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body("data: {\"id\":\"recovered\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"done\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n")
        .create_async().await;
    harness.listen();
    harness.complete_task("failed-child", BgTaskKind::Agent, false);
    consume_error_notification(&mut harness).await;
    harness
        .cfg
        .session_manager
        .pre_close_session(&harness.session_id);
}

#[tokio::test]
#[serial]
async fn assembly_failure_blocks_old_inputs_but_not_a_new_task_error() {
    let mut harness = ActivationHarness::new().await;
    harness
        .cfg
        .session_manager
        .get_session(&harness.session_id)
        .unwrap()
        .activation
        .allow();
    harness.complete_child("old-input");
    let frozen = harness
        .sessions
        .lock()
        .await
        .get_mut(&harness.session_id)
        .unwrap()
        .frozen
        .take();
    let result = crate::host::dispatch_prompt_turn(
        json!({"sessionId": harness.session_id, "message":{"role":"user", "content":[]}}),
        crate::host::PromptOrigin::Continuation { mq_steering: true },
        Some(0),
        &harness.sessions,
        &harness.locks,
        &harness.transport,
        &harness.cfg,
        &harness.sender,
    )
    .await;
    assert!(result.unwrap_err().message.contains("frozen"));
    assert!(!crate::host::activation::mq_allowed(
        &harness.cfg,
        &harness.session_id
    ));
    assert!(harness.receiver.as_mut().unwrap().try_recv().is_err());
    harness
        .sessions
        .lock()
        .await
        .get_mut(&harness.session_id)
        .unwrap()
        .frozen = frozen;
    harness.listen();
    harness.complete_task("failed-workflow", BgTaskKind::Workflow, false);
    consume_error_notification(&mut harness).await;
    harness
        .cfg
        .session_manager
        .pre_close_session(&harness.session_id);
}
