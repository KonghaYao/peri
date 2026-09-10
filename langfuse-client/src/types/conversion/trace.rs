use super::{build_span_id, rfc3339_to_nano, OtelAttribute, OtelSpan, OtelStatus};
use crate::types::TraceBody;

pub(super) fn trace_create(body: &TraceBody, timestamp: &str) -> OtelSpan {
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
    OtelSpan {
        trace_id: Some(span_id.clone()),
        span_id: Some(span_id),
        parent_span_id: None,
        name: body.name.clone().or_else(|| Some("trace".into())),
        kind: Some(1), // INTERNAL
        start_time_unix_nano: rfc3339_to_nano(timestamp),
        end_time_unix_nano: body.timestamp.as_ref().and_then(|t| rfc3339_to_nano(t)),
        attributes: Some(attrs),
        status: Some(OtelStatus::default()),
    }
}
