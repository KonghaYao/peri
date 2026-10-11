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
