//! Goal 持久化存储（事实源 `peri-acp-types::goal`）。
pub use peri_acp_types::goal::{GoalStore, GoalStoreError, InMemoryGoalStore};
// 测试（`store_test.rs`）经 `super::*` 引用 ThreadGoal（原 store.rs 私有 use 语义）

#[cfg(test)]
#[path = "store_test.rs"]
mod tests;
