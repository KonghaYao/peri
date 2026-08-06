//! Type contract layer between layers. Depends only on serde/chrono/thiserror/async-trait.
//!
//! Active modules:
//! - `event_data` — unstable-event payload structs consumed by peri-tui
//! - `peri_caps` — capability negotiation flags consumed by both peri-acp and peri-tui
//! - `summary` — migrated event DTOs re-exported via peri-acp::event
//! - `messages` — 消息契约（BaseMessage/MessageContent/...），peri-agent 保留 re-export
//! - `thread` — Thread 元数据契约（ThreadMeta/ThreadId/...）
//! - `store` — ThreadStore 持久化契约（trait + CompactionLifecycle + MessageFlags）
//! - `projection` — compact 投影指令纯数据契约

pub mod event_data;
pub mod identity;
pub mod messages;
pub mod peri_caps;
pub use peri_caps::PeriCaps;
pub mod projection;
pub mod store;
pub mod summary;
pub mod thread;
