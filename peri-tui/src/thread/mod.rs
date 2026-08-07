//! Thread 持久化与浏览。
//!
//! (I16-C) `browser::ThreadBrowser` 已退役——kit 单路径下
//! 使用 `kit/panels/thread_browser.rs::ThreadBrowserPanel`（独立实现）。
//! 本模块仅 re-export `peri-resources` 中的 `ThreadStore` trait 与
//! `SqliteThreadStore` 实现（契约类型位于 peri-acp-types）。

pub use peri_acp_types::store::ThreadStore;
pub use peri_acp_types::thread::{ThreadId, ThreadMeta};
pub use peri_resources::sessions::SqliteThreadStore;
