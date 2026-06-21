//! Goal Steering 子系统 — 长程目标跟踪 + 计费 + steering 注入。
//!
//! 本模块提供 pure data model 和 store trait，无 ACP/middleware 依赖。
//! 并发状态机见 `peri-acp::session::goal_state::GoalState`。

pub mod controller;
pub mod model;
pub mod store;
pub mod view;

pub use controller::{is_active as is_goal_active, GoalController};
pub use model::{GoalAccounting, GoalStatus, ThreadGoal};
pub use store::{GoalStore, GoalStoreError, InMemoryGoalStore};
pub use view::{GoalStateView, GoalViewSnapshot};
