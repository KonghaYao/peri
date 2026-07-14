//! HITL RequestPermission 响应消费者——HITL_RESPONSE_TX channel → acp_client RPC。
//!
//! 仿 `ask_user_action.rs` 前例：HitlPopup 在 Enter/Esc 时通过 channel 发送
//! `HitlResponseAction`，消费者独立 tokio task 调用 `AcpTuiClient::send_response`。
//!
//! ## 协议
//!
//! `RequestPermissionResponse` 使用 `outcome` 内部标签（ACP schema）：
//! ```json
//! // Approve (allow once)
//! {"outcome": {"outcome": "selected", "optionId": "allow_once"}}
//!
//! // Reject (cancelled)
//! {"outcome": {"outcome": "cancelled"}}
//! ```

use serde_json::json;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use crate::acp_client::AcpTuiClient;

/// HITL 用户操作——由 HitlPopup 在 Enter/Esc 时通过 HITL_RESPONSE_TX 发送。
#[derive(Debug, Clone)]
pub enum HitlResponseAction {
    /// 用户批准（Enter）。`request_id_str` 为 `serde_json::to_string(&RequestId)` 序列化结果。
    Approve { request_id_str: String },
    /// 用户拒绝（Esc）。
    Reject { request_id_str: String },
}

/// 启动 hitl 响应消费者后台任务。
///
/// 参数：
/// - `acp_client`：克隆自 build_app_and_acp 返回的 AcpTuiClient
/// - `rx`：HITL_RESPONSE_TX 的接收端
/// - `shutdown`：与 notifier / bridge / submit_consumer 共享的同一 CancellationToken
pub fn spawn_hitl_response_consumer(
    acp_client: AcpTuiClient,
    mut rx: mpsc::UnboundedReceiver<HitlResponseAction>,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        info!("kit hitl_response_consumer: started");
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    info!("kit hitl_response_consumer: shutdown signal received, exiting");
                    break;
                }
                msg = rx.recv() => {
                    match msg {
                        None => {
                            info!("kit hitl_response_consumer: HITL_RESPONSE_TX dropped, exiting");
                            break;
                        }
                        Some(HitlResponseAction::Approve { request_id_str }) => {
                            handle_approve(&acp_client, &request_id_str).await;
                        }
                        Some(HitlResponseAction::Reject { request_id_str }) => {
                            handle_reject(&acp_client, &request_id_str).await;
                        }
                    }
                }
            }
        }
    })
}

/// 处理批准：构造 selected/allow_once response JSON，调用 send_response。
async fn handle_approve(acp_client: &AcpTuiClient, request_id_str: &str) {
    let id = match serde_json::from_str::<peri_acp::transport::types::RequestId>(request_id_str) {
        Ok(id) => id,
        Err(e) => {
            error!(error = %e, request_id_str, "kit hitl_consumer: failed to deserialize RequestId");
            return;
        }
    };

    let response = json!({"outcome": {"outcome": "selected", "optionId": "allow_once"}});

    info!(request_id = %request_id_str, "kit hitl_consumer: sending Approve (allow_once) response");

    if let Err(e) = acp_client.send_response(id, Ok(response)).await {
        error!(error = %e, "kit hitl_consumer: send_response (approve) failed");
    }
}

/// 处理拒绝：构造 cancelled response JSON，调用 send_response。
async fn handle_reject(acp_client: &AcpTuiClient, request_id_str: &str) {
    let id = match serde_json::from_str::<peri_acp::transport::types::RequestId>(request_id_str) {
        Ok(id) => id,
        Err(e) => {
            error!(error = %e, request_id_str, "kit hitl_consumer: failed to deserialize RequestId (reject)");
            return;
        }
    };

    let response = json!({"outcome": {"outcome": "cancelled"}});

    info!(request_id = %request_id_str, "kit hitl_consumer: sending Reject (cancelled) response");

    if let Err(e) = acp_client.send_response(id, Ok(response)).await {
        error!(error = %e, "kit hitl_consumer: send_response (reject) failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peri_acp::transport::AcpTransport;
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
}
