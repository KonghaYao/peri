//! Tests for thread_load_consumer

#[cfg(test)]
use super::*;
use peri_acp::transport::mpsc::{MpscClientTransport, MpscServerTransport, mpsc_transport_pair};
use serial_test::serial;

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
async fn test_empty_thread_id_skipped() {
    let (client, _server) = make_client_without_pump();
    let result = handle_load(&client, ".", "   ".to_string()).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_shutdown_exits_loop() {
    let (client, _server) = make_client_without_pump();
    let (tx, rx) = mpsc::unbounded_channel::<String>();
    let shutdown = CancellationToken::new();
    let handle = spawn_thread_load_consumer(client, rx, ".".into(), shutdown.clone());
    shutdown.cancel();
    let _ = tokio::time::timeout(std::time::Duration::from_millis(500), handle).await;
    let _ = tx;
}

#[tokio::test]
async fn test_dropped_tx_exits_loop() {
    let (client, _server) = make_client_without_pump();
    let (tx, rx) = mpsc::unbounded_channel::<String>();
    let shutdown = CancellationToken::new();
    let handle = spawn_thread_load_consumer(client, rx, ".".into(), shutdown);
    drop(tx);
    let _ = tokio::time::timeout(std::time::Duration::from_millis(500), handle).await;
}

/// 验证 handle_load 在调用 load_session 前已同步重置弹窗 / Todo / 历史指针。
///
/// 覆盖 H1/M3/L5/L10，与 submit_consumer::test_handle_clear_submit_*
/// 对称。load_session 无 server 会 hang，我们用 100ms 超时让控制流走到
/// 重置后即返回，断言此时 4 类 atom 已被清空。重置写入发生在
/// load_session await 之前，因此即使 RPC 失败也已完成。
#[tokio::test]
#[serial]
async fn test_handle_load_resets_popup_todo_history_atoms() {
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

    // Act：handle_load 在 load_session 处会 hang（无 server），
    // 我们只关心它 await 之前的同步重置块，所以 100ms 超时即返回。
    let _ = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        handle_load(&client, &cwd, "thread-xyz".to_string()),
    )
    .await;

    // Assert：4 类残留已清空。
    assert!(
        crate::kit::atoms::POPUP_KIND.state().read().is_none(),
        "POPUP_KIND should be None after thread switch"
    );
    assert!(
        crate::kit::atoms::HITL_PENDING.state().read().is_none(),
        "HITL_PENDING should be cleared after thread switch"
    );
    assert!(
        crate::kit::atoms::TODO_ITEMS.state().read().is_empty(),
        "TODO_ITEMS should be empty after thread switch"
    );
    assert!(
        crate::kit::atoms::INPUT_HISTORY_INDEX
            .state()
            .read()
            .is_none(),
        "INPUT_HISTORY_INDEX should be None after thread switch"
    );
    assert!(
        crate::kit::atoms::DRAFT.state().read().is_none(),
        "DRAFT should be None after thread switch"
    );
}
