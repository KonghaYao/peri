//! AskUser 响应消费者——ASK_USER_RESPONSE_TX channel → acp_client RPC。
//!
//! 遵循 `rewind_action.rs` 前例：popup 组件在 Confirm/Esc 时通过 channel 发送
//! `AskUserResponseAction`，消费者独立 tokio task 调用 `AcpTuiClient::send_response`。
//!
//! ## 协议
//!
//! `ElicitationAction` 为 `#[serde(tag = "action")]` 内部标签：
//! ```json
//! // Submit (Accept)
//! {"action": "accept", "content": {"q_id": "label"}}
//!
//! // Cancel
//! {"action": "cancel"}
//! ```

use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use crate::acp_client::AcpTuiClient;

/// AskUser 用户操作——由 AskUserPopup 在 Enter/Esc 时通过 ASK_USER_RESPONSE_TX 发送。
#[derive(Debug, Clone)]
pub enum AskUserResponseAction {
    /// 用户提交答案；answers 为 `BTreeMap<String, Value>` 的 serde_json Value。
    Submit {
        /// `serde_json::to_string(&RequestId)` 序列化结果
        request_id_str: String,
        /// 答案 map，key = question_id，value = ElicitationContentValue 的 JSON
        answers: Value,
    },
    /// 用户取消（Esc）
    Cancel { request_id_str: String },
}

/// 启动 ask_user 响应消费者后台任务。
///
/// 参数：
/// - `acp_client`：克隆自 build_app_and_acp 返回的 AcpTuiClient
/// - `rx`：ASK_USER_RESPONSE_TX 的接收端
/// - `shutdown`：与 notifier / bridge / submit_consumer 共享的同一 CancellationToken
pub fn spawn_ask_user_consumer(
    acp_client: AcpTuiClient,
    mut rx: mpsc::UnboundedReceiver<AskUserResponseAction>,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        info!("kit ask_user_consumer: started");
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    info!("kit ask_user_consumer: shutdown signal received, exiting");
                    break;
                }
                msg = rx.recv() => {
                    match msg {
                        None => {
                            info!("kit ask_user_consumer: ASK_USER_RESPONSE_TX dropped, exiting");
                            break;
                        }
                        Some(AskUserResponseAction::Cancel { request_id_str }) => {
                            handle_cancel(&acp_client, &request_id_str).await;
                        }
                        Some(AskUserResponseAction::Submit { request_id_str, answers }) => {
                            handle_submit(&acp_client, &request_id_str, &answers).await;
                        }
                    }
                }
            }
        }
    })
}

/// 处理提交：构造 Accept response JSON，调用 send_response。
async fn handle_submit(acp_client: &AcpTuiClient, request_id_str: &str, answers: &Value) {
    let id = match serde_json::from_str::<peri_acp::transport::types::RequestId>(request_id_str) {
        Ok(id) => id,
        Err(e) => {
            error!(error = %e, request_id_str, "kit ask_user_consumer: failed to deserialize RequestId");
            return;
        }
    };

    let response = json!({
        "action": "accept",
        "content": answers
    });

    info!(request_id = %request_id_str, "kit ask_user_consumer: sending Accept response");

    if let Err(e) = acp_client.send_response(id, Ok(response)).await {
        error!(error = %e, "kit ask_user_consumer: send_response failed");
    }
}

/// 处理取消：构造 Cancel response JSON，调用 send_response。
async fn handle_cancel(acp_client: &AcpTuiClient, request_id_str: &str) {
    let id = match serde_json::from_str::<peri_acp::transport::types::RequestId>(request_id_str) {
        Ok(id) => id,
        Err(e) => {
            error!(error = %e, request_id_str, "kit ask_user_consumer: failed to deserialize RequestId (cancel)");
            return;
        }
    };

    let response = json!({"action": "cancel"});

    info!(request_id = %request_id_str, "kit ask_user_consumer: sending Cancel response");

    if let Err(e) = acp_client.send_response(id, Ok(response)).await {
        error!(error = %e, "kit ask_user_consumer: send_response (cancel) failed");
    }
}

#[cfg(test)]
mod tests {
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
        }

        let cancel = AskUserResponseAction::Cancel {
            request_id_str: "\"abc\"".into(),
        };
        assert!(matches!(cancel, AskUserResponseAction::Cancel { .. }));
    }
}
