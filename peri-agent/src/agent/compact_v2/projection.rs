//! Projection — 消息投影类型和 Provider 能力定义
//!
//! ## render_llm_view 纯函数
//!
//! 根据 `MicroCompactPlan` + `ProviderCapabilities` 渲染 LLM 可见消息列表：
//! - 不修改 Transcript，不写 flags，不调数据库
//! - 正确处理所有 ContentBlock 类型（Text/Image/Document/ToolUse/ToolResult/Reasoning）
//! - Tool input 投影后保持 JSON object 根类型
//! - CJK 截断用字符边界而非字节切片
//! - Image/Document Base64 payload 移除
//!
//! 消息/内容构造（借用式 `MessageDraft` 与逐块投影）在 sibling `projection_render`；
//! 本模块保留计划类型、指令恢复、估算与渲染编排。

mod projection_plan;

pub use peri_acp_types::projection::{
    MessageProjectionDirective, ProjectionAction, ProjectionActionEntry, ProjectionTarget,
};
pub use projection_plan::PROJECTION_POLICY_VERSION;

pub(super) use projection_plan::invalid;

#[path = "projection_archive.rs"]
mod archive;

#[path = "projection_render.rs"]
mod render;
pub use projection_plan::ProviderProtocol;

pub use projection_plan::ProviderCapabilities;

pub use projection_plan::MicroCompactPlan;

pub use projection_plan::NO_PERSISTED_DIRECTIVES;

pub use projection_plan::DIRECTIVE_VERSION_MISMATCH;

pub use projection_plan::CORRUPTED_PROJECTION;

pub use projection_plan::PersistedDirectiveRestore;

pub use projection_plan::RestoredProjectionDirectives;

pub use projection_plan::plan_from_persisted_directives;

// ─── 测试专用调用计数 ───────────────────────────────────────────────────────────
//
// 按线程隔离，避免 cargo test 并发执行时测试之间互相污染计数；
// 纯恢复路径（`plan_from_persisted_directives`）不得调用字符估算，由
// projection_restore_test 断言调用次数恒为 0。

#[cfg(test)]
thread_local! {
    static ESTIMATE_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
#[cfg(test)]
pub(crate) use projection_plan::reset_estimate_calls;

#[cfg(test)]
pub(crate) use projection_plan::estimate_call_count;

#[cfg(test)]
pub(crate) use projection_plan::estimate_projection_chars;

use projection_plan::effective_tool_result_action;

pub(crate) use projection_plan::render_persisted_llm_view_with_protected;

pub use projection_plan::render_persisted_llm_view;

pub use projection_plan::render_llm_view;

pub use projection_plan::render_micro_plan;

use projection_plan::head_tail_geometry;

use projection_plan::validate_projected_view;

#[cfg(test)]
#[path = "projection_borrowed_test.rs"]
mod borrowed_tests;
