//! peri-runtime — Runtime 层：多 session 编排器（`docs/top-level.md` §3）。
//!
//! 薄编排器：唯一持有 `session_id -> SessionHandle` 映射；无状态、无持久态、
//! 无业务配置（session 状态在 Agent 层各 session 内，其余全部注入）。
//!
//! 职责（§3/§9，伞形 PRD 决策 20/21）：
//! - 多 session 编排：注册/销毁（[`Runtime::register`]/[`Runtime::destroy`]）、
//!   cancel 定位与转发（[`Runtime::cancel`]）、run 发起（[`Runtime::run`]）
//! - 事件聚合补打（[`Runtime::stamp`]）：session_id 按 session 维度补打、
//!   session_seq 单调递增（§9 事件契约，复用 `peri-acp-types::identity` 类型）
//!
//! 依赖方向（§0）：仅 `peri-acp-types` / `peri-agent`；不依赖 peri-acp /
//! peri-tui / peri-controller。生产接线（Agent EventBus → Runtime 聚合）随
//! executor 拆分（L5）落地，本 crate 不承载业务装配。

pub mod error;
pub mod runtime;

pub use error::RuntimeError;
pub use runtime::{Runtime, SessionHandle, UnstampedEvent};
