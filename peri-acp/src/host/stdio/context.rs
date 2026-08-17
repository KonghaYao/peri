//! ACP Stdio 传输的共享上下文和 session 状态。
//!
//! stdio 部署单元的过渡期装配上下文：`cfg` 为统一装配产物
//! （[`crate::host::assemble::assemble_server_config`]，与 TUI/notify 路径
//! 同一份 `AcpServerConfig`），`sessions` 为 stdio 侧会话状态映射，统一后
//! 改用宿主 [`crate::host::SessionState`]（会话创建方即 writer，见
//! `SessionState::lease`）。handler 的字段引用统一经 `ctx.cfg.xxx`。

use peri_acp_types::interaction::{
    ApprovalDecision, InteractionContext, InteractionResponse, QuestionAnswer,
    UserInteractionBroker,
};

/// Stdio 传输环境的共享上下文
pub(super) struct StdioContext {
    pub(super) cfg: crate::host::AcpServerConfig,
    pub(super) sessions:
        parking_lot::RwLock<std::collections::HashMap<String, crate::host::SessionState>>,
}

/// Stdio 模式下的简化 Broker：直接 approve 所有权限请求，questions 返回空答案。
pub(super) struct StdioBroker;

impl StdioBroker {
    pub(super) fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl UserInteractionBroker for StdioBroker {
    async fn request(&self, context: InteractionContext) -> InteractionResponse {
        match context {
            InteractionContext::Approval { items } => InteractionResponse::Decisions(
                items
                    .into_iter()
                    .map(|_| ApprovalDecision::Approve { source: None })
                    .collect(),
            ),
            InteractionContext::Questions { requests } => InteractionResponse::Answers(
                requests
                    .into_iter()
                    .map(|q| QuestionAnswer {
                        id: q.id,
                        selected: vec![],
                        text: Some(String::new()),
                    })
                    .collect(),
            ),
        }
    }
}
