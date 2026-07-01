//! Thread 持久化与浏览。
//!
//! (I16-C) `browser::ThreadBrowser`（legacy 面板）已退役——kit 单路径下
//! 使用 `kit/panels/thread_browser.rs::ThreadBrowserPanel`（独立实现）。
//! 本模块仅 re-export `peri_agent::thread` 中的 `ThreadStore` trait 与
//! `SqliteThreadStore` 实现。

pub use peri_agent::thread::{SqliteThreadStore, ThreadId, ThreadMeta, ThreadStore};
