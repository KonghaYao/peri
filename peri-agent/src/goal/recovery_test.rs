use super::goal_recovery_reason;
use peri_acp_types::error::{AgentError, WorkBudgetKind};

#[test]
fn test_user_cancel_does_not_schedule_goal_recovery() {
    assert_eq!(goal_recovery_reason(&AgentError::Interrupted), None);
}

#[test]
fn test_work_budget_exhausted_does_not_schedule_goal_recovery() {
    let error = AgentError::WorkBudgetExhausted {
        budget: WorkBudgetKind::ReasonRequests,
        used: 10,
        limit: 10,
    };
    assert_eq!(goal_recovery_reason(&error), None);
}

#[test]
fn test_deterministic_compact_configuration_errors_do_not_schedule_recovery() {
    assert_eq!(goal_recovery_reason(&AgentError::CompactNoLlm), None);
    assert_eq!(
        goal_recovery_reason(&AgentError::CompactSummaryBudgetInvalid {
            reason: "no budget"
        }),
        None
    );
    assert_eq!(
        goal_recovery_reason(&AgentError::CompactBudgetUnrecovered {
            input_tokens: 100,
            context_window: 200,
            full_attempts: 2,
        }),
        None
    );
}

#[test]
fn test_model_http_errors_are_recoverable() {
    for status in [400, 429, 500, 503] {
        let error = AgentError::LlmHttpError {
            status,
            message: "provider failure".to_string(),
        };
        assert_eq!(
            goal_recovery_reason(&error),
            Some("model_error"),
            "HTTP {status} 应允许 goal 恢复重试"
        );
    }
}

#[test]
fn test_compact_retry_exhaustion_is_recoverable() {
    let error = AgentError::CompactRetriesExhausted {
        attempts: 3,
        context_tokens: 100,
        context_window: 200,
    };
    assert_eq!(goal_recovery_reason(&error), Some("compact_failed"));
}

#[test]
fn test_output_truncation_and_tool_failure_are_recoverable() {
    assert_eq!(
        goal_recovery_reason(&AgentError::OutputTruncated { attempts: 3 }),
        Some("output_truncated")
    );
    assert_eq!(
        goal_recovery_reason(&AgentError::ToolExecutionFailed {
            tool: "Bash".to_string(),
            reason: "exited".to_string(),
        }),
        Some("tool_failed")
    );
}

use async_trait::async_trait;
use peri_acp_types::goal::GoalViewSnapshot;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

struct RecordingGoalController {
    recoveries: AtomicU32,
    last_reason: parking_lot::Mutex<String>,
}

#[async_trait]
impl super::GoalController for RecordingGoalController {
    async fn create_goal(&self, _objective: String) -> Result<(), String> {
        Ok(())
    }
    async fn complete_goal(&self) -> Result<(), String> {
        Ok(())
    }
    async fn block_goal(&self, _reason: String) -> Result<(), String> {
        Ok(())
    }
    async fn clear_goal(&self) -> Result<(), String> {
        Ok(())
    }
    async fn recover_after_error(&self, reason: &str) {
        self.recoveries.fetch_add(1, Ordering::Relaxed);
        *self.last_reason.lock() = reason.to_string();
    }
    fn snapshot(&self) -> GoalViewSnapshot {
        GoalViewSnapshot::default()
    }
}

fn recording_controller() -> Arc<RecordingGoalController> {
    Arc::new(RecordingGoalController {
        recoveries: AtomicU32::new(0),
        last_reason: parking_lot::Mutex::new(String::new()),
    })
}

#[tokio::test]
async fn test_terminal_model_error_schedules_goal_recovery() {
    let concrete = recording_controller();
    let controller: Arc<dyn super::GoalController> = concrete.clone();
    super::schedule_goal_recovery_after_error(
        Some(&controller),
        &AgentError::LlmHttpError {
            status: 429,
            message: "rate limited".to_string(),
        },
        false,
    )
    .await;

    assert_eq!(concrete.recoveries.load(Ordering::Relaxed), 1);
    assert_eq!(*concrete.last_reason.lock(), "model_error");
}

#[tokio::test]
async fn test_cancelled_turn_does_not_schedule_goal_recovery() {
    let concrete = recording_controller();
    let controller: Arc<dyn super::GoalController> = concrete.clone();
    super::schedule_goal_recovery_after_error(
        Some(&controller),
        &AgentError::LlmHttpError {
            status: 503,
            message: "unavailable".to_string(),
        },
        true,
    )
    .await;

    assert_eq!(concrete.recoveries.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn test_excluded_error_does_not_schedule_goal_recovery() {
    let concrete = recording_controller();
    let controller: Arc<dyn super::GoalController> = concrete.clone();
    super::schedule_goal_recovery_after_error(
        Some(&controller),
        &AgentError::WorkBudgetExhausted {
            budget: WorkBudgetKind::ReasonRequests,
            used: 1,
            limit: 1,
        },
        false,
    )
    .await;

    assert_eq!(concrete.recoveries.load(Ordering::Relaxed), 0);
}
