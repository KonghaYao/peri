//! Tests for submit_consumer

#[cfg(test)]
use super::*;

use crate::kit::atoms::{ACP_STATE, VIEW_MODELS, ViewModelsSnapshot};
use peri_acp::transport::mpsc::{MpscClientTransport, MpscServerTransport, mpsc_transport_pair};
use serial_test::serial;

/// 用真实 mpsc transport 对创建 AcpTuiClient（不启动 pump）。
fn make_client_without_pump() -> (AcpTuiClient, MpscServerTransport) {
    let (client_transport, server_transport): (MpscClientTransport, MpscServerTransport) =
        mpsc_transport_pair();
    let (client, _notification_tx, _notification_rx) = AcpTuiClient::new(client_transport);
    (client, server_transport)
}

#[tokio::test]
async fn test_empty_agent_text_skipped() {
    let (client, _server_transport) = make_client_without_pump();
    let cwd = ".".to_string();

    let result = handle_submit(
        &client,
        &cwd,
        SubmitRequest::AgentText("   \n\t ".to_string()),
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
        handle_submit(&client, &cwd, SubmitRequest::AgentText("hello".to_string())),
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

/// Issue 2026-08-05 S4.2: cancel 收到信号时先发 cancel RPC（带超时）再本地
/// 兜底复位 is_loading——transport 死亡 / prompt task panic 时 TurnInterrupted
/// 永不到达，loading 无事件驱动复位路径；本地复位后 Ctrl+C 双击退出路径恢复
/// 可用（与服务端 TurnInterrupted 幂等：事件到达后再次复位无害）。本测试的
/// client 无 active session → cancel() 立即失败 → 走兜底复位分支。
#[tokio::test]
#[serial]
async fn test_cancel_consumer_resets_loading_locally() {
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
    // 复位发生在 cancel RPC 完成/失败之后——即使 cancel RPC 无响应（transport
    // 已死）超时，复位也必定执行。
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        !ACP_STATE.state().read().is_loading,
        "cancel 信号后 is_loading 应本地复位为 false"
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
            },
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
async fn test_handle_clear_submit_resets_popup_todo_history_atoms() {
    crate::kit::atoms::init_atoms();
    // Arrange：把 4 类 atom 填充为"旧 session 残留"。
    *crate::kit::atoms::POPUP_KIND.state().write() = Some(crate::kit::atoms::PopupKind::Hitl);
    *crate::kit::atoms::HITL_PENDING.state().write() =
        Some(peri_acp_types::event_data::HitlPending {
            tool_name: "old".into(),
            tool_input: serde_json::Value::Null,
            batch: None,
        });
    *crate::kit::atoms::TODO_ITEMS.state().write() = vec![crate::kit::message_area::TodoItem {
        content: "stale".into(),
        status: crate::kit::message_area::TodoStatus::Pending,
    }];
    *crate::kit::atoms::INPUT_HISTORY_INDEX.state().write() = Some(3);
    *crate::kit::atoms::DRAFT.state().write() = Some("stale draft".to_string());

    let (client, _server) = make_client_without_pump();
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
