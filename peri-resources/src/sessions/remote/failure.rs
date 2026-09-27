//! SDK 失败 → 私有分类 → 领域失败。
//!
//! 原始 SDK 文本（`Error::Http(String)`、`Constraint(String)` 等载荷）只在本模块内用于
//! 判别，**不进入**领域失败的 detail、日志或诊断输出：那些载荷可能包含 URL、
//! Authorization 头或 SQL 片段。对外只给稳定分类。

use peri_acp_types::session_resources::{SessionResourceError, SessionResourceErrorKind};
use turso_serverless::Error as SdkError;

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
    /// 目标不是本驱动能读的库。
    NotAdb,
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
            Self::NotAdb => "not_a_database",
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
            Self::NotAdb | Self::Corrupt => {
                SessionResourceError::new(SessionResourceErrorKind::Corrupt {
                    detail: "remote session store contents are unreadable".to_owned(),
                })
            }
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

/// SDK 失败 → 稳定分类。参数顺序固定，先判约束/只读这类确定性最高的失败。
pub(crate) fn classify(error: &SdkError) -> RemoteFailureClass {
    match error {
        SdkError::QueryReturnedNoRows => RemoteFailureClass::NotFound,
        SdkError::Constraint(_) => RemoteFailureClass::Constraint,
        SdkError::Readonly(_) => RemoteFailureClass::Readonly,
        SdkError::Busy(_) | SdkError::BusySnapshot(_) => RemoteFailureClass::Busy,
        SdkError::NotAdb(_) => RemoteFailureClass::NotAdb,
        SdkError::Corrupt(_) => RemoteFailureClass::Corrupt,
        SdkError::Interrupt(_) => RemoteFailureClass::Timeout,
        SdkError::Misuse(_)
        | SdkError::ConversionFailure(_)
        | SdkError::ToSqlConversionFailure(_) => RemoteFailureClass::Unsupported,
        SdkError::Error(_) | SdkError::DatabaseFull(_) => RemoteFailureClass::ServerError,
        SdkError::Http(message) => classify_http(message),
        SdkError::BatchStatementFailed { error, .. }
        | SdkError::BatchRollbackFailed { error, .. } => classify(error),
        // `Error` 是 non_exhaustive：未来变体一律保守归入未知，不假装成功。
        _ => RemoteFailureClass::Unknown,
    }
}

/// 这一失败是否让**当前连接代际**不再可信。
///
/// 只有「没有拿到确定的远端回答」才算：连接/DNS/TLS/非预期状态码（`Transport`）、
/// 预算内没有结果（`Timeout`）、驱动未分类的失败（`Unknown`）。远端明确回答的拒绝
/// （约束、只读、缺行、鉴权、引擎错误）说明流本身仍然活着，不因此换连接——
/// 把确定回答也当作连接失效，只会让一次普通的业务拒绝变成一次多余的重连。
pub(crate) fn invalidates_connection(error: &SdkError) -> bool {
    matches!(
        classify(error),
        RemoteFailureClass::Transport | RemoteFailureClass::Timeout | RemoteFailureClass::Unknown
    )
}

/// 只读 HTTP 失败文本里的安全特征：只看关键字，不转发文本。
fn classify_http(message: &str) -> RemoteFailureClass {
    let lower = message.to_ascii_lowercase();
    if lower.contains("401")
        || lower.contains("403")
        || lower.contains("unauthorized")
        || lower.contains("forbidden")
    {
        RemoteFailureClass::AuthRejected
    } else if lower.contains("timed out") || lower.contains("timeout") {
        RemoteFailureClass::Timeout
    } else {
        RemoteFailureClass::Transport
    }
}
