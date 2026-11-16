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

use super::IngestionEvent;
use super::ObservationLevel;
#[cfg(test)]
use crate::encoding::write_views;
use crate::encoding::{FrozenInput, OtlpSink, SpanHead, SpanStatus};
use crate::error::LangfuseError;
use crate::snapshot::{InputSource, TelemetryEvent};
use validation::{build_span, conversion_error};

#[cfg(test)]
#[path = "conversion_test.rs"]
mod tests;

#[path = "conversion/event_view.rs"]
mod event_view;
pub(crate) use event_view::append_common_obs_attrs;
pub(crate) use event_view::build_status;
pub(crate) use event_view::frozen_input;
#[cfg(test)]
pub(crate) use event_view::ingestion_events_to_otel;
pub(crate) use event_view::span_head;
pub(crate) use event_view::write_span;
pub(crate) use event_view::CommonAttrs;
pub(crate) use event_view::EventView;
