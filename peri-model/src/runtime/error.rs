use std::fmt;

use serde::{Deserialize, Serialize};

/// 传输层失败的分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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

/// 重试耗尽时最后一次失败的分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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

/// Provider 协议失败的有界详情。
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
            summary: SafeErrorContext::new(summary),
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

const MAX_ERROR_CONTEXT_LEN: usize = 128;

/// 具有长度上限的错误上下文。
#[derive(Debug, Clone, PartialEq, Eq)]
struct SafeErrorContext(String);

impl SafeErrorContext {
    fn new(value: impl AsRef<str>) -> Option<Self> {
        let value = value.as_ref();
        (value.len() <= MAX_ERROR_CONTEXT_LEN).then(|| Self(value.to_owned()))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ModelErrorInner {
    Transport {
        kind: TransportErrorKind,
        provider: Option<SafeErrorContext>,
    },
    HttpStatus {
        status: u16,
        provider: Option<SafeErrorContext>,
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
        diagnostic: Option<ModelErrorDiagnostic>,
    },
}

/// A bounded model failure projection for Agent and ACP boundaries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelErrorDiagnostic {
    category: ModelErrorCategory,
    status: Option<u16>,
    provider: Option<String>,
    request_id: Option<String>,
    transport: Option<TransportErrorKind>,
    protocol: Option<ProtocolErrorKind>,
    retry_attempts: Option<u32>,
    retry_kind: Option<RetryErrorKind>,
    message: Option<String>,
    body: Option<String>,
    causes: Vec<String>,
}

/// Untrusted diagnostic facts supplied by a serialization boundary. Keeping
/// these inputs together makes it harder to omit fields covered by validation.
pub struct ModelErrorDiagnosticParts<'a> {
    pub category: ModelErrorCategory,
    pub status: Option<u16>,
    pub provider: Option<&'a str>,
    pub request_id: Option<&'a str>,
    pub transport: Option<TransportErrorKind>,
    pub protocol: Option<ProtocolErrorKind>,
    pub retry_attempts: Option<u32>,
    pub retry_kind: Option<RetryErrorKind>,
    pub message: Option<&'a str>,
    pub body: Option<&'a str>,
    pub causes: &'a [String],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelErrorCategory {
    Transport,
    HttpStatus,
    Protocol,
    Cancelled,
    StreamInterrupted,
    RetryExhausted,
}

impl ModelErrorDiagnostic {
    /// Construct a diagnostic after validating its shape and identity lengths.
    pub fn from_parts(parts: ModelErrorDiagnosticParts<'_>) -> Option<Self> {
        let ModelErrorDiagnosticParts {
            category,
            status,
            provider,
            request_id,
            transport,
            protocol,
            retry_attempts,
            retry_kind,
            message,
            body,
            causes,
        } = parts;
        let retry_pair = match (retry_attempts, retry_kind) {
            (None, None) => true,
            (Some(attempts), Some(_)) => attempts > 0,
            _ => false,
        };
        let retry_matches_category = match retry_kind {
            None => retry_attempts.is_none(),
            Some(kind) => match category {
                ModelErrorCategory::Transport => kind == RetryErrorKind::Transport,
                ModelErrorCategory::HttpStatus => kind == RetryErrorKind::HttpStatus,
                ModelErrorCategory::Protocol => kind == RetryErrorKind::Protocol,
                ModelErrorCategory::RetryExhausted => true,
                ModelErrorCategory::Cancelled | ModelErrorCategory::StreamInterrupted => false,
            },
        };
        let valid_shape = match category {
            ModelErrorCategory::Transport => {
                transport.is_some()
                    && status.is_none()
                    && protocol.is_none()
                    && retry_pair
                    && retry_matches_category
            }
            ModelErrorCategory::HttpStatus => {
                status.is_some()
                    && transport.is_none()
                    && protocol.is_none()
                    && retry_pair
                    && retry_matches_category
            }
            ModelErrorCategory::Protocol => {
                protocol.is_some()
                    && status.is_none()
                    && transport.is_none()
                    && retry_pair
                    && retry_matches_category
            }
            ModelErrorCategory::Cancelled => {
                status.is_none()
                    && transport.is_none()
                    && protocol.is_none()
                    && retry_pair
                    && retry_matches_category
                    && provider.is_none()
                    && request_id.is_none()
            }
            ModelErrorCategory::StreamInterrupted => {
                status.is_none()
                    && transport.is_none()
                    && protocol.is_none()
                    && retry_pair
                    && retry_matches_category
            }
            ModelErrorCategory::RetryExhausted => {
                let retry_shape = retry_pair && retry_kind.is_some();
                let cause_shape = match retry_kind {
                    None => status.is_none() && transport.is_none() && protocol.is_none(),
                    Some(RetryErrorKind::Transport) => status.is_none() && protocol.is_none(),
                    Some(RetryErrorKind::HttpStatus) => transport.is_none() && protocol.is_none(),
                    Some(RetryErrorKind::Protocol) => status.is_none() && transport.is_none(),
                };
                retry_shape && cause_shape
            }
        };
        if !valid_shape {
            return None;
        }
        let provider = match provider {
            Some(value) => Some(SafeErrorContext::new(value)?.0),
            None => None,
        };
        let request_id = match request_id {
            Some(value) => Some(SafeErrorContext::new(value)?.0),
            None => None,
        };
        Some(Self {
            category,
            status,
            provider,
            request_id,
            transport,
            protocol,
            retry_attempts,
            retry_kind,
            message: message.map(bounded_text),
            body: body.map(bounded_text),
            causes: bounded_causes(causes.iter().cloned()),
        })
    }

