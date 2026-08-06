//! CronOwner — Agent-owned cron scheduler bridge（契约面归位）。
//!
//! 3.0 批 2：`CronOwner` 定义迁入 `peri-acp-types::session`（跨层契约：
//! Agent 层持有实现与执行权，ACP / middlewares 只依赖契约类型）；
//! 本文件保留 re-export 保兼容（`peri_agent::agent::session::cron_owner::CronOwner`
//! 与 `peri_acp_types::session::CronOwner` 为同一类型）。
//!
//! 职责：trigger-to-inbox 转发循环（`trigger_rx` → `SessionInbox`），
//! 取消跟随 session 生命周期（shutdown token）。

pub use peri_acp_types::session::CronOwner;

#[cfg(test)]
#[path = "cron_owner_test.rs"]
mod tests;
