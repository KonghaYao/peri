use super::{
    append_common_obs_attrs, build_span, build_status, LangfuseError, OtelAttribute, OtelSpan,
};
use crate::types::{EventBody, ObservationBody, SpanBody};

pub(super) fn span_create(body: &SpanBody, timestamp: &str) -> Result<OtelSpan, LangfuseError> {
    let span = build_span(
        body.trace_id.as_deref(),
        body.id.as_deref(),
        body.parent_observation_id.as_deref(),
        body.start_time.as_deref(),
        body.end_time.as_deref(),
        timestamp,
    )?;
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
    if let Some(ref message) = body.status_message {
        attrs.push(OtelAttribute::string(
            "langfuse.observation.status_message",
            message,
        ));
    }

    Ok(OtelSpan {
        name: body.name.clone().or_else(|| Some("span".into())),
        attributes: Some(attrs),
        status: build_status(body.level.as_ref(), body.status_message.as_deref()),
        ..span
    })
}

pub(super) fn event_create(body: &EventBody, timestamp: &str) -> Result<OtelSpan, LangfuseError> {
    let span = build_span(
        body.trace_id.as_deref(),
        body.id.as_deref(),
        body.parent_observation_id.as_deref(),
        body.start_time.as_deref(),
        None,
        timestamp,
    )?;
    let mut attrs = vec![OtelAttribute::string("langfuse.observation.type", "event")];
    append_common_obs_attrs(
        &mut attrs,
        body.input.as_ref(),
        body.output.as_ref(),
        body.metadata.as_ref(),
        body.version.as_ref(),
        body.environment.as_ref(),
    );

    Ok(OtelSpan {
        name: body.name.clone().or_else(|| Some("event".into())),
        attributes: Some(attrs),
        status: build_status(body.level.as_ref(), body.status_message.as_deref()),
        ..span
    })
}

pub(super) fn observation_create(
    body: &ObservationBody,
    timestamp: &str,
) -> Result<OtelSpan, LangfuseError> {
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
    let mut attrs = vec![OtelAttribute::string(
        "langfuse.observation.type",
        &obs_type,
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
    if let Some(ref message) = body.status_message {
        attrs.push(OtelAttribute::string(
            "langfuse.observation.status_message",
            message,
        ));
    }
    if let Some(ref session_id) = body.session_id {
        attrs.push(OtelAttribute::string("langfuse.session.id", session_id));
    }

    Ok(OtelSpan {
        name: body.name.clone().or(Some(obs_type)),
        attributes: Some(attrs),
        status: build_status(body.level.as_ref(), body.status_message.as_deref()),
        ..span
    })
}
