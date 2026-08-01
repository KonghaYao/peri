use super::{ModelError, ProtocolErrorKind, RetryErrorKind, TransportErrorKind};

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
        ModelError::retry_exhausted(3, RetryErrorKind::HttpStatus),
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
        assert!(rendered.contains("[invalid]"));
    }
}

#[test]
fn test_model_error_preserves_only_safe_structured_context() {
    let http = ModelError::http_status(429, "anthropic", Some("request_123"));
    let transport = ModelError::transport(TransportErrorKind::Timeout, Some("openai"));
    let stream = ModelError::stream_interrupted(Some("openai"), Some("request_456"));
    let retry = ModelError::retry_exhausted(3, RetryErrorKind::Transport);

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
    assert_eq!(protocol_error.summary(), Some("[invalid]"));
    assert_eq!(protocol_error.to_string(), "other failure ([invalid])");
}
