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
        crate::host::PromptOrigin::Continuation {
            mq_steering: true,
            pending_result: false,
            recovery: false,
        },
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
        crate::host::PromptOrigin::Continuation {
            mq_steering: true,
            pending_result: false,
            recovery: false,
        },
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
            let _ = crate::host::notify::handle_notification(
                "session/cancel",
                &json!({"sessionId": harness.session_id}),
                &mut sessions,
                &harness.cfg,
            )
            .await;
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
        .has_ensure_processing()
        .await
        .expect("test MQ policy"));
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

/// 资格查询之后、最终准入之前到达的 Stop 必须阻止模型 I/O。
#[tokio::test]
#[serial]
async fn stop_after_eligibility_queries_blocks_model_execution() {
    let mut harness = ActivationHarness::new().await;
    harness._mock.remove_async().await;
    let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = requests.clone();
    let model = harness
        ._server
        .mock("POST", mockito::Matcher::Any)
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body_from_request(move |_| {
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            b"data: {\"id\":\"stop-probe\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"done\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n".to_vec()
        })
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
        "{}:before_final_admission",
        harness.session_id
    ));
    let continuation = dispatch(
        &harness,
        crate::host::PromptOrigin::Continuation {
            mq_steering: true,
            pending_result: false,
            recovery: false,
        },
        Some(0),
    );
    tokio::time::timeout(Duration::from_secs(10), gate.entered.notified())
        .await
        .unwrap();
    {
        let mut sessions = harness.sessions.lock().await;
        assert!(sessions[&harness.session_id].cancel_token.is_none());
        crate::host::notify::handle_notification(
            "session/cancel",
            &json!({"sessionId": harness.session_id}),
            &mut sessions,
            &harness.cfg,
        )
        .await;
        assert_eq!(sessions[&harness.session_id].continuation_epoch, 1);
    }
    assert!(harness
        .cfg
        .session_manager
        .get_session(&harness.session_id)
        .unwrap()
        .activation
        .user_stopped());
    gate.release.notify_one();
    let result = tokio::time::timeout(Duration::from_secs(15), continuation)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    harness
        .cfg
        .session_manager
        .pre_close_session(&harness.session_id);
    assert_eq!(
        result,
        Value::Null,
        "an acknowledged Stop must reject the continuation before model I/O"
    );
    assert_eq!(
        requests.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "Stop acknowledged before admission must prevent model I/O"
    );
    drop(model);
}

/// 资格查询之后到达的新用户输入必须取代旧续跑：只有新输入发起模型请求。
#[tokio::test]
#[serial]
async fn new_prompt_after_eligibility_queries_supersedes_continuation() {
    let mut harness = ActivationHarness::new().await;
    route_prediction_requests(&mut harness).await;
    let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = requests.clone();
    let model = harness
        ._server
        .mock("POST", mockito::Matcher::Any)
        .match_request(|request| {
            !String::from_utf8_lossy(request.body().unwrap())
                .contains("请根据以上对话预测用户下一步输入")
        })
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body_from_request(move |_| {
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            b"data: {\"id\":\"new-prompt-probe\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"done\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n".to_vec()
        })
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
        "{}:before_final_admission",
        harness.session_id
    ));
    let continuation = dispatch(
        &harness,
        crate::host::PromptOrigin::Continuation {
            mq_steering: true,
            pending_result: false,
            recovery: false,
        },
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
    let old_result = tokio::time::timeout(Duration::from_secs(15), continuation)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let user_result = tokio::time::timeout(Duration::from_secs(15), user)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    harness
        .cfg
        .session_manager
        .pre_close_session(&harness.session_id);
    assert_eq!(
        old_result,
        Value::Null,
        "new input must supersede the queued continuation"
    );
    assert_eq!(user_result["stopReason"], "end_turn");
    assert_eq!(
        requests.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "new input must supersede the old turn"
    );
    drop(model);
}

/// 正常续跑在同一复查点之后仍必须放行。
#[tokio::test]
#[serial]
async fn continuation_after_eligibility_queries_still_dispatches() {
    let mut harness = ActivationHarness::new().await;
    route_prediction_requests(&mut harness).await;
    let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = requests.clone();
    let model = harness
        ._server
        .mock("POST", mockito::Matcher::Any)
        .match_request(|request| {
            !String::from_utf8_lossy(request.body().unwrap())
                .contains("请根据以上对话预测用户下一步输入")
        })
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body_from_request(move |_| {
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            b"data: {\"id\":\"normal-continuation\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"done\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n".to_vec()
        })
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
        "{}:before_final_admission",
        harness.session_id
    ));
    let continuation = dispatch(
        &harness,
        crate::host::PromptOrigin::Continuation {
            mq_steering: true,
            pending_result: false,
            recovery: false,
        },
        Some(0),
    );
    tokio::time::timeout(Duration::from_secs(10), gate.entered.notified())
        .await
        .expect("continuation must reach the final admission gate");
    gate.release.notify_one();
    let result = tokio::time::timeout(Duration::from_secs(15), continuation)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    harness
        .cfg
        .session_manager
        .pre_close_session(&harness.session_id);
    assert_ne!(result, Value::Null);
    assert_eq!(result["stopReason"], "end_turn");
    assert_eq!(
        requests.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "a valid continuation still performs exactly one model request"
    );
    drop(model);
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
        crate::host::PromptOrigin::Continuation {
            mq_steering: true,
            pending_result: false,
            recovery: false,
        },
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
        .await
        .continuation
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
        crate::host::PromptOrigin::Continuation {
            mq_steering: true,
            pending_result: false,
            recovery: false,
        },
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
