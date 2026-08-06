//! SessionInbox — await-wake wrapper around v2 MessageQueue（契约面归位）。
//!
//! 3.0 批 2：`SessionInbox` / `InboxHandle` 定义迁入 `peri-acp-types::session`
//! （跨层契约：Agent 层持有实现与执行权，ACP / middlewares 只依赖契约类型）；
//! 本文件保留 re-export 保兼容（`peri_agent::agent::session::inbox::SessionInbox`
//! 与 `peri_acp_types::session::SessionInbox` 为同一类型）。
//!
//! ## Invariants
//!
//! 1. `await_wake` 是 **non-destructive** — it does NOT drain. `stages/receive.rs`
//!    uses `drain_all` in the loop body; `drain_for_receive` and `drain_for_end`
//!    remain as public APIs for external flush paths (executor helpers, tests).
//! 2. Pushers from Agent/ACP layer use `InboxHandle` (cloneable, `Send + Sync`).
//! 3. TUI should NOT have access to `InboxHandle` — all async events
//!    (cron/channel/workflow/bg_results) flow through Agent/ACP layer →
//!    `InboxHandle::push` → `MessageQueue` → `await_wake` → Receive's `drain_all`.

pub use peri_acp_types::session::{InboxHandle, SessionInbox};

#[cfg(test)]
#[path = "inbox_test.rs"]
mod tests;
