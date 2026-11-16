//! Compact v2 — 标记代替删除的上下文压缩
//!
//! 触发与准入（唯一决策点见 [`decision`]）：
//! - budget < micro_compact_threshold：跳过
//! - force=true：直接 Full（不受最小收益门槛限制）
//! - 自动关闭、驻留字节超限、C == 0 的独立路径保持原语义
//! - 进入 Micro 区间：先规划一次并得到最终完整渲染的净收益 G（`estimated_tokens_saved`）
//!   - 未达 Full 条件：G >= M（完整窗口的 10%，向上取整）且候选非空 → Micro，否则延后
//!   - 达到 Full 条件：G >= max(M, R) 且候选非空 → Micro，否则直接 Full
//!   - 先决定最终策略，再执行一次提交；Full 路径不得先提交本轮 Micro
//! - shadow 只报告决策，不提交
//!
//! 与 v1 的区别：v2 基于 `MessageTranscript` 标记 API，不修改消息本体，
//! 旧消息标 `excluded` 后 `visible_messages()` 自动过滤。
//! Full Compact 通过 `peri_model::Model` 标准链路请求摘要。
//! 所有注入消息使用 `BaseMessage::human()` —— 禁止 System，防止 hoist 污染 FrozenContext。

mod compact_engine;

pub mod config;
pub(crate) mod decision;
pub mod exposure;
pub mod full;
pub mod hard_memory;
pub mod micro;
pub mod planner;
pub mod projection;
pub mod smart;

// ─── 公共重导出：保持外部调用路径不变 ─────────────────────────────────────────────

pub use config::{CompactConfig, CONTINUATION_HINT};
pub use decision::min_micro_gain_tokens;
pub use full::{extract_file_info, extract_skill_names};
pub use hard_memory::{
    from_parts, snapshot_from_runtime, HardMemoryGoal, HardMemorySnapshot, HardMemorySource,
    HardMemoryTask, RuntimeHardMemory, HARD_MEMORY_HEADER,
};
pub use micro::micro_compact;
pub use planner::{
    plan_micro, ApplyReport, CompactPolicy, ContextPressure, FullEscalationReason,
    ResidentHistoryPressure,
};
pub use projection::{
    plan_from_persisted_directives, render_llm_view, MessageProjectionDirective, MicroCompactPlan,
    PersistedDirectiveRestore, ProjectionAction, ProjectionActionEntry, ProjectionTarget,
    ProviderCapabilities, ProviderProtocol, RestoredProjectionDirectives, CORRUPTED_PROJECTION,
    DIRECTIVE_VERSION_MISMATCH, NO_PERSISTED_DIRECTIVES, PROJECTION_POLICY_VERSION,
};

pub use compact_engine::CompactResult;

/// Compact 执行语义结果（事实源 peri-acp-types::compact）
pub use peri_acp_types::compact::CompactOutcome;

pub use compact_engine::FullSummaryNotice;

pub use compact_engine::CompactAction;

pub use compact_engine::determine_compact_action;

pub use compact_engine::run_compact;

pub use compact_engine::run_compact_with_protected;

pub use compact_engine::run_compact_with_facts;

pub use compact_engine::run_compact_with_facts_and_resident;

pub use compact_engine::run_compact_with_hard_memory;

#[cfg(test)]
#[path = "projection_test.rs"]
mod projection_tests;

#[cfg(test)]
#[path = "projection_render_test.rs"]
mod projection_render_tests;

#[cfg(test)]
#[path = "projection_restore_test.rs"]
mod projection_restore_tests;

#[cfg(test)]
#[path = "projection_estimate_test.rs"]
mod projection_estimate_tests;

#[cfg(test)]
#[path = "projection_tool_use_test.rs"]
mod projection_tool_use_tests;

#[cfg(test)]
mod _test;

#[cfg(test)]
#[path = "trigger_test.rs"]
mod trigger_test;
pub(crate) use compact_engine::micro_failure;

#[cfg(test)]
#[path = "archive_lifecycle_test.rs"]
mod archive_lifecycle_tests;

#[cfg(test)]
#[path = "hard_memory_test.rs"]
mod hard_memory_tests;

#[cfg(test)]
#[path = "hard_memory_compact_test.rs"]
mod hard_memory_compact_tests;

#[cfg(test)]
#[path = "resident_budget_test.rs"]
mod resident_budget_tests;

#[cfg(test)]
#[path = "gain_gate_test.rs"]
mod gain_gate_tests;

#[cfg(test)]
#[path = "projection_position_test.rs"]
mod projection_position_tests;
