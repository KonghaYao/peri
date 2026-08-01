use std::fmt;

use serde::Serialize;

/// 传输层失败的安全分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportErrorKind {
    Connection,
    Timeout,
    Tls,
    Other,
}

impl fmt::Display for TransportErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Connection => "connection",
            Self::Timeout => "timeout",
            Self::Tls => "tls",
            Self::Other => "other",
        };
        formatter.write_str(value)
    }
}

/// 重试耗尽时最后一次失败的安全分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum RetryErrorKind {
    Transport,
    HttpStatus,
    Protocol,
}

impl fmt::Display for RetryErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Transport => "transport",
            Self::HttpStatus => "http status",
            Self::Protocol => "protocol",
        };
        formatter.write_str(value)
    }
}

/// Provider 协议失败的稳定分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolErrorKind {
    InvalidJsonObject,
    AssistantMessageRequired,
    StreamEndedWithoutCompleted,
    ToolCallMissingId,
    ToolCallMissingName,
    ToolCallInvalidArguments,
    InvalidEndpoint,
    Provider,
    Other,
}

impl fmt::Display for ProtocolErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::InvalidJsonObject => "invalid JSON object",
            Self::AssistantMessageRequired => "assistant message required",
            Self::StreamEndedWithoutCompleted => "stream ended without completion",
            Self::ToolCallMissingId => "tool call missing id",
            Self::ToolCallMissingName => "tool call missing name",
            Self::ToolCallInvalidArguments => "tool call has invalid arguments",
            Self::InvalidEndpoint => "invalid endpoint",
            Self::Provider => "provider failure",
            Self::Other => "other failure",
        };
        formatter.write_str(value)
    }
}

/// Provider 协议失败的安全详情。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolError {
    kind: ProtocolErrorKind,
    summary: Option<SafeErrorContext>,
}

impl ProtocolError {
    fn new(kind: ProtocolErrorKind) -> Self {
        Self {
            kind,
            summary: None,
        }
    }

    fn with_summary(kind: ProtocolErrorKind, summary: impl AsRef<str>) -> Self {
        Self {
            kind,
            summary: Some(SafeErrorContext::new(summary)),
        }
    }

    pub fn kind(&self) -> ProtocolErrorKind {
        self.kind
    }

    pub fn summary(&self) -> Option<&str> {
        self.summary.as_ref().map(SafeErrorContext::as_str)
    }
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.kind)?;
        if let Some(summary) = &self.summary {
            write!(formatter, " ({})", summary.as_str())?;
        }
        Ok(())
    }
}

const INVALID_ERROR_CONTEXT: &str = "[invalid]";
const MAX_ERROR_CONTEXT_LEN: usize = 128;

/// 经过长度、字符集和敏感内容检查的错误上下文。
///
/// 仅内部错误变体可以保存此值，防止未验证的 provider 或 request id 进入格式化输出。
#[derive(Debug, Clone, PartialEq, Eq)]
struct SafeErrorContext(String);

