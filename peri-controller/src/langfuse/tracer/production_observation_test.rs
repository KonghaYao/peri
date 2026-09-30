use std::sync::Arc;

use langfuse_client::{IngestionEvent, SpanBody};
use peri_acp_types::session::TurnTelemetryOutcome;
use peri_agent::agent::events::{Stage, StageStatus};
use peri_agent::messages::BaseMessage;
use peri_agent::tools::ToolDefinition;

use super::stages::MAIN_AGENT_KEY;
use super::LangfuseTracer;
use crate::langfuse::config::LangfuseConfig;
use crate::langfuse::fake_session::FakeLangfuseSession;

fn tracer(rate: f64) -> (LangfuseTracer, Arc<FakeLangfuseSession>) {
    let session = FakeLangfuseSession::new("session-observation");
    let tracer = LangfuseTracer::new(
        session.clone(),
        "session-observation".to_string(),
        LangfuseConfig {
            trace_sampling: rate,
            ..Default::default()
        },
    );
    (tracer, session)
}

fn workflow_spans(events: &[IngestionEvent]) -> Vec<&SpanBody> {
    events
        .iter()
        .filter_map(|event| match event {
            IngestionEvent::SpanCreate { body, .. }
                if body.name.as_deref() == Some("workflow-wf") =>
            {
                Some(body)
            }
            _ => None,
        })
        .collect()
}

fn assert_complete_span(body: &SpanBody, plan: &str) {
    assert!(body.id.as_ref().is_some_and(|id| !id.is_empty()));
    let start = chrono::DateTime::parse_from_rfc3339(body.start_time.as_deref().unwrap()).unwrap();
    let end = chrono::DateTime::parse_from_rfc3339(body.end_time.as_deref().unwrap()).unwrap();
    assert!(start <= end);
    assert_eq!(body.input, Some(serde_json::json!({"plan": plan})));
    assert!(body.output.is_some());
    assert_eq!(body.session_id.as_deref(), Some("session-observation"));
}

#[tokio::test]
async fn workflow_completed_exports_once_with_frozen_start_input_and_parent() {
    let (mut tracer, session) = tracer(1.0);
    tracer.on_turn_start("input");
    tracer.on_stage_start(Stage::Act, "turn");
    let parent = tracer.agent_observation_id.clone();
    tracer.on_workflow_start("wf", "original plan");
    assert!(workflow_spans(&session.events_snapshot()).is_empty());
    tracer.on_workflow_start("wf", "duplicate plan");
    tracer.on_workflow_end("wf", 3, 9);
    tracer.on_workflow_end("wf", 99, 99);
    tracer
        .on_turn_end(TurnTelemetryOutcome::Completed)
        .await
        .unwrap();

    let events = session.events_snapshot();
    let spans = workflow_spans(&events);
    assert_eq!(spans.len(), 1);
    assert_complete_span(spans[0], "original plan");
    assert_eq!(spans[0].trace_id.as_deref(), Some(tracer.trace_id.as_str()));
    assert_eq!(
        spans[0].parent_observation_id.as_deref(),
        Some(parent.as_str())
    );
    assert_eq!(
        spans[0].output,
        Some(serde_json::json!({"agents_spawned": 3, "tool_calls": 9}))
    );
    assert!(spans[0].metadata.is_none());
    assert!(!events
        .iter()
        .any(|event| matches!(event, IngestionEvent::SpanUpdate { .. })));
}

#[tokio::test]
async fn workflow_stage_overwrite_closes_once_and_ignores_late_ends() {
    let (mut tracer, session) = tracer(1.0);
    tracer.on_turn_start("input");
    tracer.on_stage_start(Stage::Act, "turn");
    let old_stage = tracer.stages.active_handle(MAIN_AGENT_KEY).unwrap().clone();
    tracer.on_workflow_start("wf", "overwritten plan");
    tracer.on_stage_start(Stage::Reason, "turn");
    let new_stage_id = tracer
        .stages
        .active_handle(MAIN_AGENT_KEY)
        .unwrap()
        .span_id
        .clone();
    tracer.on_workflow_end("wf", 8, 12);
    tracer.on_stage_end(MAIN_AGENT_KEY, &old_stage, StageStatus::Error);
    assert_eq!(
        tracer.stages.active_handle(MAIN_AGENT_KEY).unwrap().span_id,
        new_stage_id
    );
    tracer
        .on_turn_end(TurnTelemetryOutcome::Completed)
        .await
        .unwrap();

    let events = session.events_snapshot();
    let spans = workflow_spans(&events);
    assert_eq!(spans.len(), 1);
    assert_complete_span(spans[0], "overwritten plan");
    assert_eq!(spans[0].metadata.as_ref().unwrap()["incomplete"], true);
    assert_eq!(
        spans[0].output.as_ref().unwrap()["error_class"],
        "lifecycle_incomplete"
    );
}

