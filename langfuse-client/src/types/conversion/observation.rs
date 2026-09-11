use super::{
    append_common_obs_attrs, build_span_id, build_status, rfc3339_to_nano, OtelAttribute, OtelSpan,
};
use crate::types::{EventBody, ObservationBody, SpanBody};

pub(super) fn span_create(body: &SpanBody) -> OtelSpan {
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

    OtelSpan {
        trace_id: Some(trace_id),
        span_id: Some(span_id),
        parent_span_id,
        name: body.name.clone(),
        kind: Some(1),
        start_time_unix_nano: body.start_time.as_ref().and_then(|t| rfc3339_to_nano(t)),
        end_time_unix_nano: body.end_time.as_ref().and_then(|t| rfc3339_to_nano(t)),
        attributes: Some(attrs),
        status: build_status(body.level.as_ref(), body.status_message.as_deref()),
    }
}

pub(super) fn span_update(body: &SpanBody) -> OtelSpan {
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

    OtelSpan {
        trace_id: Some(trace_id),
        span_id: Some(span_id),
        parent_span_id,
        name: body.name.clone(),
        kind: Some(1),
        start_time_unix_nano: body.start_time.as_ref().and_then(|t| rfc3339_to_nano(t)),
        end_time_unix_nano: body.end_time.as_ref().and_then(|t| rfc3339_to_nano(t)),
        attributes: Some(attrs),
        status: build_status(body.level.as_ref(), body.status_message.as_deref()),
    }
}

pub(super) fn event_create(body: &EventBody) -> OtelSpan {
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

    OtelSpan {
        trace_id: Some(trace_id),
        span_id: Some(span_id),
        parent_span_id,
        name: body.name.clone(),
        kind: Some(1),
        start_time_unix_nano: body.start_time.as_ref().and_then(|t| rfc3339_to_nano(t)),
        end_time_unix_nano: None, // Events don't have end_time
        attributes: Some(attrs),
        status: build_status(body.level.as_ref(), body.status_message.as_deref()),
    }
}

pub(super) fn observation_create(body: &ObservationBody) -> OtelSpan {
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

    OtelSpan {
        trace_id: Some(trace_id),
        span_id: Some(span_id),
        parent_span_id,
        name: body.name.clone(),
        kind: Some(1),
        start_time_unix_nano: body.start_time.as_ref().and_then(|t| rfc3339_to_nano(t)),
        end_time_unix_nano: body.end_time.as_ref().and_then(|t| rfc3339_to_nano(t)),
        attributes: Some(attrs),
        status: build_status(body.level.as_ref(), body.status_message.as_deref()),
    }
}

pub(super) fn observation_update(body: &ObservationBody) -> OtelSpan {
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

    OtelSpan {
        trace_id: Some(trace_id),
        span_id: Some(span_id),
        parent_span_id,
        name: body.name.clone(),
        kind: Some(1),
        start_time_unix_nano: body.start_time.as_ref().and_then(|t| rfc3339_to_nano(t)),
        end_time_unix_nano: body.end_time.as_ref().and_then(|t| rfc3339_to_nano(t)),
        attributes: Some(attrs),
        status: build_status(body.level.as_ref(), body.status_message.as_deref()),
    }
}
