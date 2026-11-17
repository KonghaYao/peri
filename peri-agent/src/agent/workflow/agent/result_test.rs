use crate::agent::stages::LoopResult;
use crate::agent::workflow::agent::observation::RunStats;
use crate::agent::workflow::agent::observation::WorkflowObservation;
use crate::session::FrozenContext;
use crate::session::Session;
use peri_acp_types::command::PromptStopReason;
use peri_acp_types::session::ExecutionFailure;
use peri_acp_types::session::TurnTelemetryOutcome;
use peri_acp_types::thread::AgentStatus;
use peri_acp_types::workflow::AgentRunParams;
use peri_acp_types::workflow::AgentRunResult;
use peri_acp_types::workflow::Usage;
use serde_json::json;
use std::sync::Arc;
use std::time::Instant;

fn params(schema: Option<serde_json::Value>) -> AgentRunParams {
    serde_json::from_value(json!({"runId": "run", "agentId": 7, "prompt": "test",
        "phase": "review", "schema": schema}))
    .expect("from_value must accept this test fixture representation")
}

#[test]
fn completed_plain_output_preserves_wire_string_and_fallback_statistics() {
    let result = crate::agent::workflow::agent::result::completed_result(
        "plain text".into(),
        RunStats {
            tool_count: 3,
            ..RunStats::default()
        },
        &params(None),
        "effective-model",
        Instant::now(),
    );
    let AgentRunResult::Ok {
        output,
        usage,
        model,
        tool_count,
        token_count,
        phase,
        duration_ms,
    } = result
    else {
        panic!("plain text without schema must succeed")
    };
    assert_eq!(output, json!("plain text"));
    assert_eq!(
        (usage.output_tokens, token_count, tool_count),
        (2, Some(2), Some(3))
    );
    assert_eq!(model.as_deref(), Some("effective-model"));
    assert_eq!(phase.as_deref(), Some("review"));
    assert!(duration_ms.is_some());
}

#[test]
fn completed_structured_output_validates_without_changing_wire_or_actual_usage() {
    let output = r#"{"answer": "ok"}"#;
    let schema = json!({"type": "object", "required": ["answer"],
        "properties": {"answer": {"type": "string"}}});
    let result = crate::agent::workflow::agent::result::completed_result(
        output.into(),
        RunStats {
            output_tokens: 37,
            last_model: Some("provider-model".into()),
            tool_count: 2,
        },
        &params(Some(schema.clone())),
        "effective",
        Instant::now(),
    );
    let AgentRunResult::Ok {
        output: actual,
        usage,
        model,
        token_count,
        ..
    } = result
    else {
        panic!("valid output must succeed")
    };
    assert_eq!(actual, json!(output));
    assert_eq!((usage.output_tokens, token_count), (37, Some(37)));
    assert_eq!(model.as_deref(), Some("provider-model"));
    let invalid = crate::agent::workflow::agent::result::completed_result(
        "{}".into(),
        RunStats::default(),
        &params(Some(schema)),
        "effective",
        Instant::now(),
    );
    assert!(
        matches!(invalid, AgentRunResult::Dead { reason, detail, .. }
        if reason.as_deref() == Some("no-structured-output")
        && detail.as_deref() == Some("missing required field: answer"))
    );
}

#[test]
fn completed_structured_output_accepts_integer_nested_array() {
    let schema = json!({
        "type": "object",
        "required": ["packet_revision", "attempts"],
        "properties": {
            "packet_revision": {"type": "integer"},
            "attempts": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["attempt_number", "status"],
                    "properties": {"attempt_number": {"type": "integer"}}
                }
            }
        }
    });
    let result = crate::agent::workflow::agent::result::completed_result(
        r#"{"packet_revision":2,"attempts":[{"attempt_number":1,"status":"valid"}]}"#.into(),
        RunStats::default(),
        &params(Some(schema)),
        "effective",
        Instant::now(),
    );

    assert!(matches!(result, AgentRunResult::Ok { .. }));
}

#[test]
fn completed_structured_output_accepts_numeric_integer_semantics() {
    for output in ["2", "2.0", "2e1"] {
        let result = crate::agent::workflow::agent::result::completed_result(
            output.into(),
            RunStats::default(),
            &params(Some(json!({"type": "integer"}))),
            "effective",
            Instant::now(),
        );
        assert!(matches!(result, AgentRunResult::Ok { .. }), "{output}");
    }
}

#[test]
fn completed_structured_output_accepts_integer_and_fractional_number_values() {
    let schema = json!({"type": "number"});
    for output in ["2", "2.5"] {
        let result = crate::agent::workflow::agent::result::completed_result(
            output.into(),
            RunStats::default(),
            &params(Some(schema.clone())),
            "effective",
            Instant::now(),
        );
        assert!(matches!(result, AgentRunResult::Ok { .. }), "{output}");
    }
}

