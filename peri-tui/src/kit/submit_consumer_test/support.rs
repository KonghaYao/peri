//! submit_consumer 测试共享夹具。

use super::super::*;
use std::time::Duration;

use peri_acp::transport::AcpTransport;
use peri_acp::transport::mpsc::{MpscClientTransport, MpscServerTransport, mpsc_transport_pair};
use peri_acp::transport::types::{IncomingMessage, RequestId};

pub(super) async fn expect_prompt(server: &MpscServerTransport) -> (RequestId, String, String) {
    let incoming = tokio::time::timeout(Duration::from_secs(5), server.recv())
        .await
        .expect("expected session/prompt")
        .expect("transport must stay open");
    let IncomingMessage::Request {
        id, method, params, ..
    } = incoming
    else {
        panic!("expected request, got {incoming:?}");
    };
    assert_eq!(method, "session/prompt");
    let session_id = params["sessionId"]
        .as_str()
        .expect("prompt carries sessionId")
        .to_string();
    let text = params["message"]["content"]
        .as_str()
        .expect("text-only prompt content")
        .to_string();
    (id, session_id, text)
}

/// 接收一个指定方法的 ACP 请求（用于 session/new、session/close 等控制链路）。
pub(super) async fn expect_request(
    server: &MpscServerTransport,
    expected_method: &str,
) -> (RequestId, serde_json::Value) {
    let incoming = tokio::time::timeout(Duration::from_secs(5), server.recv())
        .await
        .expect("expected ACP request")
        .expect("transport must stay open");
    let IncomingMessage::Request {
        id, method, params, ..
    } = incoming
    else {
        panic!("expected request, got {incoming:?}");
    };
    assert_eq!(method, expected_method);
    (id, params)
}

pub(super) async fn wait_for_session(client: &AcpTuiClient, expected: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while client.current_session_id().as_deref() != Some(expected) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("session must become stable");
}

/// 用户文本提交。
pub(super) fn agent_text(text: &str) -> SubmitRequest {
    SubmitRequest::AgentText {
        text: text.to_string(),
        attachments: Vec::new(),
    }
}

/// 用真实 mpsc transport 对创建 AcpTuiClient（不启动 pump）。
pub(super) fn make_client_without_pump() -> (AcpTuiClient, MpscServerTransport) {
    let (client_transport, server_transport): (MpscClientTransport, MpscServerTransport) =
        mpsc_transport_pair();
    let (client, _notification_tx, _notification_rx) = AcpTuiClient::new(client_transport);
    (client, server_transport)
}

pub(super) fn make_interactive_client_without_pump() -> (AcpTuiClient, MpscServerTransport) {
    let (client_transport, server_transport): (MpscClientTransport, MpscServerTransport) =
        mpsc_transport_pair();
    let (client, _notification_tx, _notification_rx) =
        AcpTuiClient::new_interactive(client_transport);
    (client, server_transport)
}
