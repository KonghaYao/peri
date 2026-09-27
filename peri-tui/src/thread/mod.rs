//! Thread 持久化与浏览。
//!
//! (I16-C) `browser::ThreadBrowser` 已退役——kit 单路径下
//! 使用 `kit/panels/thread_browser.rs::ThreadBrowserPanel`（独立实现）。
//! 会话数据一律经 `peri_acp_types::session_resources::SessionResources` 门面；
//! 打开一律经 Resources 门面的部署入口（typed open request），
//! 本模块不再转发任何独立只读 seam 或裸存储类型。

pub use peri_acp_types::session_resources::SessionResources;
pub use peri_acp_types::thread::{ThreadId, ThreadMeta};
