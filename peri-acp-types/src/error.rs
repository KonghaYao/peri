//! 层边界错误契约（§9 错误模型：边界类型化，层内 anyhow）。
//!
//! `AgentError` 为 Agent 层边界错误枚举（终止类语义：Interrupted 等防 `?`
//! 误报失败），事实源归契约层；`peri-agent::error` 保留 re-export。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkBudgetKind {
    ReasonRequests,
    Dispatches,
    Recoveries,
}

impl std::fmt::Display for WorkBudgetKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::ReasonRequests => "reason requests",
            Self::Dispatches => "tool dispatches",
            Self::Recoveries => "work recoveries",
        })
    }
}

/// Agent 层边界错误
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("Max iterations exceeded ({0})")]
    MaxIterationsExceeded(usize),

    #[error("Work budget exhausted for {budget} ({used}/{limit}). This work is blocked; send a new instruction to start a new task using the saved conversation. Resuming this work requires explicit budget reset authorization.")]
    WorkBudgetExhausted {
        budget: WorkBudgetKind,
        used: u64,
        limit: u64,
    },

    #[error("Model output reached the token limit for {attempts} consecutive responses; the task is incomplete.")]
    OutputTruncated { attempts: usize },

    #[error("Tool not found: {0}")]
    ToolNotFound(String),

    #[error("Tool execution failed: {tool} - {reason}")]
    ToolExecutionFailed { tool: String, reason: String },

    #[error("LLM error: {0}")]
    LlmError(String),

    #[error("LLM HTTP 错误 ({status}): {message}")]
    LlmHttpError { status: u16, message: String },

    /// Typed model runtime failure.  Legacy LlmError/LlmHttpError remain for
    /// local callers that already own a textual error, but model boundaries
    /// must retain the validated `ModelError` facts.
    #[error("LLM model error: {0}")]
    ModelError(#[source] peri_model::ModelError),

    /// 可见增量后的流中断恢复预算耗尽。
    #[error("Stream recovery exhausted after {attempts} attempts: {source}")]
    StreamRecoveryExhausted {
        attempts: usize,
        source: peri_model::ModelError,
    },

    #[error("Middleware error: {middleware} - {reason}")]
    MiddlewareError { middleware: String, reason: String },

    #[error("Tool rejected: {tool} - {reason}")]
    ToolRejected { tool: String, reason: String },

    #[error("Serialization error: {0}")]
    SerializationError(#[from] serde_json::Error),

    /// 用户主动中断（Ctrl+C）
    #[error("Interrupted by user")]
    Interrupted,

    #[error("Full Compact requires LLM instance")]
    CompactNoLlm,

    #[error("Full Compact failed: LLM returned empty summary")]
    CompactEmptyResponse,

    #[error("Full Compact failed: summary response did not complete")]
    CompactIncompleteResponse { stop_reason: peri_model::StopReason },

    #[error("Full Compact failed after {attempts} attempts while context usage is {context_tokens}/{context_window} tokens. The turn was stopped before another model request; retry or change the compact model.")]
    CompactRetriesExhausted {
        attempts: u32,
        context_tokens: u64,
        context_window: u32,
    },

    #[error("Full Compact did not restore the context budget after {full_attempts} attempts for the same work ({input_tokens}/{context_window} input tokens). Reduce retained instructions or use a larger context window.")]
    CompactBudgetUnrecovered {
        input_tokens: u32,
        context_window: u32,
        full_attempts: u32,
    },

    #[error(transparent)]
    Other(anyhow::Error),
}

impl From<anyhow::Error> for AgentError {
    fn from(error: anyhow::Error) -> Self {
        if let Some(Self::WorkBudgetExhausted {
            budget,
            used,
            limit,
        }) = error.downcast_ref::<Self>()
        {
            return Self::WorkBudgetExhausted {
                budget: *budget,
                used: *used,
                limit: *limit,
            };
        }
        Self::Other(error)
    }
}

pub type AgentResult<T> = Result<T, AgentError>;

pub(crate) fn bounded_error_causes(causes: Vec<String>) -> Vec<String> {
    let omitted = causes.len() > 64;
    let mut bounded: Vec<_> = causes
        .into_iter()
        .take(if omitted { 63 } else { 64 })
        .map(|cause| {
            if cause.chars().count() > 2_000 {
                format!(
                    "{} [TRUNCATED: cause text exceeds 2000 characters]",
                    crate::session::bounded_error_message(&cause, 1_940)
                )
            } else {
                cause
            }
        })
        .collect();
    if omitted {
        bounded.push("[TRUNCATED: additional causes omitted]".to_owned());
    }
    bounded
}

/// Serde-safe model diagnostics used by canonical tool/background results.
/// The wrapped model projection has private identity fields and no derived
/// deserializer; this adapter validates those fields again on JSON ingress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafeModelErrorDiagnostic(peri_model::ModelErrorDiagnostic);

impl SafeModelErrorDiagnostic {
    pub fn from_model(diagnostic: peri_model::ModelErrorDiagnostic) -> Self {
        Self(diagnostic)
    }

    pub fn message(&self) -> Option<&str> {
        self.0.message()
    }

    pub fn body(&self) -> Option<&str> {
        self.0.body()
    }

    pub fn causes(&self) -> &[String] {
        self.0.causes()
    }

    pub fn category_name(&self) -> &'static str {
        self.0.category_name()
    }

    pub fn status(&self) -> Option<u16> {
        self.0.status()
    }

    pub fn provider(&self) -> Option<&str> {
        self.0.provider()
    }

    pub fn request_id(&self) -> Option<&str> {
        self.0.request_id()
    }

    pub fn transport(&self) -> Option<peri_model::TransportErrorKind> {
        self.0.transport()
    }

    pub fn protocol(&self) -> Option<peri_model::ProtocolErrorKind> {
        self.0.protocol()
    }

    pub fn retry_attempts(&self) -> Option<u32> {
        self.0.retry_attempts()
    }

    pub fn retry_kind(&self) -> Option<peri_model::RetryErrorKind> {
        self.0.retry_kind()
    }
}

