use super::DeliveryReason;
use super::PostconditionObservation;
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

#[test]
fn delivery_reason_values_are_stable_wire_words() {
    for (reason, expected) in [
        (DeliveryReason::NotRequested, "not_requested"),
        (DeliveryReason::UnattributedChange, "unattributed_change"),
        (
            DeliveryReason::ObservationIncomplete,
            "observation_incomplete",
        ),
    ] {
        assert_eq!(serde_json::to_value(reason).unwrap(), expected);
    }
    for (observation, expected) in [
        (PostconditionObservation::Complete, "complete"),
        (PostconditionObservation::NotApplicable, "not_applicable"),
        (PostconditionObservation::Incomplete, "incomplete"),
    ] {
        assert_eq!(serde_json::to_value(observation).unwrap(), expected);
    }
}

#[test]
fn old_readers_ignore_new_reason_fields_and_keep_unknown() {
    let mut payload = legacy_payload(None);
    payload["delivery_reason"] = serde_json::json!("not_requested");
    payload["postcondition_observation"] = serde_json::json!("complete");
    let result: WorkflowTaskResult =
        serde_json::from_value(payload).expect("payload with new optional fields must deserialize");
    assert_eq!(
        result.delivery_status,
        super::DeliveryStatus::Unknown,
        "新增原因字段不改变旧 delivery 枚举的读法"
    );
}

/// completed 且未请求交付：通知不得再声称 “delivery is blocked”。
#[test]
fn completed_without_delivery_request_does_not_report_blocked_delivery() {
    let result: WorkflowTaskResult =
        serde_json::from_value(legacy_payload(None)).expect("legacy payload must deserialize");
    assert_eq!(result.notification_status_phrase(), "completed");
    let notification = result.to_notification();
    assert!(
        !notification.contains("delivery is blocked"),
        "未请求交付的 completed run 不得显示交付阻塞: {notification}"
    );
}