    pub fn category(&self) -> ModelErrorCategory {
        self.category
    }

    pub fn category_name(&self) -> &'static str {
        match self.category() {
            ModelErrorCategory::Transport => "transport",
            ModelErrorCategory::HttpStatus => "http_status",
            ModelErrorCategory::Protocol => "protocol",
            ModelErrorCategory::Cancelled => "cancelled",
            ModelErrorCategory::StreamInterrupted => "stream_interrupted",
            ModelErrorCategory::RetryExhausted => "retry_exhausted",
        }
    }

    pub fn status(&self) -> Option<u16> {
        self.status
    }

    pub fn provider(&self) -> Option<&str> {
        self.provider.as_deref()
    }

    pub fn request_id(&self) -> Option<&str> {
        self.request_id.as_deref()
    }

    pub fn transport(&self) -> Option<TransportErrorKind> {
        self.transport
    }

    pub fn protocol(&self) -> Option<ProtocolErrorKind> {
        self.protocol
    }

    pub fn retry_attempts(&self) -> Option<u32> {
        self.retry_attempts
    }

    pub fn retry_kind(&self) -> Option<RetryErrorKind> {
        self.retry_kind
    }

    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    pub fn body(&self) -> Option<&str> {
        self.body.as_deref()
    }

    pub fn causes(&self) -> &[String] {
        &self.causes
    }

    fn with_retry(mut self, attempts: u32, kind: RetryErrorKind) -> Self {
        self.retry_attempts = Some(attempts);
        self.retry_kind = Some(kind);
        self
    }
}

/// 模型调用失败的结构化、有界错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelError(
    ModelErrorInner,
    Option<(ModelErrorDiagnostic, bool)>,
    ErrorDetails,
);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ErrorDetails {
    message: Option<String>,
    body: Option<String>,
    causes: Vec<String>,
}

pub(crate) const MAX_DIAGNOSTIC_BYTES: usize = 16_384;
const MAX_CAUSES: usize = 16;
const TRUNCATED_MARKER: &str = "[TRUNCATED]";

fn bounded_text(value: impl AsRef<str>) -> String {
    let value = value.as_ref();
    if value.len() <= MAX_DIAGNOSTIC_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_DIAGNOSTIC_BYTES - TRUNCATED_MARKER.len();
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{TRUNCATED_MARKER}", &value[..end])
}

fn bounded_causes(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut values = values.into_iter();
    let mut causes = values
        .by_ref()
        .take(MAX_CAUSES)
        .map(bounded_text)
        .collect::<Vec<_>>();
    if values.next().is_some() {
        causes[MAX_CAUSES - 1] = "[TRUNCATED: additional causes omitted]".into();
    }
    causes
}

impl ModelError {
    fn new(inner: ModelErrorInner) -> Self {
        Self(inner, None, ErrorDetails::default())
    }

    pub fn with_message(mut self, message: impl AsRef<str>) -> Self {
        self.2.message = Some(bounded_text(message));
        self
    }

    pub fn with_body(mut self, body: impl AsRef<str>) -> Self {
        self.2.body = Some(bounded_text(body));
        self
    }

    pub fn with_causes(mut self, causes: impl IntoIterator<Item = String>) -> Self {
        self.2.causes = bounded_causes(causes);
        self
    }

