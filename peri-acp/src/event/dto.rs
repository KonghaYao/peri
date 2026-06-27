//! ACP 事件 DTO 类型层
//!
//! 本模块定义 ACP 协议中跨边界传输的 DTO（Data Transfer Object）类型。
//! TUI / IDE 等消费方应使用这些 DTO，避免直接依赖 `peri_agent` / `peri_middlewares`
//! 内部类型。
//!
//! ## 设计原则
//!
//! - **零内部类型泄漏**：DTO 字段只用原始类型（String / 数值 / bool）或 DTO 自身
//! - **serde 友好**：所有 DTO 实现 `Serialize` / `Deserialize`，可 JSON 序列化
//! - **1:1 映射 ExecutorEvent**：每个 DTO 对应一个 v1 ExecutorEvent 的载荷，
//!   mapper 负责 ExecutorEvent → DTO 转换
//!
//! ## 与 AcpEvent 的关系
//!
//! `AcpEvent` 是 ACP 协议层的事件枚举，DTO 作为其变体的字段类型。
//! 例如 `AcpEvent::CompactCompleted { files: Vec<CompactFileInfoDto>, ... }`。

use serde::{Deserialize, Serialize};

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

/// Token 使用量（DTO，替代 `peri_agent::llm::types::TokenUsage`）
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

/// LLM 响应停止原因（DTO，替代 `peri_agent::llm::types::StopReason`）
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

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compact_file_info_dto_roundtrip() {
        let dto = CompactFileInfoDto {
            path: "/tmp/foo.rs".to_string(),
            lines: 42,
        };
        let json = serde_json::to_string(&dto).unwrap();
        let back: CompactFileInfoDto = serde_json::from_str(&json).unwrap();
        assert_eq!(dto, back);
    }

    #[test]
    fn test_workflow_progress_dto_roundtrip_minimal() {
        let dto = WorkflowProgressDto {
            run_id: "run-1".to_string(),
            workflow_name: "review".to_string(),
            event_type: "run_started".to_string(),
            agent_id: None,
            phase: None,
            label: None,
            agent_status: None,
            token_count: None,
            tool_count: None,
            run_status: None,
            message: None,
        };
        let json = serde_json::to_string(&dto).unwrap();
        // skip_serializing_if 应让 None 字段不出现在 JSON 中
        assert!(!json.contains("agent_id"));
        assert!(!json.contains("phase"));
        let back: WorkflowProgressDto = serde_json::from_str(&json).unwrap();
        assert_eq!(dto, back);
    }

    #[test]
    fn test_workflow_progress_dto_roundtrip_full() {
        let dto = WorkflowProgressDto {
            run_id: "run-1".to_string(),
            workflow_name: "review".to_string(),
            event_type: "agent_done".to_string(),
            agent_id: Some(42),
            phase: Some("review".to_string()),
            label: Some("reviewer".to_string()),
            agent_status: Some("done".to_string()),
            token_count: Some(1234),
            tool_count: Some(5),
            run_status: None,
            message: Some("all good".to_string()),
        };
        let json = serde_json::to_string(&dto).unwrap();
        let back: WorkflowProgressDto = serde_json::from_str(&json).unwrap();
        assert_eq!(dto, back);
    }
}
