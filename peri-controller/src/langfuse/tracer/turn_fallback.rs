//! Turn 结束时按 parent 先于 child 的顺序收束遗留 stage 与 generation。

use super::event_builder::{
    new_uuid, now_rfc3339, try_add_or_warn_via_session, try_add_prepared_or_warn_via_session,
    VERSION,
};
use super::input_snapshot::mark_rejected_input;
use super::stages::StageHandle;
use super::turn_error::failure_error_class;
use super::{usage, LangfuseTracer};
use langfuse_client::types::ObservationLevel;
use langfuse_client::{GenerationBody, IngestionEvent};
use peri_acp_types::command::PromptStopReason;
use peri_acp_types::session::{ExecutionFailure, TurnTelemetryOutcome};
use peri_agent::agent::events::StageStatus;

pub(super) struct GenerationFallbackStatus<'a> {
    pub(super) error_class: String,
    pub(super) level: ObservationLevel,
    pub(super) failure: Option<&'a ExecutionFailure>,
}

impl<'a> GenerationFallbackStatus<'a> {
    pub(super) fn for_outcome(outcome: &'a TurnTelemetryOutcome) -> Self {
        match outcome {
            TurnTelemetryOutcome::Failed { failure } => GenerationFallbackStatus {
                error_class: failure_error_class(failure),
                level: ObservationLevel::Error,
                failure: Some(failure.as_ref()),
            },
            TurnTelemetryOutcome::Stopped {
                reason: PromptStopReason::Cancelled,
            } => GenerationFallbackStatus {
                error_class: "cancelled".to_string(),
                level: ObservationLevel::Warning,
                failure: None,
            },
            TurnTelemetryOutcome::Stopped {
                reason: PromptStopReason::MaxTokens,
            } => GenerationFallbackStatus {
                error_class: "max_tokens".to_string(),
                level: ObservationLevel::Warning,
                failure: None,
            },
            TurnTelemetryOutcome::Stopped {
                reason: PromptStopReason::MaxTurnRequests,
            } => GenerationFallbackStatus {
                error_class: "max_iterations".to_string(),
                level: ObservationLevel::Warning,
                failure: None,
            },
            TurnTelemetryOutcome::Stopped {
                reason: PromptStopReason::EndTurn,
            }
            | TurnTelemetryOutcome::Completed => GenerationFallbackStatus {
                error_class: "lifecycle_incomplete".to_string(),
                level: ObservationLevel::Error,
                failure: None,
            },
        }
    }
}

impl LangfuseTracer {
    pub(super) fn close_stage_parents(&mut self) {
        // 兜底闭合仍活跃/未领取的 stage parent，必须先于 generation/tool child
        // 入队，保证 FIFO ingest 在部分投递时不会留下可避免的孤儿 observation。
        for handle in self.stages.take_all_active() {
            self.emit_stage_span_close(&handle, StageStatus::Done, None);
        }
        let stale_replayed: Vec<StageHandle> = self
            .replayed_stage_handles
            .drain()
            .map(|(_, h)| h)
            .collect();
        for handle in stale_replayed {
            self.emit_stage_span_close(&handle, StageStatus::Done, None);
        }
    }