impl SafeErrorContext {
    fn new(value: impl AsRef<str>) -> Self {
        let value = value.as_ref();
        if value.len() <= MAX_ERROR_CONTEXT_LEN
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
            && !contains_sensitive_context(value)
        {
            Self(value.to_owned())
        } else {
            Self(INVALID_ERROR_CONTEXT.into())
        }
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

fn contains_sensitive_context(value: &str) -> bool {
    let normalized = value
        .bytes()
        .filter_map(|byte| {
            byte.is_ascii_alphanumeric()
                .then_some(byte.to_ascii_lowercase())
        })
        .collect::<Vec<_>>();

    [
        b"apikey".as_slice(),
        b"authorization",
        b"cookie",
        b"credential",
        b"password",
        b"prompt",
        b"secret",
        b"token",
        b"sklive",
    ]
    .iter()
    .any(|needle| {
        normalized
            .windows(needle.len())
            .any(|window| window == *needle)
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ModelErrorInner {
    Transport {
        kind: TransportErrorKind,
        provider: Option<SafeErrorContext>,
    },
    HttpStatus {
        status: u16,
        provider: SafeErrorContext,
        request_id: Option<SafeErrorContext>,
    },
    Protocol(ProtocolError),
    Cancelled,
    StreamInterrupted {
        provider: Option<SafeErrorContext>,
        request_id: Option<SafeErrorContext>,
    },
    RetryExhausted {
        attempts: u32,
        last_error: RetryErrorKind,
    },
}

/// 模型调用失败的结构化、安全错误。
///
/// 此错误只保存经过验证的 provider、HTTP status、request id 与受限摘要，绝不保存请求/响应
/// 正文、headers、cookie 或认证凭据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelError(ModelErrorInner);

impl ModelError {
    pub fn transport(kind: TransportErrorKind, provider: Option<impl AsRef<str>>) -> Self {
        Self(ModelErrorInner::Transport {
            kind,
            provider: provider.map(SafeErrorContext::new),
        })
    }

    pub fn http_status(
        status: u16,
        provider: impl AsRef<str>,
        request_id: Option<impl AsRef<str>>,
    ) -> Self {
        Self(ModelErrorInner::HttpStatus {
            status,
            provider: SafeErrorContext::new(provider),
            request_id: request_id.map(SafeErrorContext::new),
        })
    }

    pub fn protocol(kind: ProtocolErrorKind) -> Self {
        Self(ModelErrorInner::Protocol(ProtocolError::new(kind)))
    }

    pub fn protocol_with_summary(kind: ProtocolErrorKind, summary: impl AsRef<str>) -> Self {
        Self(ModelErrorInner::Protocol(ProtocolError::with_summary(
            kind, summary,
        )))
    }

    pub fn cancelled() -> Self {
        Self(ModelErrorInner::Cancelled)
    }

    pub fn stream_interrupted(
        provider: Option<impl AsRef<str>>,
        request_id: Option<impl AsRef<str>>,
    ) -> Self {
        Self(ModelErrorInner::StreamInterrupted {
            provider: provider.map(SafeErrorContext::new),
            request_id: request_id.map(SafeErrorContext::new),
        })
    }

    pub fn retry_exhausted(attempts: u32, last_error: RetryErrorKind) -> Self {
        Self(ModelErrorInner::RetryExhausted {
            attempts,
            last_error,
        })
    }

    pub fn is_cancelled(&self) -> bool {
        matches!(self.0, ModelErrorInner::Cancelled)
    }

    pub fn is_stream_interrupted(&self) -> bool {
        matches!(self.0, ModelErrorInner::StreamInterrupted { .. })
    }

    pub fn transport_kind(&self) -> Option<TransportErrorKind> {
        match &self.0 {
            ModelErrorInner::Transport { kind, .. } => Some(*kind),
            _ => None,
        }
    }

    pub fn http_status_code(&self) -> Option<u16> {
        match &self.0 {
            ModelErrorInner::HttpStatus { status, .. } => Some(*status),
            _ => None,
        }
    }

    pub fn protocol_error(&self) -> Option<ProtocolError> {
        match &self.0 {
            ModelErrorInner::Protocol(error) => Some(error.clone()),
            _ => None,
        }
    }

    pub fn retry_error_kind(&self) -> Option<RetryErrorKind> {
        match &self.0 {
            ModelErrorInner::RetryExhausted { last_error, .. } => Some(*last_error),
            _ => None,
        }
    }

    pub fn provider(&self) -> Option<&str> {
        match &self.0 {
            ModelErrorInner::Transport { provider, .. }
            | ModelErrorInner::StreamInterrupted { provider, .. } => {
                provider.as_ref().map(SafeErrorContext::as_str)
            }
            ModelErrorInner::HttpStatus { provider, .. } => Some(provider.as_str()),
            _ => None,
        }
    }

    pub fn request_id(&self) -> Option<&str> {
        match &self.0 {
            ModelErrorInner::HttpStatus { request_id, .. }
            | ModelErrorInner::StreamInterrupted { request_id, .. } => {
                request_id.as_ref().map(SafeErrorContext::as_str)
            }
            _ => None,
        }
    }
}

impl fmt::Display for ModelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            ModelErrorInner::Transport { kind, provider } => {
                write!(formatter, "model transport error ({kind})")?;
                write_provider_suffix(formatter, provider.as_ref())
            }
            ModelErrorInner::HttpStatus {
                status,
                provider,
                request_id,
            } => {
                write!(
                    formatter,
                    "model HTTP status {status} from {}",
                    provider.as_str()
                )?;
                write_request_id_suffix(formatter, request_id.as_ref())
            }
            ModelErrorInner::Protocol(error) => write!(formatter, "model protocol error: {error}"),
            ModelErrorInner::Cancelled => formatter.write_str("model request was cancelled"),
            ModelErrorInner::StreamInterrupted {
                provider,
                request_id,
            } => {
                formatter.write_str("model stream interrupted")?;
                write_provider_suffix(formatter, provider.as_ref())?;
                write_request_id_suffix(formatter, request_id.as_ref())
            }
            ModelErrorInner::RetryExhausted {
                attempts,
                last_error,
            } => write!(
                formatter,
                "model retry exhausted after {attempts} attempts; last failure: {last_error}"
            ),
        }
    }
}

impl std::error::Error for ModelError {}

fn write_provider_suffix(
    formatter: &mut fmt::Formatter<'_>,
    provider: Option<&SafeErrorContext>,
) -> fmt::Result {
    if let Some(provider) = provider {
        write!(formatter, " from {}", provider.as_str())
    } else {
        Ok(())
    }
}

fn write_request_id_suffix(
    formatter: &mut fmt::Formatter<'_>,
    request_id: Option<&SafeErrorContext>,
) -> fmt::Result {
    if let Some(request_id) = request_id {
        write!(formatter, " (request id: {})", request_id.as_str())
    } else {
        Ok(())
    }
}

pub type ModelResult<T> = Result<T, ModelError>;

#[cfg(test)]
#[path = "error_test.rs"]
mod error_test;
