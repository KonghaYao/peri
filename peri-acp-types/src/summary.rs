//! Migrated DTOs — originally defined in `peri-acp/src/event/dto.rs`,
//! re-exported from `peri-acp/src/event/mod.rs` for backward compatibility.
//!
//! These types are the public ACP contract between TUI/IDEs and the agent
//! runtime. Do not depend on `peri_agent` types directly — use these DTOs.

use serde::{Deserialize, Serialize};

// ─── Migrated DTOs (verbatim from peri-acp/src/event/dto.rs) ─────────────────

/// Compact 完成后保留的文件信息（DTO）
///
/// 替代 `peri_agent::agent::events::CompactFileInfo`，TUI/IDE 消费方应使用本类型。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompactFileInfoDto {
    /// 文件路径
    pub path: String,
    /// 文件行数
    pub lines: usize,
}

/// Workflow 进度更新载荷（DTO）
///
/// 替代 `peri_agent::agent::events::WorkflowProgressPayload`，
/// TUI/IDE 消费方应使用本类型。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkflowProgressDto {
    /// Run ID (UUID v7)
    pub run_id: String,
    /// Workflow 名称
    pub workflow_name: String,
    /// 事件类型（run_started / phase_started / phase_done / agent_started / agent_progress / agent_done / run_done）
    pub event_type: String,
    /// Agent ID（仅 agent_* 事件有值）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<u64>,
    /// Phase 名称（仅 phase_* 事件有值）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    /// Agent 标签
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Agent 状态（started/progress/done/dead/skipped）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_status: Option<String>,
    /// Token 计数
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_count: Option<u64>,
    /// 工具调用计数
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_count: Option<u64>,
    /// Run 状态（仅 run_done 有值：completed/failed/cancelled）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_status: Option<String>,
    /// 人类可读消息（错误描述 / 进度描述）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Token 使用量（DTO，对应 `peri_model::TokenUsage`）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct TokenUsageDto {
    /// 总输入 token（含缓存 token）
    pub input_tokens: u32,
    pub output_tokens: u32,
    /// 写入缓存的 token 数（仅 Anthropic 有意义，OpenAI 始终 None）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u32>,
    /// 从缓存读取的 token 数
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u32>,
    /// API 提供商返回的请求 ID
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

/// LLM 响应停止原因（DTO，对应 `peri_model::StopReason`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StopReasonDto {
    EndTurn,
    ToolUse,
    MaxTokens,
    Other { value: String },
}

/// Todo 项状态（DTO，替代 `peri_middlewares::tools::todo::TodoStatus`）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatusDto {
    #[default]
    Pending,
    InProgress,
    Completed,
}

/// Todo 项（DTO，替代 `peri_middlewares::tools::todo::TodoItem`）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct TodoItemDto {
    pub content: String,
    #[serde(
        default,
        rename = "activeForm",
        skip_serializing_if = "Option::is_none"
    )]
    pub active_form: Option<String>,
    #[serde(default)]
    pub status: TodoStatusDto,
}
