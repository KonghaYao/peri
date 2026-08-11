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
#[serial]
async fn test_cancel_action_no_rpc() {
    let (client, _server_transport) = make_client_without_pump();
    let (tx, rx) = mpsc::unbounded_channel::<AskUserResponseAction>();
    let shutdown = CancellationToken::new();
    let handle = spawn_ask_user_consumer(client, rx, shutdown.clone());

    tx.send(AskUserResponseAction::Cancel {
        request_id_str: "\"test-id\"".to_string(),
    })
    .unwrap();
    shutdown.cancel();
    let _ = tokio::time::timeout(std::time::Duration::from_millis(500), handle).await;
}

#[tokio::test]
async fn test_shutdown_exits_loop() {
    let (client, _server_transport) = make_client_without_pump();
    let (_tx, rx) = mpsc::unbounded_channel::<AskUserResponseAction>();
    let shutdown = CancellationToken::new();
    let handle = spawn_ask_user_consumer(client, rx, shutdown.clone());

    shutdown.cancel();
    let _ = tokio::time::timeout(std::time::Duration::from_millis(500), handle).await;
}

#[tokio::test]
async fn test_dropped_tx_exits_loop() {
    let (client, _server_transport) = make_client_without_pump();
    let (tx, rx) = mpsc::unbounded_channel::<AskUserResponseAction>();
    let shutdown = CancellationToken::new();
    let handle = spawn_ask_user_consumer(client, rx, shutdown);

    drop(tx);
    let _ = tokio::time::timeout(std::time::Duration::from_millis(500), handle).await;
}

#[test]
fn test_ask_user_response_action_variants() {
    let submit = AskUserResponseAction::Submit {
        request_id_str: "\"abc\"".into(),
        answers: json!({"q1": {"String": "yes"}}),
    };
    match submit {
        AskUserResponseAction::Submit {
            request_id_str,
            answers,
        } => {
            assert_eq!(request_id_str, "\"abc\"");
            assert_eq!(answers["q1"]["String"], "yes");
        }
        AskUserResponseAction::Cancel { .. } => panic!("expected Submit"),
        AskUserResponseAction::Reject { .. } => panic!("expected Submit"),
    }

    let cancel = AskUserResponseAction::Cancel {
        request_id_str: "\"abc\"".into(),
    };
    assert!(matches!(cancel, AskUserResponseAction::Cancel { .. }));

    let reject = AskUserResponseAction::Reject {
        request_id_str: "\"abc\"".into(),
    };
    assert!(matches!(reject, AskUserResponseAction::Reject { .. }));
}

/// [Slice 4 §6.8] AskUser Submit RPC 成功后发送 InteractionResolved：result =
/// 首个非空答案（inline 快速回答的选中 label）。
/// [OnceLock] LOCAL_EVENT_TX.set 只成功一次——set 失败（先前测试已安装）时
/// 仅断言 RPC 路径。
#[tokio::test]
#[serial]
async fn test_ask_user_submit_emits_interaction_resolved_with_label() {
    let (client, server_transport) = make_client_without_pump();
    let (tx, rx) = mpsc::unbounded_channel::<AskUserResponseAction>();
    let shutdown = CancellationToken::new();
    let _handle = spawn_ask_user_consumer(client, rx, shutdown.clone());

    let (local_tx, mut local_rx) = tokio::sync::mpsc::unbounded_channel();
    let installed = crate::kit::atoms::LOCAL_EVENT_TX.set(local_tx).is_ok();
    crate::i18n::init(Some("en"));

    let request_id = peri_acp::transport::types::RequestId::String("ask-s4".to_string());
    let id_str = serde_json::to_string(&request_id).unwrap();
    tx.send(AskUserResponseAction::Submit {
        request_id_str: id_str.clone(),
        answers: json!({"q1": "Fast", "q2": ""}),
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
            let v = result.unwrap();
            assert_eq!(v["action"], "accept");
            assert_eq!(v["content"]["q1"], "Fast");
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
                assert_eq!(result, "Fast", "result = 首个非空答案 label");
            }
            other => panic!("expected InteractionResolved, got {other:?}"),
        }
    }
    shutdown.cancel();
}
