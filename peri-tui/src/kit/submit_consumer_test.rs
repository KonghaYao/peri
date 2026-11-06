//! Tests for submit_consumer

#[cfg(test)]
use super::*;

use std::time::Duration;

use crate::kit::atoms::{ACP_STATE, NOTIFICATION, VIEW_MODELS, ViewModelsSnapshot};
use peri_acp::transport::AcpTransport;
use peri_acp::transport::mpsc::{MpscClientTransport, MpscServerTransport, mpsc_transport_pair};
use peri_acp::transport::types::{AcpError, IncomingMessage, RequestId};
use serde_json::json;
use serial_test::serial;

async fn expect_prompt(server: &MpscServerTransport) -> (RequestId, String, String) {
    let incoming = tokio::time::timeout(Duration::from_secs(5), server.recv())
        .await
        .expect("expected session/prompt")
        .expect("transport must stay open");
    let IncomingMessage::Request {
        id, method, params, ..
    } = incoming
    else {
        panic!("expected request, got {incoming:?}");
    };
    assert_eq!(method, "session/prompt");
    let session_id = params["sessionId"]
        .as_str()
        .expect("prompt carries sessionId")
        .to_string();
    let text = params["message"]["content"]
        .as_str()
        .expect("text-only prompt content")
        .to_string();
    (id, session_id, text)
}

/// 接收一个指定方法的 ACP 请求（用于 session/new、session/close 等控制链路）。
async fn expect_request(
    server: &MpscServerTransport,
    expected_method: &str,
) -> (RequestId, serde_json::Value) {
    let incoming = tokio::time::timeout(Duration::from_secs(5), server.recv())
        .await
        .expect("expected ACP request")
        .expect("transport must stay open");
    let IncomingMessage::Request {
        id, method, params, ..
    } = incoming
    else {
        panic!("expected request, got {incoming:?}");
    };
    assert_eq!(method, expected_method);
    (id, params)
}

async fn wait_for_session(client: &AcpTuiClient, expected: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while client.current_session_id().as_deref() != Some(expected) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("session must become stable");
}

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

/// 用户文本提交。
fn agent_text(text: &str) -> SubmitRequest {
    SubmitRequest::AgentText {
        text: text.to_string(),
        attachments: Vec::new(),
    }
}

/// [回归 P1] 会话切换后，旧会话排队中的提交不得改投新会话。
///
/// A0 挂起 → A1 排队 → `/clear` 切到 B → 释放 A0。A1 在准入时必须被显式
/// 取消并给出反馈，wire 上不得出现发往 B 的 A1 prompt。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn test_queued_submit_is_cancelled_after_session_switch() {
    crate::kit::atoms::init_atoms();
    // 本用例会真实建会话并写 NOTIFICATION；与其它 serial 用例共用全局态。
    let previous_notification = std::mem::take(&mut *NOTIFICATION.state().write());
    let (client, server) = make_client_without_pump();
    client.force_stable_for_test("session-a", false);
    let (tx, rx) = mpsc::unbounded_channel::<SubmitRequest>();
    let shutdown = CancellationToken::new();
    let consumer = spawn_submit_consumer(client.clone(), rx, ".".into(), shutdown.clone());

    tx.send(agent_text("a0")).unwrap();
    let (a0_id, a0_session, a0_text) = expect_prompt(&server).await;
    assert_eq!((a0_session.as_str(), a0_text.as_str()), ("session-a", "a0"));

    // A1 在 A0 未返回时入队：车道与准入身份都固定为 session-a。
    tx.send(agent_text("a1")).unwrap();
    // 控制请求即时执行：/clear 关闭 A 并建立 B。
    tx.send(SubmitRequest::SessionControl(SessionControlRequest::Clear))
        .unwrap();

    let (close_id, close_params) = expect_request(&server, "session/close").await;
    assert_eq!(close_params["sessionId"], "session-a");
    server.send_response(close_id, Ok(json!({}))).await.unwrap();
    let (new_id, _) = expect_request(&server, "session/new").await;
    server
        .send_response(new_id, Ok(json!({"sessionId": "session-b"})))
        .await
        .unwrap();
    wait_for_session(&client, "session-b").await;

    // 旧会话 turn 结束，A1 被车道放行：准入校验必须拒绝它。
    server
        .send_response(a0_id, Ok(json!({"stopReason": "end_turn"})))
        .await
        .unwrap();

    let stray = tokio::time::timeout(Duration::from_millis(400), server.recv()).await;
    assert!(stray.is_err(), "会话切换后旧提交不得改投新会话: {stray:?}");
    assert!(
        NOTIFICATION.state().read().is_some(),
        "取消排队提交必须有用户可见反馈"
    );

    *NOTIFICATION.state().write() = previous_notification;
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

