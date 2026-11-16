//! GoalState — goal 子系统的并发状态机。
//!
//! 基于 `Arc<RwLock<GoalStateInner>>` + `parking_lot::RwLock`（短锁无 await）。
//! store 写入失败时退化为纯内存模式（snapshot 读仍可用），不阻塞 agent。
//!
//! 并发模型：read-and-reset + epoch（本 Task 先实现基础读写，account_progress 的
//! read-and-reset 在 Task 6 实现）。
//!
//! Blocked goal 的周期恢复见 `recovery`：会话装配时注入 mailbox 与间隔，
//! `block` 后按间隔重开 goal 并投递 Required Defer。

mod recovery;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::RwLock;
use peri_acp_types::goal::{GoalStatus, GoalStore, ThreadGoal};
use peri_acp_types::session::MessageQueue;

use recovery::{RecoveryControl, RecoverySchedule, RecoveryTrigger};

pub(crate) use goal_state::DEFAULT_GOAL_RECOVERY_INTERVAL;

pub use goal_state::GoalSnapshot;

use goal_state::GoalStateInner;

pub use goal_state::GoalState;

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;

#[path = "goal_state.rs"]
#[expect(
    clippy::module_inception,
    reason = "The facade preserves API paths; the leaf is named for its primary type as required by the Rust guide."
)]
mod goal_state;
