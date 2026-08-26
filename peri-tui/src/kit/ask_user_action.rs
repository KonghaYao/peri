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
use crate::acp_client::InteractionOwner;
use crate::i18n;

/// AskUser 用户操作——由 AskUserPopup 在 Enter/Esc 时通过 ASK_USER_RESPONSE_TX 发送。
#[derive(Debug, Clone)]
pub enum AskUserResponseAction {
    /// 用户提交答案；answers 为 `BTreeMap<String, Value>` 的 serde_json Value。
    Submit {
        /// `serde_json::to_string(&RequestId)` 序列化结果
        request_id_str: String,
        owner: InteractionOwner,
        /// 答案 map，key = question_id，value = ElicitationContentValue 的 JSON
        answers: Value,
    },
    /// 用户取消（Esc）
    Cancel {
        owner: InteractionOwner,
        request_id_str: String,
    },
    /// 用户拒绝回答（ESC → 确认弹窗 → 确认拒绝）
    Reject {
        owner: InteractionOwner,
        request_id_str: String,
    },
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
                        Some(AskUserResponseAction::Cancel { owner, request_id_str }) => {
                            handle_cancel(&acp_client, &owner, &request_id_str).await;
                        }
                        Some(AskUserResponseAction::Submit { owner, request_id_str, answers }) => {
                            handle_submit(&acp_client, &owner, &request_id_str, &answers).await;
                        }
                        Some(AskUserResponseAction::Reject { owner, request_id_str }) => {
                            handle_reject(&acp_client, &owner, &request_id_str).await;
                        }
                    }
                }
            }
        }
    })
}

/// 处理提交：构造 Accept response JSON，调用 send_response。
/// Client 先 claim owner；成功或传输失败都会 owner-aware terminalize。
/// 失败是 `ResponseTransportFailed` 终态，不恢复 pending、不重试。result =
/// 首个非空答案（inline 快速回答的选中 label），无答案时回退 `Answered`。
async fn handle_submit(
    acp_client: &AcpTuiClient,
    owner: &InteractionOwner,
    request_id_str: &str,
    answers: &Value,
) {
    let response = json!({
        "action": "accept",
        "content": answers
    });

    info!(request_id = %request_id_str, "kit ask_user_consumer: sending Accept response");

    let result = answers
        .as_object()
        .and_then(|m| {
            m.values()
                .find(|v| v.as_str().map(|s| !s.is_empty()).unwrap_or(false))
        })
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| i18n::tr("render-interaction-result-answered"));
    if let Err(e) = acp_client
        .respond_interaction(owner, response, result)
        .await
    {
        error!(error = %e, "kit ask_user_consumer: send_response failed");
    }
}

/// 处理取消：构造 Cancel response JSON，调用 send_response。
/// 成功或传输失败都 owner-aware terminalize；失败不恢复 pending、不重试。
async fn handle_cancel(acp_client: &AcpTuiClient, owner: &InteractionOwner, request_id_str: &str) {
    let response = json!({"action": "cancel"});

    info!(request_id = %request_id_str, "kit ask_user_consumer: sending Cancel response");

    if let Err(e) = acp_client
        .respond_interaction(
            owner,
            response,
            i18n::tr("render-interaction-result-rejected"),
        )
        .await
    {
        error!(error = %e, "kit ask_user_consumer: send_response (cancel) failed");
    }
}

/// 处理拒绝：发送 decline response，告诉 Agent 用户明确拒绝了回答。
/// 成功或传输失败都 owner-aware terminalize；失败不恢复 pending、不重试。
async fn handle_reject(acp_client: &AcpTuiClient, owner: &InteractionOwner, request_id_str: &str) {
    let response = serde_json::json!({"action": "decline"});

    tracing::info!(request_id = %request_id_str, "kit ask_user_consumer: sending Reject/Decline response");

    if let Err(e) = acp_client
        .respond_interaction(
            owner,
            response,
            i18n::tr("render-interaction-result-rejected"),
        )
        .await
    {
        tracing::error!(error = %e, "kit ask_user_consumer: send_response (reject) failed");
    }
}

#[cfg(test)]
#[path = "ask_user_action_test.rs"]
mod tests;
