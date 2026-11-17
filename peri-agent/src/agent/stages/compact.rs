//! Compact 阶段 — 上下文压缩
//!
//! 根据 ContextBudget 计算使用率，调用 `compact_v2::run_compact`：
//! - budget < 0.75：跳过
//! - budget ≥ 0.75：先规划一次，再由统一收益准入决定 Micro / Deferred / Full
//!   - 未达 Full 条件：净收益 G ≥ 完整窗口的 10% 才提交 Micro，否则延后
//!   - 达到 Full 条件：G ≥ max(10%, R) 才提交 Micro，否则直接 Full
//!   - 延后（Deferred）不写入、不做归档验证、不增加失败计数，但必须闭合已发出的事件
//! - force=true：直接 Full（跳过 Micro）
//!
//! 空摘要在本阶段内有界重试；高压下 Full 失败必须终止，不能静默跳过。

mod compact_stage;

pub use compact_stage::run_compact;

#[path = "context_pressure.rs"]
pub(super) mod context_pressure;
pub(crate) use compact_stage::run_compact_core;

pub(super) use compact_stage::resident_gate;

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "compact_test.rs"]
mod tests;

#[cfg(test)]
#[path = "compact_retry_test.rs"]
mod compact_retry_tests;
