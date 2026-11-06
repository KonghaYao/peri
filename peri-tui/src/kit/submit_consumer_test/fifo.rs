//! FIFO 与首次建会话时序回归。

use super::super::*;
use super::support::*;

use peri_acp::transport::AcpTransport;
use peri_acp::transport::types::AcpError;
use serde_json::json;
use serial_test::serial;
use std::time::Duration;

/// [回归 #210] TurnDone 排空旧队列后，10 条消息在模型侧必须按入队顺序提交。
///
/// 用多线程 runtime：真实 TUI 是多线程，spawn 的每条提交会被不同 worker
/// 取走，单线程 runtime 掩盖了并发提交的乱序窗口。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_drained_input_buffer_prompts_preserve_fifo() {
    let (client, server) = make_client_without_pump();
    client.force_stable_for_test("fifo-session", false);
    let (tx, rx) = mpsc::unbounded_channel::<SubmitRequest>();
    let shutdown = CancellationToken::new();
    let consumer = spawn_submit_consumer(client, rx, ".".into(), shutdown.clone());

    for index in 0..10 {
        tx.send(SubmitRequest::AgentText {
            text: format!("m{index}"),
            attachments: Vec::new(),
        })
        .unwrap();
    }

    let received = tokio::time::timeout(Duration::from_secs(5), async {
        let mut texts = Vec::new();
        for _ in 0..10 {
            let (id, _, text) = expect_prompt(&server).await;
            texts.push(text);
            server
                .send_response(id, Ok(json!({"stopReason": "end_turn"})))
                .await
                .unwrap();
        }
        texts
    })
    .await
    .expect("all prompts must be submitted");

    let expected: Vec<String> = (0..10).map(|index| format!("m{index}")).collect();
    assert_eq!(received, expected, "prompt 提交顺序必须与入队顺序一致");
    shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_millis(500), consumer).await;
}

/// [回归 #210] 前一条 prompt RPC 失败不得吞掉后续消息。
#[tokio::test]
async fn test_failed_prompt_does_not_block_following_prompts() {
    let (client, server) = make_client_without_pump();
    client.force_stable_for_test("fifo-session", false);
    let (tx, rx) = mpsc::unbounded_channel::<SubmitRequest>();
    let shutdown = CancellationToken::new();
    let consumer = spawn_submit_consumer(client, rx, ".".into(), shutdown.clone());

    for index in 0..3 {
        tx.send(SubmitRequest::AgentText {
            text: format!("m{index}"),
            attachments: Vec::new(),
        })
        .unwrap();
    }

    let (first_id, _, first_text) = expect_prompt(&server).await;
    assert_eq!(first_text, "m0");
    server
        .send_response(first_id, Err(AcpError::new(-32000, "boom")))
        .await
        .unwrap();

    let received = tokio::time::timeout(Duration::from_secs(5), async {
        let mut texts = Vec::new();
        for _ in 0..2 {
            let (id, _, text) = expect_prompt(&server).await;
            texts.push(text);
            server
                .send_response(id, Ok(json!({"stopReason": "end_turn"})))
                .await
                .unwrap();
        }
        texts
    })
    .await
    .expect("后续 prompt 必须继续提交");

    assert_eq!(received, vec!["m1".to_string(), "m2".to_string()]);
    shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_millis(500), consumer).await;
}

