use async_trait::async_trait;
use serde::{Deserialize, Serialize};

// ─── ApprovalItem ──────────────────────────────────────────────────────────────

/// 工具调用审批项
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalItem {
    pub tool_call_id: String,
    pub tool_name: String,
    pub tool_input: serde_json::Value,
}

// ─── QuestionItem ──────────────────────────────────────────────────────────────

/// 问题选项
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    pub description: Option<String>,
}

/// 单个问题
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionItem {
    pub id: String,
    pub question: String,
    pub header: String,
    pub options: Vec<QuestionOption>,
    pub multi_select: bool,
}

// ─── InteractionContext ────────────────────────────────────────────────────────

/// 人机交互上下文（描述需要用户响应的场景）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum InteractionContext {
    /// 工具调用前审批（原 HITL BatchApprovalRequest）
    Approval { items: Vec<ApprovalItem> },
    /// 向用户提问（原 AskUserBatchRequest）
    Questions { requests: Vec<QuestionItem> },
}

// ─── InteractionResponse ───────────────────────────────────────────────────────

/// 单项审批决策（对齐 HitlDecision 四种语义）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ApprovalDecision {
    Approve {
        source: Option<String>,
    },
    Reject {
        reason: String,
        source: Option<String>,
    },
    Edit {
        new_input: serde_json::Value,
    },
    Respond {
        message: String,
    },
}

/// 问题答案
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionAnswer {
    pub id: String,
    pub selected: Vec<String>,
    pub text: Option<String>,
}

/// 交互响应
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum InteractionResponse {
    Decisions(Vec<ApprovalDecision>),
    Answers(Vec<QuestionAnswer>),
    /// 用户明确拒绝了交互（如在 AskUserQuestion 确认弹窗中选择拒绝）
    Rejected,
}

// ─── UserInteractionBroker ─────────────────────────────────────────────────────

/// 统一人机交互 broker trait
///
/// 将 HITL（工具审批）和 AskUser（问答）两条路径统一为单一接口。
/// 应用层（TUI / CLI / 测试）实现此 trait，通过 `request` 方法挂起等待用户响应。
///
/// # 使用示例
///
/// ```rust,ignore
/// let broker: Arc<dyn UserInteractionBroker> = Arc::new(TuiInteractionBroker::new(tx));
/// let hitl = HumanInTheLoopMiddleware::with_shared_mode(
///     broker.clone(),
///     default_requires_approval,
///     Arc::new(SharedPermissionMode::new(PermissionMode::Default)),
///     None,
/// );
/// let ask_user_tool = AskUserTool::new(broker);
/// ```
#[async_trait]
pub trait UserInteractionBroker: Send + Sync {
    /// 发起一次人机交互，挂起直到用户响应
    async fn request(&self, ctx: InteractionContext) -> InteractionResponse;
}

// ─── ChannelNotificationSender ─────────────────────────────────────────────────

/// 发送 channel 通知的抽象（由 McpClientPool 在 peri-middlewares 中实现）
#[async_trait]
pub trait ChannelNotificationSender: Send + Sync {
    async fn send_notification(
        &self,
        server_name: &str,
        method: &str,
        params: serde_json::Value,
    ) -> Result<(), String>;
}

pub mod channel_types;
pub use channel_types::{
    short_request_id, ChannelNotification, PermissionRequest, PermissionResponse,
};

pub mod channel_state;
pub use channel_state::ChannelState;

pub mod channel_broker;
pub mod multiplex;

pub use channel_broker::ChannelBroker;
pub use multiplex::MultiplexBroker;

#[cfg(test)]
mod channel_broker_test;
#[cfg(test)]
mod multiplex_test;
