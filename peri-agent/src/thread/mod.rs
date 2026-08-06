//! Thread 持久化 re-export。
//!
//! 契约类型（`ThreadStore` trait / `ThreadMeta` / `ThreadId` 等）已下沉 peri-acp-types；
//! 存储实现（`SqliteThreadStore` / `FilesystemThreadStore`）已迁入 peri-resources。
//! 本模块仅保留 re-export，保证既有引用路径（`peri_agent::thread::*`）不变。

pub use peri_acp_types::store::{CompactionLifecycle, MessageFlags, ThreadStore};
pub use peri_acp_types::thread::{
    AgentStatus, CancelPolicy, ThreadId, ThreadMeta, ThreadMetaParseError,
};
pub use peri_resources::sessions::{FilesystemThreadStore, SqliteThreadStore};
