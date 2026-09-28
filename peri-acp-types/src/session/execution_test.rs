use super::*;
use crate::error::AgentError;

#[test]
fn test_compact_incomplete_response_has_safe_llm_failure() {
    for (stop_reason, expected) in [
        (peri_model::StopReason::MaxTokens, "output token limit"),
        (peri_model::StopReason::ToolUse, "unexpected tool call"),
        (
            peri_model::StopReason::Other {
                value: "secret=must-not-leak".into(),
            },
            "unexpected stop reason",
        ),
    ] {
        let error = AgentError::CompactIncompleteResponse { stop_reason };
        assert!(!error.to_string().contains("must-not-leak"));
        let failure = ExecutionFailure::from_agent_error(&error);
        assert_eq!(failure.kind, ExecutionFailureKind::Llm);
        assert!(failure.public_message.contains(expected));
        assert!(!failure.public_message.contains("must-not-leak"));
        assert!(failure.diagnostic.is_none());
        assert!(failure.http_status.is_none());
    }
}
