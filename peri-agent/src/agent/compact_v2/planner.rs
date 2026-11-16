//! Planner — Compact 计划和策略类型
//!
//! planner 只能读取 MessageTranscript 和 CompactConfig，绝对不能调用
//! set_truncated、set_excluded、send_persist 或 provider。

mod compact_planner;

pub use compact_planner::ContextPressure;

pub use compact_planner::ResidentHistoryPressure;

pub use compact_planner::is_full_archivable_entry;

pub use compact_planner::CompactPolicy;
/// 需要升级到 Full Compact 的原因
/// 升级到 Full Compact 的原因（事实源 peri-acp-types::compact）
pub use peri_acp_types::compact::FullEscalationReason;

pub use compact_planner::ApplyReport;

#[path = "planner/preview_catalogs.rs"]
mod preview_catalogs;
use preview_catalogs as catalogs;
#[path = "planner/costs.rs"]
mod costs;
#[path = "planner/index.rs"]
pub(crate) mod index;
#[path = "planner/planning.rs"]
mod planning;
#[path = "planner/sources.rs"]
mod sources;

pub use compact_planner::MicroPlanningFacts;
pub use planning::plan_micro_with_facts;

pub use compact_planner::ArchivePlanningScope;

pub use compact_planner::PlanningDiagnostics;

#[cfg(test)]
thread_local! {
    static PLAN_MICRO_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
#[cfg(test)]
pub use compact_planner::reset_plan_micro_calls;

#[cfg(test)]
pub use compact_planner::plan_micro_call_count;

pub use compact_planner::plan_micro;

#[cfg(test)]
#[path = "planner_test.rs"]
mod tests;
