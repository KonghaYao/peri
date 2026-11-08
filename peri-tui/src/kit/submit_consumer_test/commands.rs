//! 远程命令、KeepGoing、清空与视图命令回归。

use super::super::*;
use super::support::*;

use crate::kit::atoms::{VIEW_MODELS, ViewModelsSnapshot};
use peri_acp::transport::AcpTransport;
use serde_json::json;
use serial_test::serial;

/// [回归 review] 新式服务端不占车道，但无会话时的远程命令仍须建立会话并投递，
/// 不能被当作「既无绑定也无 cohort」直接取消；建立期间切换则取消（决策守卫）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn test_remote_command_without_session_creates_it() {
    crate::kit::atoms::init_atoms();
    let previous_commands = crate::kit::atoms::AVAILABLE_SLASH_COMMANDS
        .state()
        .read()
        .clone();
    let previous_window_capable = crate::kit::atoms::HISTORY_WINDOW_CAPABLE.get();
    let (client, server) = make_client_without_pump();
    let negotiator = client.clone();
    let negotiate = tokio::spawn(async move { negotiator.register_ui_commands(&[]).await });
    let (init_id, _) = expect_request(&server, "initialize").await;
    server
        .send_response(
            init_id,
            Ok(json!({"agentCapabilities": {"_meta": {"peri.userInputQueue": true}}})),
        )
        .await
        .unwrap();
    negotiate.await.unwrap().unwrap();
    assert!(client.supports_user_input_queue());
    *crate::kit::atoms::AVAILABLE_SLASH_COMMANDS.state().write() =
        vec![crate::kit::slash_projection::SlashCommandEntry {
            fullname: "demo:remote".into(),
            level: 2,
            ..Default::default()
        }];

    let (tx, rx) = mpsc::unbounded_channel::<SubmitRequest>();
    let shutdown = CancellationToken::new();
    let consumer = spawn_submit_consumer(client.clone(), rx, ".".into(), shutdown.clone());

    tx.send(agent_text("/demo:remote")).unwrap();
    let (new_id, _) = expect_request(&server, "session/new").await;
    server
        .send_response(new_id, Ok(json!({"sessionId": "remote-session"})))
        .await
        .unwrap();
    // 新式服务端在会话建立后拉取输入快照，之后才发送 prompt。
    let (snapshot_id, _) = expect_request(&server, "session/input/snapshot").await;
    server
        .send_response(
            snapshot_id,
            Ok(json!({"sessionId": "remote-session", "generation": "g", "revision": 1, "items": []})),
        )
        .await
        .unwrap();

    let (_prompt_id, session_id, text) = expect_prompt(&server).await;
    assert_eq!(
        (session_id.as_str(), text.as_str()),
        ("remote-session", "/demo:remote"),
        "远程命令必须落到新建会话，而不是被取消"
    );

    *crate::kit::atoms::AVAILABLE_SLASH_COMMANDS.state().write() = previous_commands;
    crate::kit::atoms::HISTORY_WINDOW_CAPABLE.set(previous_window_capable);
    shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_millis(500), consumer).await;
}

/// [回归 review] 服务端自管输入队列时，KeepGoing 必须直达挂起注入路径，
/// 不得排在一条挂起未返回的 prompt 之后。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn test_keepgoing_bypasses_lane_when_server_queues_input() {
    let previous_window_capable = crate::kit::atoms::HISTORY_WINDOW_CAPABLE.get();
    let (client, server) = make_client_without_pump();
    let negotiator = client.clone();
    let negotiate = tokio::spawn(async move { negotiator.register_ui_commands(&[]).await });
    let (init_id, _) = expect_request(&server, "initialize").await;
    server
        .send_response(
            init_id,
            Ok(json!({"agentCapabilities": {"_meta": {"peri.userInputQueue": true}}})),
        )
        .await
        .unwrap();
    negotiate.await.unwrap().unwrap();
    assert!(client.supports_user_input_queue());

    client.force_stable_for_test("session-a", false);
    let (tx, rx) = mpsc::unbounded_channel::<SubmitRequest>();
    let shutdown = CancellationToken::new();
    let consumer = spawn_submit_consumer(client.clone(), rx, ".".into(), shutdown.clone());

    // 第一条 keepgoing 故意不返回：模拟 suspended turn 上仍挂着的 prompt RPC。
    tx.send(SubmitRequest::KeepGoing).unwrap();
    let (first_id, first_session, first_text) = expect_prompt(&server).await;
    assert_eq!(first_session, "session-a");
    assert_eq!(first_text, "");

    tx.send(SubmitRequest::KeepGoing).unwrap();
    let (second_id, second_session, second_text) =
        tokio::time::timeout(Duration::from_millis(500), expect_prompt(&server))
            .await
            .expect("KeepGoing 不得等待挂起的 prompt 完成");
    assert_eq!(second_session, "session-a");
    assert_eq!(second_text, "");

    server
        .send_response(second_id, Ok(json!({"stopReason": "end_turn"})))
        .await
        .unwrap();
    server
        .send_response(first_id, Ok(json!({"stopReason": "end_turn"})))
        .await
        .unwrap();

    crate::kit::atoms::HISTORY_WINDOW_CAPABLE.set(previous_window_capable);
    shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_millis(500), consumer).await;
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