#[tokio::test]
async fn workflow_turn_end_fallback_exports_once_and_late_end_is_ignored() {
    let (mut tracer, session) = tracer(1.0);
    tracer.on_turn_start("input");
    tracer.on_stage_start(Stage::Act, "turn");
    tracer.on_workflow_start("wf", "unfinished plan");
    tracer
        .on_turn_end(TurnTelemetryOutcome::Completed)
        .await
        .unwrap();
    tracer.on_workflow_end("wf", 2, 3);

    let events = session.events_snapshot();
    let spans = workflow_spans(&events);
    assert_eq!(spans.len(), 1);
    assert_complete_span(spans[0], "unfinished plan");
    assert_eq!(
        spans[0].status_message.as_deref(),
        Some("lifecycle_incomplete")
    );
}

#[tokio::test]
async fn workflow_stage_end_fallback_survives_duplicate_stage_end() {
    let (mut tracer, session) = tracer(1.0);
    tracer.on_turn_start("input");
    tracer.on_stage_start(Stage::Act, "turn");
    let mut handle = tracer.stages.active_handle(MAIN_AGENT_KEY).unwrap().clone();
    handle.start_time = "2026-09-01T00:00:00Z".to_string();
    tracer.on_workflow_start("wf", "stage-end plan");
    tracer.on_stage_end(MAIN_AGENT_KEY, &handle, StageStatus::Done);
    tracer.on_stage_end(MAIN_AGENT_KEY, &handle, StageStatus::Error);
    tracer.emit_stage_span_close(&handle, StageStatus::Error, None);
    tracer.on_workflow_end("wf", 2, 3);
    tracer
        .on_turn_end(TurnTelemetryOutcome::Completed)
        .await
        .unwrap();

    let events = session.events_snapshot();
    let spans = workflow_spans(&events);
    assert_eq!(spans.len(), 1);
    assert_complete_span(spans[0], "stage-end plan");
    assert_eq!(events.iter().filter(|event| matches!(event,
        IngestionEvent::SpanCreate { body, .. } if body.id.as_deref() == Some(handle.span_id.as_str())
    )).count(), 1);
}

#[tokio::test]
async fn workflow_end_before_start_and_outside_act_do_not_export() {
    let (mut tracer, session) = tracer(1.0);
    tracer.on_turn_start("input");
    tracer.on_workflow_end("wf", 2, 3);
    tracer.on_stage_start(Stage::Reason, "turn");
    tracer.on_workflow_start("wf", "invalid stage plan");
    tracer.on_workflow_end("wf", 2, 3);
    tracer
        .on_turn_end(TurnTelemetryOutcome::Completed)
        .await
        .unwrap();
    assert!(workflow_spans(&session.events_snapshot()).is_empty());
}

#[tokio::test]
async fn unsampled_workflow_and_generation_do_not_export() {
    let (mut tracer, session) = tracer(0.0);
    tracer.on_turn_start("input");
    tracer.on_stage_start(Stage::Act, "turn");
    tracer.on_workflow_start("wf", "plan");
    tracer.on_workflow_end("wf", 2, 3);
    tracer.on_llm_start("main", 0, &[BaseMessage::human("private")], &[]);
    tracer.on_llm_end("main", 0, "model", "provider", "output", None, None);
    tracer
        .on_turn_end(TurnTelemetryOutcome::Completed)
        .await
        .unwrap();
    assert!(session.events_snapshot().is_empty());
}

#[tokio::test]
async fn turn_uses_session_attributes_without_session_create() {
    let (mut tracer, session) = tracer(1.0);
    tracer.on_turn_start("input");
    tracer
        .on_turn_end(TurnTelemetryOutcome::Completed)
        .await
        .unwrap();
    let events = session.events_snapshot();
    assert!(!events
        .iter()
        .any(|event| matches!(event, IngestionEvent::SessionCreate { .. })));
    assert!(events.iter().any(|event| matches!(event,
        IngestionEvent::TraceCreate { body, .. } if body.session_id.as_deref() == Some("session-observation")
    )));
    assert!(events.iter().any(|event| matches!(event,
        IngestionEvent::ObservationCreate { body, .. } if body.session_id.as_deref() == Some("session-observation")
    )));
}

