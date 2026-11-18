//! 会话资源门面 — 消费侧唯一的会话行为契约。
//!
//! 业务侧（Agent / ACP / Controller / TUI）只依赖本 trait 的行为语义，注入的是
//! `Arc<dyn SessionResources>`。数据的持久化位置（本机 SQLite / Turso Cloud）与
//! 本机执行事实（发现、绑定、排空）由资源内部两面分别实现，
//! 数据端口不向业务暴露事务、CAS、SQL batch、连接或补偿令牌。
//!
//! 三条必须成立的约束：
//!
//! 1. **不提供 no-op 默认实现。** 每个行为都是必需方法：无法完成的行为必须返回
//!    明确失败，不能让调用方把「不支持」当成「没有数据」。
//! 2. **声明即保证。** 行为要么满足完整后置条件，要么在副作用前失败；声明
//!    [`DataCapabilities::Complete`] 的 adapter 不得对任何行为退化为 no-op。
//! 3. **效果确定性单独表达。** 失败原因与「是否已生效」分别建模，见
//!    [`SessionResourceError`] 与 [`MutationOutcome`]。
//!
//! 只读入口（`AccessMode::ReadOnly`）不得靠创建一个临时未绑定会话绕过执行授权。
//! 旧 `ThreadStore`（[`crate::store::ThreadStore`]）是迁移桥，见其模块文档。

pub mod history;

pub mod history_window;
pub use history_window::{
    HistoryEntryMetadata, HistoryWindowEntry, HistoryWindowPagePlan, HistoryWindowReadIntent,
    HistoryWindowRows, HistoryWindowTailExpectation, InheritedHistoryEntry,
    SessionHistoryWindowBounds,
};

mod metadata;
pub use metadata::*;

mod session_resource_error;
use session_resource_error as error;

pub use error::{
    CloseSettlement, SessionResourceError, SessionResourceErrorKind, SessionResourceResult,
};

#[expect(
    clippy::module_inception,
    reason = "The facade preserves API paths; the leaf is named for its primary type as required by the Rust guide."
)]
mod session_resources;
pub use self::session_resources::SessionResources;

mod child_resume_claim;
pub use child_resume_claim::ChildResumeClaim;

mod session_initialization;
pub use session_initialization::SessionInitialization;

mod session_store_shutdown_port;
pub use lifecycle::SessionStoreShutdownPort;
use session_store_shutdown_port as lifecycle;

mod terminal_settlement_outcome;
pub use settlement::TerminalSettlementOutcome;
use terminal_settlement_outcome as settlement;

#[cfg(test)]
#[path = "session_resources_test.rs"]
mod tests;
