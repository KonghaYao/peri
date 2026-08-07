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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageProjectionDirective {
    pub policy_version: u32,
    /// 仅含本消息的 action entries，不含 BaseMessage 内容或 Base64
    #[serde(default)]
    pub entries: Vec<ProjectionActionEntry>,
}
