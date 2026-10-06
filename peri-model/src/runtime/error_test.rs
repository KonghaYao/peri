use super::{
    ModelError, ModelErrorCategory, ModelErrorDiagnostic, ModelErrorDiagnosticParts,
    ProtocolErrorKind, RetryErrorKind, TransportErrorKind,
};

#[test]
fn error_context_preserves_content_and_rejects_only_oversized_identity() {
    let value = "sk-live-secret Authorization 诊断";
    let error = ModelError::http_status(401, value, Some(value));
    assert_eq!(error.provider(), Some(value));
    assert_eq!(error.request_id(), Some(value));
    assert!(format!("{error:?} {error}").contains(value));
    let oversized = "x".repeat(129);
    let error = ModelError::http_status(401, &oversized, Some(&oversized));
    assert_eq!(error.provider(), None);
    assert_eq!(error.request_id(), None);
}

#[test]
fn error_details_survive_retry_and_interruption_projection() {
    let error = ModelError::protocol_with_summary(ProtocolErrorKind::Provider, "invalid JSON 诊断")
        .with_body("{raw secret body}")
        .with_causes(vec!["transport secret cause".into()]);
    let retried = ModelError::retry_exhausted_with_context(3, RetryErrorKind::Protocol, &error);
    for diagnostic in [error.diagnostic(), retried.diagnostic()] {
        assert_eq!(diagnostic.message(), Some("invalid JSON 诊断"));
        assert_eq!(diagnostic.body(), Some("{raw secret body}"));
        assert_eq!(diagnostic.causes(), &["transport secret cause"]);
    }
    let projected = error
        .clone()
        .with_interruption_diagnostic(error.diagnostic(), true);
    assert_eq!(
        projected.interruption_diagnostic(),
        Some(&error.diagnostic())
    );
    assert!(projected.interruption_logged());
    assert!(retried.to_string().contains("raw secret body"));
}

#[test]
fn diagnostic_text_is_utf8_bounded_and_causes_are_bounded() {
    let error = ModelError::protocol(ProtocolErrorKind::Provider)
        .with_message("界".repeat(20_000))
        .with_body("界".repeat(20_000))
        .with_causes(vec!["界".repeat(20_000); 30]);
    let diagnostic = error.diagnostic();
    assert!(diagnostic.message().unwrap().len() <= super::MAX_DIAGNOSTIC_BYTES);
    assert!(diagnostic.body().unwrap().len() <= super::MAX_DIAGNOSTIC_BYTES);
    assert!(diagnostic.message().unwrap().ends_with("[TRUNCATED]"));
    assert!(diagnostic.body().unwrap().ends_with("[TRUNCATED]"));
    assert_eq!(diagnostic.causes().len(), 16);
    assert!(diagnostic
        .causes()
        .iter()
        .all(|cause| cause.len() <= super::MAX_DIAGNOSTIC_BYTES));
    assert_eq!(
        diagnostic.causes().last().unwrap(),
        "[TRUNCATED: additional causes omitted]"
    );
}

#[test]
fn test_model_error_preserves_only_safe_structured_context() {
    let http = ModelError::http_status(429, "anthropic", Some("request_123"));
    let transport = ModelError::transport(TransportErrorKind::Timeout, Some("openai"));
    let stream = ModelError::stream_interrupted(Some("openai"), Some("request_456"));
    let retry = ModelError::retry_exhausted(3, RetryErrorKind::Transport).expect("valid attempts");

    assert_eq!(
        http.to_string(),
        "model HTTP status 429 from anthropic (request id: request_123)"
    );
    assert_eq!(http.http_status_code(), Some(429));
    assert_eq!(http.provider(), Some("anthropic"));
    assert_eq!(http.request_id(), Some("request_123"));
    assert_eq!(
        transport.transport_kind(),
        Some(TransportErrorKind::Timeout)
    );
    assert_eq!(
        stream.to_string(),
        "model stream interrupted from openai (request id: request_456)"
    );
    assert_eq!(
        retry.to_string(),
        "model retry exhausted after 3 attempts; last failure: transport"
    );
    assert_eq!(retry.retry_error_kind(), Some(RetryErrorKind::Transport));
}

