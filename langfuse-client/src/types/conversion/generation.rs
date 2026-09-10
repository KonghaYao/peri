use super::{
    append_common_obs_attrs, build_span_id, build_status, rfc3339_to_nano, OtelAttribute,
    OtelAttributeValue, OtelSpan,
};
use crate::types::GenerationBody;

pub(super) fn generation_create(body: &GenerationBody) -> OtelSpan {
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

pub(super) fn generation_update(body: &GenerationBody) -> OtelSpan {
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
