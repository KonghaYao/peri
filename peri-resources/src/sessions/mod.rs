//! peri-sessions — 会话持久化子模块（自 peri-agent/src/thread 迁入）。
//!
//! 直操 sqlite：`SqliteThreadStore` 为生产实现；`FilesystemThreadStore` 为纯测试用途。
//! 契约类型（`ThreadStore` trait / `ThreadMeta` / `BaseMessage` / `MessageFlags`）位于
//! peri-acp-types（接口契约归 peri-acp-types），本模块仅实现，不解释业务语义。

mod filesystem;
mod sqlite_store;

pub use filesystem::FilesystemThreadStore;
pub use sqlite_store::SqliteThreadStore;
