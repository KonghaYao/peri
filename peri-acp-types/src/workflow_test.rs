use super::DeliveryReason;
use super::PostconditionObservation;
use super::WorkflowOutcome;
use super::WorkflowRunStatus;
use super::WorkflowTaskResult;
use super::WorkflowToolErrorCode;
use super::WorkflowToolOutcome;

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
    assert!(
        !result.unknown_side_effects,
        "旧 payload 缺 unknownSideEffects 时按 false 读取"
    );
}

/// issue #407：脚本工具结果的 wire 形状（tagged ok/error，camelCase）与错误码。
#[test]
fn workflow_tool_outcome_wire_shape_is_tagged_and_typed() {
    let ok = WorkflowToolOutcome::Ok {
        structured_content: Some(serde_json::json!({"answer": 42})),
        content: "42".into(),
    };
    let ok_wire = serde_json::to_value(&ok).expect("tool outcome 可序列化");
    assert_eq!(ok_wire["kind"], "ok");
    assert_eq!(ok_wire["structuredContent"]["answer"], 42);
    assert_eq!(ok_wire["content"], "42");
    let decoded: WorkflowToolOutcome =
        serde_json::from_value(ok_wire).expect("tool outcome 可反序列化");
    assert_eq!(decoded, ok);
    assert!(!ok.marks_unknown_side_effects());

    let error = WorkflowToolOutcome::error(
        WorkflowToolErrorCode::OutcomeUnknown,
        "Read",
        "no raw payload",
    );
    let error_wire = serde_json::to_value(&error).expect("tool outcome 可序列化");
    assert_eq!(error_wire["kind"], "error");
    assert_eq!(error_wire["code"], "outcome_unknown");
    assert_eq!(error_wire["tool"], "Read");
    assert_eq!(error_wire["message"], "no raw payload");
    assert!(error.marks_unknown_side_effects());

    // 错误码字面量是跨侧契约（Node 侧按同一字符串抛类型化错误）。
    for (code, expected) in [
        (WorkflowToolErrorCode::ToolNotFound, "tool_not_found"),
        (WorkflowToolErrorCode::InvalidArguments, "invalid_arguments"),
        (WorkflowToolErrorCode::ToolUnavailable, "tool_unavailable"),
        (WorkflowToolErrorCode::PolicyDenied, "policy_denied"),
        (
            WorkflowToolErrorCode::ApprovalUnavailable,
            "approval_unavailable",
        ),
        (WorkflowToolErrorCode::NotExecuted, "not_executed"),
        (WorkflowToolErrorCode::ToolError, "tool_error"),
        (WorkflowToolErrorCode::ProtocolError, "protocol_error"),
        (WorkflowToolErrorCode::TaskAccepted, "task_accepted"),
        (WorkflowToolErrorCode::OutcomeUnknown, "outcome_unknown"),
        (WorkflowToolErrorCode::Cancelled, "cancelled"),
    ] {
        assert_eq!(serde_json::to_value(code).unwrap(), expected);
    }
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

/// issue #407：未知工具副作用只在事实为 true 时出现在完成通知里。
#[test]
fn unknown_side_effects_notice_appears_only_when_the_fact_is_set() {
    let mut result: WorkflowTaskResult =
        serde_json::from_value(legacy_payload(None)).expect("legacy payload must deserialize");
    assert!(!result.unknown_side_effects);
    let quiet = result.to_notification();
    assert!(
        !quiet.contains("Unknown tool side effects"),
        "false 时不得出现未知副作用行: {quiet}"
    );

    result.unknown_side_effects = true;
    let flagged = result.to_notification();
    assert!(
        flagged.contains(
            "Unknown tool side effects: at least one script tool call had no settled result; \
             verify external effects before relying on this run."
        ),
        "true 时必须明确提示核对外部影响: {flagged}"
    );
    // 该行紧随 execution/outcome 投影，属于完成摘要的一部分。
    let projection = flagged
        .find("Execution: ")
        .expect("projection line must exist");
    let notice = flagged
        .find("Unknown tool side effects")
        .expect("unknown side effects line must exist");
    assert!(notice > projection);
}

/// 旧 journal 的 attempt 缺 `execution`：反序列化为 None，不得伪造执行结果。
#[test]
fn legacy_attempt_without_execution_deserializes_as_unknown() {
    let attempt: super::WorkflowAttempt = serde_json::from_value(serde_json::json!({
        "runId": "legacy-run",
        "agentId": 3,
        "journalSeq": 4,
        "consumed": true,
        "disposition": "produced"
    }))
    .expect("legacy attempt payload must deserialize");
    assert!(attempt.execution.is_none());
}

/// attempt 执行结果与 disposition 正交：produced 仍可携带 dead/limited 事实。
#[test]
fn attempt_execution_round_trips_independently_of_disposition() {
    let attempt: super::WorkflowAttempt = serde_json::from_value(serde_json::json!({
        "runId": "run-1",
        "agentId": 3,
        "journalSeq": 4,
        "consumed": true,
        "disposition": "produced",
        "execution": {
            "outcome": "limited",
            "reason": "max-iterations",
            "iterationLimit": 400,
            "iterationLimitSource": "agent_definition",
            "iterationsUsed": 400,
            "tokenBudget": 60000,
            "tokenCount": 75323
        }
    }))
    .expect("attempt execution payload must deserialize");
    assert_eq!(
        attempt.disposition,
        super::AttemptDisposition::Produced,
        "disposition 保持原语义"
    );
    let execution = attempt.execution.expect("execution must be present");
    assert_eq!(execution.outcome, super::AttemptOutcome::Limited);
    assert_eq!(execution.iteration_limit, Some(400));
    assert_eq!(
        execution.iteration_limit_source,
        Some(super::IterationLimitSource::AgentDefinition)
    );
    assert_eq!(execution.iterations_used, Some(400));
    assert_eq!(execution.token_budget, Some(60_000));
    assert_eq!(execution.token_count, Some(75_323));
}

/// 未采集字段保持缺席：不把未知用量写成 0。
#[test]
fn unmeasured_attempt_execution_fields_stay_absent_on_the_wire() {
    let execution = super::AttemptExecution {
        outcome: super::AttemptOutcome::Dead,
        reason: Some("killed".into()),
        iteration_limit: None,
        iteration_limit_source: None,
        iterations_used: None,
        token_budget: None,
        token_count: None,
    };
    let wire = serde_json::to_value(&execution).expect("execution must serialize");
    assert_eq!(wire["outcome"], "dead");
    for field in [
        "iterationLimit",
        "iterationLimitSource",
        "iterationsUsed",
        "tokenBudget",
        "tokenCount",
    ] {
        assert!(wire.get(field).is_none(), "{field} must stay absent");
    }
}

#[test]
fn limit_breaches_serialize_every_scope_with_attempt_identity() {
    let breach = super::LimitBreach {
        scope: super::LimitScope::Tokens,
        limit: Some(60_000),
        observed: Some(75_323),
        attempt: Some(super::LimitBreachAttempt {
            run_id: "run-1".into(),
            task_id: Some("run-1:3".into()),
            agent_id: 3,
        }),
        detected_at: "2026-11-21T00:00:00Z".into(),
    };
    let wire = serde_json::to_value(&breach).expect("breach must serialize");
    assert_eq!(wire["scope"], "tokens");
    assert_eq!(wire["limit"], 60_000);
    assert_eq!(wire["observed"], 75_323);
    assert_eq!(wire["attempt"]["taskId"], "run-1:3");
    assert_eq!(wire["attempt"]["agentId"], 3);

    let unmeasured = super::LimitBreach {
        scope: super::LimitScope::ElapsedMs,
        limit: Some(1_000),
        observed: None,
        attempt: None,
        detected_at: "2026-11-21T00:00:00Z".into(),
    };
    let wire = serde_json::to_value(&unmeasured).expect("breach must serialize");
    assert!(wire.get("observed").is_none(), "未采集不得写成 0");
    assert!(wire.get("attempt").is_none());
}

#[test]
fn iteration_limit_exhaustion_uses_a_stable_structured_reason() {
    let dead: super::AgentRunResult = serde_json::from_value(serde_json::json!({
        "kind": "dead",
        "reason": super::AGENT_DEAD_REASON_MAX_ITERATIONS,
        "detail": "max iterations exceeded (400)"
    }))
    .expect("dead payload must deserialize");
    assert!(dead.iteration_limit_exhausted());
    let other: super::AgentRunResult = serde_json::from_value(serde_json::json!({
        "kind": "dead",
        "reason": "runagent-threw"
    }))
    .expect("dead payload must deserialize");
    assert!(!other.iteration_limit_exhausted());
}
