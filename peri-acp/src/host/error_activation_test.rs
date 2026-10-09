use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

impl ActivationHarness {
    pub(super) fn start_scheduler(&mut self) {
        let receiver = self.receiver.take().unwrap();
        self.cfg
            .host_task_spawner
            .spawn(
                crate::host::task_scope::HostTaskOwnerKind::Host,
                crate::host::task_scope::HostTaskKind::ContinuationScheduler,
                crate::host::continuation::run_continuation_scheduler(
                    receiver,
                    self.sessions.clone(),
                    self.locks.clone(),
                    self.cfg.clone(),
                    self.transport.clone(),
                    Arc::downgrade(&self.sender),
                    self.cfg.host_task_spawner.clone(),
                    self.cfg.host_task_spawner.shutdown_token(),
                ),
            )
            .unwrap();
    }

    pub(super) async fn wait_for_terminals(&self, count: usize) {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let terminals = self
                    .recorded
                    .notifications()
                    .iter()
                    .filter(|(method, _)| method == "peri/agent_event_done")
                    .count();
                let sessions = self.sessions.lock().await;
                let state = &sessions[&self.session_id];
                if terminals >= count
                    && state.cancel_token.is_none()
                    && !state.continuation_in_flight
                {
                    break;
                }
                drop(sessions);
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("real scheduler must settle the expected executions");
    }
}

fn failed_child(task_id: &str) -> peri_acp_types::event::BackgroundTaskResult {
    peri_acp_types::event::BackgroundTaskResult {
        task_id: task_id.into(),
        agent_name: "child".into(),
        prompt_summary: "child".into(),
        success: false,
        output: "late task error".into(),
        tool_calls_count: 0,
        duration_ms: 1,
        child_thread_id: None,
        timed_out: false,
        subagent_failure: None,
        shell_output: None,
    }
}

