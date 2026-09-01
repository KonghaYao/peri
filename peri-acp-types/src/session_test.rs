//! session.rs 契约类型测试。
//!
//! 覆盖 ExecutionFailure DTO 与 `PromptResult` 的失败语义
//! （spec/issues/2026-08-18-acp-error-handler.md Commit 1）：
//! - `PromptResult::default()` 必须产生安全的 fatal failure，不能作为成功
//!   `EndTurn` 继续交给 ACP（结果缺失语义）；
//! - `ExecutionFailure` 的 public message 非空并脱敏（D5 fallback 契约）。

use crate::command::PromptStopReason;
use crate::error::AgentError;
use crate::session::{ExecutionFailure, ExecutionFailureKind, PromptResult};

/// 默认结果缺失语义：必须带 fatal failure，而不是成功 EndTurn。
#[test]
fn default_prompt_result_is_safe_fatal_failure() {
    let result = PromptResult::default();
    assert!(!result.ok, "缺失结果不得记为成功");
    let failure = result
        .failure
        .expect("缺失结果必须携带 fatal failure，不能被 ACP 当成功 EndTurn");
    assert_eq!(failure.kind, ExecutionFailureKind::Internal);
    assert!(
        !failure.public_message.is_empty(),
        "fallback message 必须非空"
    );
}

/// 默认结果的 stop_reason 保持现有行为（Commit 1 不改 wire），
/// 失败分类由 failure 字段承载。
#[test]
fn default_prompt_result_keeps_existing_stop_reason() {
    let result = PromptResult::default();
    assert_eq!(result.stop_reason, PromptStopReason::EndTurn);
    assert!(result.messages.is_empty());
    assert!(result.recall_items.is_empty());
    assert!(!result.history_replaced_by_compaction);
}

/// `ExecutionFailure::internal` 保证 public message 非空：空输入回落稳定
/// fallback 文案（D5：不允许前端因空串静默丢弃）。
#[test]
fn internal_failure_empty_message_falls_back_to_non_empty() {
    let failure = ExecutionFailure::internal("");
    assert_eq!(failure.kind, ExecutionFailureKind::Internal);
    assert!(!failure.public_message.is_empty());
    assert_eq!(
        failure.public_message,
        crate::session::EXECUTION_FAILURE_FALLBACK_MESSAGE
    );
}

/// 非空输入原样保留（调用方已传 `user_facing_message()` 的脱敏消息）。
#[test]
fn internal_failure_keeps_non_empty_message() {
    let failure = ExecutionFailure::internal("An LLM API error occurred.");
    assert_eq!(failure.kind, ExecutionFailureKind::Internal);
    assert_eq!(failure.public_message, "An LLM API error occurred.");
}

/// `missing_result` 与空输入 fallback 语义一致：稳定 Internal + 非空文案。
#[test]
fn missing_result_is_internal_non_empty() {
    let failure = ExecutionFailure::missing_result();
    assert_eq!(failure.kind, ExecutionFailureKind::Internal);
    assert!(failure.http_status.is_none());
    assert!(!failure.public_message.is_empty());
}

#[test]
fn llm_http_failure_preserves_status_and_redacted_original_meaning() {
    let failure = ExecutionFailure::from_agent_error(&AgentError::LlmHttpError {
        status: 421,
        message: "Misdirected Request Authorization: Bearer top-secret token=hidden endpoint=https://api.example.test/v1?api_key=hidden".to_string(),
    });

    assert_eq!(failure.kind, ExecutionFailureKind::LlmHttp);
    assert_eq!(failure.http_status, Some(421));
    assert!(failure.public_message.contains("LLM HTTP 421"));
    assert!(failure.public_message.contains("Misdirected Request"));
    assert!(!failure.public_message.contains("top-secret"));
    assert!(!failure.public_message.contains("token=hidden"));
    assert!(!failure.public_message.contains("api_key=hidden"));
    assert!(failure.public_message.contains("[redacted]"));
}

#[test]
fn llm_error_redacts_structured_prefixed_and_labeled_url_secrets() {
    let failure = ExecutionFailure::from_agent_error(&AgentError::LlmError(
        r#"provider rejected Authorization:"Bearer auth-secret" "api_key": "key-secret" endpoint_url="https://api.example.test/v1?token=query-secret&mode=debug""#.to_string(),
    ));

    assert_eq!(failure.kind, ExecutionFailureKind::Llm);
    assert!(failure.public_message.contains("provider rejected"));
    assert!(failure.public_message.contains("Authorization:"));
    assert!(failure.public_message.contains("[redacted]"));
    assert!(failure.public_message.contains("\"api_key\": \"[redacted]"));
    assert!(failure
        .public_message
        .contains("endpoint_url=\"https://api.example.test/v1?[redacted]"));
    for secret in ["auth-secret", "key-secret", "query-secret"] {
        assert!(!failure.public_message.contains(secret));
    }
}

#[test]
fn llm_failure_message_is_limited_without_splitting_unicode() {
    let failure = ExecutionFailure::from_agent_error(&AgentError::LlmError("错".repeat(2_100)));

    assert_eq!(failure.kind, ExecutionFailureKind::Llm);
    assert!(failure.http_status.is_none());
    assert!(failure.public_message.ends_with('…'));
    assert_eq!(failure.public_message.chars().count(), 2_001);
}