    pub(crate) fn with_error(self, error: &(dyn std::error::Error + 'static)) -> Self {
        let mut causes = Vec::new();
        let mut source = error.source();
        while let Some(cause) = source {
            if causes.len() > MAX_CAUSES {
                break;
            }
            causes.push(bounded_text(cause.to_string()));
            source = cause.source();
        }
        self.with_message(error.to_string()).with_causes(causes)
    }

    /// 中断降级前的有界诊断及 observer 是否已负责记录；不改变错误分类。
    pub fn interruption_diagnostic(&self) -> Option<&ModelErrorDiagnostic> {
        self.1.as_ref().map(|(diagnostic, _)| diagnostic)
    }

    pub fn interruption_logged(&self) -> bool {
        self.1.as_ref().is_some_and(|(_, logged)| *logged)
    }

    pub(crate) fn with_interruption_diagnostic(
        mut self,
        diagnostic: ModelErrorDiagnostic,
        logged: bool,
    ) -> Self {
        self.1 = Some((diagnostic, logged));
        self
    }

    pub fn transport(kind: TransportErrorKind, provider: Option<impl AsRef<str>>) -> Self {
        Self::new(ModelErrorInner::Transport {
            kind,
            provider: provider.and_then(|value| SafeErrorContext::new(value)),
        })
    }

    pub fn http_status(
        status: u16,
        provider: impl AsRef<str>,
        request_id: Option<impl AsRef<str>>,
    ) -> Self {
        Self::new(ModelErrorInner::HttpStatus {
            status,
            provider: SafeErrorContext::new(provider),
            request_id: request_id.and_then(|value| SafeErrorContext::new(value)),
        })
    }

    pub fn protocol(kind: ProtocolErrorKind) -> Self {
        Self::new(ModelErrorInner::Protocol(ProtocolError::new(kind)))
    }

    pub fn protocol_with_summary(kind: ProtocolErrorKind, summary: impl AsRef<str>) -> Self {
        Self::new(ModelErrorInner::Protocol(ProtocolError::with_summary(
            kind,
            summary.as_ref(),
        )))
        .with_message(summary)
    }

    pub fn cancelled() -> Self {
        Self::new(ModelErrorInner::Cancelled)
    }

    pub fn stream_interrupted(
        provider: Option<impl AsRef<str>>,
        request_id: Option<impl AsRef<str>>,
    ) -> Self {
        Self::new(ModelErrorInner::StreamInterrupted {
            provider: provider.and_then(|value| SafeErrorContext::new(value)),
            request_id: request_id.and_then(|value| SafeErrorContext::new(value)),
        })
    }

    pub fn retry_exhausted(attempts: u32, last_error: RetryErrorKind) -> Option<Self> {
        (attempts > 0).then_some(Self::new(ModelErrorInner::RetryExhausted {
            attempts,
            last_error,
            diagnostic: None,
        }))
    }

    pub(crate) fn retry_exhausted_with_context(
        attempts: u32,
        last_error: RetryErrorKind,
        error: &Self,
    ) -> Self {
        debug_assert!(
            attempts > 0,
            "retry exhaustion requires at least one attempt"
        );
        Self::new(ModelErrorInner::RetryExhausted {
            attempts,
            last_error,
            diagnostic: Some(error.diagnostic().with_retry(attempts, last_error)),
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
            ModelErrorInner::RetryExhausted { diagnostic, .. } => {
                diagnostic.as_ref().and_then(|value| value.transport)
            }
            _ => None,
        }
    }

    pub fn http_status_code(&self) -> Option<u16> {
        match &self.0 {
            ModelErrorInner::HttpStatus { status, .. } => Some(*status),
            ModelErrorInner::RetryExhausted { diagnostic, .. } => {
                diagnostic.as_ref().and_then(|value| value.status)
            }
            _ => None,
        }
    }

    pub fn protocol_error(&self) -> Option<ProtocolError> {
        match &self.0 {
            ModelErrorInner::Protocol(error) => Some(error.clone()),
            ModelErrorInner::RetryExhausted { diagnostic, .. } => diagnostic
                .as_ref()
                .and_then(|value| value.protocol)
                .map(ProtocolError::new),
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
            ModelErrorInner::HttpStatus { provider, .. } => {
                provider.as_ref().map(SafeErrorContext::as_str)
            }
            ModelErrorInner::RetryExhausted { diagnostic, .. } => diagnostic
                .as_ref()
                .and_then(|value| value.provider.as_deref()),
            _ => None,
        }
    }

    pub fn request_id(&self) -> Option<&str> {
        match &self.0 {
            ModelErrorInner::HttpStatus { request_id, .. }
            | ModelErrorInner::StreamInterrupted { request_id, .. } => {
                request_id.as_ref().map(SafeErrorContext::as_str)
            }
            ModelErrorInner::RetryExhausted { diagnostic, .. } => diagnostic
                .as_ref()
                .and_then(|value| value.request_id.as_deref()),
            _ => None,
        }
    }

    /// Return the bounded diagnostic projection. Invalid or absent provider and
    /// request identities are represented as `None`, never as a sentinel.
    pub fn diagnostic(&self) -> ModelErrorDiagnostic {
        let mut diagnostic = match &self.0 {
            ModelErrorInner::Transport { kind, provider } => ModelErrorDiagnostic {
                category: ModelErrorCategory::Transport,
                status: None,
                provider: provider.as_ref().map(|value| value.as_str().to_owned()),
                request_id: None,
                transport: Some(*kind),
                protocol: None,
                retry_attempts: None,
                retry_kind: None,
                message: None,
                body: None,
                causes: Vec::new(),
            },
            ModelErrorInner::HttpStatus {
                status,
                provider,
                request_id,
            } => ModelErrorDiagnostic {
                category: ModelErrorCategory::HttpStatus,
                status: Some(*status),
                provider: provider.as_ref().map(|value| value.as_str().to_owned()),
                request_id: request_id.as_ref().map(|value| value.as_str().to_owned()),
                transport: None,
                protocol: None,
                retry_attempts: None,
                retry_kind: None,
                message: None,
                body: None,
                causes: Vec::new(),
            },
            ModelErrorInner::Protocol(error) => ModelErrorDiagnostic {
                category: ModelErrorCategory::Protocol,
                status: None,
                provider: None,
                request_id: None,
                transport: None,
                protocol: Some(error.kind()),
                retry_attempts: None,
                retry_kind: None,
                message: None,
                body: None,
                causes: Vec::new(),
            },
            ModelErrorInner::Cancelled => ModelErrorDiagnostic {
                category: ModelErrorCategory::Cancelled,
                status: None,
                provider: None,
                request_id: None,
                transport: None,
                protocol: None,
                retry_attempts: None,
                retry_kind: None,
                message: None,
                body: None,
                causes: Vec::new(),
            },
            ModelErrorInner::StreamInterrupted {
                provider,
                request_id,
            } => ModelErrorDiagnostic {
                category: ModelErrorCategory::StreamInterrupted,
                status: None,
                provider: provider.as_ref().map(|value| value.as_str().to_owned()),
                request_id: request_id.as_ref().map(|value| value.as_str().to_owned()),
                transport: None,
                protocol: None,
                retry_attempts: None,
                retry_kind: None,
                message: None,
                body: None,
                causes: Vec::new(),
            },
            ModelErrorInner::RetryExhausted {
                attempts,
                last_error,
                diagnostic,
            } => diagnostic.clone().unwrap_or(ModelErrorDiagnostic {
                category: ModelErrorCategory::RetryExhausted,
                status: None,
                provider: None,
                request_id: None,
                transport: None,
                protocol: None,
                retry_attempts: Some(*attempts),
                retry_kind: Some(*last_error),
                message: None,
                body: None,
                causes: Vec::new(),
            }),
        };
        if self.2.message.is_some() {
            diagnostic.message = self.2.message.clone();
        }
        if self.2.body.is_some() {
            diagnostic.body = self.2.body.clone();
        }
        if !self.2.causes.is_empty() {
            diagnostic.causes = self.2.causes.clone();
        }
        diagnostic
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
                write!(formatter, "model HTTP status {status}")?;
                write_provider_suffix(formatter, provider.as_ref())?;
                write_request_id_suffix(formatter, request_id.as_ref())
            }
            ModelErrorInner::Protocol(error) if self.2.message.is_some() => {
                write!(formatter, "model protocol error: {}", error.kind())
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
                ..
            } => write!(
                formatter,
                "model retry exhausted after {attempts} attempts; last failure: {last_error}"
            ),
        }?;
        let diagnostic = self.diagnostic();
        if let Some(message) = diagnostic.message() {
            write!(formatter, ": {message}")?;
        }
        if let Some(body) = diagnostic.body() {
            write!(formatter, "; body: {body}")?;
        }
        for cause in diagnostic.causes() {
            write!(formatter, "; caused by: {cause}")?;
        }
        Ok(())
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
