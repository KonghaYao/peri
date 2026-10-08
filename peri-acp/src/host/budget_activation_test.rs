use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

#[tokio::test]
#[serial]
async fn budget_boundary_result_is_automatically_processed_by_host_scheduler() {
    let mut harness = ActivationHarness::new().await;
    harness._mock.remove_async().await;
    let limit = peri_agent::session::config::SessionConfig::default().max_iterations();
    let calls = Arc::new(AtomicUsize::new(0));
    let followup_body = Arc::new(std::sync::Mutex::new(None));
    let published_result = Arc::new(std::sync::Mutex::new(None));
    let runtime = harness
        .cfg
        .session_manager
        .get_session(&harness.session_id)
        .unwrap();
    let queue = runtime.v2_message_queue.clone();
    drop(runtime);
    let complete = peri_agent::session::bg_complete::session_bg_complete_callback(
        Arc::new(harness.cfg.session_manager.clone()),
        harness.session_id.clone(),
    );
    let _auxiliary = harness._server.mock("POST", mockito::Matcher::Any)
        .match_request(|request| {
            let body: Value = serde_json::from_slice(request.body().unwrap()).unwrap();
            !body["tools"].as_array().is_some_and(|tools| !tools.is_empty())
        })
        .with_status(200).with_header("content-type", "text/event-stream")
        .with_body("data: {\"id\":\"auxiliary\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"done\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n")
        .create_async().await;
    let model = harness._server.mock("POST", mockito::Matcher::Any)
        .match_body(mockito::Matcher::Regex(r#""tools"\s*:\s*\[\s*\{"#.into()))
        .expect(limit + 1)
        .with_status(200).with_header("content-type", "text/event-stream")
        .with_body_from_request({
            let calls = calls.clone();
            let followup_body = followup_body.clone();
            let published_result = published_result.clone();
            let queue = queue.clone();
            move |request| {
                let call = calls.fetch_add(1, Ordering::SeqCst) + 1;
                if call < limit {
                    queue.push(QueuedMessage::defer(
                        MessageSource::SystemInjected, BaseMessage::human("next step"),
                    ));
                } else if call == limit {
                    complete(&peri_acp_types::event::BackgroundTaskResult {
                        task_id: "budget-boundary-child".into(), agent_name: "child".into(),
                        prompt_summary: "late child".into(), success: true,
                        output: "unique-budget-boundary-result".into(), tool_calls_count: 0,
                        duration_ms: 1, child_thread_id: None, timed_out: false,
                        subagent_failure: None, shell_output: None,
                    }, BgTaskKind::Agent).unwrap();
                    let pending = queue.drain_all();
                    assert_eq!(pending.len(), 1);
                    *published_result.lock().unwrap() = Some(pending[0].clone());
                    queue.push_batch(pending);
                } else {
                    *followup_body.lock().unwrap() = Some(
                        serde_json::from_slice::<Value>(request.body().unwrap()).unwrap()
                    );
                }
                b"data: {\"id\":\"test-response\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"done\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n".to_vec()
            }
        }).create_async().await;
    let receiver = harness.receiver.take().unwrap();
    harness
        .cfg
        .host_task_spawner
        .spawn(
            crate::host::task_scope::HostTaskOwnerKind::Host,
            crate::host::task_scope::HostTaskKind::ContinuationScheduler,
            crate::host::continuation::run_continuation_scheduler(
                receiver,
                harness.sessions.clone(),
                harness.locks.clone(),
                harness.cfg.clone(),
                harness.transport.clone(),
                Arc::downgrade(&harness.sender),
                harness.cfg.host_task_spawner.clone(),
                harness.cfg.host_task_spawner.shutdown_token(),
            ),
        )
        .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(180),
        crate::host::dispatch_prompt_turn(
            json!({"sessionId": harness.session_id, "message":{"role":"user", "content":[{"type":"text", "text":"run to the budget boundary"}]}}),
            crate::host::PromptOrigin::User, None, &harness.sessions, &harness.locks,
            &harness.transport, &harness.cfg, &harness.sender,
        )).await.unwrap().unwrap();
    assert_eq!(response["stopReason"], "max_turn_requests");
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let terminals = harness
                .recorded
                .notifications()
                .iter()
                .filter(|(method, _)| method == "peri/agent_event_done")
                .count();
            let sessions = harness.sessions.lock().await;
            let state = &sessions[&harness.session_id];
            if terminals >= 2 && state.cancel_token.is_none() && !state.continuation_in_flight {
                break;
            }
            drop(sessions);
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the real scheduler must finish the followup without a manual prompt");
    assert_eq!(calls.load(Ordering::SeqCst), limit + 1);
    model.assert_async().await;
    let request = followup_body.lock().unwrap().take().unwrap();
    let messages = serde_json::to_string(&request["messages"]).unwrap();
    assert_eq!(messages.matches("unique-budget-boundary-result").count(), 1);
    let history = harness
        .cfg
        .session_resources
        .load_session_history(&harness.session_id)
        .await
        .unwrap();
    let published = published_result.lock().unwrap().take().unwrap();
    let delivery_id = published.delivery_id.unwrap();
    assert_eq!(
        published.admission_sequence,
        Some(queue.admission_watermark())
    );
    assert_eq!(
        history
            .iter()
            .filter(|payload| payload.id() == delivery_id)
            .count(),
        1
    );
    assert!(queue.is_empty());
    let sessions = harness.sessions.lock().await;
    assert!(sessions[&harness.session_id].cancel_token.is_none());
    assert!(!sessions[&harness.session_id].continuation_in_flight);
    drop(sessions);
    harness
        .cfg
        .session_manager
        .pre_close_session(&harness.session_id);
}
