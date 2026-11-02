use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

/// mockito 的回调运行在它自己的 tokio 运行时里，`ready_only` 会拒绝就地阻塞；
/// 与终态投递相同，换一个独立 runtime 线程等待真实提交。
fn on_mq_runtime<T: Send + 'static>(
    work: impl std::future::Future<Output = T> + Send + 'static,
) -> T {
    std::thread::spawn(|| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test MQ bridge runtime")
            .block_on(work)
    })
    .join()
    .expect("test MQ bridge thread panicked")
}

#[tokio::test]
#[serial]
async fn late_background_result_is_processed_without_default_iteration_limit() {
    let mut harness = ActivationHarness::new().await;
    harness._mock.remove_async().await;
    let limit = 3;
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
                    let queue = queue.clone();
                    on_mq_runtime(async move {
                        queue
                            .push_async(QueuedMessage::defer(
                                MessageSource::SystemInjected,
                                BaseMessage::human("next step"),
                            ))
                            .await;
                    });
                } else if call == limit {
                    complete(&peri_acp_types::event::BackgroundTaskResult {
                        task_id: "budget-boundary-child".into(), agent_name: "child".into(),
                        prompt_summary: "late child".into(), success: true,
                        output: "unique-budget-boundary-result".into(), tool_calls_count: 0,
                        duration_ms: 1, child_thread_id: None, timed_out: false,
                        subagent_failure: None, shell_output: None,
                    }, BgTaskKind::Agent).unwrap();
                    // MQ 的 Receive 会原子提交到历史，不能再 drain 后重投；这里只读快照，
                    // 让晚到结果留在队列里由下一轮 Receive 交付。
                    let published = {
                        let queue = queue.clone();
                        on_mq_runtime(async move {
                            queue
                                .snapshot()
                                .await
                                .expect("test MQ snapshot")
                                .into_iter()
                                .find(|message| message.source == MessageSource::SubAgentComplete)
                                .expect("late child result must stay queued")
                        })
                    };
                    *published_result.lock().unwrap() = Some(published);
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
    assert_eq!(response["stopReason"], "end_turn");
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
            if terminals >= 1 && state.cancel_token.is_none() && !state.continuation_in_flight {
                break;
            }
            drop(sessions);
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the initial run must consume the late result without a budget continuation");
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
        published.admission_sequence, None,
        "MQ 以 (enqueued_at, id) 定序，旧接纳序号不再赋值"
    );
    assert_eq!(
        history
            .iter()
            .filter(|payload| payload.id() == delivery_id)
            .count(),
        1
    );
    assert!(queue.is_empty().await.expect("test MQ len"));
    let sessions = harness.sessions.lock().await;
    assert!(sessions[&harness.session_id].cancel_token.is_none());
    assert!(!sessions[&harness.session_id].continuation_in_flight);
    drop(sessions);
    harness
        .cfg
        .session_manager
        .pre_close_session(&harness.session_id);
}