impl serde::Serialize for SafeModelErrorDiagnostic {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for SafeModelErrorDiagnostic {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        struct Wire {
            category: String,
            status: Option<u16>,
            provider: Option<String>,
            request_id: Option<String>,
            transport: Option<peri_model::TransportErrorKind>,
            protocol: Option<peri_model::ProtocolErrorKind>,
            retry_attempts: Option<u32>,
            retry_kind: Option<peri_model::RetryErrorKind>,
            message: Option<String>,
            body: Option<String>,
            #[serde(default)]
            causes: Vec<String>,
        }

        let wire = Wire::deserialize(deserializer)?;
        let category = match wire.category.as_str() {
            "transport" => peri_model::ModelErrorCategory::Transport,
            "http_status" => peri_model::ModelErrorCategory::HttpStatus,
            "protocol" => peri_model::ModelErrorCategory::Protocol,
            "cancelled" => peri_model::ModelErrorCategory::Cancelled,
            "stream_interrupted" => peri_model::ModelErrorCategory::StreamInterrupted,
            "retry_exhausted" => peri_model::ModelErrorCategory::RetryExhausted,
            _ => {
                return Err(serde::de::Error::custom(
                    "unknown model diagnostic category",
                ))
            }
        };
        let diagnostic =
            peri_model::ModelErrorDiagnostic::from_parts(peri_model::ModelErrorDiagnosticParts {
                category,
                status: wire.status,
                provider: wire.provider.as_deref(),
                request_id: wire.request_id.as_deref(),
                transport: wire.transport,
                protocol: wire.protocol,
                retry_attempts: wire.retry_attempts,
                retry_kind: wire.retry_kind,
                message: wire.message.as_deref(),
                body: wire.body.as_deref(),
                causes: &wire.causes,
            })
            .ok_or_else(|| serde::de::Error::custom("invalid model diagnostic shape or length"))?;
        Ok(Self(diagnostic))
    }
}

/// Child identity plus safe model facts retained in canonical parent results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafeSubagentFailure {
    child_thread_id: String,
    category: String,
    message: String,
    causes: Vec<String>,
    diagnostic: Option<Box<SafeModelErrorDiagnostic>>,
}

