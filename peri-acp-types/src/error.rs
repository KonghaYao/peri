//! 层边界错误契约（§9 错误模型：边界类型化，层内 anyhow）。
//!
//! `AgentError` 为 Agent 层边界错误枚举（终止类语义：Interrupted 等防 `?`
//! 误报失败），事实源归契约层；`peri-agent::error` 保留 re-export。

/// Agent 层边界错误
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("Max iterations exceeded ({0})")]
    MaxIterationsExceeded(usize),

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

    #[error("Full Compact did not restore the context budget after {full_attempts} attempts for the same work ({input_tokens}/{context_window} input tokens). Reduce retained instructions or use a larger context window.")]
    CompactBudgetUnrecovered {
        input_tokens: u32,
        context_window: u32,
        full_attempts: u32,
    },

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type AgentResult<T> = Result<T, AgentError>;

/// Serde-safe model diagnostics used by canonical tool/background results.
/// The wrapped model projection has private identity fields and no derived
/// deserializer; this adapter validates those fields again on JSON ingress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafeModelErrorDiagnostic(peri_model::ModelErrorDiagnostic);

impl SafeModelErrorDiagnostic {
    pub fn from_model(diagnostic: peri_model::ModelErrorDiagnostic) -> Self {
        Self(diagnostic)
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
            })
            .ok_or_else(|| serde::de::Error::custom("unsafe model diagnostic identity"))?;
        Ok(Self(diagnostic))
    }
}

/// Child identity plus safe model facts retained in canonical parent results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafeSubagentFailure {
    child_thread_id: String,
    diagnostic: SafeModelErrorDiagnostic,
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
            diagnostic,
        })
    }

    pub fn child_thread_id(&self) -> &str {
        &self.child_thread_id
    }

    pub fn diagnostic(&self) -> &SafeModelErrorDiagnostic {
        &self.diagnostic
    }

    pub fn render_model_summary(&self) -> String {
        let mut result = format!(
            "child_thread_id: {}\nmodel_error_category: {}",
            self.child_thread_id,
            self.diagnostic.category_name()
        );
        if let Some(status) = self.diagnostic.status() {
            result.push_str(&format!("\nmodel_error_status: {status}"));
        }
        if let Some(provider) = self.diagnostic.provider() {
            result.push_str(&format!("\nmodel_error_provider: {provider}"));
        }
        if let Some(request_id) = self.diagnostic.request_id() {
            result.push_str(&format!("\nmodel_error_request_id: {request_id}"));
        }
        if let Some(transport) = self.diagnostic.transport() {
            result.push_str(&format!("\nmodel_error_transport: {transport}"));
        }
        if let Some(protocol) = self.diagnostic.protocol() {
            result.push_str(&format!("\nmodel_error_protocol: {protocol}"));
        }
        if let Some(attempts) = self.diagnostic.retry_attempts() {
            result.push_str(&format!("\nmodel_error_retry_attempts: {attempts}"));
        }
        if let Some(kind) = self.diagnostic.retry_kind() {
            result.push_str(&format!("\nmodel_error_retry_kind: {kind}"));
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
        let mut state = serializer.serialize_struct("SafeSubagentFailure", 2)?;
        state.serialize_field("child_thread_id", &self.child_thread_id)?;
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
            diagnostic: SafeModelErrorDiagnostic,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.child_thread_id, wire.diagnostic)
            .ok_or_else(|| serde::de::Error::custom("unsafe subagent child identity"))
    }
}

