//! 投影指令纯数据契约（自 peri-agent/src/agent/compact_v2/projection 下沉）。
//!
//! 仅含持久化可序列化部分（`MessageFlags.projection` 载体）；
//! 渲染逻辑 `render_llm_view` 与 `ProviderCapabilities` 保留在 peri-agent。

use serde::{Deserialize, Serialize};

use crate::messages::MessageId;

const fn default_compact_tool_input_keep_head() -> usize {
    350
}

const fn default_compact_tool_input_keep_tail() -> usize {
    100
}

/// 投影目标（消息、块或工具调用）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProjectionTarget {
    Message,
    ContentBlock { index: usize },
    ToolCall { tool_call_id: String },
}

/// 投影动作 — 决定 LLM view 中消息/块如何呈现
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProjectionAction {
    Keep,
    CompactText {
        max_chars: usize,
    },
    CompactToolResult {
        keep_head: usize,
        keep_tail: usize,
        preserve_recovery_handle: bool,
    },
    /// 按字典序持久化的 root JSON object 顶层 key。
    ///
    /// 未知字段、非 string 值及非 object 根类型均安全地不执行任何操作；不支持嵌套路径、
    /// JSON Pointer 或数组索引。
    CompactToolInput {
        fields: Vec<String>,
        #[serde(default = "default_compact_tool_input_keep_head")]
        keep_head: usize,
        #[serde(default = "default_compact_tool_input_keep_tail")]
        keep_tail: usize,
    },
    ReplaceMedia {
        placeholder: String,
    },
    Exclude,
}

/// 单个投影条目：消息 id → 目标 → 动作
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionActionEntry {
    pub message_id: MessageId,
    pub target: ProjectionTarget,
    pub action: ProjectionAction,
}

/// 消息级投影指令 — 存储于 MessageFlags 中，可序列化/可恢复
///
/// 本类型是 `MessageFlags.projection` 的**持久化载荷形状**（本机与远端的
/// `messages.projection` 列都是它的 JSON）：以渲染指令为主，另承载一项渲染之外的
/// 事实——[`Self::superseded_by`] 的 Full Compact 取代关系。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageProjectionDirective {
    pub policy_version: u32,
    /// 仅含本消息的 action entries，不含 BaseMessage 内容或 Base64
    #[serde(default)]
    pub entries: Vec<ProjectionActionEntry>,
    /// 取代本条的摘要消息 id（Full Compact 归档时写入）。
    ///
    /// **渲染之外的事实**：写入它的同时 `entries` 为空（Full 归档按既有语义清空渲染
    /// 指令），因此它不产生任何渲染动作；只有 `MessageFlags.excluded` 为真时才被读取，
    /// rewind 用它精确判定「排除是否仍有依据」（见
    /// [`crate::store::history::rewind_clear_excluded_ids`]）。
    ///
    /// 清零 `excluded` 的存储路径把整条载荷一并清掉（本机 `clear_rewind_excluded`、
    /// 远端 `session_history::CLEAR_EXCLUDED_SQL`），所以它不会残留在已恢复正文的条目上。旧行没有
    /// 这个字段（`None`），旧构建读到它也不认识——serde 默认忽略未知字段，两侧都不会因此
    /// 拒绝历史。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<MessageId>,
}