/// [回归 #210] 第一条 prompt 未返回时，同会话第二条不得越过它提交。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_pending_prompt_blocks_same_session_successor() {
    let (client, server) = make_client_without_pump();
    client.force_stable_for_test("fifo-session", false);
    let (tx, rx) = mpsc::unbounded_channel::<SubmitRequest>();
    let shutdown = CancellationToken::new();
    let consumer = spawn_submit_consumer(client, rx, ".".into(), shutdown.clone());

    for index in 0..2 {
        tx.send(SubmitRequest::AgentText {
            text: format!("m{index}"),
            attachments: Vec::new(),
        })
        .unwrap();
    }

    let (first_id, _, first_text) = expect_prompt(&server).await;
    assert_eq!(first_text, "m0");
    let early = tokio::time::timeout(Duration::from_millis(300), server.recv()).await;
    assert!(
        early.is_err(),
        "第一条 prompt 未完成时，同会话第二条不得提交: {early:?}"
    );

    server
        .send_response(first_id, Ok(json!({"stopReason": "end_turn"})))
        .await
        .unwrap();
    let (second_id, _, second_text) = expect_prompt(&server).await;
    assert_eq!(second_text, "m1");
    server
        .send_response(second_id, Ok(json!({"stopReason": "end_turn"})))
        .await
        .unwrap();
    shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_millis(500), consumer).await;
}

/// [回归 #210] 旧会话的 prompt 未返回时，新会话的提交不得被拖住。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_pending_prompt_does_not_block_another_session() {
    let (client, server) = make_client_without_pump();
    client.force_stable_for_test("session-a", false);
    let (tx, rx) = mpsc::unbounded_channel::<SubmitRequest>();
    let shutdown = CancellationToken::new();
    let consumer = spawn_submit_consumer(client.clone(), rx, ".".into(), shutdown.clone());

    tx.send(SubmitRequest::AgentText {
        text: "a".to_string(),
        attachments: Vec::new(),
    })
    .unwrap();
    let (first_id, first_session, first_text) = expect_prompt(&server).await;
    assert_eq!(first_session, "session-a");
    assert_eq!(first_text, "a");

    // 第一条 prompt 故意不返回：切换到新会话后，第二条必须立即提交。
    client.force_stable_for_test("session-b", false);
    tx.send(SubmitRequest::AgentText {
        text: "b".to_string(),
        attachments: Vec::new(),
    })
    .unwrap();
    let (second_id, second_session, second_text) =
        tokio::time::timeout(Duration::from_secs(2), expect_prompt(&server))
            .await
            .expect("新会话的 prompt 不得等待旧会话 turn 结束");
    assert_eq!(second_session, "session-b");
    assert_eq!(second_text, "b");

    server
        .send_response(second_id, Ok(json!({"stopReason": "end_turn"})))
        .await
        .unwrap();
    server
        .send_response(first_id, Ok(json!({"stopReason": "end_turn"})))
        .await
        .unwrap();
    shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_millis(500), consumer).await;
}
/// [回归 P2] 首次建会话期间排队的消息，不得被建好后才入队的消息越过。
///
/// 延迟 `session/new`：m0/m1 在无会话时进入初始化车道；建好 S 后 m2 入队。
/// 旧实现把 m2 放进 Some(S) 车道，m0 的 TurnDone 排空时 m2 会越过 m1。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn test_initializing_lane_keeps_fifo_after_session_created() {
    let (client, server) = make_client_without_pump();
    let (tx, rx) = mpsc::unbounded_channel::<SubmitRequest>();
    let shutdown = CancellationToken::new();
    let consumer = spawn_submit_consumer(client.clone(), rx, ".".into(), shutdown.clone());

    tx.send(agent_text("m0")).unwrap();
    tx.send(agent_text("m1")).unwrap();

    // 初始化挂起：m0/m1 都已入队，但会话尚未建立。
    let (new_id, _) = expect_request(&server, "session/new").await;
    server
        .send_response(new_id, Ok(json!({"sessionId": "session-s"})))
        .await
        .unwrap();
    wait_for_session(&client, "session-s").await;
    // 建好后第三条入队：必须继续排在 m1 之后。
    tx.send(agent_text("m2")).unwrap();

    let (first_id, first_session, first_text) = expect_prompt(&server).await;
    assert_eq!(
        (first_session.as_str(), first_text.as_str()),
        ("session-s", "m0")
    );
    let overlap = tokio::time::timeout(Duration::from_millis(250), server.recv()).await;
    assert!(
        overlap.is_err(),
        "m0 未完成时不得出现下一条 prompt: {overlap:?}"
    );
    server
        .send_response(first_id, Ok(json!({"stopReason": "end_turn"})))
        .await
        .unwrap();

    let (second_id, second_session, second_text) = expect_prompt(&server).await;
    assert_eq!(
        (second_session.as_str(), second_text.as_str()),
        ("session-s", "m1")
    );
    let overlap = tokio::time::timeout(Duration::from_millis(250), server.recv()).await;
    assert!(overlap.is_err(), "m2 不得越过仍在排队的 m1: {overlap:?}");
    server
        .send_response(second_id, Ok(json!({"stopReason": "end_turn"})))
        .await
        .unwrap();

    let (third_id, third_session, third_text) = expect_prompt(&server).await;
    assert_eq!(
        (third_session.as_str(), third_text.as_str()),
        ("session-s", "m2")
    );
    server
        .send_response(third_id, Ok(json!({"stopReason": "end_turn"})))
        .await
        .unwrap();

    shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_millis(500), consumer).await;
}