impl AgentError {
    /// 返回用户可见的错误描述（脱敏后的消息）
    /// 对 Other/LlmError/LlmHttpError/SerializationError 返回通用描述
    pub fn user_facing_message(&self) -> String {
        match self {
            Self::Other(_) => "An internal error occurred. Check logs for details.".to_string(),
            Self::LlmError(_) => {
                "An LLM API error occurred. Please check your API configuration.".to_string()
            }
            Self::LlmHttpError { .. } => {
                "An LLM API error occurred. Please check your API configuration.".to_string()
            }
            Self::ModelError(error) => {
                let diagnostic = error.diagnostic();
                let status = diagnostic
                    .status()
                    .map(|status| format!(" (HTTP {status}"))
                    .unwrap_or_else(|| " (".to_string());
                let request_id = diagnostic
                    .request_id()
                    .map(|request_id| format!(", request id: {request_id}"))
                    .unwrap_or_default();
                if diagnostic.status().is_some() || !request_id.is_empty() {
                    format!(
                        "An LLM API error occurred{}{request_id}). Please try again.",
                        status
                    )
                } else {
                    "An LLM API error occurred. Please try again.".to_string()
                }
            }
            Self::SerializationError(_) => {
                "A serialization error occurred. Please try again.".to_string()
            }
            other => other.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SafeModelErrorDiagnostic, SafeSubagentFailure};

    #[test]
    fn safe_subagent_failure_roundtrips_only_allowlisted_facts() {
        let model_error =
            peri_model::ModelError::http_status(500, "provider.example", Some("req-123"));
        let failure = SafeSubagentFailure::new(
            "child-123",
            SafeModelErrorDiagnostic::from_model(model_error.diagnostic()),
        )
        .expect("valid child identity");
        let value = serde_json::to_value(&failure).expect("serialize safe facts");
        assert_eq!(value["child_thread_id"], "child-123");
        assert_eq!(value["diagnostic"]["status"], 500);
        assert_eq!(value["diagnostic"]["provider"], "provider.example");
        assert!(!value.to_string().contains("summary"));
        let restored: SafeSubagentFailure =
            serde_json::from_value(value).expect("validated safe facts deserialize");
        assert_eq!(
            restored.render_model_summary(),
            failure.render_model_summary()
        );
    }

    #[test]
    fn safe_subagent_failure_rejects_credential_like_identity_on_ingress() {
        let value = serde_json::json!({
            "child_thread_id": "child-123",
            "diagnostic": {
                "category": "http_status",
                "status": 401,
                "provider": "sk-ant-api03-secret",
                "request_id": "req-123",
                "transport": null,
                "protocol": null,
                "retry_attempts": null,
                "retry_kind": null
            }
        });
        assert!(serde_json::from_value::<SafeSubagentFailure>(value).is_err());
    }

    #[test]
    fn safe_subagent_failure_rejects_unbounded_child_identity_on_ingress() {
        let value = serde_json::json!({
            "child_thread_id": "child/with/raw/control",
            "diagnostic": {
                "category": "protocol",
                "status": null,
                "provider": null,
                "request_id": null,
                "transport": null,
                "protocol": "stream_ended_without_completed",
                "retry_attempts": null,
                "retry_kind": null
            }
        });
        assert!(serde_json::from_value::<SafeSubagentFailure>(value).is_err());
    }

    #[test]
    fn safe_diagnostic_ingress_rejects_contradictory_category_facts() {
        let value = serde_json::json!({
            "child_thread_id": "child-123",
            "diagnostic": {
                "category": "cancelled",
                "status": 500,
                "provider": "provider.example",
                "request_id": "req-123",
                "transport": "timeout",
                "protocol": null,
                "retry_attempts": 3,
                "retry_kind": "http_status"
            }
        });
        assert!(serde_json::from_value::<SafeSubagentFailure>(value).is_err());

        let unpaired_retry = serde_json::json!({
            "child_thread_id": "child-123",
            "diagnostic": {
                "category": "retry_exhausted",
                "status": 429,
                "provider": "provider.example",
                "request_id": "req-123",
                "transport": null,
                "protocol": null,
                "retry_attempts": null,
                "retry_kind": "http_status"
            }
        });
        assert!(serde_json::from_value::<SafeSubagentFailure>(unpaired_retry).is_err());
    }

    #[test]
    fn producer_retry_diagnostic_roundtrips_through_safe_serde() {
        let model_error =
            peri_model::ModelError::retry_exhausted(3, peri_model::RetryErrorKind::HttpStatus)
                .expect("valid attempts");
        let safe = SafeModelErrorDiagnostic::from_model(model_error.diagnostic());
        let wire = serde_json::to_value(&safe).expect("serialize safe retry diagnostic");
        let restored: SafeModelErrorDiagnostic =
            serde_json::from_value(wire).expect("deserialize safe retry diagnostic");
        assert_eq!(restored, safe);
        assert_eq!(restored.category_name(), "retry_exhausted");
        assert_eq!(restored.retry_attempts(), Some(3));
        assert_eq!(restored.status(), None);
    }

    #[test]
    fn producer_diagnostics_roundtrip_all_model_categories() {
        let errors = [
            peri_model::ModelError::transport(
                peri_model::TransportErrorKind::Timeout,
                Some("provider.example"),
            ),
            peri_model::ModelError::http_status(429, "provider.example", Some("req-429")),
            peri_model::ModelError::protocol(peri_model::ProtocolErrorKind::Provider),
            peri_model::ModelError::cancelled(),
            peri_model::ModelError::stream_interrupted(
                Some("provider.example"),
                Some("req-stream"),
            ),
            peri_model::ModelError::retry_exhausted(3, peri_model::RetryErrorKind::Transport)
                .expect("valid attempts"),
            peri_model::ModelError::retry_exhausted(3, peri_model::RetryErrorKind::HttpStatus)
                .expect("valid attempts"),
            peri_model::ModelError::retry_exhausted(3, peri_model::RetryErrorKind::Protocol)
                .expect("valid attempts"),
        ];
        for error in errors {
            let safe = SafeModelErrorDiagnostic::from_model(error.diagnostic());
            let restored: SafeModelErrorDiagnostic =
                serde_json::from_value(serde_json::to_value(&safe).unwrap()).unwrap();
            assert_eq!(restored, safe, "category {}", safe.category_name());
        }
    }
}