#[tokio::test]
async fn abandoned_generation_keeps_full_fallback_messages_and_tools() {
    let (mut tracer, session) = tracer(1.0);
    let messages = vec![BaseMessage::human("长上下文🙂".repeat(4096))];
    let tools = vec![ToolDefinition {
        name: "lookup".to_string(),
        description: "lookup description".to_string(),
        parameters: serde_json::json!({"type": "object", "properties": {"query": {"type": "string"}}}),
    }];
    let expected = serde_json::json!({"messages": &messages, "tools": &tools});
    tracer.on_turn_start("input");
    tracer.on_llm_start("main", 0, &messages, &tools);
    tracer.on_llm_retrying("main", 0, 1, 3, 10, "protected error");
    tracer
        .on_turn_end(TurnTelemetryOutcome::Completed)
        .await
        .unwrap();
    let events = session.events_snapshot();
    let bodies: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            IngestionEvent::GenerationCreate { body, .. } => Some(body),
            _ => None,
        })
        .collect();
    assert_eq!(bodies.len(), 1);
    assert_eq!(bodies[0].input.as_ref(), Some(&expected));
    assert_eq!(bodies[0].metadata.as_ref().unwrap()["retry_count"], 1);
    assert_eq!(bodies[0].metadata.as_ref().unwrap()["incomplete"], true);
}

#[tokio::test]
async fn abandoned_generation_prefers_complete_raw_request() {
    let (mut tracer, session) = tracer(1.0);
    let raw = serde_json::json!({"model": "provider-model", "messages": [{"content": "完整🙂".repeat(4096)}], "tools": [{"name": "lookup", "schema": {"type": "object"}}], "provider_extra": {"enabled": true}});
    tracer.on_turn_start("input");
    tracer.on_llm_start("main", 0, &[BaseMessage::human("fallback")], &[]);
    tracer.on_llm_request_payload("main", 0, Arc::new(raw.clone()));
    tracer
        .on_turn_end(TurnTelemetryOutcome::Completed)
        .await
        .unwrap();
    let events = session.events_snapshot();
    let input = events
        .iter()
        .find_map(|event| match event {
            IngestionEvent::GenerationCreate { body, .. } => body.input.as_ref(),
            _ => None,
        })
        .unwrap();
    assert_eq!(input, &raw);
}

fn start_subagent(tracer: &mut LangfuseTracer) -> (String, String) {
    tracer.set_main_agent_id("main".to_string());
    tracer.on_turn_start("input");
    let stage = tracer
        .on_stage_start_gated("main", Stage::Act, "turn")
        .unwrap();
    tracer.on_tool_start(
        "main",
        "invocation",
        "Agent",
        &serde_json::json!({"task": "complete task🙂", "options": {"full": true}}),
    );
    tracer.on_subagent_start("main", "child", "worker", false);
    (
        tracer.subagent.observation_id_of("child").unwrap(),
        stage.span_id,
    )
}

fn assert_completed_subagent(events: &[IngestionEvent], observation_id: &str, parent: &str) {
    let bodies: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            IngestionEvent::ObservationCreate { body, .. }
                if body.id.as_deref() == Some(observation_id) =>
            {
                Some(body)
            }
            _ => None,
        })
        .collect();
    assert_eq!(bodies.len(), 1);
    let body = bodies[0];
    assert_eq!(
        body.input,
        Some(serde_json::json!({"task": "complete task🙂", "options": {"full": true}}))
    );
    assert_eq!(body.parent_observation_id.as_deref(), Some(parent));
    assert_eq!(body.session_id.as_deref(), Some("session-observation"));
    let start = chrono::DateTime::parse_from_rfc3339(body.start_time.as_deref().unwrap()).unwrap();
    let end = chrono::DateTime::parse_from_rfc3339(body.end_time.as_deref().unwrap()).unwrap();
    assert!(start <= end);
    assert!(body.output.is_some());
    assert!(!events.iter().any(|event| matches!(
        event,
        IngestionEvent::ObservationUpdate { .. }
            | IngestionEvent::SpanUpdate { .. }
            | IngestionEvent::GenerationUpdate { .. }
            | IngestionEvent::SessionCreate { .. }
    )));
}

