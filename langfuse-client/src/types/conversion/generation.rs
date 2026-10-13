use super::{
    append_common_obs_attrs, build_span, build_status, span_head, CommonAttrs, EventView,
    LangfuseError, OtlpSink,
};
use crate::types::GenerationBody;

/// generation-create → OTLP span；attribute 顺序即输出字节顺序（与旧 serde 基线一致）。
///
/// `model_parameters`/`usage`/`cost_details` 边序列化边转义嵌入 `stringValue`：
/// 不再先 `to_string` 造整串，`HashMap` 迭代顺序在两个方向上都是同一份。
pub(super) fn generation_create(
    body: &GenerationBody,
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
    sink.begin_span(
        &span_head(&span),
        body.name.as_deref().unwrap_or("generation"),
    )?;
    sink.attr_str("langfuse.observation.type", "generation")?;
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
    if let Some(params) = body.model_parameters.as_ref() {
        sink.attr_json("langfuse.observation.model.parameters", params)?;
    }
    if let Some(usage) = body.usage.as_ref() {
        sink.attr_json("langfuse.observation.usage_details", usage)?;
    }
    if let Some(usage_details) = body.usage_details.as_ref() {
        for (key, value) in usage_details {
            sink.attr_int(&format!("gen_ai.usage.{key}"), i64::from(*value))?;
        }
    }
    if let Some(cost_details) = body.cost_details.as_ref() {
        sink.attr_json("langfuse.observation.cost_details", cost_details)?;
    }
    if let Some(prompt_name) = body.prompt_name.as_deref() {
        sink.attr_str("langfuse.observation.prompt.name", prompt_name)?;
    }
    if let Some(completion_start) = body.completion_start_time.as_deref() {
        sink.attr_str(
            "langfuse.observation.completion_start_time",
            completion_start,
        )?;
    }
    if let Some(session_id) = body.session_id.as_deref() {
        sink.attr_str("langfuse.session.id", session_id)?;
    }
    sink.end_span(build_status(
        body.level.as_ref(),
        body.status_message.as_deref(),
    ))
}
