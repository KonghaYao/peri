//! Redacted PostgreSQL and transport errors exposed through the session resource contract.

use super::connection::TransportError;
use peri_acp_types::session_resources::{SessionResourceError, SessionResourceErrorKind};

/// 远程失败的稳定分类（不含服务端文本、URL、凭证）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RemoteFailureClass {
    /// 认证或授权被服务端拒绝。
    AuthRejected,
    /// 目标行不存在。
    NotFound,
    /// 预算内没有结果。
    Timeout,
    /// 连接、DNS、TLS 或非预期状态码。
    Transport,
    /// 服务端执行错误（非约束类）。
    ServerError,
    /// 约束冲突（唯一键、外键等）。
    Constraint,
    /// 目标库只读。
    Readonly,
    /// 写冲突/快照忙：可重试，但不代表未生效。
    Busy,
    /// 数据损坏。
    Corrupt,
    /// 驱动侧类型转换或用法不支持。
    Unsupported,
    /// 本驱动未分类的其余失败。
    Unknown,
}

impl RemoteFailureClass {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::AuthRejected => "auth_rejected",
            Self::NotFound => "not_found",
            Self::Timeout => "timeout",
            Self::Transport => "transport",
            Self::ServerError => "server_error",
            Self::Constraint => "constraint",
            Self::Readonly => "readonly",
            Self::Busy => "busy",
            Self::Corrupt => "corrupt",
            Self::Unsupported => "unsupported",
            Self::Unknown => "unknown",
        }
    }

    /// 领域失败映射。
    ///
    /// 注意：A 契约目前没有专门的「凭证被拒绝」变体，`AuthRejected` 暂按配置/输入类失败
    /// 上报（效果确定性为未生效）；是否新增独立变体待 A 阶段确认，本模块不擅自扩展契约。
    pub(crate) fn into_session_resource_error(self) -> SessionResourceError {
        match self {
            Self::AuthRejected => {
                SessionResourceError::new(SessionResourceErrorKind::InvalidInput {
                    detail: "remote session store rejected the credential".to_owned(),
                })
            }
            Self::NotFound => SessionResourceError::new(SessionResourceErrorKind::NotFound),
            Self::Timeout => SessionResourceError::new(SessionResourceErrorKind::Timeout),
            Self::Readonly => SessionResourceError::new(SessionResourceErrorKind::ReadOnlyStore),
            Self::Constraint => SessionResourceError::new(SessionResourceErrorKind::InvalidInput {
                detail: "remote constraint violation".to_owned(),
            }),
            Self::Corrupt => SessionResourceError::new(SessionResourceErrorKind::Corrupt {
                detail: "remote session store contents are unreadable".to_owned(),
            }),
            Self::Transport
            | Self::ServerError
            | Self::Busy
            | Self::Unsupported
            | Self::Unknown => SessionResourceError::new(SessionResourceErrorKind::Unavailable {
                detail: format!("remote session store {}", self.as_str()),
            }),
        }
    }
}

/// Map server diagnostics by SQLSTATE. Server messages can contain row values and credentials.
pub(super) fn sqlstate_class(code: &str) -> RemoteFailureClass {
    use RemoteFailureClass::*;
    match code {
        "42501" => AuthRejected,
        "25006" => Readonly,
        "40001" | "40P01" | "55P03" => Busy,
        "57014" => Timeout,
        "XX001" | "XX002" => Corrupt,
        _ if code.starts_with("28") => AuthRejected,
        _ if code.starts_with("23") => Constraint,
        _ if code.starts_with("08") || code.starts_with("57P0") => Transport,
        _ => ServerError,
    }
}

pub(super) fn invalidates_connection(error: &TransportError) -> bool {
    matches!(
        error.class(),
        RemoteFailureClass::Transport | RemoteFailureClass::Timeout | RemoteFailureClass::Unknown
    )
}

pub(super) fn from_transport_error(error: &TransportError) -> SessionResourceError {
    tracing::debug!(
        class = error.class().as_str(),
        "PostgreSQL session store operation failed"
    );
    error.class().into_session_resource_error()
}
