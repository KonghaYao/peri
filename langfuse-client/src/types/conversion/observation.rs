use super::{
    append_common_obs_attrs, build_span, build_status, span_head, CommonAttrs, EventView,
    LangfuseError, OtlpSink,
};
use crate::types::{EventBody, ObservationBody, SpanBody};

/// span-create → OTLP span；属性顺序：type、公共观测属性、session、status_message。
pub(super) fn span_create(
    body: &SpanBody,
    timestamp: &str,
    view: &EventView<'_>,
    sink: &mut OtlpSink<'_>,
) -> Result<(), LangfuseError> {
    let span = build_span(
        body.trace_id.as_deref(),
        body.id.as_deref(),
        body.parent_observation_id.as_deref(),
        body.start_time.as_deref(),
        body.end_time.as_deref(),
        timestamp,
    )?;
    sink.begin_span(&span_head(&span), body.name.as_deref().unwrap_or("span"))?;
    sink.attr_str("langfuse.observation.type", "span")?;
    append_common_obs_attrs(
        sink,
        view,
        CommonAttrs {
            input: body.input.as_ref(),
            output: body.output.as_ref(),
            metadata: body.metadata.as_ref(),
            version: body.version.as_ref(),
            environment: body.environment.as_ref(),
        },
    )?;
    if let Some(session_id) = body.session_id.as_deref() {
        sink.attr_str("langfuse.session.id", session_id)?;
    }
    if let Some(message) = body.status_message.as_deref() {
        sink.attr_str("langfuse.observation.status_message", message)?;
    }
    sink.end_span(build_status(
        body.level.as_ref(),
        body.status_message.as_deref(),
    ))
}

/// event-create → OTLP span；没有 end time 与会话属性（与基线一致）。
pub(super) fn event_create(
    body: &EventBody,
    timestamp: &str,
    view: &EventView<'_>,
    sink: &mut OtlpSink<'_>,
) -> Result<(), LangfuseError> {
    let span = build_span(
        body.trace_id.as_deref(),
        body.id.as_deref(),
        body.parent_observation_id.as_deref(),
        body.start_time.as_deref(),
        None,
        timestamp,
    )?;
    sink.begin_span(&span_head(&span), body.name.as_deref().unwrap_or("event"))?;
    sink.attr_str("langfuse.observation.type", "event")?;
    append_common_obs_attrs(
        sink,
        view,
        CommonAttrs {
            input: body.input.as_ref(),
            output: body.output.as_ref(),
            metadata: body.metadata.as_ref(),
            version: body.version.as_ref(),
            environment: body.environment.as_ref(),
        },
    )?;
    sink.end_span(build_status(
        body.level.as_ref(),
        body.status_message.as_deref(),
    ))
}

/// observation-create → OTLP span；类型名来自 body 的小写枚举名，缺省名回退到该类型。
pub(super) fn observation_create(
    body: &ObservationBody,
    timestamp: &str,
    view: &EventView<'_>,
    sink: &mut OtlpSink<'_>,
) -> Result<(), LangfuseError> {
    let span = build_span(
        body.trace_id.as_deref(),
        body.id.as_deref(),
        body.parent_observation_id.as_deref(),
        body.start_time.as_deref(),
        body.end_time.as_deref(),
        timestamp,
    )?;
    let obs_type = serde_json::to_value(&body.r#type)?;
    let obs_type = obs_type.as_str().unwrap_or("SPAN").to_ascii_lowercase();
    let name = body.name.as_deref().unwrap_or(&obs_type);
    sink.begin_span(&span_head(&span), name)?;
    sink.attr_str("langfuse.observation.type", &obs_type)?;
    append_common_obs_attrs(
        sink,
        view,
        CommonAttrs {
            input: body.input.as_ref(),
            output: body.output.as_ref(),
            metadata: body.metadata.as_ref(),
            version: body.version.as_ref(),
            environment: body.environment.as_ref(),
        },
    )?;
    if let Some(model) = body.model.as_deref() {
        sink.attr_str("langfuse.observation.model.name", model)?;
    }
    if let Some(message) = body.status_message.as_deref() {
        sink.attr_str("langfuse.observation.status_message", message)?;
    }
    if let Some(session_id) = body.session_id.as_deref() {
        sink.attr_str("langfuse.session.id", session_id)?;
    }
    sink.end_span(build_status(
        body.level.as_ref(),
        body.status_message.as_deref(),
    ))
}
