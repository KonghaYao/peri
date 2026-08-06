//! 人机交互契约（自 peri-agent 迁入；`peri-agent::interaction` 保留 re-export）。
//!
//! 统一 HITL（工具审批）与 AskUser（问答）两条路径；Channel 共享状态
//! （`ChannelState`）为 MCP handler / TUI broker 的跨层契约。
//! `ChannelBroker` / `MultiplexBroker` 实现留在 peri-agent（`channel_broker.rs`）。

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use parking_lot::Mutex as SyncMutex;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

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

// ─── channel_types ─────────────────────────────────────────────────────────────

/// 一条来自外部 channel 的通知
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelNotification {
    pub source: String, // "plugin:weixin@anthropic" 或 "server:my-mcp"
    pub chat_id: String,
    pub text: String,
}

/// MCP 工具调用的权限请求（经 channel 外发）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub request_id: String, // short ID，用户手打 yes <id> 使用
    pub tool_name: String,
    pub arguments: serde_json::Value,
    pub source: String, // "peri"
}

/// 用户对权限请求的响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionResponse {
    pub request_id: String,
    pub approved: bool,
    pub reason: String,
}

/// 生成短请求 ID（UUID v7 前 6 位 hex）
pub fn short_request_id() -> String {
    uuid::Uuid::now_v7().to_string().chars().take(6).collect()
}

// ─── ChannelState ──────────────────────────────────────────────────────────────

/// Channel 共享状态 — 桥接 MCP handler 与 TUI/broker
///
/// 单一实例，由 ServiceRegistry 持有，为 ChannelHandler、ChannelBroker、
/// `/channel` 命令提供共享的授权表、待审批 Map 和消息发送器注册表。
pub struct ChannelState {
    /// 已授权的 server → source 映射
    /// key: MCP server name，value: source 标识（如 "plugin:weixin@anthropic:weixin" 或 "server:my-mcp"）
    pub authorized: parking_lot::RwLock<HashMap<String, String>>,
    /// 待审批的权限请求：short_request_id → oneshot sender
    pub pending_permissions: SyncMutex<HashMap<String, oneshot::Sender<PermissionResponse>>>,
    /// 各 session 的消息发送器：session_id → mpsc sender
    pub channel_msg_txs:
        parking_lot::RwLock<HashMap<String, mpsc::UnboundedSender<ChannelNotification>>>,
}

impl ChannelState {
    /// Create a new shared ChannelState instance
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            authorized: parking_lot::RwLock::new(HashMap::new()),
            pending_permissions: SyncMutex::new(HashMap::new()),
            channel_msg_txs: parking_lot::RwLock::new(HashMap::new()),
        })
    }

    /// Authorize a channel server, return the source identifier
    pub fn authorize(&self, server_name: &str, source: String) {
        self.authorized
            .write()
            .insert(server_name.to_string(), source);
    }

    /// Revoke authorization for a channel server
    pub fn revoke(&self, server_name: &str) {
        self.authorized.write().remove(server_name);
    }

    /// Close all authorized channels
    pub fn close_all(&self) {
        self.authorized.write().clear();
    }

    /// Register a session's message receiver for channel notifications
    pub fn register_session(
        &self,
        session_id: String,
        tx: mpsc::UnboundedSender<ChannelNotification>,
    ) {
        self.channel_msg_txs.write().insert(session_id, tx);
    }

    /// Unregister a session's message receiver
    pub fn unregister_session(&self, session_id: &str) {
        self.channel_msg_txs.write().remove(session_id);
    }
}
