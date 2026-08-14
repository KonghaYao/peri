//! `session_list` 帧：S→C 按需会话列表查询结果（§6.3 workspace 扩展）。
//!
//! 与 [`crate::action::ActionEnvelope::SessionList`]（`session/list` action）
//! 配对：client 切换对话时按需查询，server 向 agent 侧发 `session/list` RPC
//! 后把**准确列表**回投本帧（agent 侧是真实数据源，非轮询投影的过滤）。

use serde::{Deserialize, Serialize};

use crate::schema::SessionSummaryProjection;

/// `session_list` 帧载荷（S→C）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionListFrame {
    /// 幂等键（回显原 action 的 commandId）。
    pub command_id: String,
    /// 查询的对话（server 解析的 cwd 已标注在每个条目上）。
    pub chat_id: String,
    /// 该对话（其 cwd）下的 ACP 会话列表；空数组 = 无会话。
    pub sessions: Vec<SessionSummaryProjection>,
}

/// 不含正文的 prompt delivery 公共状态。内部 outbox 状态不会直接暴露。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptDeliveryStatus {
    Projected,
    Completed,
    Failed,
    DeliveryUnknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptStatusItem {
    pub command_id: String,
    pub turn_id: Option<String>,
    pub status: PromptDeliveryStatus,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

/// `session/prompt-status` 的只读响应。`runtime_restored=false` 是显式契约：
/// 历史 delivery evidence 不代表旧 ACP runtime 被复活。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptStatusFrame {
    pub command_id: String,
    pub session_id: String,
    pub runtime_restored: bool,
    /// True when the bounded response omitted older records. Unresolved facts
    /// are retained before terminal history.
    pub truncated: bool,
    /// True when at least one catalogued historical runtime could not be
    /// inspected. An empty `prompts` array is not proof of clean history when
    /// this flag is set.
    pub evidence_incomplete: bool,
    pub prompts: Vec<PromptStatusItem>,
}
