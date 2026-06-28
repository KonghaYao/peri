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

// DTOs 已迁移至 `peri-acp-types` crate，此处 re-export 保持向后兼容。
pub use peri_acp_types::summary::{
    CompactFileInfoDto, StopReasonDto, TodoItemDto, TodoStatusDto, TokenUsageDto,
    WorkflowProgressDto,
};

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
