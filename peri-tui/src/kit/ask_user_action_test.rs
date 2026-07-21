//! Tests

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