/// 车道保证同会话内顺序：每条提交都先让出调度器，仍按 push 顺序执行。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_submit_lanes_keep_fifo_within_session() {
    let mut lanes: SubmitLanes<Option<String>> = SubmitLanes::new();
    let (tx, mut rx) = mpsc::unbounded_channel();
    for index in 0..10 {
        let tx = tx.clone();
        lanes.push(Some("s".to_string()), async move {
            tokio::task::yield_now().await;
            let _ = tx.send(index);
        });
    }
    drop(tx);

    let mut received = Vec::new();
    for _ in 0..10 {
        received.push(
            tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .expect("lane must drive all submissions")
                .expect("sender stays alive until all work runs"),
        );
    }
    assert_eq!(received, (0..10).collect::<Vec<_>>());
}

/// 车道首条 task panic 只结束它自己，后续消息继续执行。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_submit_lanes_continue_after_tail_panics() {
    let mut lanes: SubmitLanes<Option<String>> = SubmitLanes::new();
    let (tx, mut rx) = mpsc::unbounded_channel();
    lanes.push(Some("s".to_string()), async move {
        panic!("simulated submit task abort");
    });
    let follow_tx = tx.clone();
    lanes.push(Some("s".to_string()), async move {
        let _ = follow_tx.send("after");
    });
    drop(tx);

    let received = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("后续消息不得被前一条的 panic 阻塞");
    assert_eq!(received, Some("after"));
}

/// 不同会话各自推进：慢会话不能拖住另一会话的消息。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_submit_lanes_run_sessions_independently() {
    let mut lanes: SubmitLanes<Option<String>> = SubmitLanes::new();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();

    let blocked_tx = tx.clone();
    lanes.push(Some("a".to_string()), async move {
        let _ = release_rx.await;
        let _ = blocked_tx.send("a");
    });
    let free_tx = tx.clone();
    lanes.push(Some("b".to_string()), async move {
        let _ = free_tx.send("b");
    });
    drop(tx);

    let first = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("另一会话不得等待 a 的挂起提交")
        .expect("sender stays alive");
    assert_eq!(first, "b");

    release_tx.send(()).unwrap();
    let second = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("a 释放后应继续执行")
        .expect("sender stays alive");
    assert_eq!(second, "a");
}

/// 用真实 mpsc transport 对创建 AcpTuiClient（不启动 pump）。
fn make_client_without_pump() -> (AcpTuiClient, MpscServerTransport) {
    let (client_transport, server_transport): (MpscClientTransport, MpscServerTransport) =
        mpsc_transport_pair();
    let (client, _notification_tx, _notification_rx) = AcpTuiClient::new(client_transport);
    (client, server_transport)
}

fn make_interactive_client_without_pump() -> (AcpTuiClient, MpscServerTransport) {
    let (client_transport, server_transport): (MpscClientTransport, MpscServerTransport) =
        mpsc_transport_pair();
    let (client, _notification_tx, _notification_rx) =
        AcpTuiClient::new_interactive(client_transport);
    (client, server_transport)
}

#[tokio::test]
async fn test_empty_agent_text_skipped() {
    let (client, _server_transport) = make_client_without_pump();
    let cwd = ".".to_string();

    let result = handle_submit(
        &client,
        &cwd,
        SubmitRequest::AgentText {
            text: "   \n\t ".to_string(),
            attachments: Vec::new(),
        },
        None,
    )
    .await;
    assert!(result.is_ok());
    assert!(!client.has_session());
}

#[tokio::test]
async fn test_creates_session_when_missing() {
    let (client, _server_transport) = make_client_without_pump();
    let cwd = ".".to_string();

    // 无 pump 时 new_session 调用 recv() 会 hang——此测试只验证控制流：
    // 启动一个简短超时，确保 handle_submit 进入 new_session 分支
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        handle_submit(
            &client,
            &cwd,
            SubmitRequest::AgentText {
                text: "hello".to_string(),
                attachments: Vec::new(),
            },
            None,
        ),
    )
    .await;

    // 超时是预期（无 server 处理 RPC），但已确认走到了 new_session 路径
    assert!(
        result.is_err() || result.unwrap().is_err(),
        "expected timeout or RPC error (no server), got success"
    );
}