impl SafeSubagentFailure {
    pub fn new(
        child_thread_id: impl AsRef<str>,
        diagnostic: SafeModelErrorDiagnostic,
    ) -> Option<Self> {
        let id = child_thread_id.as_ref();
        if id.is_empty()
            || id.len() > 128
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return None;
        }
        Some(Self {
            child_thread_id: id.to_owned(),
            category: diagnostic.category_name().to_owned(),
            message: crate::session::bounded_error_message(
                &model_error_facts(&diagnostic.0).join(", "),
                2_000,
            ),
            causes: diagnostic.causes().to_vec(),
            diagnostic: Some(Box::new(diagnostic)),
        })
    }

    pub fn child_thread_id(&self) -> &str {
        &self.child_thread_id
    }

    pub fn from_agent_error(child_thread_id: &str, error: &AgentError) -> Option<Self> {
        if child_thread_id.is_empty()
            || child_thread_id.len() > 128
            || !child_thread_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return None;
        }
        let diagnostic = match error {
            AgentError::ModelError(source) | AgentError::StreamRecoveryExhausted { source, .. } => {
                Some(Box::new(SafeModelErrorDiagnostic::from_model(
                    source.diagnostic(),
                )))
            }
            _ => None,
        };
        Some(Self {
            child_thread_id: child_thread_id.to_owned(),
            category: error.category_name().to_owned(),
            message: crate::session::bounded_error_message(&error.user_facing_message(), 2_000),
            causes: bounded_error_causes(error.cause_chain()),
            diagnostic,
        })
    }

    pub fn category_name(&self) -> &str {
        &self.category
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn causes(&self) -> &[String] {
        &self.causes
    }

    pub fn diagnostic(&self) -> Option<&SafeModelErrorDiagnostic> {
        self.diagnostic.as_deref()
    }

    pub fn render_model_summary(&self) -> String {
        let mut result = format!(
            "child_thread_id: {}\nagent_error_category: {}\nerror: {}",
            self.child_thread_id, self.category, self.message
        );
        for cause in &self.causes {
            result.push_str(&format!("\ncause: {cause}"));
        }
        if let Some(diagnostic) = &self.diagnostic {
            result.push_str(&format!(
                "\nmodel_error_category: {}",
                diagnostic.category_name()
            ));
            if let Some(status) = diagnostic.status() {
                result.push_str(&format!("\nmodel_error_status: {status}"));
            }
            if let Some(provider) = diagnostic.provider() {
                result.push_str(&format!("\nmodel_error_provider: {provider}"));
            }
            if let Some(request_id) = diagnostic.request_id() {
                result.push_str(&format!("\nmodel_error_request_id: {request_id}"));
            }
            if let Some(transport) = diagnostic.transport() {
                result.push_str(&format!("\nmodel_error_transport: {transport}"));
            }
            if let Some(protocol) = diagnostic.protocol() {
                result.push_str(&format!("\nmodel_error_protocol: {protocol}"));
            }
            if let Some(attempts) = diagnostic.retry_attempts() {
                result.push_str(&format!("\nmodel_error_retry_attempts: {attempts}"));
            }
            if let Some(kind) = diagnostic.retry_kind() {
                result.push_str(&format!("\nmodel_error_retry_kind: {kind}"));
            }
            if let Some(message) = diagnostic.message() {
                result.push_str(&format!("\nmodel_error_message: {message}"));
            }
            if let Some(body) = diagnostic.body() {
                result.push_str(&format!("\nmodel_error_body: {body}"));
            }
        }
        result
    }
}

impl serde::Serialize for SafeSubagentFailure {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("SafeSubagentFailure", 5)?;
        state.serialize_field("child_thread_id", &self.child_thread_id)?;
        state.serialize_field("category", &self.category)?;
        state.serialize_field("message", &self.message)?;
        state.serialize_field("causes", &self.causes)?;
        state.serialize_field("diagnostic", &self.diagnostic)?;
        state.end()
    }
}

impl<'de> serde::Deserialize<'de> for SafeSubagentFailure {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        struct Wire {
            child_thread_id: String,
            category: String,
            message: String,
            causes: Vec<String>,
            diagnostic: Option<SafeModelErrorDiagnostic>,
        }
        let wire = Wire::deserialize(deserializer)?;
        let id = &wire.child_thread_id;
        if id.is_empty()
            || id.len() > 128
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
            || wire.category.is_empty()
            || wire.category.len() > 128
            || wire.message.trim().is_empty()
            || wire.message.chars().count() > 2_001
            || wire.causes.len() > 64
            || wire
                .causes
                .iter()
                .any(|cause| cause.chars().count() > 2_001)
        {
            return Err(serde::de::Error::custom(
                "invalid child failure shape or length",
            ));
        }
        Ok(Self {
            child_thread_id: wire.child_thread_id,
            category: wire.category,
            message: wire.message,
            causes: wire.causes,
            diagnostic: wire.diagnostic.map(Box::new),
        })
    }
}

