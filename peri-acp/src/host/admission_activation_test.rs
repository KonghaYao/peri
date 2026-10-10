use super::*;

async fn route_prediction_requests(harness: &mut ActivationHarness) {
    harness._mock.remove_async().await;
    harness._mock = harness._server.mock("POST", mockito::Matcher::Any)
        .match_request(|request| String::from_utf8_lossy(request.body().unwrap()).contains("请根据以上对话预测用户下一步输入"))
        .with_status(200).with_header("content-type", "text/event-stream")
        .with_body("data: {\"id\":\"prediction\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"done\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n")
        .create_async().await;
}

fn dispatch(
    harness: &ActivationHarness,
    origin: crate::host::PromptOrigin,
    epoch: Option<u64>,
) -> tokio::task::JoinHandle<Result<Value, crate::transport::types::AcpError>> {
    let sessions = harness.sessions.clone();
    let locks = harness.locks.clone();
    let transport = harness.transport.clone();
    let cfg = harness.cfg.clone();
    let sender = harness.sender.clone();
    let session_id = harness.session_id.clone();
    let content = if matches!(origin, crate::host::PromptOrigin::User) {
        json!("new user input")
    } else {
        json!([])
    };
    tokio::spawn(async move {
        crate::host::dispatch_prompt_turn(
            json!({"sessionId": session_id, "message":{"role":"user", "content":content}}),
            origin,
            epoch,
            &sessions,
            &locks,
            &transport,
            &cfg,
            &sender,
        )
        .await
    })
}

#[tokio::test]
#[serial]
async fn user_input_between_continuation_validation_and_commit_revokes_old_turn() {
    let mut harness = ActivationHarness::new().await;
    route_prediction_requests(&mut harness).await;
    let model = harness._server.mock("POST", mockito::Matcher::Any)
        .match_request(|request| !String::from_utf8_lossy(request.body().unwrap()).contains("请根据以上对话预测用户下一步输入"))
        .expect(1).with_status(200).with_header("content-type", "text/event-stream")
        .with_body("data: {\"id\":\"test\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"done\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n")
        .create_async().await;
    harness
        .cfg
        .session_manager
        .get_session(&harness.session_id)
        .unwrap()
        .activation
        .allow();
    harness.complete_child("pending-child");
    let gate = crate::host::prompt_dispatch::admission_gate::install(&harness.session_id);
    let continuation = dispatch(
        &harness,
        crate::host::PromptOrigin::Continuation { mq_steering: true },
        Some(0),
    );
    tokio::time::timeout(Duration::from_secs(10), gate.entered.notified())
        .await
        .unwrap();
    let user = dispatch(&harness, crate::host::PromptOrigin::User, None);
    tokio::time::timeout(Duration::from_secs(10), async {
        while harness.sessions.lock().await[&harness.session_id].continuation_epoch == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    gate.release.notify_one();
    let result = tokio::time::timeout(Duration::from_secs(10), continuation)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        result,
        Value::Null,
        "obsolete continuation must not start a model turn"
    );
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(10), user)
            .await
            .unwrap()
            .unwrap()
            .unwrap()["stopReason"],
        "end_turn"
    );
    model.assert_async().await;
    harness
        .cfg
        .session_manager
        .pre_close_session(&harness.session_id);
}

async fn revoke_before_commit(closing: bool) {
    let mut harness = ActivationHarness::new().await;
    harness._mock.remove_async().await;
    let model = harness
        ._server
        .mock("POST", mockito::Matcher::Any)
        .expect(0)
        .create_async()
        .await;
    harness
        .cfg
        .session_manager
        .get_session(&harness.session_id)
        .unwrap()
        .activation
        .allow();
    harness.complete_child("pending-child");
    let gate = crate::host::prompt_dispatch::admission_gate::install(&harness.session_id);
    let continuation = dispatch(
        &harness,
        crate::host::PromptOrigin::Continuation { mq_steering: true },
        Some(0),
    );
    tokio::time::timeout(Duration::from_secs(10), gate.entered.notified())
        .await
        .unwrap();
    {
        let mut sessions = harness.sessions.lock().await;
        if closing {
            sessions.get_mut(&harness.session_id).unwrap().closing = true;
        } else {
            crate::host::notify::handle_notification(
                "session/cancel",
                &json!({"sessionId": harness.session_id}),
                &mut sessions,
                &harness.cfg,
            );
        }
    }
    gate.release.notify_one();
    let result = tokio::time::timeout(Duration::from_secs(10), continuation)
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(result, Ok(Value::Null) | Err(_)),
        "revoked continuation unexpectedly ran: {result:?}"
    );
    assert!(harness
        .cfg
        .session_manager
        .v2_queue_for(&harness.session_id)
        .unwrap()
        .has_ensure_processing());
    assert!(!harness.sessions.lock().await[&harness.session_id].continuation_in_flight);
    model.assert_async().await;
    harness
        .cfg
        .session_manager
        .pre_close_session(&harness.session_id);
}

