//! IngestionEvent → OTLP 映射（契约 §3.4、§5.3/§5.4）。
//!
//! 全 crate 只有这一套 ID/时间/字段映射，改为按来源视图流式输出：编码
//! （`encoding::write_otlp`/`measure_otlp`）与需要物化的回归断言共用同一份 emitter，
//! 只替换 sink，不存在第二份业务分支。
//!
//! 属性书写顺序即输出字节顺序（与旧 serde 基线逐字节一致）：大 input/output 不再经
//! `Value::to_string`，冻结来源由 `json_string` 分块嵌入。

mod generation;
mod metadata;
mod observation;
mod trace;
mod validation;

use serde_json::Value;

use super::{otlp::*, IngestionEvent, ObservationLevel};
#[cfg(test)]
use crate::encoding::write_views;
use crate::encoding::{FrozenInput, OtlpSink, SpanHead, SpanStatus};
use crate::error::LangfuseError;
use crate::snapshot::{InputSource, TelemetryEvent};
use validation::{build_span, conversion_error};

/// 一条事件的映射输入：事件原文加上已冻结的 input/output 来源。
pub(crate) struct EventView<'a> {
    pub(crate) event: &'a IngestionEvent,
    pub(crate) input: Option<&'a InputSource>,
    pub(crate) output: Option<&'a InputSource>,
}

impl<'a> EventView<'a> {
    /// 借用遥测事件的事件原文与冻结来源（不克隆正文）。
    pub(crate) fn from_telemetry(event: &'a TelemetryEvent) -> Self {
        Self {
            event: &event.event,
            input: event.input.as_ref(),
            output: event.output.as_ref(),
        }
    }

    /// 只有事件原文、没有替代来源的视图（结构化回归物化使用，见 [`ingestion_events_to_otel`]）。
    #[cfg(test)]
    pub(crate) fn bare(event: &'a IngestionEvent) -> Self {
        Self {
            event,
            input: None,
            output: None,
        }
    }
}

/// 校验后的身份与时间转成 sink 视图；`OtelSpan` 只在 `types` 内可见，sink 不再依赖它。
pub(crate) fn span_head(span: &OtelSpan) -> SpanHead<'_> {
    SpanHead {
        trace_id: span.trace_id.as_deref().unwrap_or_default(),
        span_id: span.span_id.as_deref().unwrap_or_default(),
        parent_span_id: span.parent_span_id.as_deref(),
        kind: span.kind.unwrap_or(1),
        start_time_unix_nano: span.start_time_unix_nano.as_deref().unwrap_or_default(),
        end_time_unix_nano: span.end_time_unix_nano.as_deref().unwrap_or_default(),
    }
}

/// 观测类事件的公共属性来源；书写顺序即输出字节顺序。
pub(crate) struct CommonAttrs<'a> {
    pub(crate) input: Option<&'a Value>,
    pub(crate) output: Option<&'a Value>,
    pub(crate) metadata: Option<&'a Value>,
    pub(crate) version: Option<&'a String>,
    pub(crate) environment: Option<&'a String>,
}

/// 把一条事件写成 OTLP span；SDK log 与会话记录不产生 span，Update/Score 使整批失败。
pub(crate) fn write_span(
    view: &EventView<'_>,
    sink: &mut OtlpSink<'_>,
) -> Result<(), LangfuseError> {
    let timestamp = view.event.event_timestamp();
    match view.event {
        IngestionEvent::TraceCreate { body, .. } => {
            trace::trace_create(body, timestamp, view, sink)
        }
        IngestionEvent::SpanCreate { body, .. } => {
            observation::span_create(body, timestamp, view, sink)
        }
        IngestionEvent::GenerationCreate { body, .. } => {
            generation::generation_create(body, timestamp, view, sink)
        }
        IngestionEvent::EventCreate { body, .. } => {
            observation::event_create(body, timestamp, view, sink)
        }
        IngestionEvent::ObservationCreate { body, .. } => {
            observation::observation_create(body, timestamp, view, sink)
        }
        IngestionEvent::SpanUpdate { .. }
        | IngestionEvent::GenerationUpdate { .. }
        | IngestionEvent::ObservationUpdate { .. } => Err(conversion_error(
            "update events require a complete create event",
        )),
        IngestionEvent::ScoreCreate { .. } => Err(conversion_error(
            "score events require the dedicated score API",
        )),
        IngestionEvent::SdkLog { .. }
        | IngestionEvent::SessionCreate { .. }
        | IngestionEvent::SessionUpdate { .. } => Ok(()),
    }
}

/// 结构化物化入口：驱动同一 emitter 再解析回 OTLP 结构，供既有回归断言使用；
/// 生产编码走 [`crate::encoding::write_otlp`]，本入口不构成第二条业务分支。
///
/// Create 事件导出完整 span；Update 与 Score 使整批失败而不是伪装成新 span；SdkLog 与
/// 会话记录不是 span，会被过滤掉。
#[cfg(test)]
pub(crate) fn ingestion_events_to_otel(
    events: &[IngestionEvent],
) -> Result<OtelTraceExportRequest, LangfuseError> {
    let mut body = Vec::new();
    write_views(events.iter().map(EventView::bare), &mut body)?;
    serde_json::from_slice(&body).map_err(LangfuseError::from)
}

/// input/output 来源选择：冻结来源优先，未冻结时退回 body 内联值（两者不会同时存在）。
pub(crate) fn frozen_input<'a>(
    body: Option<&'a Value>,
    source: Option<&'a InputSource>,
) -> Option<FrozenInput<'a>> {
    source
        .map(FrozenInput::Source)
        .or_else(|| body.map(FrozenInput::Value))
}

/// 公共观测属性：input、output、metadata（含逐字段展开）、version、environment。
pub(crate) fn append_common_obs_attrs(
    sink: &mut OtlpSink<'_>,
    view: &EventView<'_>,
    common: CommonAttrs<'_>,
) -> Result<(), LangfuseError> {
    if let Some(input) = frozen_input(common.input, view.input) {
        sink.attr_frozen("langfuse.observation.input", input)?;
    }
    if let Some(output) = frozen_input(common.output, view.output) {
        sink.attr_frozen("langfuse.observation.output", output)?;
    }
    if let Some(metadata) = common.metadata {
        metadata::append_metadata_attrs(sink, "langfuse.observation.metadata", metadata)?;
    }
    if let Some(version) = common.version {
        sink.attr_str("langfuse.version", version)?;
    }
    if let Some(environment) = common.environment {
        sink.attr_str("langfuse.environment", environment)?;
    }
    Ok(())
}

/// 观测等级映射为 OTLP 状态：只有 Error 带 `code=2` 与可选消息。
pub(crate) fn build_status<'a>(
    level: Option<&ObservationLevel>,
    status_message: Option<&'a str>,
) -> SpanStatus<'a> {
    match level {
        Some(ObservationLevel::Error) => SpanStatus::Error {
            message: status_message,
        },
        _ => SpanStatus::Default,
    }
}

#[cfg(test)]
#[path = "conversion_test.rs"]
mod tests;
