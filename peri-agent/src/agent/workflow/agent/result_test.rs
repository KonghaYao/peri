use super::*;
use crate::session::FrozenContext;
use serde_json::json;

fn params(schema: Option<serde_json::Value>) -> AgentRunParams {
    serde_json::from_value(json!({"runId": "run", "agentId": 7, "prompt": "test",
        "phase": "review", "schema": schema}))
    .unwrap()
}

#[test]
fn completed_plain_output_preserves_wire_string_and_fallback_statistics() {
    let result = completed_result(
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
    let result = completed_result(
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
    let invalid = completed_result(
        "{}".into(),
        RunStats::default(),
        &params(Some(schema)),
        "effective",
        Instant::now(),
    );
    assert!(matches!(invalid, AgentRunResult::Dead { reason, detail }
        if reason.as_deref() == Some("no-structured-output")
        && detail.as_deref() == Some("missing required field: answer")));
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
    let result = completed_result(
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
        let result = completed_result(
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
        let result = completed_result(
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
        let result = completed_result(
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
    let result = completed_result(
        r#"{"packet_revision":2.5}"#.into(),
        RunStats::default(),
        &params(Some(schema)),
        "effective",
        Instant::now(),
    );

    assert!(matches!(
        result,
        AgentRunResult::Dead { reason, detail }
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
    let result = completed_result(
        r#"{"attempts":[{}]}"#.into(),
        RunStats::default(),
        &params(Some(schema)),
        "effective",
        Instant::now(),
    );

    assert!(matches!(
        result,
        AgentRunResult::Dead { reason, detail }
        if reason.as_deref() == Some("no-structured-output")
            && detail.as_deref() == Some("missing required field: attempts[0].attempt_number")
    ));
}

#[test]
fn run_projection_keeps_forwarder_failure_priority_and_cancel_terminal() {
    let session = Session::new(Arc::from("/unused"), FrozenContext::builder().build(), None);
    let params = params(None);
    let observation = WorkflowObservation::new(&params, None, None);
    let failed = project_run_result(
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
    let interrupted = project_run_result(
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
    let completed = project_run_result(
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
