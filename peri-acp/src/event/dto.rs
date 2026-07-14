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
#[path = "dto_test.rs"]
mod tests;