#[tokio::test]
#[serial]
async fn stop_between_continuation_validation_and_commit_revokes_old_turn() {
    revoke_before_commit(false).await;
}

#[tokio::test]
#[serial]
async fn closing_between_continuation_validation_and_commit_revokes_old_turn() {
    revoke_before_commit(true).await;
}

#[tokio::test]
#[serial]
async fn stop_after_atomic_admission_cancels_registered_token_before_model_start() {
    let mut harness = ActivationHarness::new().await;
    harness._mock.remove_async().await;
    let model = harness
        ._server
        .mock("POST", mockito::Matcher::Any)
        .expect(0)
        .create_async()
        .await;
    harness
        .cfg
        .session_manager
        .get_session(&harness.session_id)
        .unwrap()
        .activation
        .allow();
    harness.complete_child("pending-child");
    let gate = crate::host::prompt_dispatch::admission_gate::install(&format!(
        "{}:committed",
        harness.session_id
    ));
    let continuation = dispatch(
        &harness,
        crate::host::PromptOrigin::Continuation { mq_steering: true },
        Some(0),
    );
    tokio::time::timeout(Duration::from_secs(10), gate.entered.notified())
        .await
        .unwrap();
    let token = {
        let mut sessions = harness.sessions.lock().await;
        let state = &sessions[&harness.session_id];
        assert!(state.continuation_in_flight);
        let token = state.cancel_token.clone().unwrap();
        assert!(crate::host::notify::handle_notification(
            "session/cancel",
            &json!({"sessionId": harness.session_id}),
            &mut sessions,
            &harness.cfg
        )
        .is_none());
        token
    };
    assert!(token.is_cancelled());
    gate.release.notify_one();
    let result = tokio::time::timeout(Duration::from_secs(10), continuation)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(result["stopReason"], "cancelled");
    model.assert_async().await;
    let sessions = harness.sessions.lock().await;
    let state = &sessions[&harness.session_id];
    assert!(!state.continuation_in_flight);
    assert!(!state.continuation_armed);
    assert!(state.cancel_token.is_none());
    drop(sessions);
    harness
        .cfg
        .session_manager
        .pre_close_session(&harness.session_id);
}

#[tokio::test]
#[serial]
async fn user_input_after_atomic_admission_waits_for_the_committed_turn() {
    let mut harness = ActivationHarness::new().await;
    route_prediction_requests(&mut harness).await;
    let bodies = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    let model = harness._server.mock("POST", mockito::Matcher::Any)
        .match_request(|request| !String::from_utf8_lossy(request.body().unwrap()).contains("请根据以上对话预测用户下一步输入"))
        .expect(2).with_status(200).with_header("content-type", "text/event-stream")
        .with_body_from_request({
            let bodies = bodies.clone();
            move |request| {
                bodies.lock().unwrap().push(serde_json::from_slice(request.body().unwrap()).unwrap());
                b"data: {\"id\":\"test\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"done\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n".to_vec()
            }
        }).create_async().await;
    harness
        .cfg
        .session_manager
        .get_session(&harness.session_id)
        .unwrap()
        .activation
        .allow();
    harness.complete_child("pending-child");
    let gate = crate::host::prompt_dispatch::admission_gate::install(&format!(
        "{}:committed",
        harness.session_id
    ));
    let continuation = dispatch(
        &harness,
        crate::host::PromptOrigin::Continuation { mq_steering: true },
        Some(0),
    );
    tokio::time::timeout(Duration::from_secs(10), gate.entered.notified())
        .await
        .unwrap();
    let user = dispatch(&harness, crate::host::PromptOrigin::User, None);
    tokio::time::timeout(Duration::from_secs(10), async {
        while harness.sessions.lock().await[&harness.session_id].continuation_epoch == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!user.is_finished());
    assert!(bodies.lock().unwrap().is_empty());
    gate.release.notify_one();
    for turn in [continuation, user] {
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(10), turn)
                .await
                .unwrap()
                .unwrap()
                .unwrap()["stopReason"],
            "end_turn"
        );
    }
    model.assert_async().await;
    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2);
    assert!(!serde_json::to_string(&bodies[0]["messages"])
        .unwrap()
        .contains("new user input"));
    assert!(serde_json::to_string(&bodies[1]["messages"])
        .unwrap()
        .contains("new user input"));
    drop(bodies);
    harness
        .cfg
        .session_manager
        .pre_close_session(&harness.session_id);
}
