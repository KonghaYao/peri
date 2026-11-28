//! Deterministic checks for the access-log field rules.
//!
//! These are pure helpers: the middleware itself is exercised end-to-end by the
//! OTLP stub integration test, where the SSE body must arrive untouched.

use super::{is_event_stream, is_plain_identifier, session_id_from_path};

#[test]
fn session_id_is_read_only_from_the_matched_sessions_path() {
    assert_eq!(
        session_id_from_path("/v1/agents/sessions/agent-01/events"),
        Some("agent-01")
    );
    assert_eq!(
        session_id_from_path("/v1/agents/sessions/agent_01"),
        Some("agent_01")
    );
    assert_eq!(session_id_from_path("/v1/agents/sessions"), None);
    assert_eq!(session_id_from_path("/v1/agents/metrics"), None);
}

#[test]
fn session_id_rejects_encoded_or_unexpected_values() {
    assert_eq!(
        session_id_from_path("/v1/agents/sessions/agent%2Fsecret/events"),
        None,
        "a percent-encoded id must not be decoded or logged"
    );
    assert_eq!(
        session_id_from_path("/v1/agents/sessions/agent.01"),
        None,
        "only the LogicalId alphabet is logged"
    );
    assert_eq!(
        session_id_from_path(&format!("/v1/agents/sessions/{}/events", "a".repeat(129))),
        None
    );
}

#[test]
fn plain_identifier_accepts_the_logical_id_alphabet() {
    assert!(is_plain_identifier("agent-0192f0c5"));
    assert!(is_plain_identifier("A_b-9"));
    assert!(!is_plain_identifier(""));
    assert!(!is_plain_identifier("a b"));
    assert!(!is_plain_identifier("a/b"));
}

#[test]
fn only_the_events_route_skips_the_latency_histogram() {
    assert!(is_event_stream("/v1/agents/sessions/{logical_id}/events"));
    assert!(!is_event_stream("/v1/agents/sessions/{logical_id}"));
    assert!(!is_event_stream("/v1/agents/metrics"));
    assert!(!is_event_stream("/healthz"));
}
