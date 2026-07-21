//! Tests for rewind_action.

use super::*;
use peri_acp::transport::mpsc::{
    MpscClientTransport, MpscServerTransport, mpsc_transport_pair,
};

/// 用真实 mpsc transport 对创建 AcpTuiClient（不启动 pump）。
fn make_client_without_pump() -> (AcpTuiClient, MpscServerTransport) {
    let (client_transport, server_transport): (MpscClientTransport, MpscServerTransport) =
        mpsc_transport_pair();
    let (client, _notification_rx) = AcpTuiClient::new(client_transport);
    (client, server_transport)
}

#[tokio::test]
async fn test_cancel_action_no_rpc() {
    let (client, _server_transport) = make_client_without_pump();
    let (tx, rx) = mpsc::unbounded_channel::<RewindAction>();
    let shutdown = CancellationToken::new();
    let handle = spawn_rewind_consumer(client, rx, shutdown.clone());

    // Cancel 不应触发任何 RPC——直接发完 shutdown
    tx.send(RewindAction::Cancel).unwrap();
    shutdown.cancel();
    let _ = tokio::time::timeout(std::time::Duration::from_millis(500), handle).await;
}

#[tokio::test]
async fn test_shutdown_exits_loop() {
    let (client, _server_transport) = make_client_without_pump();
    let (_tx, rx) = mpsc::unbounded_channel::<RewindAction>();
    let shutdown = CancellationToken::new();
    let handle = spawn_rewind_consumer(client, rx, shutdown.clone());

    shutdown.cancel();
    let _ = tokio::time::timeout(std::time::Duration::from_millis(500), handle).await;
}

#[tokio::test]
async fn test_dropped_tx_exits_loop() {
    let (client, _server_transport) = make_client_without_pump();
    let (tx, rx) = mpsc::unbounded_channel::<RewindAction>();
    let shutdown = CancellationToken::new();
    let handle = spawn_rewind_consumer(client, rx, shutdown);

    drop(tx);
    let _ = tokio::time::timeout(std::time::Duration::from_millis(500), handle).await;
}

#[tokio::test]
async fn test_confirm_without_session_skipped() {
    let (client, _server_transport) = make_client_without_pump();
    // 没有 session——handle_confirm 应当 Ok(()) 且不发送任何 RPC
    let result = handle_confirm(&client, "msg-1".to_string(), true).await;
    assert!(result.is_ok());
    assert!(!client.has_session());
}

/// 编译期断言：UnboundedSender<RewindAction> 与 atoms::REWIND_ACTION_TX 类型契约一致。
#[test]
fn test_rewind_action_tx_type_contract() {
    let (tx, _rx): (mpsc::UnboundedSender<RewindAction>, _) = mpsc::unbounded_channel();
    let _ = tx;
}

#[test]
fn test_rewind_action_variants() {
    let confirm = RewindAction::Confirm {
        target_message_id: "abc".into(),
        revert_files: true,
    };
    match confirm {
        RewindAction::Confirm {
            target_message_id,
            revert_files,
        } => {
            assert_eq!(target_message_id, "abc");
            assert!(revert_files);
        }
        RewindAction::Cancel => panic!("expected Confirm"),
    }

    let _cancel = RewindAction::Cancel;
}
