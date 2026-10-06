use super::{AgentError, SafeModelErrorDiagnostic, SafeSubagentFailure};
use crate::session::{ExecutionFailure, ExecutionFailureKind};

#[test]
fn compact_retries_preserve_budget_facts() {
    let error = AgentError::CompactRetriesExhausted {
        attempts: 3,
        context_tokens: 109_000,
        context_window: 100_000,
    };
    let failure = ExecutionFailure::from_agent_error(&error);
    assert_eq!(failure.public_message, error.user_facing_message());
    assert!(failure.public_message.contains("109000/100000 tokens"));
    assert_eq!(
        failure.error_category.as_deref(),
        Some("compact_retries_exhausted")
    );
}

#[test]
fn stream_recovery_preserves_attempts_and_raw_model_details() {
    let source = peri_model::ModelError::stream_interrupted(
        Some("Bearer synthetic-provider"),
        Some("request with spaces"),
    )
    .with_message("socket closed while streaming")
    .with_body("token=synthetic-body")
    .with_causes(vec!["transport root".to_owned()]);
    let error = AgentError::StreamRecoveryExhausted {
        attempts: 6,
        source: source.clone(),
    };
    let failure = ExecutionFailure::from_agent_error(&error);
    assert_eq!(failure.kind, ExecutionFailureKind::Llm);
    assert_eq!(failure.diagnostic, Some(source.diagnostic()));
    for fact in [
        "recovery exhausted after 6 attempts",
        "socket closed while streaming",
        "token=synthetic-body",
        "transport root",
        "Bearer synthetic-provider",
        "request with spaces",
    ] {
        assert!(failure.public_message.contains(fact), "missing {fact}");
    }
}

#[test]
fn stream_recovery_preserves_http_classification() {
    let source = peri_model::ModelError::http_status(
        503,
        "Bearer synthetic-provider",
        Some("Bearer synthetic-request"),
    );
    let error = AgentError::StreamRecoveryExhausted {
        attempts: 2,
        source: source.clone(),
    };
    let failure = ExecutionFailure::from_agent_error(&error);
    assert_eq!(failure.kind, ExecutionFailureKind::LlmHttp);
    assert_eq!(failure.http_status, Some(503));
    assert_eq!(failure.diagnostic, Some(source.diagnostic()));
    assert!(failure.public_message.contains("Bearer synthetic-request"));
}

#[test]
fn model_messages_keep_typed_failure_facts() {
    let errors = [
        (
            peri_model::ModelError::http_status(429, "provider.example", Some("req-429")),
            "HTTP 429",
        ),
        (
            peri_model::ModelError::transport(
                peri_model::TransportErrorKind::Timeout,
                Some("provider.example"),
            ),
            "timeout",
        ),
        (
            peri_model::ModelError::protocol(
                peri_model::ProtocolErrorKind::StreamEndedWithoutCompleted,
            ),
            "stream ended without completion",
        ),
        (peri_model::ModelError::cancelled(), "request cancelled"),
        (
            peri_model::ModelError::stream_interrupted(Some("provider.example"), None::<&str>),
            "stream interrupted after partial output",
        ),
        (
            peri_model::ModelError::retry_exhausted(6, peri_model::RetryErrorKind::HttpStatus)
                .unwrap(),
            "retry exhausted after 6 attempts",
        ),
    ];
    for (source, fact) in errors {
        let error = AgentError::ModelError(source);
        assert!(error.user_facing_message().contains(fact));
    }
}

#[test]
fn free_form_errors_preserve_content() {
    let error = AgentError::LlmError("sk-synthetic raw provider body".to_owned());
    assert_eq!(
        error.user_facing_message(),
        "LLM error: sk-synthetic raw provider body"
    );
    let error = AgentError::LlmHttpError {
        status: 401,
        message: "invalid synthetic api key".to_owned(),
    };
    assert!(error
        .user_facing_message()
        .contains("invalid synthetic api key"));
    let source = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
    let text = source.to_string();
    assert!(AgentError::SerializationError(source)
        .user_facing_message()
        .contains(&text));
}

#[test]
fn anyhow_chain_preserves_order_and_context() {
    let source = anyhow::anyhow!("database root token=synthetic")
        .context("append transcript")
        .context("finish turn");
    let error = AgentError::Other(source);
    assert_eq!(
        error.cause_chain(),
        vec![
            "finish turn",
            "append transcript",
            "database root token=synthetic"
        ]
    );
    let failure = ExecutionFailure::from_agent_error(&error);
    assert_eq!(failure.error_category.as_deref(), Some("other"));
    assert_eq!(failure.causes, error.cause_chain());
    assert!(failure
        .public_message
        .contains("finish turn: append transcript: database root token=synthetic"));
}

