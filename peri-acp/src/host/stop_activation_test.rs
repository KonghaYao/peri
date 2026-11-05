use super::*;
use peri_acp_types::messages::MessageContent;
use peri_acp_types::session::{DispatchUserInputsRequest, EnqueueUserInputRequest, UserInputState};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const STOP_RUNNING_ID: &str = "00000000-0000-0000-0000-0000000000b1";
const STOP_INPUT_ID: &str = "00000000-0000-0000-0000-0000000000bb";
const STOP_INPUT_TEXT: &str = "Stop 前的暂存输入";
const STOP_SSE_BODY: &str = "data: {\"id\":\"stop-test\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"done\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";

async fn read_model_request(socket: &mut TcpStream) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    loop {
        let mut chunk = [0_u8; 4096];
        let read = socket.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        bytes.extend_from_slice(&chunk[..read]);
        let Some(header_end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") else {
            continue;
        };
        let headers = std::str::from_utf8(&bytes[..header_end]).ok()?;
        let length: usize = headers
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, value)| value.trim().parse().ok())?;
        let start = header_end + 4;
        if bytes.len() >= start + length {
            return Some(bytes[start..start + length].to_vec());
        }
    }
}

fn stop_sse_response() -> Vec<u8> {
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        STOP_SSE_BODY.len()
    );
    [header.as_bytes(), STOP_SSE_BODY.as_bytes()].concat()
}

/// 首个模型请求挂起（不写响应），后续请求返回 SSE：若 Stop 被绕过，续跑会留下可观测的第二次执行。
fn spawn_held_model_server(
    listener: tokio::net::TcpListener,
) -> (
    tokio::task::JoinHandle<()>,
    tokio::sync::oneshot::Receiver<()>,
    tokio::sync::oneshot::Sender<()>,
) {
    let (first_seen_tx, first_seen_rx) = tokio::sync::oneshot::channel();
    let (hold_release_tx, hold_release_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let mut first_seen = Some(first_seen_tx);
        let mut hold_release = Some(hold_release_rx);
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let first_seen = first_seen.take();
            let hold_release = hold_release.take();
            tokio::spawn(async move {
                if read_model_request(&mut socket).await.is_none() {
                    return;
                }
                if let Some(first_seen) = first_seen {
                    let _ = first_seen.send(());
                }
                if let Some(hold_release) = hold_release {
                    let _ = hold_release.await;
                    return;
                }
                let _ = socket.write_all(&stop_sse_response()).await;
            });
        }
    });
    (server, first_seen_rx, hold_release_tx)
}

async fn wait_for_cancelled_done(harness: &ActivationHarness) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let done = harness
                .recorded
                .notifications()
                .iter()
                .any(|(method, params)| {
                    method == "peri/agent_event_done" && params["stopReason"] == "cancelled"
                });
            if done {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("Stop 必须结束在途 turn");
}

