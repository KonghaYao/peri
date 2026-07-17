//! Goal Steering 子系统 — 长程目标跟踪 + 计费 + steering 注入。
//!
//! 本模块提供 pure data model 和 store trait，无 ACP/middleware 依赖。
//! 并发状态机见 `peri-acp::session::goal_state::GoalState`。
//!
//! ## BRIDGE_DESIGN（P1-9）
//!
//! `GoalController` / `GoalStateView` 是为破解 peri-middlewares → peri-acp
//! 循环依赖而设计的桥接抽象：
//!
//! ```text
//! peri-agent (trait 定义) ← peri-middlewares (依赖 trait)
//!        ↑ impl                         ↑ 调用
//! peri-acp (GoalState 实现)    peri-middlewares (GoalMiddleware)
//! ```
//!
//! peri-acp 的 `GoalState` 实现 `GoalController + GoalStateView`，
//! peri-middlewares 的 `GoalMiddleware` 通过 `Arc<dyn GoalController>` 注入。
//! 两 crate 都不直接依赖对方——trait 定义在公共层 peri-agent。
//!
//! **替代方案评估**：
//! - 方案 A（当前）：BRIDGE trait 在 peri-agent，有文档标注 → 低成本，保留 DI 灵活性
//! - 方案 B：创建 peri-bridge-types crate → 过度工程化，对单个 trait 不划算
//! - 方案 C：合并 peri-agent + peri-acp → 违反关注点分离原则

pub mod controller;
pub mod model;
pub mod store;
pub mod view;

pub use controller::{is_active as is_goal_active, GoalController};
pub use model::{GoalAccounting, GoalStatus, ThreadGoal};
pub use store::{GoalStore, GoalStoreError, InMemoryGoalStore};
pub use view::{GoalStateView, GoalViewSnapshot};
