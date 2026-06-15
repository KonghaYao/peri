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
        match event {
            super::IngestionEvent::TraceCreate { body, .. } => {
                let mut attrs = Vec::new();
                if let Some(ref session_id) = body.session_id {
                    attrs.push(OtelAttribute::string("langfuse.session.id", session_id));
                }
                if let Some(ref user_id) = body.user_id {
                    attrs.push(OtelAttribute::string("langfuse.user.id", user_id));
                }
                if let Some(ref release) = body.release {
                    attrs.push(OtelAttribute::string("langfuse.release", release));
                }
                if let Some(ref version) = body.version {
                    attrs.push(OtelAttribute::string("langfuse.version", version));
                }
                if let Some(ref env) = body.environment {
                    attrs.push(OtelAttribute::string("langfuse.environment", env));
                }
                if let Some(ref tags) = body.tags {
                    // Tags as comma-separated string
                    attrs.push(OtelAttribute::string("langfuse.trace.tags", tags.join(",")));
                }
                if let Some(ref input) = body.input {
                    attrs.push(OtelAttribute::string(
                        "langfuse.trace.input",
                        input.to_string(),
                    ));
                }
                if let Some(ref output) = body.output {
                    attrs.push(OtelAttribute::string(
                        "langfuse.trace.output",
                        output.to_string(),
                    ));
                }
                if let Some(ref name) = body.name {
                    attrs.push(OtelAttribute::string("langfuse.trace.name", name));
                }
                // trace.id becomes spanId for the root span; traceId is also set
                let span_id = build_span_id(body.id.as_deref().unwrap_or(""));
                spans.push(OtelSpan {
                    trace_id: Some(span_id.clone()),
                    span_id: Some(span_id),
                    parent_span_id: None,
                    name: body.name.clone().or_else(|| Some("trace".into())),
                    kind: Some(1), // INTERNAL
                    start_time_unix_nano: rfc3339_to_nano(event.event_timestamp()),
                    end_time_unix_nano: body.timestamp.as_ref().and_then(|t| rfc3339_to_nano(t)),
                    attributes: Some(attrs),
                    status: Some(OtelStatus::default()),
                });
            }
            super::IngestionEvent::SpanCreate { body, .. } => {
                let mut attrs = vec![OtelAttribute::string("langfuse.observation.type", "span")];
                append_common_obs_attrs(
                    &mut attrs,
                    body.input.as_ref(),
                    body.output.as_ref(),
                    body.metadata.as_ref(),
                    body.version.as_ref(),
                    body.environment.as_ref(),
                );
                if let Some(ref session_id) = body.session_id {
                    attrs.push(OtelAttribute::string("langfuse.session.id", session_id));
                }
                if let Some(ref msg) = body.status_message {
                    attrs.push(OtelAttribute::string(
                        "langfuse.observation.status_message",
                        msg,
                    ));
                }

                let trace_id = build_span_id(body.trace_id.as_deref().unwrap_or(""));
                let span_id = build_span_id(body.id.as_deref().unwrap_or(""));
                let parent_span_id = body.parent_observation_id.as_deref().map(build_span_id);

                spans.push(OtelSpan {
                    trace_id: Some(trace_id),
                    span_id: Some(span_id),
                    parent_span_id,
                    name: body.name.clone(),
                    kind: Some(1),
                    start_time_unix_nano: body.start_time.as_ref().and_then(|t| rfc3339_to_nano(t)),
                    end_time_unix_nano: body.end_time.as_ref().and_then(|t| rfc3339_to_nano(t)),
                    attributes: Some(attrs),
                    status: build_status(body.level.as_ref(), body.status_message.as_deref()),
                });
            }
            super::IngestionEvent::SpanUpdate { body, .. } => {
                // For updates, we still create a span — Langfuse OTel deduplicates by spanId
                let mut attrs = vec![OtelAttribute::string("langfuse.observation.type", "span")];
                if let Some(ref session_id) = body.session_id {
                    attrs.push(OtelAttribute::string("langfuse.session.id", session_id));
                }
                append_common_obs_attrs(
                    &mut attrs,
                    body.input.as_ref(),
                    body.output.as_ref(),
                    body.metadata.as_ref(),
                    body.version.as_ref(),
                    body.environment.as_ref(),
                );

                let trace_id = build_span_id(body.trace_id.as_deref().unwrap_or(""));
                let span_id = build_span_id(body.id.as_deref().unwrap_or(""));
                let parent_span_id = body.parent_observation_id.as_deref().map(build_span_id);

                spans.push(OtelSpan {
                    trace_id: Some(trace_id),
                    span_id: Some(span_id),
                    parent_span_id,
                    name: body.name.clone(),
                    kind: Some(1),
                    start_time_unix_nano: body.start_time.as_ref().and_then(|t| rfc3339_to_nano(t)),
                    end_time_unix_nano: body.end_time.as_ref().and_then(|t| rfc3339_to_nano(t)),
                    attributes: Some(attrs),
                    status: build_status(body.level.as_ref(), body.status_message.as_deref()),
                });
            }
            super::IngestionEvent::GenerationCreate { body, .. } => {
                let mut attrs = vec![OtelAttribute::string(
                    "langfuse.observation.type",
                    "generation",
                )];
                append_common_obs_attrs(
                    &mut attrs,
                    body.input.as_ref(),
                    body.output.as_ref(),
                    body.metadata.as_ref(),
                    body.version.as_ref(),
                    body.environment.as_ref(),
                );
                if let Some(ref model) = body.model {
                    attrs.push(OtelAttribute::string(
                        "langfuse.observation.model.name",
                        model,
                    ));
                }
                if let Some(ref params) = body.model_parameters {
                    if let Ok(json) = serde_json::to_string(params) {
                        attrs.push(OtelAttribute::string(
                            "langfuse.observation.model.parameters",
                            json,
                        ));
                    }
                }
                if let Some(ref usage) = body.usage {
                    if let Ok(json) = serde_json::to_string(usage) {
                        attrs.push(OtelAttribute::string(
                            "langfuse.observation.usage_details",
                            json,
                        ));
                    }
                }
                if let Some(ref usage_details) = body.usage_details {
                    for (k, v) in usage_details {
                        attrs.push(OtelAttribute::new(
                            format!("gen_ai.usage.{}", k),
                            OtelAttributeValue::int(*v as i64),
                        ));
                    }
                }
                if let Some(ref cost_details) = body.cost_details {
                    if let Ok(json) = serde_json::to_string(cost_details) {
                        attrs.push(OtelAttribute::string(
                            "langfuse.observation.cost_details",
                            json,
                        ));
                    }
                }
                if let Some(ref prompt_name) = body.prompt_name {
                    attrs.push(OtelAttribute::string(
                        "langfuse.observation.prompt.name",
                        prompt_name,
                    ));
                }
                if let Some(ref completion_start) = body.completion_start_time {
                    attrs.push(OtelAttribute::string(
                        "langfuse.observation.completion_start_time",
                        completion_start,
                    ));
                }
                if let Some(ref session_id) = body.session_id {
                    attrs.push(OtelAttribute::string("langfuse.session.id", session_id));
                }

                let trace_id = build_span_id(body.trace_id.as_deref().unwrap_or(""));
                let span_id = build_span_id(body.id.as_deref().unwrap_or(""));
                let parent_span_id = body.parent_observation_id.as_deref().map(build_span_id);

                spans.push(OtelSpan {
                    trace_id: Some(trace_id),
                    span_id: Some(span_id),
                    parent_span_id,
                    name: body.name.clone(),
                    kind: Some(1),
                    start_time_unix_nano: body.start_time.as_ref().and_then(|t| rfc3339_to_nano(t)),
                    end_time_unix_nano: body.end_time.as_ref().and_then(|t| rfc3339_to_nano(t)),
                    attributes: Some(attrs),
                    status: build_status(body.level.as_ref(), body.status_message.as_deref()),
                });
            }
            super::IngestionEvent::GenerationUpdate { body, .. } => {
                let mut attrs = vec![OtelAttribute::string(
                    "langfuse.observation.type",
                    "generation",
                )];
                append_common_obs_attrs(
                    &mut attrs,
                    body.input.as_ref(),
                    body.output.as_ref(),
                    body.metadata.as_ref(),
                    body.version.as_ref(),
                    body.environment.as_ref(),
                );
                if let Some(ref model) = body.model {
                    attrs.push(OtelAttribute::string(
                        "langfuse.observation.model.name",
                        model,
                    ));
                }
                if let Some(ref usage_details) = body.usage_details {
                    for (k, v) in usage_details {
                        attrs.push(OtelAttribute::new(
                            format!("gen_ai.usage.{}", k),
                            OtelAttributeValue::int(*v as i64),
                        ));
                    }
                }
                if let Some(ref session_id) = body.session_id {
                    attrs.push(OtelAttribute::string("langfuse.session.id", session_id));
                }

                let trace_id = build_span_id(body.trace_id.as_deref().unwrap_or(""));
                let span_id = build_span_id(body.id.as_deref().unwrap_or(""));
                let parent_span_id = body.parent_observation_id.as_deref().map(build_span_id);

                spans.push(OtelSpan {
                    trace_id: Some(trace_id),
                    span_id: Some(span_id),
                    parent_span_id,
                    name: body.name.clone(),
                    kind: Some(1),
                    start_time_unix_nano: body.start_time.as_ref().and_then(|t| rfc3339_to_nano(t)),
                    end_time_unix_nano: body.end_time.as_ref().and_then(|t| rfc3339_to_nano(t)),
                    attributes: Some(attrs),
                    status: build_status(body.level.as_ref(), body.status_message.as_deref()),
                });
            }
            super::IngestionEvent::EventCreate { body, .. } => {
                let mut attrs = vec![OtelAttribute::string("langfuse.observation.type", "event")];
                append_common_obs_attrs(
                    &mut attrs,
                    body.input.as_ref(),
                    body.output.as_ref(),
                    body.metadata.as_ref(),
                    body.version.as_ref(),
                    body.environment.as_ref(),
                );

                let trace_id = build_span_id(body.trace_id.as_deref().unwrap_or(""));
                let span_id = build_span_id(body.id.as_deref().unwrap_or(""));
                let parent_span_id = body.parent_observation_id.as_deref().map(build_span_id);

                spans.push(OtelSpan {
                    trace_id: Some(trace_id),
                    span_id: Some(span_id),
                    parent_span_id,
                    name: body.name.clone(),
                    kind: Some(1),
                    start_time_unix_nano: body.start_time.as_ref().and_then(|t| rfc3339_to_nano(t)),
                    end_time_unix_nano: None, // Events don't have end_time
                    attributes: Some(attrs),
                    status: build_status(body.level.as_ref(), body.status_message.as_deref()),
                });
            }
            super::IngestionEvent::ObservationCreate { body, .. } => {
                let obs_type_str = serde_json::to_value(&body.r#type)
                    .ok()
                    .and_then(|v| v.as_str().map(|s| s.to_lowercase()))
                    .unwrap_or_else(|| "span".to_string());
                let mut attrs = vec![OtelAttribute::string(
                    "langfuse.observation.type",
                    &obs_type_str,
                )];
                append_common_obs_attrs(
                    &mut attrs,
                    body.input.as_ref(),
                    body.output.as_ref(),
                    body.metadata.as_ref(),
                    body.version.as_ref(),
                    body.environment.as_ref(),
                );
                if let Some(ref model) = body.model {
                    attrs.push(OtelAttribute::string(
                        "langfuse.observation.model.name",
                        model,
                    ));
                }
                if let Some(ref msg) = body.status_message {
                    attrs.push(OtelAttribute::string(
                        "langfuse.observation.status_message",
                        msg,
                    ));
                }
                if let Some(ref session_id) = body.session_id {
                    attrs.push(OtelAttribute::string("langfuse.session.id", session_id));
                }

                let trace_id = build_span_id(body.trace_id.as_deref().unwrap_or(""));
                let span_id = build_span_id(body.id.as_deref().unwrap_or(""));
                let parent_span_id = body.parent_observation_id.as_deref().map(build_span_id);

                spans.push(OtelSpan {
                    trace_id: Some(trace_id),
                    span_id: Some(span_id),
                    parent_span_id,
                    name: body.name.clone(),
                    kind: Some(1),
                    start_time_unix_nano: body.start_time.as_ref().and_then(|t| rfc3339_to_nano(t)),
                    end_time_unix_nano: body.end_time.as_ref().and_then(|t| rfc3339_to_nano(t)),
                    attributes: Some(attrs),
                    status: build_status(body.level.as_ref(), body.status_message.as_deref()),
                });
            }
            super::IngestionEvent::ObservationUpdate { body, .. } => {
                let obs_type_str = serde_json::to_value(&body.r#type)
                    .ok()
                    .and_then(|v| v.as_str().map(|s| s.to_lowercase()))
                    .unwrap_or_else(|| "span".to_string());
                let mut attrs = vec![OtelAttribute::string(
                    "langfuse.observation.type",
                    &obs_type_str,
                )];
                append_common_obs_attrs(
                    &mut attrs,
                    body.input.as_ref(),
                    body.output.as_ref(),
                    body.metadata.as_ref(),
                    body.version.as_ref(),
                    body.environment.as_ref(),
                );
                if let Some(ref session_id) = body.session_id {
                    attrs.push(OtelAttribute::string("langfuse.session.id", session_id));
                }

                let trace_id = build_span_id(body.trace_id.as_deref().unwrap_or(""));
                let span_id = build_span_id(body.id.as_deref().unwrap_or(""));
                let parent_span_id = body.parent_observation_id.as_deref().map(build_span_id);

                spans.push(OtelSpan {
                    trace_id: Some(trace_id),
                    span_id: Some(span_id),
                    parent_span_id,
                    name: body.name.clone(),
                    kind: Some(1),
                    start_time_unix_nano: body.start_time.as_ref().and_then(|t| rfc3339_to_nano(t)),
                    end_time_unix_nano: body.end_time.as_ref().and_then(|t| rfc3339_to_nano(t)),
                    attributes: Some(attrs),
                    status: build_status(body.level.as_ref(), body.status_message.as_deref()),
                });
            }
            super::IngestionEvent::ScoreCreate { body, .. } => {
                // Scores are attached via attributes on the trace
                let mut attrs = vec![];
                attrs.push(OtelAttribute::string("langfuse.score.name", &body.name));
                attrs.push(OtelAttribute::new(
                    "langfuse.score.value",
                    match &body.value {
                        serde_json::Value::Number(n) => {
                            if let Some(f) = n.as_f64() {
                                OtelAttributeValue {
                                    string_value: None,
                                    int_value: None,
                                    double_value: Some(f),
                                    bool_value: None,
                                }
                            } else if let Some(i) = n.as_i64() {
                                OtelAttributeValue::int(i)
                            } else {
                                OtelAttributeValue::string(body.value.to_string())
                            }
                        }
                        serde_json::Value::Bool(b) => OtelAttributeValue::bool(*b),
                        _ => OtelAttributeValue::string(body.value.to_string()),
                    },
                ));
                if let Some(ref trace_id) = body.trace_id {
                    attrs.push(OtelAttribute::string("langfuse.trace.id", trace_id));
                }
                if let Some(ref obs_id) = body.observation_id {
                    attrs.push(OtelAttribute::string("langfuse.observation.id", obs_id));
                }

                let span_id = build_span_id(body.id.as_deref().unwrap_or(""));
                let trace_id = build_span_id(body.trace_id.as_deref().unwrap_or(""));

                spans.push(OtelSpan {
                    trace_id: Some(trace_id),
                    span_id: Some(span_id),
                    parent_span_id: body.observation_id.as_deref().map(build_span_id),
                    name: Some(format!("score:{}", body.name)),
                    kind: Some(1),
                    start_time_unix_nano: None,
                    end_time_unix_nano: None,
                    attributes: Some(attrs),
                    status: Some(OtelStatus::default()),
                });
            }
            super::IngestionEvent::SdkLog { body, .. } => {
                // SDK logs are metadata; we skip them in OTLP as there's no natural mapping
                let attrs = vec![OtelAttribute::string(
                    "langfuse.sdk.log",
                    body.log.to_string(),
                )];
                spans.push(OtelSpan {
                    trace_id: None,
                    span_id: None,
                    parent_span_id: None,
                    name: Some("sdk-log".into()),
                    kind: Some(1),
                    start_time_unix_nano: None,
                    end_time_unix_nano: None,
                    attributes: Some(attrs),
                    status: Some(OtelStatus::default()),
                });
            }
        }
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
