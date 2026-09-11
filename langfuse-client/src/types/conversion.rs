mod generation;
mod metadata;
mod observation;
mod trace;

use super::{otlp::*, ObservationLevel};

// ─── IngestionEvent → OTLP Spans ───────────────────────

/// Strip dashes from a Langfuse observation/trace ID to derive an
/// OTel-compatible span/trace ID.
///
/// OTel span/trace IDs must be lowercase hex without dashes; Langfuse IDs
/// are UUID-like strings with dashes. Caller controls Optionality:
/// `id.as_deref().unwrap_or("")` for required IDs, `.map(build_span_id)`
/// for optional parent IDs.
fn build_span_id(id: &str) -> String {
    id.replace('-', "")
}

/// Convert a batch of IngestionEvents into an OTLP trace export request.
///
/// Mapping strategy:
/// - TraceCreate → root span with `langfuse.observation.type` = omitted (root is trace)
/// - SpanCreate → span with `langfuse.observation.type` = "span"
/// - GenerationCreate → span with `langfuse.observation.type` = "generation" + model/usage attrs
/// - ObservationCreate → span with `langfuse.observation.type` from body.type
/// - EventCreate → span with `langfuse.observation.type` = "event"
/// - ScoreCreate → span with `langfuse.observation.type` = omitted (attached to trace)
/// - Others → span with basic attributes
pub(crate) fn ingestion_events_to_otel(events: &[super::IngestionEvent]) -> OtelTraceExportRequest {
    let mut spans: Vec<OtelSpan> = Vec::with_capacity(events.len());

    for event in events {
        spans.push(match event {
            super::IngestionEvent::TraceCreate {
                body, timestamp, ..
            } => trace::trace_create(body, timestamp),
            super::IngestionEvent::SpanCreate { body, .. } => observation::span_create(body),
            super::IngestionEvent::SpanUpdate { body, .. } => observation::span_update(body),
            super::IngestionEvent::GenerationCreate { body, .. } => {
                generation::generation_create(body)
            }
            super::IngestionEvent::GenerationUpdate { body, .. } => {
                generation::generation_update(body)
            }
            super::IngestionEvent::EventCreate { body, .. } => observation::event_create(body),
            super::IngestionEvent::ObservationCreate { body, .. } => {
                observation::observation_create(body)
            }
            super::IngestionEvent::ObservationUpdate { body, .. } => {
                observation::observation_update(body)
            }
            super::IngestionEvent::ScoreCreate { body, .. } => metadata::score_create(body),
            super::IngestionEvent::SdkLog { body, .. } => metadata::sdk_log(body),
            super::IngestionEvent::SessionCreate { body, .. } => metadata::session_create(body),
            super::IngestionEvent::SessionUpdate { body, .. } => metadata::session_update(body),
        });
    }

    OtelTraceExportRequest {
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
    }
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
    if let Some(ref metadata) = metadata {
        if let Ok(json) = serde_json::to_string(metadata) {
            attrs.push(OtelAttribute::string("langfuse.observation.metadata", json));
        }
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

/// Convert RFC 3339 timestamp to Unix nanoseconds string
fn rfc3339_to_nano(rfc3339: &str) -> Option<String> {
    // Parse common RFC 3339 formats
    let ts = chrono::DateTime::parse_from_rfc3339(rfc3339).ok()?;
    Some(ts.timestamp_nanos_opt()?.to_string())
}

#[cfg(test)]
#[path = "conversion_test.rs"]
mod tests;
