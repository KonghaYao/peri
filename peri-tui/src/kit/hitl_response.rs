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
use crate::acp_client::InteractionOwner;
use crate::i18n;

/// HITL 用户操作——由 HitlPopup 在 Enter/Esc 时通过 HITL_RESPONSE_TX 发送。
#[derive(Debug, Clone)]
pub enum HitlResponseAction {
    /// 用户批准（Enter）。`request_id_str` 为 `serde_json::to_string(&RequestId)` 序列化结果。
    Approve {
        owner: InteractionOwner,
        request_id_str: String,
    },
    /// 用户拒绝（Esc）。
    Reject {
        owner: InteractionOwner,
        request_id_str: String,
    },
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
                        Some(HitlResponseAction::Approve { owner, request_id_str }) => {
                            handle_approve(&acp_client, &owner, &request_id_str).await;
                        }
                        Some(HitlResponseAction::Reject { owner, request_id_str }) => {
                            handle_reject(&acp_client, &owner, &request_id_str).await;
                        }
                    }
                }
            }
        }
    })
}

/// 处理批准：构造 selected/allow_once response JSON，调用 send_response。
/// Client 先 claim owner 再发送 wire response；成功或传输失败都会发送
/// owner-aware `InteractionTerminal`。失败是终态 `ResponseTransportFailed`，
/// 不恢复 pending，也不允许以同一 owner 重试。
async fn handle_approve(acp_client: &AcpTuiClient, owner: &InteractionOwner, request_id_str: &str) {
    let response = json!({"outcome": {"outcome": "selected", "optionId": "allow_once"}});

    info!(request_id = %request_id_str, "kit hitl_consumer: sending Approve (allow_once) response");

    if let Err(e) = acp_client
        .respond_interaction(
            owner,
            response,
            i18n::tr("render-interaction-result-allowed-once"),
        )
        .await
    {
        error!(error = %e, "kit hitl_consumer: send_response (approve) failed");
    }
}

/// 处理拒绝：构造 cancelled response JSON，调用 send_response。
/// Client 先 claim owner；成功或传输失败都 owner-aware terminalize，且失败不重试。
async fn handle_reject(acp_client: &AcpTuiClient, owner: &InteractionOwner, request_id_str: &str) {
    let response = json!({"outcome": {"outcome": "cancelled"}});

    info!(request_id = %request_id_str, "kit hitl_consumer: sending Reject (cancelled) response");

    if let Err(e) = acp_client
        .respond_interaction(
            owner,
            response,
            i18n::tr("render-interaction-result-denied"),
        )
        .await
    {
        error!(error = %e, "kit hitl_consumer: send_response (reject) failed");
    }
}

#[cfg(test)]
#[path = "hitl_response_test.rs"]
mod tests;
