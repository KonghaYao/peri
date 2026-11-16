//! 统一流式 OTLP 编码：唯一的映射 emitter，只替换 sink（契约 §3.4）。
//!
//! [`write_otlp`]（写入）与 [`measure_otlp`]（计量）走同一条发射路径——信封、分隔、转义与
//! 字节计数都在 [`OtlpSink`] 内完成，因此两者不可能出现两套口径。冻结 input/output 由
//! [`json_string::write_json_string`] 分块嵌入，metadata/model_parameters/usage 等
//! [`serde_json::Value`] 由 [`json_string::EscapeWriter`] 边序列化边转义，都不物化整串，
//! 也不构造整棵 `Vec<OtelSpan>`。
//!
//! 模块路径按契约 §3.4 是 crate 根下的 `crate::encoding`（`lib.rs` 的 `pub(crate) mod
//! encoding` 挂载由集成流落地）。

pub(crate) mod json_string;
pub(crate) mod writer;

pub(crate) use writer::{BudgetedWriter, ExportContext, SinkFailure, STREAM_CHUNK_BYTES};

use std::io::Write;

use serde::Serialize;

use crate::error::LangfuseError;
use crate::payload::InputSnapshot;
use crate::snapshot::{InputSource, TelemetryEvent};
use crate::stable_json::StableJson;
use crate::types::conversion::EventView;

use json_string::{write_failure, write_quoted};

#[cfg(test)]
#[path = "stream_test.rs"]
mod tests;

mod span_view;

pub(crate) use span_view::FrozenInput;
pub(crate) use span_view::OtlpSink;
pub(crate) use span_view::SpanHead;
pub(crate) use span_view::SpanStatus;

#[cfg(test)]
pub(crate) use span_view::write_views;

pub(crate) use span_view::measure_otlp;
#[cfg(test)]
pub(crate) use span_view::write_otlp;
pub(crate) use span_view::write_otlp_counted;
