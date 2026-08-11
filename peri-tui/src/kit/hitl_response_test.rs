//! Tests

use super::*;
use peri_acp::transport::AcpTransport;
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
#[serial]
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
#[serial]
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

/// [Slice 4 §6.8] 双轨回归：Approve RPC 成功后发送 InteractionResolved 本地
/// 事件（LOCAL_EVENT_TX 接收端收到 request_id + result 文案）。
///
/// [OnceLock 全局单例] LOCAL_EVENT_TX.set 只成功一次（先例：
/// ensure_submit_tx_observable）——set 失败（先前测试已安装）时仅断言 RPC
/// 路径（双轨共存回归），本地事件断言交由首个安装者。
#[tokio::test]
#[serial]
async fn test_hitl_approve_emits_interaction_resolved_after_rpc() {
    let (client, server_transport) = make_client_without_pump();
    let (tx, rx) = mpsc::unbounded_channel::<HitlResponseAction>();
    let shutdown = CancellationToken::new();
    let _handle = spawn_hitl_response_consumer(client, rx, shutdown.clone());

    // 本地事件通道（emit_interaction_resolved 目标）；set 失败 → None
    let (local_tx, mut local_rx) = tokio::sync::mpsc::unbounded_channel();
    let installed = crate::kit::atoms::LOCAL_EVENT_TX.set(local_tx).is_ok();
    crate::i18n::init(Some("en"));

    let request_id = peri_acp::transport::types::RequestId::String("hitl-s4".to_string());
    let id_str = serde_json::to_string(&request_id).unwrap();
    tx.send(HitlResponseAction::Approve {
        request_id_str: id_str.clone(),
    })
    .unwrap();

    // 等 RPC 完成（server 端收到响应）
    let msg = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        server_transport.recv(),
    )
    .await
    .expect("timeout")
    .expect("server transport closed");
    match msg {
        peri_acp::transport::types::IncomingMessage::Response { result, .. } => {
            assert!(result.is_ok());
        }
        other => panic!("expected Response, got {other:?}"),
    }

    if installed {
        // 本地事件必须到达（RPC 成功后）
        let evt = tokio::time::timeout(std::time::Duration::from_millis(500), local_rx.recv())
            .await
            .expect("timeout: InteractionResolved 未发出")
            .expect("channel closed");
        match evt.event {
            crate::kit::acp_types::AcpEventData::InteractionResolved {
                request_id: rid,
                result,
            } => {
                assert_eq!(rid, id_str, "request_id 原样回传");
                assert_eq!(result, "Allowed once", "approve 结果文案（FTL en）");
            }
            other => panic!("expected InteractionResolved, got {other:?}"),
        }
    }
    shutdown.cancel();
}

/// [Slice 4 §6.8] Reject RPC 成功后发送 InteractionResolved（Denied 文案）。
/// [OnceLock] 同上——set 失败时仅断言 RPC 路径。
#[tokio::test]
#[serial]
async fn test_hitl_reject_emits_interaction_resolved_after_rpc() {
    let (client, server_transport) = make_client_without_pump();
    let (tx, rx) = mpsc::unbounded_channel::<HitlResponseAction>();
    let shutdown = CancellationToken::new();
    let _handle = spawn_hitl_response_consumer(client, rx, shutdown.clone());

    let (local_tx, mut local_rx) = tokio::sync::mpsc::unbounded_channel();
    let installed = crate::kit::atoms::LOCAL_EVENT_TX.set(local_tx).is_ok();
    crate::i18n::init(Some("en"));

    let request_id = peri_acp::transport::types::RequestId::String("hitl-s4r".to_string());
    let id_str = serde_json::to_string(&request_id).unwrap();
    tx.send(HitlResponseAction::Reject {
        request_id_str: id_str.clone(),
    })
    .unwrap();

    let msg = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        server_transport.recv(),
    )
    .await
    .expect("timeout")
    .expect("server transport closed");
    match msg {
        peri_acp::transport::types::IncomingMessage::Response { result, .. } => {
            assert!(result.is_ok());
        }
        other => panic!("expected Response, got {other:?}"),
    }

    if installed {
        let evt = tokio::time::timeout(std::time::Duration::from_millis(500), local_rx.recv())
            .await
            .expect("timeout: InteractionResolved 未发出")
            .expect("channel closed");
        match evt.event {
            crate::kit::acp_types::AcpEventData::InteractionResolved {
                request_id: rid,
                result,
            } => {
                assert_eq!(rid, id_str);
                assert_eq!(result, "Denied", "reject 结果文案（FTL en）");
            }
            other => panic!("expected InteractionResolved, got {other:?}"),
        }
    }
    shutdown.cancel();
}