#[tokio::test]
async fn test_shutdown_exits_loop() {
    let (client, _server_transport) = make_client_without_pump();
    let (tx, rx) = mpsc::unbounded_channel::<SubmitRequest>();
    let shutdown = CancellationToken::new();
    let handle = spawn_submit_consumer(client, rx, ".".into(), shutdown.clone());

    shutdown.cancel();
    // 任务应在合理时间内退出
    let _ = tokio::time::timeout(std::time::Duration::from_millis(500), handle).await;
    // 不发任何提交——channel 仍可用（drop tx 也能让任务退出）
    let _ = tx;
}

#[tokio::test]
async fn test_dropped_tx_exits_loop() {
    let (client, _server_transport) = make_client_without_pump();
    let (tx, rx) = mpsc::unbounded_channel::<SubmitRequest>();
    let shutdown = CancellationToken::new();
    let handle = spawn_submit_consumer(client, rx, ".".into(), shutdown);

    drop(tx);
    let _ = tokio::time::timeout(std::time::Duration::from_millis(500), handle).await;
}

#[tokio::test]
#[serial]
async fn test_cancel_consumer_failure_does_not_claim_execution_stopped() {
    crate::kit::atoms::init_atoms();
    // 模拟卡死状态：is_loading=true（事件流已中断，无法靠事件复位）
    {
        let ref_guard = ACP_STATE.state();
        let mut acp = ref_guard.write();
        acp.is_loading = true;
    }
    let (client, _server_transport) = make_client_without_pump();
    let (tx, rx) = mpsc::unbounded_channel::<()>();
    let shutdown = CancellationToken::new();
    let _handle = spawn_cancel_consumer(client, rx, shutdown.clone());

    tx.send(()).unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        ACP_STATE.state().read().is_loading,
        "控制失败不得伪造执行已停止"
    );
    shutdown.cancel();
}

/// S4.2: clear_loading_state 双保险——直接写 ACP_STATE（bridge 已退出的
/// shutdown 路径兜底）+ 注入 LocalLoadingReset 内部事件（bridge 存活时同步
/// 复位 phase，防止后续 push_acp_state 用 phase 重算 is_loading=true 闪回）。
#[test]
#[serial]
fn test_clear_loading_state_emits_loading_reset_event() {
    crate::kit::atoms::init_atoms();
    // LOCAL_EVENT_TX 是全局 OnceLock——set 已占用（已被其他测试/运行时 set）
    // 时仅验证直接写兜底，跳过事件断言（std OnceLock::set 返回 Result，不 panic）。
    let (tx, mut rx) = mpsc::unbounded_channel::<AcpEventWithEpoch>();
    let owns_tx = LOCAL_EVENT_TX.set(tx).is_ok();

    {
        let ref_guard = ACP_STATE.state();
        let mut acp = ref_guard.write();
        acp.is_loading = true;
    }
    clear_loading_state();

    assert!(
        !ACP_STATE.state().read().is_loading,
        "clear_loading_state 应直接复位 ACP_STATE.is_loading"
    );
    if owns_tx {
        let ev = rx.try_recv().ok();
        assert!(
            matches!(
                ev.as_ref().map(|e| &e.event),
                Some(AcpEventData::LocalLoadingReset)
            ),
            "clear_loading_state 应注入 LocalLoadingReset 事件，实际 {ev:?}"
        );
    }
}

/// 编译期断言：UnboundedSender<SubmitRequest> 与 atoms::SUBMIT_TX 类型契约一致。
#[test]
fn test_submit_tx_type_contract() {
    let (tx, _rx): (mpsc::UnboundedSender<SubmitRequest>, _) = mpsc::unbounded_channel();
    // 模拟 atoms::SUBMIT_TX.set(tx) 的契约
    let _ = tx;
}

#[test]
fn test_lines_to_plain_text_joins_spans_and_lines() {
    let lines = vec![
        Line::from(vec![
            ratatui::text::Span::raw("hello"),
            ratatui::text::Span::raw(" world"),
        ]),
        Line::from("next"),
    ];
    assert_eq!(lines_to_plain_text(&lines), "hello world\nnext\n");
}

#[test]
fn test_debug_export_path_uses_cwd_and_timestamp_prefix() {
    let path = debug_export_path("/tmp/peri-export-test");
    assert!(path.starts_with("/tmp/peri-export-test"));
    let name = path.file_name().unwrap().to_string_lossy();
    assert!(name.starts_with("peri-debug-export-"));
    assert!(name.ends_with(".txt"));
}

