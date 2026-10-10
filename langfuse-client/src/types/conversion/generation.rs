use super::{
    append_common_obs_attrs, build_span, build_status, LangfuseError, OtelAttribute,
    OtelAttributeValue, OtelSpan,
};
use crate::types::GenerationBody;

pub(super) fn generation_create(
    body: &GenerationBody,
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
        for (key, value) in usage_details {
            attrs.push(OtelAttribute::new(
                format!("gen_ai.usage.{}", key),
                OtelAttributeValue::int(*value as i64),
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

    Ok(OtelSpan {
        name: body.name.clone().or_else(|| Some("generation".into())),
        attributes: Some(attrs),
        status: build_status(body.level.as_ref(), body.status_message.as_deref()),
        ..span
    })
}