    pub(super) fn close_abandoned_generations(
        &mut self,
        fallback_status: &GenerationFallbackStatus<'_>,
        error_class: &str,
    ) {
        let failure = fallback_status.failure;
        for abandoned in self.generation.take_all_active() {
            let (parent_id, ownership_unresolved) = match self.llm_parent(&abandoned.agent_id) {
                Some(parent_id) => (parent_id, false),
                None => (self.agent_observation_id.clone(), true),
            };
            let terminal = abandoned.terminal.as_ref();
            let lifecycle_incomplete = terminal.is_none();
            let input = abandoned.input;
            let mut metadata = abandoned
                .retry_metadata
                .unwrap_or_else(|| serde_json::json!({}));
            if input.is_rejected() {
                // 输入冻结被拒：只留标记，不用旧 fallback 冒充实际请求（契约 §3.6）
                mark_rejected_input(&mut metadata);
            }
            if let Some(object) = metadata.as_object_mut() {
                object.insert(
                    "incomplete".to_string(),
                    serde_json::json!(lifecycle_incomplete),
                );
                object.insert(
                    "terminal_source".to_string(),
                    serde_json::json!(if lifecycle_incomplete {
                        "turn_end_fallback"
                    } else {
                        "llm_end_pending_ownership"
                    }),
                );
                object.insert(
                    "error_class".to_string(),
                    serde_json::json!(if lifecycle_incomplete {
                        error_class
                    } else {
                        "ownership_unresolved"
                    }),
                );
                object.insert(
                    "ownership_unresolved".to_string(),
                    serde_json::json!(ownership_unresolved),
                );
                if ownership_unresolved {
                    object.insert(
                        "original_agent_id".to_string(),
                        serde_json::json!(&abandoned.agent_id),
                    );
                }
                if let Some(terminal) = terminal {
                    object.insert("model".to_string(), serde_json::json!(&terminal.model));
                    if let Some(request_id) = &terminal.request_id {
                        object.insert("request_id".to_string(), serde_json::json!(request_id));
                    }
                }
                if let Some(status) = failure.and_then(|failure| failure.http_status) {
                    object.insert("http_status".to_string(), serde_json::json!(status));
                }
            }
            let (output, level, status_message, model, usage_details) = if let Some(terminal) =
                terminal
            {
                (
                    Some(safe_generation_output(&terminal.output)),
                    None,
                    Some("ownership_unresolved".to_string()),
                    Some(terminal.model.clone()),
                    terminal.usage.as_ref().map(usage::build_usage_details),
                )
            } else {
                (
                    Some(
                        failure
                            .map(|failure| super::turn_error::failure_output(failure, error_class))
                            .unwrap_or_else(|| serde_json::json!({"error_class": error_class})),
                    ),
                    Some(fallback_status.level.clone()),
                    Some(error_class.to_string()),
                    None,
                    None,
                )
            };
            let end_time = now_rfc3339();
            let body = GenerationBody {
                id: Some(abandoned.gen_id),
                trace_id: Some(self.trace_id.clone()),
                name: Some(format!("step-{}", abandoned.step)),
                start_time: Some(abandoned.start_time),
                end_time: Some(end_time.clone()),
                input: None,
                output,
                metadata: Some(metadata),
                level,
                status_message,
                model,
                usage_details,
                parent_observation_id: Some(parent_id),
                version: Some(VERSION.to_string()),
                session_id: Some(self.session_id.clone()),
                ..Default::default()
            };
            // 冻结被拒（`Rejected`）时事件不带来源，是直接事件，与 live 结束路径同一分派：
            // 携带来源才用 `try_add_prepared`，否则走同一准入的直接入口。
            let event = IngestionEvent::GenerationCreate {
                id: new_uuid(),
                timestamp: end_time,
                body,
                metadata: None,
            };
            match input.into_source() {
                Some(source) => try_add_prepared_or_warn_via_session(
                    &*self.session,
                    event,
                    Some(source),
                    None,
                    &self.trace_id,
                    "incomplete LLM GenerationCreate",
                ),
                None => try_add_or_warn_via_session(
                    &*self.session,
                    event,
                    &self.trace_id,
                    "incomplete LLM GenerationCreate",
                ),
            }
        }
    }
}

fn safe_generation_output(output: &str) -> serde_json::Value {
    if output.starts_with("ERROR: ") {
        serde_json::json!({"error_class": "provider_or_stream_failure", "text": output})
    } else if let Ok(value) = serde_json::from_str::<serde_json::Value>(output) {
        if value.is_object() {
            value
        } else {
            serde_json::json!({"text": output})
        }
    } else {
        serde_json::json!({"text": output})
    }
}
