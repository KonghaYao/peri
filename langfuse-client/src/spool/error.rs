//! 暂存端口错误（契约 §3.2）。
//!
//! 错误只表达分类与可恢复性，不携带宿主路径、凭据或正文；`Io` 保留底层
//! `std::io::Error` 以便诊断，但它不含对象正文。

use thiserror::Error;

use super::types::ObjectId;

/// 文件端口错误；`Send + Sync`，可直接进入 [`crate::error::LangfuseError::Spool`]。
#[derive(Debug, Error)]
pub enum SpoolError {
    /// 本机没有可用的私有暂存目录，或文件系统不支持所需的 fsync/lock 语义。
    #[error("telemetry spool storage is unavailable")]
    Unavailable,

    /// 预留超出 route 配额；`available` 为当前可租用额度。
    #[error(
        "telemetry spool quota exceeded: requested {requested} bytes, available {available} bytes"
    )]
    QuotaExceeded { requested: u64, available: u64 },

    /// 资源当前被其他 owner 占用（清理槽耗尽或对象已被领取）。
    #[error("telemetry spool resource is busy")]
    Busy,

    /// 对象不存在或已被删除；调用方不得据此推断「已交付」。
    #[error("telemetry spool object was not found")]
    NotFound,

    /// 对象内容或摘要与元数据不一致；损坏对象只隔离诊断与清理，绝不发送。
    #[error("telemetry spool object is corrupt")]
    Corrupt { id: ObjectId },

    /// 元数据版本、长度、期限或状态不合法。
    #[error("telemetry spool metadata is invalid")]
    InvalidMetadata,

    /// 底层 IO 失败；`operation` 为静态操作名，不含路径与凭据。
    #[error("telemetry spool {operation} failed: {source}")]
    Io {
        operation: &'static str,
        source: std::io::Error,
    },
}
