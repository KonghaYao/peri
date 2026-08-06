//! Goal 数据模型（事实源 `peri-acp-types::goal`）。
pub use peri_acp_types::goal::{GoalAccounting, GoalStatus, ThreadGoal};

#[cfg(test)]
#[path = "model_test.rs"]
mod tests;
