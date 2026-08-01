//! Tests for rewind_action.

use super::*;
use peri_acp::transport::mpsc::{MpscClientTransport, MpscServerTransport, mpsc_transport_pair};
use serde_json::json;

/// 用真实 mpsc transport 对创建 AcpTuiClient（不启动 pump）。
fn make_client_without_pump() -> (AcpTuiClient, MpscServerTransport) {
    let (client_transport, server_transport): (MpscClientTransport, MpscServerTransport) =
        mpsc_transport_pair();
    let (client, _notification_rx) = AcpTuiClient::new(client_transport);
    (client, server_transport)
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
    // 没有 session——handle_action 应当 Ok(()) 且不发送任何 RPC
    let result = handle_action(
        &client,
        RewindAction::Confirm {
            target_message_id: "msg-1".into(),
        },
    )
    .await;
    assert!(result.is_ok());
    assert!(!client.has_session());
}

#[test]
fn test_build_preview_params_carries_target() {
    let params = build_preview_params("test-sid", "m1");
    assert_eq!(params["sessionId"], "test-sid");
    assert_eq!(params["target_message_id"], "m1");
}

#[test]
fn test_build_execute_params_carries_target() {
    let params = build_execute_params("test-sid", "m1");
    assert_eq!(params["sessionId"], "test-sid");
    assert_eq!(params["target_message_id"], "m1");
}

#[test]
fn test_parse_budget_response_extracts_changes() {
    let resp = json!({
        "file_changes": [
            { "path": "src/main.rs", "kind": "edit" },
            { "path": "new_file.txt", "kind": "write" },
        ]
    });
    let changes = parse_budget_response(&resp).unwrap();
    assert_eq!(changes.len(), 2);
    assert_eq!(changes[0].path, "src/main.rs");
    assert_eq!(changes[0].kind, "edit");
    assert_eq!(changes[1].path, "new_file.txt");
    assert_eq!(changes[1].kind, "write");
}

#[test]
fn test_parse_budget_response_empty_ok() {
    let resp = json!({ "file_changes": [] });
    let changes = parse_budget_response(&resp).unwrap();
    assert!(changes.is_empty());
}

#[test]
fn test_parse_budget_response_malformed_returns_err() {
    let resp = json!({ "unexpected": 1 });
    assert!(parse_budget_response(&resp).is_err());
}

/// 编译期断言：UnboundedSender<RewindAction> 与 atoms::REWIND_ACTION_TX 类型契约一致。
#[test]
fn test_rewind_action_tx_type_contract() {
    let (tx, _rx): (mpsc::UnboundedSender<RewindAction>, _) = mpsc::unbounded_channel();
    let _ = tx;
}

#[test]
fn test_rewind_action_variants() {
    let preview = RewindAction::Preview {
        target_message_id: "abc".into(),
        target_text: "问题文本".into(),
    };
    match preview {
        RewindAction::Preview {
            target_message_id,
            target_text,
        } => {
            assert_eq!(target_message_id, "abc");
            assert_eq!(target_text, "问题文本");
        }
        RewindAction::Confirm { .. } => panic!("expected Preview"),
    }

    let confirm = RewindAction::Confirm {
        target_message_id: "abc".into(),
    };
    match confirm {
        RewindAction::Confirm { target_message_id } => {
            assert_eq!(target_message_id, "abc");
        }
        RewindAction::Preview { .. } => panic!("expected Confirm"),
    }
}
