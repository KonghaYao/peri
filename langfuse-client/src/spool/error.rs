//! 暂存端口错误（契约 §3.2）。
//!
//! 错误只表达分类与可恢复性，不携带宿主路径、凭据或正文；`Io` 保留底层
//! `std::io::Error` 以便诊断，但它不含对象正文。

use thiserror::Error;

use super::types::ObjectId;

#[path = "error/spool_error.rs"]
mod spool_error;
pub use spool_error::SpoolError;