async fn model_failure_recovers_automatically(during_failure: bool) {
    let mut harness = ActivationHarness::new().await;
    harness._mock.remove_async().await;
    harness.start_scheduler();
    let queue = harness
        .cfg
        .session_manager
        .v2_queue_for(&harness.session_id)
        .unwrap();
    let runtime = harness
        .cfg
        .session_manager
        .get_session(&harness.session_id)
        .unwrap();
    let tasks = runtime.task_manager.clone();
    drop(runtime);
    let callback = peri_agent::session::bg_complete::session_bg_complete_callback(
        Arc::new(harness.cfg.session_manager.clone()),
        harness.session_id.clone(),
    );
    let request_bodies = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    let recoveries = Arc::new(AtomicUsize::new(0));
    let failure = harness
        ._server
        .mock("POST", mockito::Matcher::Any)
        .match_request(|request| {
            !String::from_utf8_lossy(request.body().unwrap()).contains("late task error")
        })
        .expect(1)
        .with_status(400)
        .with_body_from_request(move |_| {
            if during_failure {
                tasks
                    .register(BgTaskRegistration {
                        task_id: "during-failure".into(),
                        kind: BgTaskKind::Agent,
                        summary: "child".into(),
                        pid: None,
                        kill: None,
                    })
                    .unwrap();
                tasks
                    .settle_completed(
                        "during-failure",
                        failed_child("during-failure"),
                        callback.clone(),
                    )
                    .unwrap();
            }
            br#"{"error":{"message":"invalid request","type":"invalid_request_error"}}"#.to_vec()
        })
        .create_async()
        .await;
    let recovery = harness._server.mock("POST", mockito::Matcher::Any)
        .match_request(|request| String::from_utf8_lossy(request.body().unwrap()).contains("late task error"))
        .expect(1).with_status(200).with_header("content-type", "text/event-stream")
        .with_body_from_request({
            let bodies = request_bodies.clone();
            let recoveries = recoveries.clone();
            move |request| {
                recoveries.fetch_add(1, Ordering::SeqCst);
                bodies.lock().unwrap().push(serde_json::from_slice(request.body().unwrap()).unwrap());
                b"data: {\"id\":\"recovered\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"done\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n".to_vec()
            }
        }).create_async().await;
    let error = crate::host::dispatch_prompt_turn(
        json!({"sessionId": harness.session_id, "message":{"role":"user", "content":"trigger failure"}}),
        crate::host::PromptOrigin::User, None, &harness.sessions, &harness.locks,
        &harness.transport, &harness.cfg, &harness.sender,
    ).await.unwrap_err();
    assert!(error.message.contains("400"), "{}", error.message);
    if !during_failure {
        harness.complete_task("after-failure", BgTaskKind::Agent, false);
    }
    harness.wait_for_terminals(2).await;
    failure.assert_async().await;
    recovery.assert_async().await;
    assert_eq!(recoveries.load(Ordering::SeqCst), 1);
    {
        let bodies = request_bodies.lock().unwrap();
        assert_eq!(
            serde_json::to_string(&bodies[0]["messages"])
                .unwrap()
                .matches("late task error")
                .count(),
            1
        );
    }
    let history = harness
        .cfg
        .session_resources
        .load_session_history(&harness.session_id)
        .await
        .unwrap();
    let notifications = history
        .iter()
        .filter(|payload| {
            peri_acp_types::store::serialize_persisted_payload(payload)
                .unwrap()
                .contains("late task error")
        })
        .collect::<Vec<_>>();
    assert_eq!(notifications.len(), 1);
    let sessions = harness.sessions.lock().await;
    let state = &sessions[&harness.session_id];
    let serialize_history = |payloads: &[peri_acp_types::store::PersistedPayload]| {
        payloads
            .iter()
            .map(|payload| peri_acp_types::store::serialize_persisted_payload(payload).unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        serialize_history(&state.history_payloads),
        serialize_history(&history)
    );
    assert_eq!(
        state
            .history_payloads
            .iter()
            .filter(|payload| payload.id() == notifications[0].id())
            .count(),
        1
    );
    assert!(!state.continuation_armed);
    assert!(!state.continuation_mq_steering_pending);
    assert!(state.cancel_token.is_none());
    assert!(!state.continuation_in_flight);
    drop(sessions);
    assert!(queue.is_empty());
    assert!(!crate::host::activation::mq_allowed(
        &harness.cfg,
        &harness.session_id
    ));
    harness
        .cfg
        .session_manager
        .pre_close_session(&harness.session_id);
}

#[tokio::test]
#[serial]
async fn task_error_during_model_failure_is_automatically_processed() {
    model_failure_recovers_automatically(true).await;
}

#[tokio::test]
#[serial]
async fn task_error_after_model_failure_is_automatically_processed() {
    model_failure_recovers_automatically(false).await;
}

#[tokio::test]
#[serial]
async fn assembly_failure_blocks_old_watermark_and_automatically_processes_new_error() {
    let mut harness = ActivationHarness::new().await;
    harness.start_scheduler();
    harness
        .cfg
        .session_manager
        .get_session(&harness.session_id)
        .unwrap()
        .activation
        .allow();
    let frozen = harness
        .sessions
        .lock()
        .await
        .get_mut(&harness.session_id)
        .unwrap()
        .frozen
        .take();
    harness.complete_child("old-input");
    harness.listen();
    harness.wait_for_terminals(1).await;
    assert!(!crate::host::activation::mq_allowed(
        &harness.cfg,
        &harness.session_id
    ));
    let queue = harness
        .cfg
        .session_manager
        .v2_queue_for(&harness.session_id)
        .unwrap();
    let old_watermark = queue.admission_watermark();
    for _ in 0..8 {
        harness
            .sender
            .send(crate::session::executor::ContinuationRequest {
                session_id: harness.session_id.clone(),
                kind: BgTaskKind::Agent,
                mq_steering: true,
            })
            .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        harness
            .recorded
            .notifications()
            .iter()
            .filter(|(method, _)| method == "peri/agent_event_done")
            .count(),
        1
    );
    harness
        .sessions
        .lock()
        .await
        .get_mut(&harness.session_id)
        .unwrap()
        .frozen = frozen;
    harness.complete_task("failed-workflow", BgTaskKind::Workflow, false);
    assert!(queue.admission_watermark() > old_watermark);
    harness.wait_for_terminals(2).await;
    let history = harness
        .cfg
        .session_resources
        .load_session_history(&harness.session_id)
        .await
        .unwrap();
    for marker in ["late child result", "late task error"] {
        assert_eq!(
            history
                .iter()
                .filter(
                    |payload| peri_acp_types::store::serialize_persisted_payload(payload)
                        .unwrap()
                        .contains(marker)
                )
                .count(),
            1
        );
    }
    assert!(queue.is_empty());
    harness
        .cfg
        .session_manager
        .pre_close_session(&harness.session_id);
}
