//! steer 打断不等于 Stop：取消在途 turn 后仍须继续调度 MQ 中新放行的输入。

use super::*;
use peri_acp_types::messages::MessageContent;
use peri_acp_types::session::{DispatchUserInputsRequest, EnqueueUserInputRequest, UserInputState};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const STEER_TEXT: &str = "在途时发送的暂存输入";
const INPUT_ID: &str = "00000000-0000-0000-0000-0000000000aa";
const RUNNING_TEXT: &str = "在途提问";
const RUNNING_ID: &str = "00000000-0000-0000-0000-0000000000a1";
const ATTACH_FAILED_RUNNING_ID: &str = "00000000-0000-0000-0000-0000000000c1";
const ATTACH_FAILED_STEER_ID: &str = "00000000-0000-0000-0000-0000000000c2";
const ATTACH_FAILED_NEXT_ID: &str = "00000000-0000-0000-0000-0000000000c3";
const SSE_BODY: &str = "data: {\"id\":\"steer-test\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"done\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";

/// 读出一条完整的模型请求体；连接提前断开时返回 `None`。
async fn read_request(socket: &mut TcpStream) -> Option<Vec<u8>> {
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

fn sse_response() -> Vec<u8> {
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        SSE_BODY.len()
    );
    [header.as_bytes(), SSE_BODY.as_bytes()].concat()
}

/// 在成对通道之间记录请求类型；续跑仍由真实 `run_continuation_scheduler` 执行。
fn start_observed_scheduler(
    harness: &mut ActivationHarness,
    observed: Arc<std::sync::Mutex<Vec<bool>>>,
) {
    let mut receiver = harness.receiver.take().expect("scheduler receiver");
    let (forward_tx, forward_rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(request) = receiver.recv().await {
            observed.lock().unwrap().push(request.mq_steering);
            if forward_tx.send(request).is_err() {
                return;
            }
        }
    });
    harness
        .cfg
        .host_task_spawner
        .spawn(
            crate::host::task_scope::HostTaskOwnerKind::Host,
            crate::host::task_scope::HostTaskKind::ContinuationScheduler,
            crate::host::continuation::run_continuation_scheduler(
                forward_rx,
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
}

async fn wait_for_done_reason(harness: &ActivationHarness, reason: &str) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let done = harness
                .recorded
                .notifications()
                .iter()
                .any(|(method, params)| {
                    method == "peri/agent_event_done" && params["stopReason"] == reason
                });
            if done {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("必须等到 {reason} 收尾"));
}