#[tokio::test]
async fn subagent_exports_complete_immutable_observation_only_after_both_signals() {
    let (mut tracer, session) = tracer(1.0);
    let (observation_id, parent) = start_subagent(&mut tracer);
    assert!(!session
        .events_snapshot()
        .iter()
        .any(|event| matches!(event, IngestionEvent::ObservationCreate { .. })));
    tracer.on_subagent_stop("main", "child", "complete result🙂", false);
    assert!(!session
        .events_snapshot()
        .iter()
        .any(|event| matches!(event, IngestionEvent::ObservationCreate { .. })));
    tracer.on_tool_end("main", "invocation", "tool-output", false);
    tracer.on_subagent_stop("main", "child", "duplicate-output", true);
    tracer
        .on_turn_end(TurnTelemetryOutcome::Completed)
        .await
        .unwrap();
    let events = session.events_snapshot();
    assert_completed_subagent(&events, &observation_id, &parent);
    let body = events
        .iter()
        .find_map(|event| match event {
            IngestionEvent::ObservationCreate { body, .. }
                if body.id.as_deref() == Some(observation_id.as_str()) =>
            {
                Some(body)
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(body.output.as_ref().unwrap()["text"], "complete result🙂");
}

#[tokio::test]
async fn failed_turn_closes_cached_subagent_once_with_full_start_record() {
    let (mut tracer, session) = tracer(1.0);
    let (observation_id, parent) = start_subagent(&mut tracer);
    tracer.on_tool_end("main", "invocation", "deferred tool output", false);
    tracer
        .on_turn_end(TurnTelemetryOutcome::Failed {
            failure: peri_acp_types::session::ExecutionFailure::internal("protected error"),
        })
        .await
        .unwrap();
    tracer.on_subagent_stop("main", "child", "late-stop", false);
    let events = session.events_snapshot();
    assert_completed_subagent(&events, &observation_id, &parent);
    let metadata = events
        .iter()
        .find_map(|event| match event {
            IngestionEvent::ObservationCreate { body, .. }
                if body.id.as_deref() == Some(observation_id.as_str()) =>
            {
                body.metadata.as_ref()
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(metadata["incomplete_reason"], "MissingStop");
}

#[tokio::test]
async fn cancelled_turn_closes_cached_subagent_once() {
    let (mut tracer, session) = tracer(1.0);
    let (observation_id, parent) = start_subagent(&mut tracer);
    tracer
        .on_turn_end(TurnTelemetryOutcome::Stopped {
            reason: peri_acp_types::command::PromptStopReason::Cancelled,
        })
        .await
        .unwrap();
    tracer.on_subagent_stop("main", "child", "late-stop", false);
    assert_completed_subagent(&session.events_snapshot(), &observation_id, &parent);
}

#[tokio::test]
async fn gated_generation_preserves_raw_request_through_late_subagent_join() {
    let (mut tracer, session) = tracer(1.0);
    tracer.set_main_agent_id("main".to_string());
    tracer.on_turn_start("input");
    let raw = serde_json::json!({"messages": [{"content": "native🙂".repeat(4096)}], "tools": [{"name": "native-tool"}], "provider_extra": {"enabled": true}});
    tracer.on_llm_start("child", 0, &[BaseMessage::human("fallback")], &[]);
    tracer.on_llm_request_payload("child", 0, Arc::new(raw.clone()));
    tracer.on_subagent_start("main", "child", "worker", false);
    tracer.on_stage_start_gated("main", Stage::Act, "turn");
    tracer.on_tool_start(
        "main",
        "invocation",
        "Agent",
        &serde_json::json!({"task": "work"}),
    );
    tracer.on_llm_end(
        "child",
        0,
        "provider-model",
        "provider",
        "answer",
        None,
        None,
    );
    tracer.on_tool_end("main", "invocation", "tool-output", false);
    tracer.on_subagent_stop("main", "child", "answer", false);
    tracer
        .on_turn_end(TurnTelemetryOutcome::Completed)
        .await
        .unwrap();
    let events = session.events_snapshot();
    let bodies: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            IngestionEvent::GenerationCreate { body, .. } => Some(body),
            _ => None,
        })
        .collect();
    assert_eq!(bodies.len(), 1);
    assert_eq!(bodies[0].input.as_ref(), Some(&raw));
}