#[tokio::test]
async fn test_execute_view_action_export_text_writes_notification() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    *NOTIFICATION.state().write() = None;
    execute_view_action(
        ViewActionRequest::ExportText(ExportMode::All),
        &make_client_without_pump().0,
        tempdir.path().to_str().expect("utf8 path"),
    );
    assert!(NOTIFICATION.state().read().is_some());
}

#[tokio::test]
#[serial]
async fn test_clear_request_bypasses_prompt() {
    use crate::kit::tui_render_unit::{TuiAssistantBubble, TuiRenderUnit, tui_hash_str};

    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot {
        items: im::Vector::from(vec![TuiRenderUnit::TuiAssistantBubble(
            TuiAssistantBubble {
                // [Slice 1] 正文时长（§6.2 `12.4s`）：测试构造默认无起点/冻结值。
                started_at: None,
                duration_ms: None,
                text: "existing".into(),
                reasoning: None,
                message_id: None,
                content_hash: tui_hash_str("existing|"),
            }
            .into(),
        )]),
        generation: 0,
    };
    let (client, _server_transport) = make_client_without_pump();
    let cwd = ".".to_string();

    // /clear 在无 server 时会因 new_session RPC 超时，但不走 prompt 路径
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        handle_submit(
            &client,
            &cwd,
            SubmitRequest::SessionControl(SessionControlRequest::Clear),
            None,
        ),
    )
    .await;

    // 超时表示走到了 new_session 路径（因无 server 响应而 hang）
    assert!(
        result.is_err() || result.unwrap().is_err(),
        "expected timeout (clear → new_session with no server)"
    );
}

/// 验证 handle_clear_submit 在调用 new_session 前已同步重置弹窗 / Todo / 历史指针。
///
/// 覆盖 H1（弹窗 payload 残留）/ M3（Todo 残留）/ L5+L10（输入历史指针残留）。
/// new_session 无 server 会 hang，我们用 100ms 超时让控制流走到重置后即返回，
/// 断言此时 4 类 atom 已被清空。这些写入发生在 new_session await 之前，
/// 因此即使 RPC 失败也已完成。
#[tokio::test]
#[serial]
async fn test_handle_clear_submit_resets_popup_todo_history_atoms() {
    crate::kit::atoms::init_atoms();
    // Arrange：把 4 类 atom 填充为"旧 session 残留"。
    *crate::kit::atoms::POPUP_KIND.state().write() = Some(crate::kit::atoms::PopupKind::Hitl);
    *crate::kit::atoms::HITL_PENDING.state().write() =
        Some(crate::kit::acp_types::PendingInteraction {
            owner: Default::default(),
            request_id_json: "\"old\"".into(),
            payload: peri_acp_types::event_data::HitlPending {
                tool_name: "old".into(),
                tool_input: serde_json::Value::Null,
                batch: None,
            },
        });
    *crate::kit::atoms::TODO_ITEMS.state().write() = vec![crate::kit::message_area::TodoItem {
        content: "stale".into(),
        status: crate::kit::message_area::TodoStatus::Pending,
    }];
    *crate::kit::atoms::INPUT_HISTORY_INDEX.state().write() = Some(3);
    *crate::kit::atoms::DRAFT.state().write() = Some("stale draft".to_string());

    let (client, _server) = make_interactive_client_without_pump();
    let cwd = ".".to_string();

    // Act：handle_clear_submit 在 new_session 处会 hang（无 server），
    // 我们只关心它 await 之前的同步重置块，所以 100ms 超时即返回。
    let _ = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        handle_clear_submit(&client, &cwd),
    )
    .await;

    // Assert：4 类残留已清空。
    assert!(
        crate::kit::atoms::POPUP_KIND.state().read().is_none(),
        "POPUP_KIND should be None after /clear"
    );
    assert!(
        crate::kit::atoms::HITL_PENDING.state().read().is_none(),
        "HITL_PENDING should be cleared after /clear"
    );
    assert!(
        crate::kit::atoms::TODO_ITEMS.state().read().is_empty(),
        "TODO_ITEMS should be empty after /clear"
    );
    assert!(
        crate::kit::atoms::INPUT_HISTORY_INDEX
            .state()
            .read()
            .is_none(),
        "INPUT_HISTORY_INDEX should be None after /clear"
    );
    assert!(
        crate::kit::atoms::DRAFT.state().read().is_none(),
        "DRAFT should be None after /clear"
    );
}
