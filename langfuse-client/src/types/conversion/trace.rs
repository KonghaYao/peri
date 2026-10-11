use super::{build_span, metadata, LangfuseError, OtelAttribute, OtelSpan, OtelStatus};
use crate::types::TraceBody;

pub(super) fn trace_create(body: &TraceBody, timestamp: &str) -> Result<OtelSpan, LangfuseError> {
    let span = build_span(
        body.id.as_deref(),
        body.id.as_deref(),
        None,
        body.timestamp.as_deref(),
        None,
        timestamp,
    )?;
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
    if let Some(ref metadata) = body.metadata {
        metadata::append_metadata_attrs(&mut attrs, "langfuse.trace.metadata", metadata);
    }
    Ok(OtelSpan {
        name: body.name.clone().or_else(|| Some("trace".into())),
        attributes: Some(attrs),
        status: Some(OtelStatus::default()),
        ..span
    })
}
