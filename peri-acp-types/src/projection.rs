//! 投影指令纯数据契约（自 peri-agent/src/agent/compact_v2/projection 下沉）。
//!
//! 仅含持久化可序列化部分（`MessageFlags.projection` 载体）；
//! 渲染逻辑 `render_llm_view` 与 `ProviderCapabilities` 保留在 peri-agent。

use serde::{Deserialize, Serialize};

use crate::context_archive::{ArchiveCatalog, ArchiveRoot};
use crate::messages::MessageId;

fn default_format_version() -> u32 {
    1
}

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
    /// Reuse the original archive identity instead of creating a recursive archive chain.
    PreviewArchiveRead {
        archive_ref: String,
        keep_head: usize,
        keep_tail: usize,
    },
    /// Omit every representation of a completed exchange; canonical bytes remain unchanged.
    OmitToolExchange {
        anchor_message_id: MessageId,
        group_id: String,
        tool_call_ids: Vec<String>,
    },
    /// Replace an exposed result with the factual receipt stored at its catalog anchor.
    ResultReceipt {
        anchor_message_id: MessageId,
        group_id: String,
    },
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
    #[serde(default = "default_format_version")]
    pub format_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_catalog: Option<ArchiveCatalog>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_group: Option<ArchiveGroupLink>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub archive_roots: Vec<ArchiveRoot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_receipt_budget_tokens: Option<u64>,
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
    /// rewind 清零 `excluded` 时只撤销失效取代关系；仍完整的 archive catalog
    /// 保持原有身份，已发布引用继续读取同一 canonical 内容。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<MessageId>,
}

/// A member points to its sole durable catalog; the member table is never duplicated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveGroupLink {
    pub group_id: String,
    pub anchor_message_id: MessageId,
}

impl Default for MessageProjectionDirective {
    fn default() -> Self {
        Self {
            format_version: 1,
            policy_version: 0,
            entries: Vec::new(),
            superseded_by: None,
            archive_catalog: None,
            archive_group: None,
            archive_roots: Vec::new(),
            archive_receipt_budget_tokens: None,
        }
    }
}
