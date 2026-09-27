//! Thread 持久化 re-export。
//!
//! 契约类型（`ThreadMeta` / `ThreadId` 等）与业务类型（`CompactionChange` /
//! `MessageFlags`）位于 peri-acp-types；存储实现（`SqliteThreadStore` /
//! `FilesystemThreadStore`）位于 peri-resources，本模块不再 re-export 裸存储
//! trait 与实现——消费侧一律经 `SessionResources` 门面。
//!
//! 仍保留本模块是因为部分 Agent 内部代码按 `crate::thread::*` 引用契约类型；
//! 这里只转发契约，不转发存储。

pub use peri_acp_types::store::{CompactionChange, MessageFlags};
pub use peri_acp_types::thread::{
    AgentStatus, CancelPolicy, ThreadId, ThreadMeta, ThreadMetaParseError,
};