async fn stop(harness: &ActivationHarness, sessions: &mut HashMap<String, SessionState>) {
    let outcome = crate::host::notify::handle_notification(
        "session/cancel",
        &json!({"sessionId": harness.session_id}),
        sessions,
        &harness.cfg,
    )
    .await;
    if let Some(request) = outcome.continuation {
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
    stop(&harness, &mut *harness.sessions.lock().await).await;
    harness.complete_task("after-stop", BgTaskKind::Agent, false);
    tokio::time::sleep(Duration::from_millis(100)).await;
    failure.assert_async().await;
    assert!(!crate::host::activation::mq_allowed(&harness.cfg, &harness.session_id).await);
    assert!(harness
        .cfg
        .session_manager
        .v2_queue_for(&harness.session_id)
        .unwrap()
        .has_ensure_processing()
        .await
        .expect("test MQ policy"));
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
    let ticket = harness
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
    harness
        .task_tickets
        .lock()
        .unwrap()
        .insert("independent-child".into(), ticket);
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
    stop(&harness, &mut *harness.sessions.lock().await).await;
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
    assert!(queue.is_empty().await.expect("test MQ len"));
    harness.complete_task("second-child", BgTaskKind::Agent, false);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(queue.has_ensure_processing().await.expect("test MQ policy"));
    assert!(!crate::host::activation::mq_allowed(&harness.cfg, &harness.session_id).await);
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

/// Stop suppresses autonomous activation: a deferred monitor wake stays in the
/// queue and does not start a turn. The next user prompt re-enables activation
/// and consumes the deferred message inside that turn.
#[tokio::test]
#[serial]
async fn stop_suppression_blocks_defer_until_the_next_user_prompt_restores_it() {
    let mut harness = ActivationHarness::new().await;
    harness.start_scheduler();
    harness.listen();
    stop(&harness, &mut *harness.sessions.lock().await).await;
    assert!(!crate::host::activation::mq_allowed(&harness.cfg, &harness.session_id).await);

    let queue = harness
        .cfg
        .session_manager
        .v2_queue_for(&harness.session_id)
        .unwrap();
    queue
        .push_async(QueuedMessage::defer(
            MessageSource::McpNotification,
            BaseMessage::human("monitor alert"),
        ))
        .await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        harness
            .recorded
            .notifications()
            .iter()
            .all(|(method, _)| method != "peri/agent_event_done"),
        "a suppressed defer must not start a turn"
    );
    assert!(
        queue.has_ensure_processing().await.expect("test MQ policy"),
        "the defer stays queued"
    );

    let response = crate::host::dispatch_prompt_turn(
        json!({"sessionId": harness.session_id, "message": {
            "role": "user", "content": "continue after stop"
        }}),
        crate::host::PromptOrigin::User,
        None,
        &harness.sessions,
        &harness.locks,
        &harness.transport,
        &harness.cfg,
        &harness.sender,
    )
    .await
    .unwrap();
    assert_eq!(response["stopReason"], "end_turn");
    assert!(
        harness
            .cfg
            .session_manager
            .get_session(&harness.session_id)
            .unwrap()
            .activation
            .is_allowed(),
        "a user prompt restores automatic activation"
    );
    assert!(
        queue.is_empty().await.expect("test MQ len"),
        "the user turn consumes the deferred wake"
    );
    harness
        .cfg
        .session_manager
        .pre_close_session(&harness.session_id);
}

/// Stop 与 steer 落在同一 attempt 时 Stop 优先：取消分支必须 suppress，且不得续跑。
#[tokio::test]
#[serial]
async fn stop_on_steered_managed_turn_suppresses_activation_and_continuation() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let (server, first_seen_rx, hold_release_tx) = spawn_held_model_server(listener);

    let mut harness = ActivationHarness::with_base_url(&base_url).await;
    harness.start_scheduler();
    let mailbox = crate::host::user_input::ensure_mailbox(
        &harness.session_id,
        &harness.cfg,
        &harness.transport,
    )
    .await
    .unwrap();
    let generation = mailbox.generation().to_owned();
    mailbox
        .enqueue(&EnqueueUserInputRequest {
            session_id: harness.session_id.clone(),
            generation: generation.clone(),
            command_id: "stop-test-running".into(),
            input_id: STOP_RUNNING_ID.into(),
            content: MessageContent::text("Stop 前的在途提问"),
            original_draft: "Stop 前的在途提问".into(),
        })
        .await
        .expect("在途输入应入队");
    crate::host::user_input::schedule_mailbox(
        &harness.session_id,
        &harness.sessions,
        &harness.locks,
        &harness.cfg,
        &harness.transport,
        &harness.sender,
    );
    tokio::time::timeout(Duration::from_secs(10), first_seen_rx)
        .await
        .expect("managed turn 必须发起模型请求")
        .expect("模型请求必须完整到达");

    // 暂存区发送：先把 reason 置 Steer 并取消在途请求。
    mailbox
        .enqueue(&EnqueueUserInputRequest {
            session_id: harness.session_id.clone(),
            generation: generation.clone(),
            command_id: "stop-test-steer".into(),
            input_id: STOP_INPUT_ID.into(),
            content: MessageContent::text(STOP_INPUT_TEXT),
            original_draft: STOP_INPUT_TEXT.into(),
        })
        .await
        .expect("暂存区入队应被接受");
    mailbox
        .dispatch(&DispatchUserInputsRequest {
            session_id: harness.session_id.clone(),
            generation,
            command_id: "stop-test-dispatch".into(),
            input_ids: vec![STOP_INPUT_ID.into()],
        })
        .await
        .expect("暂存区发送应被接受");

    // 同一 attempt 上按 Stop：notify 路径先置 reason=Stop，再取消并 suppress。
    stop(&harness, &mut *harness.sessions.lock().await).await;
    wait_for_cancelled_done(&harness).await;

    assert!(
        !harness
            .cfg
            .session_manager
            .get_session(&harness.session_id)
            .unwrap()
            .activation
            .is_allowed(),
        "Stop 必须抑制自动续跑"
    );
    assert!(
        !crate::host::activation::mq_allowed(&harness.cfg, &harness.session_id).await,
        "Stop 后暂存输入不得自动激活"
    );

    // 给真实 scheduler 一个窗口：若 steer 例外越过 Stop，这里会出现第二次结束事件与历史写入。
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        harness
            .recorded
            .notifications()
            .iter()
            .filter(|(method, _)| method == "peri/agent_event_done")
            .count(),
        1,
        "Stop 后不得被 steer continuation 续跑"
    );
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
                    .contains(STOP_INPUT_TEXT)
            })
            .count(),
        0,
        "Stop 不得把暂存输入写进历史"
    );
    assert!(
        !harness
            .cfg
            .session_manager
            .v2_queue_for(&harness.session_id)
            .unwrap()
            .is_empty()
            .await
            .expect("test MQ len"),
        "Stop 必须保留未交付的暂存输入"
    );
    assert!(
        mailbox
            .snapshot()
            .await
            .items
            .iter()
            .any(|item| item.input_id == STOP_INPUT_ID && item.state == UserInputState::Queued),
        "Stop 必须把暂存输入退回排队"
    );

    let _ = hold_release_tx.send(());
    server.abort();
    harness
        .cfg
        .session_manager
        .pre_close_session(&harness.session_id);
}