fn done_reasons(harness: &ActivationHarness) -> Vec<String> {
    harness
        .recorded
        .notifications()
        .into_iter()
        .filter(|(method, _)| method == "peri/agent_event_done")
        .map(|(_, params)| {
            params["stopReason"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// G9 真回归：managed turn 正常收尾必须 disarm 预留 guard，否则 `fail_reserved`
/// 会撤销本轮期间 steer 新放行的发送选择，续跑被 scheduler 的 `mq_allowed` 复查丢弃。
#[tokio::test]
#[serial]
async fn steer_managed_turn_schedules_continuation_without_second_dispatch() {
    // 首个模型请求挂起不写响应：dispatch 发生时 turn 必然在等模型流，而不是靠 sleep 猜时序。
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let (first_seen_tx, first_seen_rx) = tokio::sync::oneshot::channel::<()>();
    let (hold_release_tx, hold_release_rx) = tokio::sync::oneshot::channel::<()>();
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
                if read_request(&mut socket).await.is_none() {
                    return;
                }
                if let Some(first_seen) = first_seen {
                    let _ = first_seen.send(());
                }
                if let Some(hold_release) = hold_release {
                    // 挂起点：释放前不写任何响应，取消必须能打断这次请求。
                    let _ = hold_release.await;
                    return;
                }
                let _ = socket.write_all(&sse_response()).await;
            });
        }
    });

    let mut harness = ActivationHarness::with_base_url(&base_url).await;
    let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
    start_observed_scheduler(&mut harness, Arc::clone(&observed));

    let mailbox = crate::host::user_input::ensure_mailbox(
        &harness.session_id,
        &harness.cfg,
        &harness.transport,
    )
    .await
    .unwrap();
    let generation = mailbox.generation().to_owned();
    let running = mailbox
        .enqueue(&EnqueueUserInputRequest {
            session_id: harness.session_id.clone(),
            generation: generation.clone(),
            command_id: "enqueue-running".into(),
            input_id: RUNNING_ID.into(),
            content: MessageContent::text(RUNNING_TEXT),
            original_draft: RUNNING_TEXT.into(),
        })
        .await
        .expect("在途输入应入队");
    assert_eq!(running.results[0].state, UserInputState::Dispatching);

    // 生产 managed 入口（与 server_loop 一致）：会创建旧用例绕过的预留 guard。
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

    // 首次点击暂存区发送，之后不再有第二次点击。
    mailbox
        .enqueue(&EnqueueUserInputRequest {
            session_id: harness.session_id.clone(),
            generation: generation.clone(),
            command_id: "enqueue-steer-managed".into(),
            input_id: INPUT_ID.into(),
            content: MessageContent::text(STEER_TEXT),
            original_draft: STEER_TEXT.into(),
        })
        .await
        .expect("暂存区入队应被接受");
    let dispatched = mailbox
        .dispatch(&DispatchUserInputsRequest {
            session_id: harness.session_id.clone(),
            generation,
            command_id: "dispatch-steer-managed".into(),
            input_ids: vec![INPUT_ID.into()],
        })
        .await
        .expect("暂存区发送应被接受");
    assert_eq!(dispatched.results[0].state, UserInputState::Dispatching);

    wait_for_done_reason(&harness, "cancelled").await;

    let queue = harness
        .cfg
        .session_manager
        .v2_queue_for(&harness.session_id)
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let staged_once = {
                let history = harness
                    .cfg
                    .session_resources
                    .load_session_history(&harness.session_id)
                    .await
                    .expect("test history");
                history
                    .iter()
                    .filter(|payload| {
                        peri_acp_types::store::serialize_persisted_payload(payload)
                            .unwrap()
                            .contains(STEER_TEXT)
                    })
                    .count()
                    == 1
            };
            if staged_once && queue.is_empty().await.expect("test MQ len") {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("首次点击后必须自动续跑：MQ 清空且 steer 文本恰好写入历史一次");

    assert!(
        observed.lock().unwrap().iter().any(|steering| *steering),
        "收尾必须发出 mq_steering continuation"
    );
    let snapshot = mailbox.snapshot().await;
    assert!(
        snapshot
            .items
            .iter()
            .all(|item| item.input_id != INPUT_ID || item.state != UserInputState::Queued),
        "steer 输入不得被 fail_reserved 退回暂存区"
    );
    assert!(
        snapshot
            .terminal_items
            .iter()
            .any(|item| item.input_id == INPUT_ID && item.state == UserInputState::Delivered),
        "steer 输入必须交付"
    );
    // 第二个 done 由独立 event pump 发送，可能晚于历史落库；纳入等待后再断言完整序列。
    wait_for_done_reason(&harness, "end_turn").await;
    assert_eq!(
        done_reasons(&harness),
        ["cancelled", "end_turn"],
        "首轮被 steer 取消，暂存输入由收尾后的调度消费"
    );

    let _ = hold_release_tx.send(());
    server.abort();
    harness
        .cfg
        .session_manager
        .pre_close_session(&harness.session_id);
}