/// [回归 review #359] 首次建立会话从 NoSession 进入 Transitioning 后，m1 仍须
/// 与 m0 同车道：m0 的 prompt 未返回时，wire 上不得出现 m1。
///
/// 用 registration probe 确认 m1 已被 consumer 接收并完成分道，而不是靠睡眠
/// 猜测时序。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn test_establishment_keeps_single_lane_while_transitioning() {
    let (client, server) = make_client_without_pump();
    let (registration_tx, mut registration_rx) = mpsc::unbounded_channel();
    admission_probe::install_registration_probe(registration_tx);

    let (tx, rx) = mpsc::unbounded_channel::<SubmitRequest>();
    let shutdown = CancellationToken::new();
    let consumer = spawn_submit_consumer(client.clone(), rx, ".".into(), shutdown.clone());

    tx.send(agent_text("establish-m0")).unwrap();
    let (new_id, _) = expect_request(&server, "session/new").await;

    // route 已是 Transitioning(g+1)：m1 到达 consumer 后必须并入 m0 的车道。
    tx.send(agent_text("establish-m1")).unwrap();
    let m0_lane = next_registration(&mut registration_rx, "establish-m0").await;
    let m1_lane = next_registration(&mut registration_rx, "establish-m1").await;
    assert_eq!(m0_lane, m1_lane, "同一次建立必须在单一有序车道上");

    server
        .send_response(new_id, Ok(json!({"sessionId": "session-s"})))
        .await
        .unwrap();
    let (m0_id, m0_session, m0_text) = expect_prompt(&server).await;
    assert_eq!(
        (m0_session.as_str(), m0_text.as_str()),
        ("session-s", "establish-m0")
    );

    // m0 prompt 保持未响应：m1 不得出现在 wire 上。
    let overlap = tokio::time::timeout(Duration::from_millis(400), server.recv()).await;
    assert!(
        overlap.is_err(),
        "m0 prompt 未响应时 m1 不得越过它: {overlap:?}"
    );

    server
        .send_response(m0_id, Ok(json!({"stopReason": "end_turn"})))
        .await
        .unwrap();
    let (m1_id, m1_session, m1_text) = expect_prompt(&server).await;
    assert_eq!(
        (m1_session.as_str(), m1_text.as_str()),
        ("session-s", "establish-m1")
    );
    server
        .send_response(m1_id, Ok(json!({"stopReason": "end_turn"})))
        .await
        .unwrap();

    admission_probe::clear_registration_probe();
    shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_millis(500), consumer).await;
}

async fn next_registration(
    rx: &mut mpsc::UnboundedReceiver<(String, LaneKey)>,
    expected: &str,
) -> LaneKey {
    loop {
        let (label, lane) = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("registration probe must report both submits")
            .expect("registration probe channel must stay open");
        if label == expected {
            return lane;
        }
    }
}
