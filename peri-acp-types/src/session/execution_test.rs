use super::*;
use crate::error::AgentError;

#[test]
fn test_compact_incomplete_response_has_safe_llm_failure() {
    for (stop_reason, expected) in [
        (peri_model::StopReason::MaxTokens, "MaxTokens"),
        (peri_model::StopReason::ToolUse, "ToolUse"),
        (
            peri_model::StopReason::Other {
                value: "secret=must-not-leak".into(),
            },
            "secret=must-not-leak",
        ),
    ] {
        let error = AgentError::CompactIncompleteResponse { stop_reason };
        let failure = ExecutionFailure::from_agent_error(&error);
        assert_eq!(failure.kind, ExecutionFailureKind::Llm);
        assert!(failure.public_message.contains(expected));
        assert!(failure.diagnostic.is_none());
        assert!(failure.http_status.is_none());
    }
}

#[test]
fn test_compact_recovery_errors_keep_attempts_and_window_visible() {
    let exhausted = AgentError::CompactRecoveryExhausted {
        attempts: 3,
        last_reason: crate::error::SummaryRecoveryReason::OutputLimitTruncated,
    };
    let failure = ExecutionFailure::from_agent_error(&exhausted);
    assert_eq!(failure.kind, ExecutionFailureKind::Llm);
    assert_eq!(exhausted.category_name(), "compact_recovery_exhausted");
    assert!(failure.public_message.contains("3 summary attempts"));
    assert!(failure.public_message.contains("no background retry"));
    assert!(failure.diagnostic.is_none());
    assert!(failure.http_status.is_none());

    let exceeded = AgentError::CompactSummaryInputExceeded {
        request_index: 2,
        request_tokens: 90_000,
        output_reserve: 4096,
        safety_margin: 5_000,
        context_window: 60_000,
    };
    let failure = ExecutionFailure::from_agent_error(&exceeded);
    assert_eq!(failure.kind, ExecutionFailureKind::Llm);
    assert_eq!(exceeded.category_name(), "compact_summary_input_exceeded");
    assert!(failure.public_message.contains("summary request 2"));
    assert!(failure.public_message.contains("60000"));
    assert!(failure.diagnostic.is_none());
    assert!(failure.http_status.is_none());
}
