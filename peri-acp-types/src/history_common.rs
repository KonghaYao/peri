//! 历史读取的共用基础类型：来源/方向、稳定条目身份、durable tail、checkpoint、
//! 冻结继承快照、读取票据、正文描述符/分块与稳定错误分类。
//!
//! 这些类型同时服务于 History V2 适配器与 `SessionResources` 的存储读取端口；
//! 它们只描述**存储事实与有界读取契约**，不携带 `sessionGeneration`、窗口游标或
//! 连接级票据签名——那些属于已删除的旧窗口协议。
//!
//! ## 身份与失效
//!
//! - 条目的稳定身份是 [`HistoryEntryId`]（`{会话}:{message_id}`）；`position`/`rowid`
//!   只是同一快照内的排序键，不能作为稳定身份。
//! - [`HistoryCommittedTail`] 固定一次读取的持久上界；身份/位置复核失败按
//!   [`HistoryWindowError::SnapshotMismatch`] 处理。
//! - [`HistoryReadTicket`] 是不透明短期票据；本模块只承载值，签名与撤销由 Host 的
//!   连接/会话事实负责。票据内容不进入 `Debug`、日志或错误文本。

/// 窗口投影/预算口径版本。
pub const HISTORY_WINDOW_PROJECTION_VERSION: u16 = 1;
/// 服务端默认单页行数。
pub const HISTORY_WINDOW_DEFAULT_LIMIT: u16 = 50;
/// 单页行数上限。
pub const HISTORY_WINDOW_MAX_LIMIT: u16 = 100;
/// 服务端默认页字节预算。
pub const HISTORY_WINDOW_DEFAULT_BYTE_BUDGET: u32 = 512 * 1024;
/// 客户端可请求的页字节预算上限。
pub const HISTORY_WINDOW_MAX_BYTE_BUDGET: u32 = 4 * 1024 * 1024;
/// 正文分块读取的默认块大小。
pub const HISTORY_WINDOW_DEFAULT_CHUNK_SIZE: u32 = 64 * 1024;
/// 正文分块读取的单次上限。
pub const HISTORY_WINDOW_MAX_CHUNK_SIZE: u32 = 256 * 1024;

// ─── 来源与方向 ────────────────────────────────────────────────────────────────

// V1 与 V2 共用同一套存储事实类型：保留 `history_window` 的既有定义与语义，
// `history_common` 只做再导出，避免两套同构类型在边界处互不兼容。
pub use crate::history_window::{
    HistoryBodyChunk, HistoryBodyDescriptor, HistoryCheckpoint, HistoryCommittedTail,
    HistoryEntryId, HistoryInheritedSnapshot, HistoryInheritedWindow, HistoryReadTicket,
    HistorySource, HistoryWindowDirection,
};

// ─── 错误码 ────────────────────────────────────────────────────────────────────

// V1 与 V2 共用同一错误分类：保留 V1 的稳定 code/reason 映射，V2 复用而不复制。
pub use crate::history_window::HistoryWindowError;

#[cfg(test)]
#[path = "history_common_test.rs"]
mod tests;
