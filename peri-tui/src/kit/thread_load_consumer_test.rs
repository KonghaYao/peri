//! Tests for thread_load_consumer

#[cfg(test)]
use super::*;
use peri_acp::transport::{
    AcpTransport,
    mpsc::{MpscClientTransport, MpscServerTransport, mpsc_transport_pair},
    types::IncomingMessage,
};
use serial_test::serial;
use std::time::Duration;

/// Global atoms can leak across parallel lib tests; thread switch must not see
/// stale background tasks (consumer would open confirm and skip session/load).
fn clear_thread_load_isolation_atoms() {
    crate::kit::atoms::init_atoms();
    crate::kit::atoms::BG_TASKS.state().write().clear();
    *crate::kit::atoms::CONFIRM_PAYLOAD.state().write() = None;
}

fn session_boundary_reset_observed() -> bool {
    crate::kit::atoms::POPUP_KIND.state().read().is_none()
        && crate::kit::atoms::HITL_PENDING.state().read().is_none()
        && crate::kit::atoms::TODO_ITEMS.state().read().is_empty()
        && crate::kit::atoms::INPUT_HISTORY_INDEX
            .state()
            .read()
            .is_none()
        && crate::kit::atoms::DRAFT.state().read().is_none()
}

async fn wait_for_session_boundary_reset(timeout: Duration) {
    tokio::time::timeout(timeout, async {
        while !session_boundary_reset_observed() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("session boundary projection should clear popup/todo/history atoms");
}

async fn recv_session_load(server: &MpscServerTransport, timeout: Duration) -> IncomingMessage {
    tokio::time::timeout(timeout, async {
        loop {
            let Some(message) = server.recv().await else {
                panic!("server transport closed before session/load");
            };
            match &message {
                IncomingMessage::Request { method, .. } if method == "session/load" => {
                    return message;
                }
                IncomingMessage::Request { id, method, .. } if method == "session/close" => {
                    let _ = server
                        .send_response(id.clone(), Ok(serde_json::json!({})))
                        .await;
                }
                _ => return message,
            }
        }
    })
    .await
    .expect("session/load should become in-flight")
}

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
    let (tx, rx) = mpsc::unbounded_channel::<ThreadLoadRequest>();
    let shutdown = CancellationToken::new();
    let handle = spawn_thread_load_consumer(client, rx, ".".into(), shutdown.clone());
    shutdown.cancel();
    let _ = tokio::time::timeout(std::time::Duration::from_millis(500), handle).await;
    let _ = tx;
}

#[tokio::test]
async fn test_dropped_tx_exits_loop() {
    let (client, _server) = make_client_without_pump();
    let (tx, rx) = mpsc::unbounded_channel::<ThreadLoadRequest>();
    let shutdown = CancellationToken::new();
    let handle = spawn_thread_load_consumer(client, rx, ".".into(), shutdown);
    drop(tx);
    let _ = tokio::time::timeout(std::time::Duration::from_millis(500), handle).await;
}

#[tokio::test]
#[serial]
async fn test_shutdown_cancels_inflight_load_and_releases_reservation() {
    clear_thread_load_isolation_atoms();
    let (client, server) = make_client_without_pump();
    let (tx, rx) = mpsc::unbounded_channel::<ThreadLoadRequest>();
    let dispatcher = ThreadLoadDispatcher::new(tx, client.clone());
    let shutdown = CancellationToken::new();
    let handle = spawn_thread_load_consumer(client.clone(), rx, ".".into(), shutdown.clone());
    tokio::task::yield_now().await;
    dispatcher.send("target".into()).unwrap();

    let request = recv_session_load(&server, Duration::from_secs(5)).await;
    assert!(matches!(
        request,
        IncomingMessage::Request { ref method, .. } if method == "session/load"
    ));
    assert_eq!(client.pending_session_load_count(), 1);

    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("consumer must stop while session/load is in-flight")
        .unwrap();
    assert_eq!(
        client.pending_session_load_count(),
        0,
        "shutdown must drop the in-flight reservation"
    );
}

/// 验证 handle_load 在调用 load_session 前已同步重置弹窗 / Todo / 历史指针。
///
/// 覆盖 H1/M3/L5/L10，与 submit_consumer::test_handle_clear_submit_*
/// 对称。load_session 无 server 会 hang；在后台 spawn handle_load 并轮询 atom，
/// 直到 client 在 load_session 内完成 project_session_boundary（发生在首个
/// 阻塞 RPC 之前），避免 CI 上 100ms 固定超时在 gate 排队时过早断言。
#[tokio::test]
#[serial]
async fn test_handle_load_resets_popup_todo_history_atoms() {
    clear_thread_load_isolation_atoms();
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

    // Act：load_session 会在 session/load 上挂起；轮询直到 boundary 投影完成。
    let load_task = {
        let client = client.clone();
        let cwd = cwd.clone();
        tokio::spawn(async move { handle_load(&client, &cwd, "thread-xyz".to_string()).await })
    };
    wait_for_session_boundary_reset(Duration::from_secs(5)).await;
    load_task.abort();
    let _ = load_task.await;

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