#[test]
fn completed_structured_output_rejects_lossless_fractional_integer_lexemes() {
    for output in [
        "2.0000000000000001",
        "2.5",
        "1e-999",
        "1e-999999999999999999999",
    ] {
        let result = crate::agent::workflow::agent::result::completed_result(
            output.into(),
            RunStats::default(),
            &params(Some(json!({"type": "integer"}))),
            "effective",
            Instant::now(),
        );
        assert!(
            matches!(result, AgentRunResult::Dead { reason, .. }
                if reason.as_deref() == Some("no-structured-output")),
            "{output}"
        );
    }
}

#[test]
fn completed_structured_output_rejects_fractional_integer() {
    let schema = json!({
        "type": "object",
        "properties": {"packet_revision": {"type": "integer"}}
    });
    let result = crate::agent::workflow::agent::result::completed_result(
        r#"{"packet_revision":2.5}"#.into(),
        RunStats::default(),
        &params(Some(schema)),
        "effective",
        Instant::now(),
    );

    assert!(matches!(
        result,
        AgentRunResult::Dead { reason, detail, .. }
        if reason.as_deref() == Some("no-structured-output")
            && detail.as_deref()
                == Some("field 'packet_revision': expected type 'integer', got 'number'")
    ));
}

#[test]
fn completed_structured_output_rejects_missing_nested_required_field() {
    let schema = json!({
        "type": "object",
        "properties": {
            "attempts": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["attempt_number"]
                }
            }
        }
    });
    let result = crate::agent::workflow::agent::result::completed_result(
        r#"{"attempts":[{}]}"#.into(),
        RunStats::default(),
        &params(Some(schema)),
        "effective",
        Instant::now(),
    );

    assert!(matches!(
        result,
        AgentRunResult::Dead { reason, detail, .. }
        if reason.as_deref() == Some("no-structured-output")
            && detail.as_deref() == Some("missing required field: attempts[0].attempt_number")
    ));
}

#[test]
fn run_projection_keeps_forwarder_failure_priority_and_cancel_terminal() {
    let session = Session::new(Arc::from("/unused"), FrozenContext::builder().build(), None);
    let params = params(None);
    let observation = WorkflowObservation::new(&params, None, None);
    let failed = crate::agent::workflow::agent::result::project_run_result(
        LoopResult::Interrupted,
        Err(ExecutionFailure::internal("forwarder join failed")),
        &session,
        &observation,
        &params,
        "effective",
        Instant::now(),
    );
    assert!(matches!(&failed.result, AgentRunResult::Dead { reason, .. }
        if reason.as_deref() == Some("event-forwarder-failed")));
    assert!(
        matches!(failed.telemetry_outcome(), TurnTelemetryOutcome::Failed { failure }
        if failure.public_message == "forwarder join failed")
    );
    let interrupted = crate::agent::workflow::agent::result::project_run_result(
        LoopResult::Interrupted,
        Ok(()),
        &session,
        &observation,
        &params,
        "effective",
        Instant::now(),
    );
    assert!(matches!(
        interrupted.telemetry_outcome(),
        TurnTelemetryOutcome::Stopped {
            reason: PromptStopReason::Cancelled
        }
    ));
    let completed = crate::agent::workflow::agent::result::project_run_result(
        LoopResult::Completed,
        Ok(()),
        &session,
        &observation,
        &params,
        "effective",
        Instant::now(),
    );
    assert!(matches!(
        completed.telemetry_outcome(),
        TurnTelemetryOutcome::Completed
    ));
    assert_eq!(completed.result.token_count(), Some(0));
}

/// child 终态与 sync/background 生命周期约定一致：完成 Done、取消 Cancelled、
/// 其余失败 Error；`interrupted` 是唯一取消投影。
#[test]
fn workflow_terminal_status_follows_child_lifecycle_mapping() {
    let done = AgentRunResult::Ok {
        output: json!("ok"),
        usage: Usage { output_tokens: 0 },
        model: None,
        tool_count: None,
        token_count: None,
        phase: None,
        duration_ms: None,
    };
    assert_eq!(
        crate::agent::workflow::agent::result::terminal_status(&done),
        AgentStatus::Done
    );
    assert_eq!(
        crate::agent::workflow::agent::result::terminal_status(&AgentRunResult::Skipped),
        AgentStatus::Done
    );
    assert_eq!(
        crate::agent::workflow::agent::result::terminal_status(&AgentRunResult::Dead {
            scope_close_incomplete: false,
            reason: Some("interrupted".into()),
            detail: None,
        }),
        AgentStatus::Cancelled
    );
    let reason_items = [
        None,
        Some("runagent-threw".to_owned()),
        Some("settlement-due".to_owned()),
        Some("event-forwarder-failed".to_owned()),
        Some("initial-prompt-enqueue-failed".to_owned()),
        Some("execution-blocked".to_owned()),
    ];
    for reason in reason_items {
        assert_eq!(
            crate::agent::workflow::agent::result::terminal_status(&AgentRunResult::Dead {
                scope_close_incomplete: false,
                reason,
                detail: None
            }),
            AgentStatus::Error
        );
    }
}

