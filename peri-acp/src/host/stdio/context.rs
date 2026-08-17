//! ACP Stdio 传输的共享上下文和 session 状态。
//!
//! stdio 部署单元的过渡期装配上下文：`cfg` 为统一装配产物
//! （[`crate::host::assemble::assemble_server_config`]，与 TUI/notify 路径
//! 同一份 `AcpServerConfig`），`sessions` 为 stdio 侧会话状态映射，统一后
//! 改用宿主 [`crate::host::SessionState`]（会话创建方即 writer，见
//! `SessionState::lease`）。handler 的字段引用统一经 `ctx.cfg.xxx`。

use std::time::Duration;

use agent_client_protocol::{schema::v1::SessionId, Client, ConnectionTo, UntypedMessage};
use peri_acp_types::interaction::{
    ApprovalDecision, InteractionContext, InteractionResponse, UserInteractionBroker,
};

use crate::broker::{build_elicitation_params, parse_elicitation_response};

/// Stdio 传输环境的共享上下文
pub(super) struct StdioContext {
    pub(super) cfg: crate::host::AcpServerConfig,
    pub(super) sessions:
        parking_lot::RwLock<std::collections::HashMap<String, crate::host::SessionState>>,
}

pub(super) struct StdioQuestionBroker {
    cx: ConnectionTo<Client>,
    session_id: SessionId,
    timeout: Option<Duration>,
}

impl StdioQuestionBroker {
    pub(super) fn new(
        cx: ConnectionTo<Client>,
        session_id: SessionId,
        timeout: Option<Duration>,
    ) -> Self {
        Self {
            cx,
            session_id,
            timeout,
        }
    }
}

#[async_trait::async_trait]
impl UserInteractionBroker for StdioQuestionBroker {
    async fn request(&self, context: InteractionContext) -> InteractionResponse {
        match context {
            InteractionContext::Approval { items } => InteractionResponse::Decisions(
                items
                    .into_iter()
                    .map(|_| ApprovalDecision::Approve { source: None })
                    .collect(),
            ),
            InteractionContext::Questions { requests } => {
                let params = build_elicitation_params(&requests, self.session_id.clone());
                let message = match UntypedMessage::new("elicitation/create", params) {
                    Ok(message) => message,
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to build elicitation request");
                        return InteractionResponse::Answers(empty_answers(requests));
                    }
                };
                let response = self.cx.send_request(message).block_task();
                let result = match self.timeout {
                    Some(timeout) => tokio::time::timeout(timeout, response).await,
                    None => Ok(response.await),
                };
                match result {
                    Ok(Ok(response)) => parse_elicitation_response(response, requests),
                    Ok(Err(e)) => {
                        tracing::warn!(error = %e, "Elicitation transport error");
                        InteractionResponse::Answers(empty_answers(requests))
                    }
                    Err(_) => InteractionResponse::Rejected,
                }
            }
        }
    }
}

fn empty_answers(
    requests: Vec<peri_acp_types::interaction::QuestionItem>,
) -> Vec<peri_acp_types::interaction::QuestionAnswer> {
    requests
        .into_iter()
        .map(|q| peri_acp_types::interaction::QuestionAnswer {
            id: q.id,
            selected: vec![],
            text: Some(String::new()),
        })
        .collect()
}

/// 解析 `PERI_ASK_USER_TIMEOUT_SECS` 环境变量值（纯逻辑，便于单测）：
/// 缺失/非法回落默认 300 秒；`0` 表示不超时（返回 None）。
fn parse_ask_user_timeout(value: Option<&str>) -> Option<Duration> {
    match value.and_then(|v| v.parse::<u64>().ok()).unwrap_or(300) {
        0 => None,
        seconds => Some(Duration::from_secs(seconds)),
    }
}

pub(super) fn ask_user_timeout() -> Option<Duration> {
    parse_ask_user_timeout(std::env::var("PERI_ASK_USER_TIMEOUT_SECS").ok().as_deref())
}

#[cfg(test)]
#[path = "context_test.rs"]
mod tests;
