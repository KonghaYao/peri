mod generation;
mod metadata;
mod observation;
mod trace;
mod validation;

use super::{otlp::*, ObservationLevel};
use crate::error::LangfuseError;
use validation::{build_span, conversion_error};

// ─── IngestionEvent → OTLP Spans ───────────────────────

/// Convert a batch of IngestionEvents into an OTLP trace export request.
///
/// Create events export complete spans with validated identities and timing.
/// Updates and scores fail the batch rather than masquerading as new spans.
/// Session records and SDK logs are not spans and are filtered out.
pub(crate) fn ingestion_events_to_otel(
    events: &[super::IngestionEvent],
) -> Result<OtelTraceExportRequest, LangfuseError> {
    let mut spans: Vec<OtelSpan> = Vec::with_capacity(events.len());

    for event in events {
        spans.push(match event {
            super::IngestionEvent::TraceCreate {
                body, timestamp, ..
            } => trace::trace_create(body, timestamp)?,
            super::IngestionEvent::SpanCreate {
                body, timestamp, ..
            } => observation::span_create(body, timestamp)?,
            super::IngestionEvent::GenerationCreate {
                body, timestamp, ..
            } => generation::generation_create(body, timestamp)?,
            super::IngestionEvent::EventCreate {
                body, timestamp, ..
            } => observation::event_create(body, timestamp)?,
            super::IngestionEvent::ObservationCreate {
                body, timestamp, ..
            } => observation::observation_create(body, timestamp)?,
            super::IngestionEvent::SpanUpdate { .. }
            | super::IngestionEvent::GenerationUpdate { .. }
            | super::IngestionEvent::ObservationUpdate { .. } => {
                return Err(conversion_error(
                    "update events require a complete create event",
                ));
            }
            super::IngestionEvent::ScoreCreate { .. } => {
                return Err(conversion_error(
                    "score events require the dedicated score API",
                ));
            }
            super::IngestionEvent::SdkLog { .. }
            | super::IngestionEvent::SessionCreate { .. }
            | super::IngestionEvent::SessionUpdate { .. } => continue,
        });
    }

    Ok(OtelTraceExportRequest {
        resource_spans: vec![OtelResourceSpan {
            resource: Some(OtelResource {
                attributes: Some(vec![
                    OtelAttribute::string("service.name", "peri-agent"),
                    OtelAttribute::string("service.version", env!("CARGO_PKG_VERSION")),
                ]),
            }),
            scope_spans: Some(vec![OtelScopeSpan {
                scope: Some(OtelScope {
                    name: Some("langfuse-client".into()),
                    version: Some(env!("CARGO_PKG_VERSION").into()),
                    attributes: None,
                }),
                spans: Some(spans),
            }]),
        }],
    })
}

/// Helper: append common observation-level attributes
fn append_common_obs_attrs(
    attrs: &mut Vec<OtelAttribute>,
    input: Option<&serde_json::Value>,
    output: Option<&serde_json::Value>,
    metadata: Option<&serde_json::Value>,
    version: Option<&String>,
    environment: Option<&String>,
) {
    if let Some(ref input) = input {
        attrs.push(OtelAttribute::string(
            "langfuse.observation.input",
            input.to_string(),
        ));
    }
    if let Some(ref output) = output {
        attrs.push(OtelAttribute::string(
            "langfuse.observation.output",
            output.to_string(),
        ));
    }
    if let Some(metadata) = metadata {
        metadata::append_metadata_attrs(attrs, "langfuse.observation.metadata", metadata);
    }
    if let Some(v) = version {
        attrs.push(OtelAttribute::string("langfuse.version", v.as_str()));
    }
    if let Some(env) = environment {
        attrs.push(OtelAttribute::string("langfuse.environment", env.as_str()));
    }
}

/// Helper: build OTel status from Langfuse observation level + status message
fn build_status(
    level: Option<&ObservationLevel>,
    status_message: Option<&str>,
) -> Option<OtelStatus> {
    match level {
        Some(ObservationLevel::Error) => Some(OtelStatus {
            code: Some(2), // ERROR
            message: status_message.map(|s| s.to_string()),
        }),
        _ => Some(OtelStatus::default()),
    }
}

#[cfg(test)]
#[path = "conversion_test.rs"]
mod tests;
