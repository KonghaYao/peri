//! Turn 错误的稳定分类与安全观测；原始错误正文不得进入遥测。

use super::event_builder::{new_uuid, now_rfc3339, try_add_or_warn_via_session, VERSION};
use super::LangfuseTracer;
use langfuse_client::types::{EventBody, ObservationLevel, TraceBody};
use langfuse_client::{IngestionEvent, ObservationBody, ObservationType};
use peri_acp_types::session::{ExecutionFailure, ExecutionFailureKind};

impl LangfuseTracer {
    pub(super) fn emit_error_turn(
        &self,
        sampled: bool,
        failure: Option<&ExecutionFailure>,
        error_class: &str,
    ) {
        let turn_id = self.trace_id.clone();
        let error_out = failure
            .map(|failure| failure_output(failure, error_class))
            .unwrap_or_else(
                || serde_json::json!({"error_class": error_class, "error_schema_version": 3}),
            );

        if !sampled {
            // 未采样时创建合成 Trace 和最小 agent-run parent，保证 ErrorTurn
            // 不引用不存在的 observation，且 parent 先于 child 入队。
            let trace_body = TraceBody {
                id: Some(turn_id.clone()),
                name: Some(format!("turn {}", turn_id)),
                user_id: self.user_id.clone(),
                input: None,
                output: Some(error_out.clone()),
                session_id: Some(self.session_id.clone()),
                release: None,
                version: Some(VERSION.to_string()),
                public: None,
                metadata: Some(serde_json::json!({
                    "synthetic_error": true,
                    "error_class": error_class,
                    "error_schema_version": 2,
                })),
                tags: None,
                environment: None,
                timestamp: Some(now_rfc3339()),
            };
            let trace_event = IngestionEvent::TraceCreate {
                id: new_uuid(),
                timestamp: now_rfc3339(),
                body: trace_body,
                metadata: None,
            };
            try_add_or_warn_via_session(
                &*self.session,
                trace_event,
                &turn_id,
                "ErrorTurn synthetic TraceCreate",
            );
            let parent_time = now_rfc3339();
            let parent_body = ObservationBody {
                id: Some(self.agent_observation_id.clone()),
                trace_id: Some(turn_id.clone()),
                r#type: ObservationType::Agent,
                name: Some("agent-run-synthetic-error".to_string()),
                start_time: Some(parent_time.clone()),
                end_time: Some(parent_time.clone()),
                output: Some(error_out.clone()),
                parent_observation_id: Some(turn_id.clone()),
                version: Some(VERSION.to_string()),
                ..Default::default()
            };
            try_add_or_warn_via_session(
                &*self.session,
                IngestionEvent::ObservationCreate {
                    id: new_uuid(),
                    timestamp: parent_time,
                    body: parent_body,
                    metadata: None,
                },
                &turn_id,
                "ErrorTurn synthetic agent-run ObservationCreate",
            );
        }

        // Emit ErrorTurn Event(时点标记,非 span)
        let error_span_id = new_uuid();
        let event_body = EventBody {
            id: Some(error_span_id.clone()),
            trace_id: Some(turn_id.clone()),
            name: Some("ErrorTurn".to_string()),
            start_time: Some(now_rfc3339()),
            input: None,
            output: Some(error_out),
            metadata: Some(serde_json::json!({
                "is_synthetic": !sampled,
                "was_sampled": sampled,
                "turn_id": &turn_id,
                "error_class": error_class,
                "error_schema_version": 2,
            })),
            level: Some(ObservationLevel::Error),
            status_message: None,
            version: Some(VERSION.to_string()),
            environment: None,
            parent_observation_id: Some(self.agent_observation_id.clone()),
        };
        let event_event = IngestionEvent::EventCreate {
            id: new_uuid(),
            timestamp: now_rfc3339(),
            body: event_body,
            metadata: None,
        };
        try_add_or_warn_via_session(
            &*self.session,
            event_event,
            &self.trace_id,
            "ErrorTurn EventCreate",
        );
    }
}

pub(super) fn failure_error_class(failure: &ExecutionFailure) -> String {
    match (failure.kind, failure.http_status) {
        (ExecutionFailureKind::LlmHttp, Some(429)) => "rate_limit".to_string(),
        (ExecutionFailureKind::Internal, _) => "internal".to_string(),
        (ExecutionFailureKind::Llm, _) => "llm_failure".to_string(),
        (ExecutionFailureKind::LlmHttp, _) => "llm_http".to_string(),
    }
}

pub(super) fn failure_output(failure: &ExecutionFailure, error_class: &str) -> serde_json::Value {
    serde_json::json!({
        "error_class": error_class,
        "error_kind": failure.kind.wire_name(),
        "http_status": failure.http_status,
        "message": "The operation failed. Check protected logs for details.",
        "error_schema_version": 3,
    })
}
