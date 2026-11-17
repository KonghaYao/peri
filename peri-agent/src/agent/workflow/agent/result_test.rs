use crate::agent::stages::LoopResult;
use crate::agent::workflow::agent::observation::RunStats;
use crate::agent::workflow::agent::observation::WorkflowObservation;
use crate::session::FrozenContext;
use crate::session::Session;
use peri_acp_types::command::PromptStopReason;
use peri_acp_types::event::ExecutorEvent;
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
            usage_reported: true,
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
    let AgentRunResult::Dead {
        duration_ms,
        token_count,
        ..
    } = &failed.result
    else {
        panic!("forwarder failure must project to dead");
    };
    assert!(
        duration_ms.is_some(),
        "dead attempt must carry measured duration"
    );
    assert_eq!(
        *token_count, None,
        "a dead attempt without provider usage must not claim a measured zero"
    );
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
    let AgentRunResult::Dead {
        duration_ms,
        token_count,
        ..
    } = &interrupted.result
    else {
        panic!("interrupt must project to dead");
    };
    assert!(
        duration_ms.is_some(),
        "interrupted dead attempt must carry duration"
    );
    assert_eq!(*token_count, None);
    assert_eq!(interrupted.result.token_count(), None);
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

#[test]
fn dead_result_reports_tokens_only_when_usage_was_reported() {
    let session = Session::new(Arc::from("/unused"), FrozenContext::builder().build(), None);
    let params = params(None);
    let observation = WorkflowObservation::new(&params, None, None);
    observation.handler().on_event(ExecutorEvent::LlmCallEnd {
        step: 1,
        model: "provider".into(),
        output: String::new(),
        usage: Some(peri_model::TokenUsage::new(10, 0)),
        stop_reason: None,
        request_id: None,
        source_agent_id: None,
    });
    let interrupted = crate::agent::workflow::agent::result::project_run_result(
        LoopResult::Interrupted,
        Ok(()),
        &session,
        &observation,
        &params,
        "effective",
        Instant::now(),
    );
    assert_eq!(
        interrupted.result.token_count(),
        Some(0),
        "a reported zero is a measurement and must survive death"
    );
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
            reason: Some("interrupted".into()),
            detail: None,
            duration_ms: None,
            token_count: None,
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
                reason,
                detail: None,
                duration_ms: None,
                token_count: None,
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
        reason: Some("interrupted".into()),
        detail: None,
        duration_ms: None,
        token_count: None,
    };
    assert_eq!(
        crate::agent::workflow::agent::result::terminal_status_after_close(&cancelled, false),
        AgentStatus::Error
    );
    let failed = AgentRunResult::Dead {
        reason: None,
        detail: None,
        duration_ms: None,
        token_count: None,
    };
    assert_eq!(
        crate::agent::workflow::agent::result::terminal_status_after_close(&failed, false),
        AgentStatus::Error
    );
}
