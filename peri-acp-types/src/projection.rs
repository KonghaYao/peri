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
/// `messages.projection` 列都是它的 JSON）：另承载 Full 的取代关系与
/// [`Self::recovery_only`] 的归档正文隔离事实。
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
    /// Canonical archive bytes copied from a frozen grant, without authority to
    /// restore a complete executable exchange. These rows stay excluded through
    /// rewind and refork; their catalog remains available for bounded recovery.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub recovery_only: bool,
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
    /// 硬记忆消息的结构化采样事实；派生请求据此按最终工具目录重算可执行入口。
    ///
    /// 只有硬记忆消息携带它；它不产生渲染动作（[`Self::entries`] 仍为空），
    /// 也不替代正文——正文里的 id 与状态保持采样时的观察值。
    ///
    /// Box 因为本类型是 `MessageFlags` 的持久化载荷：内联会让每条消息的 flags 都
    /// 背上采样事实的体积，进而撑大 `PersistOp`；serde 对 `Box` 透明，JSON 形状不变。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_memory: Option<Box<RecoveryMemoryProjection>>,
}

/// Full 硬记忆消息的结构化采样事实（随该消息的投影 JSON 持久化）。
///
/// 工具目录在压缩采样之后仍可能变化（`before_reason_catalog`、动态工具发布、
/// 权限过滤）。首个 Reason 用这些事实按最终目录重新渲染可执行入口，避免把
/// 采样时的工具名当成当前能力。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryMemoryProjection {
    /// 采样时任务注册表的 revision；运行期推进记录在 `TaskSnapshot.revision`。
    #[serde(default)]
    pub sampled_revision: u64,
    /// 采样时的会话作用域；`None` = 无法证明同一运行时，派生请求不得重算。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<RecoveryTaskProjection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<RecoveryGoalProjection>,
}

/// 硬记忆里单个活跃任务的持久化投影；只保留渲染恢复提示需要的事实。
///
/// 与 `TaskRecord` 分开：`output_preview` 等不可信正文不进入持久化投影，
/// 恢复提示只能引用可信注册端给出的身份与状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryTaskProjection {
    pub task_id: String,
    pub kind: crate::tasks::BgTaskKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_owner: Option<crate::tasks::ExternalTaskOwner>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub summary: String,
    pub status: String,
    pub started_at: String,
}

/// 硬记忆里 Active goal 的持久化投影（`GoalViewSnapshot` 是视图类型，不可序列化）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryGoalProjection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objective: Option<String>,
    pub status: crate::goal::GoalStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<u64>,
    pub tokens_used: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<String>,
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
            recovery_only: false,
            recovery_memory: None,
        }
    }
}