impl AgentError {
    pub fn category_name(&self) -> &'static str {
        match self {
            Self::MaxIterationsExceeded(_) => "max_iterations",
            Self::WorkBudgetExhausted { .. } => "work_budget_exhausted",
            Self::OutputTruncated { .. } => "output_truncated",
            Self::ToolNotFound(_) => "tool_not_found",
            Self::ToolExecutionFailed { .. } => "tool_execution_failed",
            Self::LlmError(_) => "llm",
            Self::LlmHttpError { .. } => "llm_http",
            Self::ModelError(source) => source.diagnostic().category_name(),
            Self::StreamRecoveryExhausted { .. } => "stream_recovery_exhausted",
            Self::MiddlewareError { .. } => "middleware",
            Self::ToolRejected { .. } => "tool_rejected",
            Self::SerializationError(_) => "serialization",
            Self::Interrupted => "interrupted",
            Self::CompactNoLlm => "compact_no_llm",
            Self::CompactEmptyResponse => "compact_empty_response",
            Self::CompactIncompleteResponse { .. } => "compact_incomplete_response",
            Self::CompactRetriesExhausted { .. } => "compact_retries_exhausted",
            Self::CompactBudgetUnrecovered { .. } => "compact_budget_unrecovered",
            Self::Other(_) => "other",
        }
    }

    pub fn cause_chain(&self) -> Vec<String> {
        if let Self::Other(error) = self {
            return error.chain().map(ToString::to_string).collect();
        }
        if let Self::ModelError(source) | Self::StreamRecoveryExhausted { source, .. } = self {
            let mut chain = vec![self.to_string()];
            chain.extend(source.diagnostic().causes().iter().cloned());
            return chain;
        }
        let mut chain = vec![self.to_string()];
        let mut source = std::error::Error::source(self);
        while let Some(error) = source {
            chain.push(error.to_string());
            source = error.source();
        }
        chain
    }

    pub fn user_facing_message(&self) -> String {
        match self {
            Self::CompactIncompleteResponse { stop_reason } => {
                format!("Full Compact failed: the summary did not complete ({stop_reason:?}). Retry or change the compact model.")
            }
            Self::Other(error) => format!("{error:#}"),
            Self::StreamRecoveryExhausted { attempts, source } => {
                let mut facts = model_error_facts(&source.diagnostic());
                facts.push(format!("recovery exhausted after {attempts} attempts"));
                format!(
                    "An LLM API error occurred ({}). Please try again.",
                    facts.join(", ")
                )
            }
            Self::ModelError(error) => {
                let facts = model_error_facts(&error.diagnostic());
                if facts.is_empty() {
                    "An LLM API error occurred. Please try again.".to_string()
                } else {
                    format!(
                        "An LLM API error occurred ({}). Please try again.",
                        facts.join(", ")
                    )
                }
            }
            other => other.to_string(),
        }
    }
}

fn model_error_facts(diagnostic: &peri_model::ModelErrorDiagnostic) -> Vec<String> {
    use peri_model::ModelErrorCategory;

    let mut facts = Vec::new();
    match diagnostic.category() {
        // 这两个类别不携带区分性 kind，分类本身就是唯一的用户可见事实。
        ModelErrorCategory::Cancelled => facts.push("request cancelled".to_string()),
        ModelErrorCategory::StreamInterrupted => {
            facts.push("stream interrupted after partial output".to_string())
        }
        ModelErrorCategory::Transport
        | ModelErrorCategory::HttpStatus
        | ModelErrorCategory::Protocol
        | ModelErrorCategory::RetryExhausted => {}
    }
    if let Some(status) = diagnostic.status() {
        facts.push(format!("HTTP {status}"));
    }
    if let Some(kind) = diagnostic.transport() {
        facts.push(format!("transport failure: {kind}"));
    }
    if let Some(kind) = diagnostic.protocol() {
        facts.push(format!("protocol failure: {kind}"));
    }
    if let Some(attempts) = diagnostic.retry_attempts() {
        facts.push(format!("retry exhausted after {attempts} attempts"));
    }
    if let Some(request_id) = diagnostic.request_id() {
        facts.push(format!("request id: {request_id}"));
    }
    if let Some(provider) = diagnostic.provider() {
        facts.push(format!("provider: {provider}"));
    }
    if let Some(kind) = diagnostic.retry_kind() {
        facts.push(format!("retry kind: {kind}"));
    }
    if let Some(message) = diagnostic.message() {
        facts.push(format!("message: {message}"));
    }
    if let Some(body) = diagnostic.body() {
        facts.push(format!("body: {body}"));
    }
    facts.extend(
        diagnostic
            .causes()
            .iter()
            .map(|cause| format!("cause: {cause}")),
    );
    facts
}

#[cfg(test)]
#[path = "error_test.rs"]
mod tests;