#[test]
fn model_diagnostic_roundtrip_keeps_all_runtime_fields() {
    let source = peri_model::ModelError::http_status(
        500,
        "sk-synthetic-provider",
        Some("request with spaces"),
    )
    .with_message("Authorization: Bearer synthetic")
    .with_body("https://user:synthetic@example.invalid?token=synthetic")
    .with_causes(vec!["outer context".to_owned(), "root cause".to_owned()]);
    let diagnostic = SafeModelErrorDiagnostic::from_model(source.diagnostic());
    let wire = serde_json::to_value(&diagnostic).unwrap();
    assert_eq!(wire["message"], "Authorization: Bearer synthetic");
    assert_eq!(wire["causes"][1], "root cause");
    let restored: SafeModelErrorDiagnostic = serde_json::from_value(wire).unwrap();
    assert_eq!(restored, diagnostic);
    assert_eq!(
        restored.body(),
        Some("https://user:synthetic@example.invalid?token=synthetic")
    );
}

#[test]
fn subagent_model_failure_roundtrip_keeps_details() {
    let source = peri_model::ModelError::http_status(500, "provider.example", Some("req-123"))
        .with_message("child request failed")
        .with_body("token=synthetic")
        .with_causes(vec!["child root".to_owned()]);
    let failure = SafeSubagentFailure::new(
        "child-123",
        SafeModelErrorDiagnostic::from_model(source.diagnostic()),
    )
    .unwrap();
    let restored: SafeSubagentFailure =
        serde_json::from_value(serde_json::to_value(&failure).unwrap()).unwrap();
    assert_eq!(restored, failure);
    for fact in [
        "child-123",
        "500",
        "child request failed",
        "token=synthetic",
        "child root",
        "req-123",
    ] {
        assert!(
            restored.render_model_summary().contains(fact),
            "missing {fact}"
        );
    }
}

#[test]
fn subagent_internal_failure_roundtrip_keeps_cause_chain() {
    let error = AgentError::Other(anyhow::anyhow!("store root").context("child flush"));
    let failure = SafeSubagentFailure::from_agent_error("child-123", &error).unwrap();
    assert!(failure.diagnostic().is_none());
    assert_eq!(failure.category_name(), "other");
    let restored: SafeSubagentFailure =
        serde_json::from_value(serde_json::to_value(&failure).unwrap()).unwrap();
    assert_eq!(
        restored.causes(),
        &["child flush".to_owned(), "store root".to_owned()]
    );
    assert!(restored.render_model_summary().contains("store root"));
}

#[test]
fn subagent_ingress_preserves_credentials_without_disabling_identity_validation() {
    let source = peri_model::ModelError::http_status(401, "sk-synthetic-provider", Some("req-123"));
    let failure = SafeSubagentFailure::new(
        "child-123",
        SafeModelErrorDiagnostic::from_model(source.diagnostic()),
    )
    .unwrap();
    let wire = serde_json::to_value(&failure).unwrap();
    assert!(serde_json::from_value::<SafeSubagentFailure>(wire.clone()).is_ok());
    for id in ["".to_owned(), "child/invalid".to_owned(), "x".repeat(129)] {
        let mut invalid = wire.clone();
        invalid["child_thread_id"] = serde_json::Value::String(id);
        assert!(serde_json::from_value::<SafeSubagentFailure>(invalid).is_err());
    }
}

#[test]
fn model_ingress_keeps_category_and_retry_shape_validation() {
    let invalid = [
        serde_json::json!({"category":"cancelled", "status":500, "transport":"timeout"}),
        serde_json::json!({"category":"retry_exhausted", "status":429, "retry_kind":"http_status"}),
        serde_json::json!({"category":"unknown"}),
    ];
    for wire in invalid {
        assert!(serde_json::from_value::<SafeModelErrorDiagnostic>(wire).is_err());
    }
}

#[test]
fn model_categories_roundtrip_with_diagnostic_details() {
    let errors = [
        peri_model::ModelError::transport(
            peri_model::TransportErrorKind::Timeout,
            Some("provider.example"),
        ),
        peri_model::ModelError::http_status(429, "provider.example", Some("req-429")),
        peri_model::ModelError::protocol(peri_model::ProtocolErrorKind::Provider),
        peri_model::ModelError::cancelled(),
        peri_model::ModelError::stream_interrupted(Some("provider.example"), Some("req-stream")),
        peri_model::ModelError::retry_exhausted(3, peri_model::RetryErrorKind::Transport).unwrap(),
        peri_model::ModelError::retry_exhausted(3, peri_model::RetryErrorKind::HttpStatus).unwrap(),
        peri_model::ModelError::retry_exhausted(3, peri_model::RetryErrorKind::Protocol).unwrap(),
    ];
    for error in errors {
        let error = error
            .with_message("runtime message")
            .with_body("runtime body")
            .with_causes(vec!["runtime root".to_owned()]);
        let diagnostic = SafeModelErrorDiagnostic::from_model(error.diagnostic());
        let restored: SafeModelErrorDiagnostic =
            serde_json::from_value(serde_json::to_value(&diagnostic).unwrap()).unwrap();
        assert_eq!(restored, diagnostic);
    }
}

#[test]
fn bounded_causes_report_text_and_node_truncation() {
    let causes = super::bounded_error_causes(vec!["错".repeat(2_100); 70]);
    assert_eq!(causes.len(), 64);
    assert!(causes[0].contains("[TRUNCATED: cause text"));
    assert!(causes.iter().all(|cause| cause.chars().count() <= 2_001));
    assert_eq!(causes[63], "[TRUNCATED: additional causes omitted]");
}