#[tokio::test]
#[serial]
async fn steer_dispatch_cancelled_turn_keeps_activation_and_consumes_input() {
    // 首个模型请求挂起不写响应：dispatch 发生时 turn 必然在等模型流，而不是靠 sleep 猜时序。
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let (first_seen_tx, first_seen_rx) = tokio::sync::oneshot::channel::<()>();
    let (hold_release_tx, hold_release_rx) = tokio::sync::oneshot::channel::<()>();
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
                if read_request(&mut socket).await.is_none() {
                    return;
                }
                if let Some(first_seen) = first_seen {
                    let _ = first_seen.send(());
                }
                if let Some(hold_release) = hold_release {
                    // 挂起点：释放前不写任何响应，取消必须能打断这次请求。
                    let _ = hold_release.await;
                    return;
                }
                let _ = socket.write_all(&sse_response()).await;
            });
        }
    });

    let mut harness = ActivationHarness::with_base_url(&base_url).await;
    // 本用例走非 managed 的 `PromptOrigin::User` 并手工驱动续跑，只覆盖 tail 发出的
    // 请求；预留 guard 与 scheduler 复查由上方 managed 用例覆盖。
    let mailbox = crate::host::user_input::ensure_mailbox(
        &harness.session_id,
        &harness.cfg,
        &harness.transport,
    )
    .await
    .unwrap();

    let sessions = harness.sessions.clone();
    let locks = harness.locks.clone();
    let transport = harness.transport.clone();
    let cfg = harness.cfg.clone();
    let sender = harness.sender.clone();
    let session_id = harness.session_id.clone();
    let prompt = tokio::spawn(async move {
        crate::host::dispatch_prompt_turn(
            json!({"sessionId": session_id, "message": {"role": "user", "content": "在途提问"}}),
            crate::host::PromptOrigin::User,
            None,
            &sessions,
            &locks,
            &transport,
            &cfg,
            &sender,
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(10), first_seen_rx)
        .await
        .expect("首轮必须发起模型请求")
        .expect("模型请求必须完整到达");
    assert!(
        harness
            .cfg
            .session_manager
            .get_session(&harness.session_id)
            .unwrap()
            .activation
            .is_allowed(),
        "turn 启动时必须已放开调度"
    );

    let generation = mailbox.generation().to_owned();
    let enqueued = mailbox
        .enqueue(&EnqueueUserInputRequest {
            session_id: harness.session_id.clone(),
            generation: generation.clone(),
            command_id: "enqueue-steer".into(),
            input_id: INPUT_ID.into(),
            content: MessageContent::text(STEER_TEXT),
            original_draft: STEER_TEXT.into(),
        })
        .await
        .expect("暂存区入队应被接受");
    assert_eq!(enqueued.results[0].state, UserInputState::Queued);
    let dispatched = mailbox
        .dispatch(&DispatchUserInputsRequest {
            session_id: harness.session_id.clone(),
            generation,
            command_id: "dispatch-steer".into(),
            input_ids: vec![INPUT_ID.into()],
        })
        .await
        .expect("暂存区发送应被接受");
    assert_eq!(dispatched.results[0].state, UserInputState::Dispatching);

    let response = tokio::time::timeout(Duration::from_secs(10), prompt)
        .await
        .expect("steer 必须结束在途 turn")
        .unwrap()
        .unwrap();
    assert_eq!(response["stopReason"], "cancelled");
    let _ = hold_release_tx.send(());

    // 缺陷点：steer 取消曾把 activation 永久压成 suppress，收尾后无人再放开。
    assert!(
        crate::host::activation::mq_allowed(&harness.cfg, &harness.session_id).await,
        "steer 取消不能抑制后续 MQ 调度"
    );

    let request = tokio::time::timeout(
        Duration::from_secs(10),
        harness.receiver.as_mut().unwrap().recv(),
    )
    .await
    .expect("收尾后必须发出 mq_steering continuation")
    .expect("continuation channel 必须保持打开");
    assert!(request.mq_steering);

    let queue = harness
        .cfg
        .session_manager
        .v2_queue_for(&harness.session_id)
        .unwrap();
    assert!(
        !queue.is_empty().await.expect("test MQ len"),
        "continuation 之前输入仍是待发事实"
    );

    let epoch = {
        let mut sessions = harness.sessions.lock().await;
        crate::host::continuation::take_continuation_for_request(
            sessions.get_mut(&harness.session_id).unwrap(),
            &request,
        )
        .expect("continuation 必须能取得本轮 epoch")
    };
    let continuation = crate::host::dispatch_prompt_turn(
        json!({"sessionId": harness.session_id, "message": {"role": "user", "content": []}}),
        crate::host::PromptOrigin::Continuation {
            mq_steering: true,
            pending_result: false,
        },
        Some(epoch),
        &harness.sessions,
        &harness.locks,
        &harness.transport,
        &harness.cfg,
        &harness.sender,
    )
    .await
    .unwrap();
    assert_eq!(continuation["stopReason"], "end_turn");
    assert!(queue.is_empty().await.expect("test MQ len"));

    let history = harness
        .cfg
        .session_resources
        .load_session_history(&harness.session_id)
        .await
        .unwrap();
    let occurrences = history
        .iter()
        .filter(|payload| {
            peri_acp_types::store::serialize_persisted_payload(payload)
                .unwrap()
                .contains(STEER_TEXT)
        })
        .count();
    assert_eq!(occurrences, 1, "暂存输入只能进入历史一次");

    server.abort();
    harness
        .cfg
        .session_manager
        .pre_close_session(&harness.session_id);
}

/// P1-2 回归：`run_prompt` 在 attach 失败时返回 `Ok(Value::Null)`，此时 active
/// 仍属本 ticket；外层不得 disarm，必须让 Drop 的 `fail_reserved` 清理后再接受新输入。
#[tokio::test]
#[serial]
async fn failed_attach_keeps_drop_fallback_and_allows_next_dispatch() {
    let harness = ActivationHarness::new().await;
    let mailbox = crate::host::user_input::ensure_mailbox(
        &harness.session_id,
        &harness.cfg,
        &harness.transport,
    )
    .await
    .unwrap();
    let generation = mailbox.generation().to_owned();

    // 运行中输入已放行：无 active 时 enqueue 直接进入 Dispatching/ready。
    let running = mailbox
        .enqueue(&EnqueueUserInputRequest {
            session_id: harness.session_id.clone(),
            generation: generation.clone(),
            command_id: "enqueue-attach-failure".into(),
            input_id: ATTACH_FAILED_RUNNING_ID.into(),
            content: MessageContent::text(RUNNING_TEXT),
            original_draft: RUNNING_TEXT.into(),
        })
        .await
        .expect("运行中输入应入队");
    assert_eq!(running.results[0].state, UserInputState::Dispatching);

    // 持锁让 schedule_mailbox 的任务停在 attach 之前，保证随后的暂停必然命中 attach。
    let prompt_lock = Arc::new(tokio::sync::Mutex::new(()));
    let held = Arc::clone(&prompt_lock).lock_owned().await;
    {
        let mut locks = harness.locks.lock().await;
        locks.insert(harness.session_id.clone(), prompt_lock);
    }
    crate::host::user_input::schedule_mailbox(
        &harness.session_id,
        &harness.sessions,
        &harness.locks,
        &harness.cfg,
        &harness.transport,
        &harness.sender,
    );

    // T1 仍在等锁、尚未 attach。先替它绑定取消令牌再 Stop：Stop 只清掉没有
    // 令牌的 active，T1 因此留在 active 上，随后的 attach 必然因 paused 失败。
    let ticket = mailbox
        .active_run_ticket()
        .expect("schedule_mailbox 应已预留 T1");
    assert!(
        mailbox
            .attach_attempt(&ticket, tokio_util::sync::CancellationToken::new())
            .await,
        "T1 首次 attach 必须成功"
    );
    mailbox.stop().await;
    let revision_before_release = mailbox.snapshot().await.revision;
    drop(held);

    // Drop 兜底会再次 fail_reserved（active 属于 T1）并清空 active。
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if mailbox.snapshot().await.revision > revision_before_release {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("未结算的尝试必须由 Drop 清理 active，而不是永久残留");
    assert!(
        !harness
            .recorded
            .notifications()
            .iter()
            .any(|(method, _)| method == "peri/agent_event_done"),
        "attach 失败必须走 Ok(Value::Null)，不应触发 Err 的 error done"
    );

    // T1 失败后立即 dispatch B，再入队 C：B 必须能重新预留执行（C 仍在待发区）。
    // A 仍是队首，入队只恢复其执行资格；B 需要显式发送才进入 ready。
    let steered = mailbox
        .enqueue(&EnqueueUserInputRequest {
            session_id: harness.session_id.clone(),
            generation: generation.clone(),
            command_id: "enqueue-after-failure".into(),
            input_id: ATTACH_FAILED_STEER_ID.into(),
            content: MessageContent::text(STEER_TEXT),
            original_draft: STEER_TEXT.into(),
        })
        .await
        .expect("失败后新输入应入队");
    assert_eq!(steered.results[0].state, UserInputState::Queued);
    let dispatched = mailbox
        .dispatch(&DispatchUserInputsRequest {
            session_id: harness.session_id.clone(),
            generation: generation.clone(),
            command_id: "dispatch-after-failure".into(),
            input_ids: vec![ATTACH_FAILED_STEER_ID.into()],
        })
        .await
        .expect("失败后仍可显式发送");
    assert_eq!(dispatched.results[0].state, UserInputState::Dispatching);

    let queued = mailbox
        .enqueue(&EnqueueUserInputRequest {
            session_id: harness.session_id.clone(),
            generation,
            command_id: "enqueue-after-failure-tail".into(),
            input_id: ATTACH_FAILED_NEXT_ID.into(),
            content: MessageContent::text("失败后排队"),
            original_draft: "失败后排队".into(),
        })
        .await
        .expect("后续普通输入应入队");
    assert_eq!(queued.results[0].state, UserInputState::Queued);

    assert!(
        mailbox.reserve_run().is_some(),
        "失败 turn 清理 active 后必须能再次预留执行"
    );

    harness
        .cfg
        .session_manager
        .pre_close_session(&harness.session_id);
}
