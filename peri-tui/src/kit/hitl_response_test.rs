//! Tests

use super::*;
use peri_acp::transport::AcpTransport;
use peri_acp::transport::mpsc::{MpscClientTransport, MpscServerTransport, mpsc_transport_pair};

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
    let (_tx, rx) = mpsc::unbounded_channel::<HitlResponseAction>();
    let shutdown = CancellationToken::new();
    let handle = spawn_hitl_response_consumer(client, rx, shutdown.clone());

    shutdown.cancel();
    let _ = tokio::time::timeout(std::time::Duration::from_millis(500), handle).await;
}

#[tokio::test]
async fn test_dropped_tx_exits_loop() {
    let (client, _server_transport) = make_client_without_pump();
    let (tx, rx) = mpsc::unbounded_channel::<HitlResponseAction>();
    let shutdown = CancellationToken::new();
    let handle = spawn_hitl_response_consumer(client, rx, shutdown);

    drop(tx);
    let _ = tokio::time::timeout(std::time::Duration::from_millis(500), handle).await;
}

/// H2: HitlResponseAction::Approve 调用 send_response 发送正确 JSON。
/// 通过 server transport 的 recv() 接收响应消息，验证 outcome 结构。
#[tokio::test]
async fn test_hitl_response_approve_sends_selected_allow_once() {
    let (client, server_transport) = make_client_without_pump();
    let (tx, rx) = mpsc::unbounded_channel::<HitlResponseAction>();
    let shutdown = CancellationToken::new();
    let _handle = spawn_hitl_response_consumer(client, rx, shutdown.clone());

    // RequestId 序列化为 JSON string: "String(\"hitl-1\")" → serde_json::to_string
    let request_id = peri_acp::transport::types::RequestId::String("hitl-1".to_string());
    let id_str = serde_json::to_string(&request_id).unwrap();
    tx.send(HitlResponseAction::Approve {
        request_id_str: id_str,
    })
    .unwrap();

    // 给 consumer 一点时间处理
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    shutdown.cancel();

    // 从 server transport 读取响应消息
    let server = server_transport;
    let msg = tokio::time::timeout(std::time::Duration::from_millis(500), server.recv())
        .await
        .expect("timeout")
        .expect("server transport closed");
    match msg {
        peri_acp::transport::types::IncomingMessage::Response { result, .. } => {
            let value = result.expect("response 应为 Ok(Value)");
            let outcome = value.get("outcome").expect("response 应包含 outcome 字段");
            assert_eq!(outcome["outcome"], "selected");
            assert_eq!(outcome["optionId"], "allow_once");
        }
        other => panic!("expected Response, got {other:?}"),
    }
}

/// H2: HitlResponseAction::Reject 调用 send_response 发送 cancelled JSON。
#[tokio::test]
async fn test_hitl_response_reject_sends_cancelled() {
    let (client, server_transport) = make_client_without_pump();
    let (tx, rx) = mpsc::unbounded_channel::<HitlResponseAction>();
    let shutdown = CancellationToken::new();
    let _handle = spawn_hitl_response_consumer(client, rx, shutdown.clone());

    let request_id = peri_acp::transport::types::RequestId::String("hitl-2".to_string());
    let id_str = serde_json::to_string(&request_id).unwrap();
    tx.send(HitlResponseAction::Reject {
        request_id_str: id_str,
    })
    .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    shutdown.cancel();

    let server = server_transport;
    let msg = tokio::time::timeout(std::time::Duration::from_millis(500), server.recv())
        .await
        .expect("timeout")
        .expect("server transport closed");
    match msg {
        peri_acp::transport::types::IncomingMessage::Response { result, .. } => {
            let value = result.expect("response 应为 Ok(Value)");
            let outcome = value.get("outcome").expect("response 应包含 outcome 字段");
            assert_eq!(outcome["outcome"], "cancelled");
        }
        other => panic!("expected Response, got {other:?}"),
    }
}

#[test]
fn test_hitl_response_action_variants() {
    let approve = HitlResponseAction::Approve {
        request_id_str: "\"abc\"".into(),
    };
    assert!(matches!(approve, HitlResponseAction::Approve { .. }));

    let reject = HitlResponseAction::Reject {
        request_id_str: "\"abc\"".into(),
    };
    assert!(matches!(reject, HitlResponseAction::Reject { .. }));
}