/// close 未结清时任何结果都降级为 Error。
#[test]
fn workflow_terminal_status_after_incomplete_close() {
    let done = AgentRunResult::Ok {
        output: json!("ok"),
        usage: Usage { output_tokens: 0 },
        model: None,
        tool_count: None,
        token_count: None,
        phase: None,
        duration_ms: None,
    };
    assert_eq!(
        crate::agent::workflow::agent::result::terminal_status_after_close(&done, true),
        AgentStatus::Done
    );
    assert_eq!(
        crate::agent::workflow::agent::result::terminal_status_after_close(&done, false),
        AgentStatus::Error
    );
    let cancelled = AgentRunResult::Dead {
        scope_close_incomplete: false,
        reason: Some("interrupted".into()),
        detail: None,
    };
    assert_eq!(
        crate::agent::workflow::agent::result::terminal_status_after_close(&cancelled, false),
        AgentStatus::Error
    );
    let failed = AgentRunResult::Dead {
        scope_close_incomplete: false,
        reason: None,
        detail: None,
    };
    assert_eq!(
        crate::agent::workflow::agent::result::terminal_status_after_close(&failed, false),
        AgentStatus::Error
    );
}

#[test]
fn scope_close_failure_replaces_success_before_return_and_telemetry() {
    let projected = super::ProjectedResult {
        result: AgentRunResult::Ok {
            output: json!("implementation completed"),
            usage: Usage { output_tokens: 1 },
            model: None,
            tool_count: None,
            token_count: None,
            phase: None,
            duration_ms: None,
        },
        failure: None,
    }
    .after_scope_close(false);
    assert!(
        matches!(&projected.result, AgentRunResult::Dead { reason, detail, .. }
        if reason.as_deref() == Some("scope-close-incomplete")
        && detail.as_deref().unwrap().contains("outstanding resources are unconfirmed"))
    );
    assert_eq!(
        super::terminal_status(&projected.result),
        AgentStatus::Error
    );
    assert!(matches!(
        projected.telemetry_outcome(),
        TurnTelemetryOutcome::Failed { .. }
    ));
}

#[test]
fn incomplete_scope_close_keeps_the_first_failure_reason() {
    // The forwarder join error already blocks handoff; masking it with the close
    // failure would lose the cause the caller needs to diagnose the run.
    let original = AgentRunResult::Dead {
        scope_close_incomplete: false,
        reason: Some("event-forwarder-failed".into()),
        detail: Some("Workflow agent event forwarding failed".into()),
    };
    let projected = super::ProjectedResult {
        result: original.clone(),
        failure: Some(ExecutionFailure::internal(
            "Workflow agent event forwarding failed",
        )),
    }
    .after_scope_close(false);
    assert_eq!(projected.result, original.after_scope_close(false));
    assert!(projected.result.unsettled_scope_error().is_some());
    match projected.telemetry_outcome() {
        TurnTelemetryOutcome::Failed { failure } => assert!(
            failure.public_message.contains("forwarding failed"),
            "telemetry 必须保留首个失败: {}",
            failure.public_message
        ),
        other => panic!("失败结果必须投影为 Failed 遥测: {other:?}"),
    }
}

#[test]
fn successful_scope_close_preserves_the_original_outcome() {
    for original in [
        AgentRunResult::Skipped,
        AgentRunResult::Dead {
            scope_close_incomplete: false,
            reason: Some("interrupted".into()),
            detail: None,
        },
    ] {
        let projected = super::ProjectedResult {
            result: original.clone(),
            failure: None,
        }
        .after_scope_close(true);
        assert_eq!(projected.result, original);
    }
}

#[test]
fn formal_report_survives_compaction_and_cleanup_through_schema_projection() {
    let session = Session::new(Arc::from("/unused"), FrozenContext::builder().build(), None);
    let ids = session.transcript().write().append_batch(vec![
        crate::messages::BaseMessage::human("Review the changed artifact"),
        crate::messages::BaseMessage::ai("<workflow-result>{\"status\":\"passed\",\"artifact\":\"revision-2\"}</workflow-result>"),
        crate::messages::BaseMessage::ai("Background process cleaned up."),
        crate::messages::BaseMessage::tool_result("tool", "<workflow-result>untrusted tool text</workflow-result>"),
    ]);
    session.transcript().write().set_excluded(ids[1], true);
    let params = params(Some(
        json!({"type": "object", "required": ["status", "artifact"],
        "properties": {"status": {"type": "string"}, "artifact": {"type": "string"}}}),
    ));
    let observation = WorkflowObservation::new(&params, None, None);
    let projected = super::project_run_result(
        LoopResult::Completed,
        Ok(()),
        &session,
        &observation,
        &params,
        "effective",
        Instant::now(),
    )
    .after_scope_close(true);
    let AgentRunResult::Ok { output, .. } = projected.result else {
        panic!("the formal JSON report must survive cleanup and schema validation");
    };
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(output.as_str().unwrap()).unwrap(),
        json!({"status": "passed", "artifact": "revision-2"})
    );
}
