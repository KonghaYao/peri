use super::{
    build_span, frozen_input, metadata, span_head, EventView, LangfuseError, OtlpSink, SpanStatus,
};
use crate::types::TraceBody;

/// trace-create → OTLP span；属性书写顺序即输出字节顺序（与旧 serde 基线逐字节一致）。
pub(super) fn trace_create(
    body: &TraceBody,
    timestamp: &str,
    view: &EventView<'_>,
    sink: &mut OtlpSink<'_>,
) -> Result<(), LangfuseError> {
    let span = build_span(
        body.id.as_deref(),
        body.id.as_deref(),
        None,
        body.timestamp.as_deref(),
        None,
        timestamp,
    )?;
    sink.begin_span(&span_head(&span), body.name.as_deref().unwrap_or("trace"))?;
    if let Some(session_id) = body.session_id.as_deref() {
        sink.attr_str("langfuse.session.id", session_id)?;
    }
    if let Some(user_id) = body.user_id.as_deref() {
        sink.attr_str("langfuse.user.id", user_id)?;
    }
    if let Some(release) = body.release.as_deref() {
        sink.attr_str("langfuse.release", release)?;
    }
    if let Some(version) = body.version.as_deref() {
        sink.attr_str("langfuse.version", version)?;
    }
    if let Some(environment) = body.environment.as_deref() {
        sink.attr_str("langfuse.environment", environment)?;
    }
    if let Some(tags) = body.tags.as_ref() {
        let joined = tags.join(",");
        sink.attr_str("langfuse.trace.tags", &joined)?;
    }
    if let Some(input) = frozen_input(body.input.as_ref(), view.input) {
        sink.attr_frozen("langfuse.trace.input", input)?;
    }
    if let Some(output) = frozen_input(body.output.as_ref(), view.output) {
        sink.attr_frozen("langfuse.trace.output", output)?;
    }
    if let Some(name) = body.name.as_deref() {
        sink.attr_str("langfuse.trace.name", name)?;
    }
    if let Some(metadata) = body.metadata.as_ref() {
        metadata::append_metadata_attrs(sink, "langfuse.trace.metadata", metadata)?;
    }
    sink.end_span(SpanStatus::Default)
}
