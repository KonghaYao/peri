use super::{
    ModelError, ModelErrorCategory, ModelErrorDiagnostic, ModelErrorDiagnosticParts,
    ProtocolErrorKind, RetryErrorKind, TransportErrorKind,
};

#[test]
fn test_model_error_never_formats_request_secrets_or_raw_body() {
    let errors = [
        ModelError::http_status(401, "openai", Some("request_123")),
        ModelError::transport(TransportErrorKind::Connection, Some("openai")),
        ModelError::protocol_with_summary(
            ProtocolErrorKind::Provider,
            "provider rejected request with sk-live-secret Authorization: Bearer sk-live-secret; very long user prompt",
        ),
        ModelError::stream_interrupted(Some("openai"), Some("request_123")),
        ModelError::retry_exhausted(3, RetryErrorKind::HttpStatus).expect("valid attempts"),
    ];

    for error in errors {
        let rendered = format!("{error:?} {error}");
        assert!(!rendered.contains("sk-live-secret"));
        assert!(!rendered.contains("Authorization"));
        assert!(!rendered.contains("very long user prompt"));
    }
}

#[test]
fn test_model_error_replaces_malicious_provider_and_request_id_in_debug_and_display() {
    let provider = "sk-live-secret Authorization";
    let request_id = "prompt=very secret request";
    let errors = [
        ModelError::transport(TransportErrorKind::Connection, Some(provider)),
        ModelError::http_status(401, provider, Some(request_id)),
        ModelError::stream_interrupted(Some(provider), Some(request_id)),
    ];

    for error in errors {
        let rendered = format!("{error:?} {error}");
        assert!(!rendered.contains(provider));
        assert!(!rendered.contains(request_id));
        assert!(!rendered.contains("sk-live-secret"));
        assert!(!rendered.contains("Authorization"));
        assert!(!rendered.contains("prompt"));
        assert!(!rendered.contains("[invalid]"));
        assert_eq!(error.provider(), None);
        assert_eq!(error.request_id(), None);
    }
}

#[test]
fn test_model_error_diagnostic_omits_invalid_identity_without_sentinel() {
    let error = ModelError::http_status(401, "provider with spaces", Some("request_id=secret"));
    let diagnostic = error.diagnostic();

    assert_eq!(diagnostic.category_name(), "http_status");
    assert_eq!(diagnostic.status(), Some(401));
    assert_eq!(diagnostic.provider(), None);
    assert_eq!(diagnostic.request_id(), None);
}

#[test]
fn test_model_error_rejects_sk_credential_family_in_all_safe_context_fields() {
    let credential = "sk-ant-api03-very-secret";
    let http = ModelError::http_status(401, credential, Some(credential));
    let protocol = ModelError::protocol_with_summary(ProtocolErrorKind::Provider, credential);

    for error in [http, protocol] {
        let rendered = format!("{error:?} {error}");
        assert!(!rendered.contains(credential));
        assert!(!rendered.contains("[invalid]"));
        let diagnostic = error.diagnostic();
        assert_eq!(diagnostic.provider(), None);
        assert_eq!(diagnostic.request_id(), None);
    }
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
        retry_kind: Some(RetryErrorKind::Transport),
    })
    .is_none());
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
fn test_protocol_error_restricts_unknown_summary() {
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
    assert!(serde_json::to_value(&diagnostic)
        .expect("diagnostic is serializable")
        .get("summary")
        .is_none());
}
