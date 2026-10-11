use peri_acp_types::{
    command::PromptStopReason,
    error::AgentError,
    event::{TurnErrorKind, TurnStatus},
    session::{ExecutionFailureKind, EXECUTION_FAILURE_FALLBACK_MESSAGE},
};

use crate::agent::stages::LoopResult;

use super::super::v2_execute::classify_loop_terminal;

#[test]
fn fatal_llm_error_preserves_original_content() {
    let terminal = classify_loop_terminal(
        &LoopResult::Error(AgentError::LlmError(
            "provider 500: Authorization: Bearer top-secret-key".to_string(),
        )),
        false,
    );
    assert!(!terminal.ok);
    assert_eq!(terminal.stop_reason, PromptStopReason::EndTurn);
    assert_eq!(terminal.turn_status, TurnStatus::Error);
    assert_eq!(terminal.turn_error_kind, Some(TurnErrorKind::LlmFailure));
    let failure = terminal.failure.expect("fatal error 必须携带 failure");
    assert_eq!(failure.kind, ExecutionFailureKind::Llm);
    assert!(failure.http_status.is_none());
    assert!(
        !failure.public_message.is_empty(),
        "public message 必须非空"
    );
    assert!(failure.public_message.contains("top-secret-key"));
    assert!(failure.public_message.contains("provider 500"));
    assert!(failure.public_message.contains("Bearer top-secret-key"));
}

#[test]
fn interrupted_maps_to_no_failure() {
    let terminal = classify_loop_terminal(&LoopResult::Interrupted, false);
    assert!(!terminal.ok);
    assert_eq!(terminal.stop_reason, PromptStopReason::Cancelled);
    assert_eq!(terminal.turn_status, TurnStatus::Interrupted);
    assert_eq!(terminal.turn_error_kind, Some(TurnErrorKind::Interrupted));
    assert!(
        terminal.failure.is_none(),
        "Interrupted 不得升级为 fatal failure"
    );
}

#[test]
fn legacy_error_interrupted_maps_to_interrupted_terminal() {
    let terminal = classify_loop_terminal(&LoopResult::Error(AgentError::Interrupted), false);
    assert!(!terminal.ok);
    assert_eq!(terminal.stop_reason, PromptStopReason::Cancelled);
    assert_eq!(terminal.turn_status, TurnStatus::Interrupted);
    assert_eq!(terminal.turn_error_kind, Some(TurnErrorKind::Interrupted));
    assert!(terminal.failure.is_none());
}

#[test]
fn cancelled_error_maps_to_no_failure() {
    let terminal = classify_loop_terminal(
        &LoopResult::Error(AgentError::LlmError("cancelled while failing".to_string())),
        true,
    );
    assert!(!terminal.ok);
    assert_eq!(terminal.stop_reason, PromptStopReason::Cancelled);
    assert_eq!(terminal.turn_status, TurnStatus::Interrupted);
    assert_eq!(terminal.turn_error_kind, Some(TurnErrorKind::Interrupted));
    assert!(
        terminal.failure.is_none(),
        "用户 cancel 不得升级为 fatal failure"
    );
}

#[test]
fn max_iterations_maps_to_no_failure() {
    let terminal = classify_loop_terminal(
        &LoopResult::Error(AgentError::MaxIterationsExceeded(500)),
        false,
    );
    assert!(!terminal.ok);
    assert_eq!(terminal.stop_reason, PromptStopReason::MaxTurnRequests);
    assert_eq!(terminal.turn_status, TurnStatus::Error);
    assert_eq!(terminal.turn_error_kind, Some(TurnErrorKind::MaxIterations));
    assert!(
        terminal.failure.is_none(),
        "MaxIterationsExceeded 不得升级为 fatal failure"
    );
}

#[test]
fn output_truncated_maps_to_max_tokens_without_fatal_failure() {
    let terminal = classify_loop_terminal(
        &LoopResult::Error(AgentError::OutputTruncated { attempts: 3 }),
        false,
    );
    assert!(!terminal.ok);
    assert_eq!(terminal.stop_reason, PromptStopReason::MaxTokens);
    assert_eq!(terminal.turn_status, TurnStatus::Error);
    assert_eq!(terminal.turn_error_kind, Some(TurnErrorKind::LlmFailure));
    assert!(terminal.failure.is_none());
    assert_eq!(
        peri_acp_types::session::TurnTelemetryOutcome::from_result(
            terminal.stop_reason,
            terminal.failure,
        ),
        peri_acp_types::session::TurnTelemetryOutcome::Stopped {
            reason: PromptStopReason::MaxTokens,
        },
    );
}

#[test]
fn cancelled_output_truncation_maps_to_cancelled() {
    let terminal = classify_loop_terminal(
        &LoopResult::Error(AgentError::OutputTruncated { attempts: 3 }),
        true,
    );
    assert_eq!(terminal.stop_reason, PromptStopReason::Cancelled);
    assert_eq!(terminal.turn_status, TurnStatus::Interrupted);
    assert!(terminal.failure.is_none());
}

#[test]
fn cancelled_max_iterations_maps_to_interrupted_terminal() {
    let terminal = classify_loop_terminal(
        &LoopResult::Error(AgentError::MaxIterationsExceeded(500)),
        true,
    );
    assert!(!terminal.ok);
    assert_eq!(terminal.stop_reason, PromptStopReason::Cancelled);
    assert_eq!(terminal.turn_status, TurnStatus::Interrupted);
    assert_eq!(terminal.turn_error_kind, Some(TurnErrorKind::Interrupted));
    assert!(terminal.failure.is_none());
}

#[test]
fn completed_maps_to_success_no_failure() {
    let terminal = classify_loop_terminal(&LoopResult::Completed, false);
    assert!(terminal.ok);
    assert_eq!(terminal.stop_reason, PromptStopReason::EndTurn);
    assert_eq!(terminal.turn_status, TurnStatus::Done);
    assert_eq!(terminal.turn_error_kind, None);
    assert!(terminal.failure.is_none());
}

#[test]
fn completed_after_late_cancel_remains_committed_success() {
    let terminal = classify_loop_terminal(&LoopResult::Completed, true);
    assert!(terminal.ok);
    assert_eq!(terminal.stop_reason, PromptStopReason::EndTurn);
    assert_eq!(terminal.turn_status, TurnStatus::Done);
    assert_eq!(terminal.turn_error_kind, None);
    assert!(terminal.failure.is_none());
}

#[test]
fn llm_http_error_preserves_original_message() {
    let terminal = classify_loop_terminal(
        &LoopResult::Error(AgentError::LlmHttpError {
            status: 421,
            message: "Misdirected Request token=secret-provider-body".to_string(),
        }),
        false,
    );
    assert!(!terminal.ok);
    assert_eq!(terminal.turn_status, TurnStatus::Error);
    assert_eq!(terminal.turn_error_kind, Some(TurnErrorKind::LlmFailure));
    let failure = terminal.failure.expect("fatal error 必须携带 failure");
    assert_eq!(failure.kind, ExecutionFailureKind::LlmHttp);
    assert_eq!(failure.http_status, Some(421));
    assert!(failure.public_message.contains("LLM HTTP 421"));
    assert!(failure.public_message.contains("Misdirected Request"));
    assert!(failure.public_message.contains("secret-provider-body"));
    assert!(!failure.public_message.contains("[redacted]"));
}

#[test]
fn fallback_message_is_non_empty_and_safe() {
    assert!(!EXECUTION_FAILURE_FALLBACK_MESSAGE.is_empty());
    assert!(!EXECUTION_FAILURE_FALLBACK_MESSAGE.contains("secret"));
}
