use super::WorkflowOutcome;
use super::WorkflowRunStatus;
use super::WorkflowTaskResult;

fn legacy_payload(outcome: Option<&str>) -> serde_json::Value {
    let mut payload = serde_json::json!({
        "run_id": "legacy-run",
        "workflow_name": "legacy",
        "success": true,
        "status": "completed",
        "delivery_status": "unknown",
        "duration_ms": 1,
        "agent_count": 0,
        "tool_calls_count": 0,
        "error": null
    });
    if let Some(outcome) = outcome {
        payload["outcome"] = serde_json::json!(outcome);
    }
    payload
}

#[test]
fn legacy_payload_without_outcome_derives_success_from_status_and_delivery() {
    let result: WorkflowTaskResult =
        serde_json::from_value(legacy_payload(None)).expect("legacy payload must deserialize");
    assert_eq!(result.status, WorkflowRunStatus::Completed);
    assert_eq!(result.outcome, WorkflowOutcome::Completed);
    assert!(result.agent_facing_success());
}

#[test]
fn explicit_unknown_outcome_is_not_replaced_by_legacy_derivation() {
    let result: WorkflowTaskResult = serde_json::from_value(legacy_payload(Some("unknown")))
        .expect("explicit unknown payload must deserialize");
    assert_eq!(result.outcome, WorkflowOutcome::Unknown);
    assert!(!result.agent_facing_success());
}