#[test]
fn diagnostic_accepts_retry_producer_shapes_and_rejects_mismatches() {
    assert!(ModelError::retry_exhausted(0, RetryErrorKind::HttpStatus).is_none());
    let exhausted_without_cause = ModelError::retry_exhausted(3, RetryErrorKind::HttpStatus)
        .expect("valid attempts")
        .diagnostic();
    assert_eq!(
        exhausted_without_cause.category(),
        ModelErrorCategory::RetryExhausted
    );
    assert_eq!(exhausted_without_cause.retry_attempts(), Some(3));
    assert_eq!(
        exhausted_without_cause.retry_kind(),
        Some(RetryErrorKind::HttpStatus)
    );

    let restored = ModelErrorDiagnostic::from_parts(ModelErrorDiagnosticParts {
        category: ModelErrorCategory::RetryExhausted,
        status: None,
        provider: None,
        request_id: None,
        transport: None,
        protocol: None,
        retry_attempts: Some(3),
        message: None,
        body: None,
        causes: &[],
        retry_kind: Some(RetryErrorKind::HttpStatus),
    })
    .expect("retry exhausted without an underlying cause is a valid producer shape");
    assert_eq!(restored, exhausted_without_cause);

    let retrying_http = ModelErrorDiagnostic::from_parts(ModelErrorDiagnosticParts {
        category: ModelErrorCategory::HttpStatus,
        status: Some(429),
        provider: Some("provider.example"),
        request_id: Some("req-429"),
        transport: None,
        protocol: None,
        retry_attempts: Some(6),
        message: None,
        body: None,
        causes: &[],
        retry_kind: Some(RetryErrorKind::HttpStatus),
    })
    .expect("original HTTP category may carry a matching retry pair");
    assert_eq!(retrying_http.status(), Some(429));
    assert_eq!(retrying_http.retry_attempts(), Some(6));

    assert!(ModelErrorDiagnostic::from_parts(ModelErrorDiagnosticParts {
        category: ModelErrorCategory::HttpStatus,
        status: Some(429),
        provider: None,
        request_id: None,
        transport: None,
        protocol: None,
        retry_attempts: Some(6),
        message: None,
        body: None,
        causes: &[],
        retry_kind: Some(RetryErrorKind::Transport),
    })
    .is_none());
}

#[test]
fn diagnostic_parts_preserve_content_and_serialize_new_fields() {
    let diagnostic = ModelErrorDiagnostic::from_parts(ModelErrorDiagnosticParts {
        category: ModelErrorCategory::HttpStatus,
        status: Some(401),
        provider: Some("provider secret 诊断"),
        request_id: Some("request secret 诊断"),
        transport: None,
        protocol: None,
        retry_attempts: None,
        retry_kind: None,
        message: Some("actual secret failure"),
        body: Some("provider secret body"),
        causes: &["actual secret cause".into()],
    })
    .unwrap();
    let serialized = serde_json::to_value(&diagnostic).unwrap();
    assert_eq!(serialized["message"], "actual secret failure");
    assert_eq!(serialized["body"], "provider secret body");
    assert_eq!(serialized["causes"][0], "actual secret cause");
    assert_eq!(diagnostic.provider(), Some("provider secret 诊断"));
    assert_eq!(diagnostic.request_id(), Some("request secret 诊断"));
}

#[test]
fn test_protocol_error_kinds_are_explicit_and_stable() {
    let cases = [
        (ProtocolErrorKind::InvalidJsonObject, "invalid JSON object"),
        (
            ProtocolErrorKind::AssistantMessageRequired,
            "assistant message required",
        ),
        (
            ProtocolErrorKind::StreamEndedWithoutCompleted,
            "stream ended without completion",
        ),
        (ProtocolErrorKind::ToolCallMissingId, "tool call missing id"),
        (
            ProtocolErrorKind::ToolCallMissingName,
            "tool call missing name",
        ),
        (
            ProtocolErrorKind::ToolCallInvalidArguments,
            "tool call has invalid arguments",
        ),
        (ProtocolErrorKind::InvalidEndpoint, "invalid endpoint"),
        (ProtocolErrorKind::Provider, "provider failure"),
        (ProtocolErrorKind::Other, "other failure"),
    ];

    for (kind, summary) in cases {
        let error = ModelError::protocol(kind);
        let protocol_error = error.protocol_error().unwrap();

        assert_eq!(protocol_error.kind(), kind);
        assert_eq!(protocol_error.summary(), None);
        assert_eq!(protocol_error.to_string(), summary);
    }
}

#[test]
fn test_protocol_error_keeps_long_summary_in_diagnostic() {
    let error = ModelError::protocol_with_summary(
        ProtocolErrorKind::Other,
        format!("invalid payload\\n{}", "x".repeat(300)),
    );
    let protocol_error = error.protocol_error().unwrap();

    assert_eq!(protocol_error.kind(), ProtocolErrorKind::Other);
    assert_eq!(protocol_error.summary(), None);
    assert_eq!(protocol_error.to_string(), "other failure");
    let diagnostic = error.diagnostic();
    assert_eq!(diagnostic.protocol(), Some(ProtocolErrorKind::Other));
    assert!(diagnostic.message().unwrap().starts_with("invalid payload"));
    assert!(serde_json::to_value(&diagnostic)
        .expect("diagnostic is serializable")
        .get("summary")
        .is_none());
}
