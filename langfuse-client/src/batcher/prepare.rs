//! 事件入队前的准备阶段：原文 / 最终编码双重计量与单条超限识别（契约 §3.4、§7 P3a）。
//!
//! 本模块不复制 OTLP 映射规则：最终编码计量经 [`OtlpMeter`] 端口注入，生产实现是 P2a 的
//! `encoding::measure_otlp`（契约 §3.4，集成流挂载 batcher.rs 时提供适配）。原文计量复用
//! `batcher::budget::event_bytes` 这一份权威实现。
//!
//! **不截断 input**：两类计量都按完整内容进行；超限只做识别与精确拒收，不做向下取整。
//! 快照未就绪（`InputSource::Preparing`）时不做任何计量——没有就绪来源就没有真实的最终
//! 编码，补一个「零内容」计量会把事件塞进超出限额的批次。
use std::slice;

use crate::error::LangfuseError;
use crate::snapshot::{InputSource, TelemetryEvent};

use super::budget::event_bytes;
use super::split::{oversize_reason, BatchLimits, EventMeasure, PreparedEvent};

/// OTLP 编码计量端口（契约 §3.4 `measure_otlp`）。
///
/// `limit` 允许实现超限即提前失败；`usize::MAX` 表示要求精确字节数（单条超限报告需要完整
/// 计量，不接受截断或取整）。生产接线为
/// `|events, limit| crate::encoding::measure_otlp(events, limit)`。
pub(crate) trait OtlpMeter: Send + Sync {
    /// 计量一批事件在最终 OTLP 正文中的编码字节数；未压缩，含信封、分隔与转义。
    fn measure(&self, events: &[TelemetryEvent], limit: usize) -> Result<u64, LangfuseError>;
}

impl<F> OtlpMeter for F
where
    F: Fn(&[TelemetryEvent], usize) -> Result<u64, LangfuseError> + Send + Sync,
{
    fn measure(&self, events: &[TelemetryEvent], limit: usize) -> Result<u64, LangfuseError> {
        self(events, limit)
    }
}

/// 准备一条事件：原文计量、最终编码计量，并按 [`BatchLimits`] 做单条超限识别。
///
/// 返回 [`PreparedEvent::Measured`] / [`PreparedEvent::Oversized`] / [`PreparedEvent::Waiting`]。
/// 计量本身失败（序列化错误、编码计量错误）按原样返回，不吞掉、不伪造 0 字节。
pub(crate) fn prepare_event(
    event: &TelemetryEvent,
    limits: &BatchLimits,
    meter: &dyn OtlpMeter,
) -> Result<PreparedEvent, LangfuseError> {
    if is_waiting(event) {
        return Ok(PreparedEvent::Waiting);
    }
    let measure = EventMeasure {
        raw_bytes: measure_raw(event)?,
        encoded_bytes: meter.measure(slice::from_ref(event), usize::MAX)?,
    };
    Ok(match oversize_reason(&measure, limits) {
        Some(reason) => PreparedEvent::Oversized { measure, reason },
        None => PreparedEvent::Measured(measure),
    })
}

/// input/output 是否仍有未收敛的快照准备任务。
fn is_waiting(event: &TelemetryEvent) -> bool {
    [event.input.as_ref(), event.output.as_ref()]
        .into_iter()
        .flatten()
        .any(|source| matches!(source, InputSource::Preparing(_)))
}

/// 冻结后事件对象自身的原文 JSON 字节数；不含被替代来源的正文。
///
/// 无上限计量：需要精确字节数用于单条超限报告；上限判定统一由 [`oversize_reason`] 完成。
fn measure_raw(event: &TelemetryEvent) -> Result<u64, LangfuseError> {
    Ok(event_bytes(&event.event, usize::MAX)? as u64)
}

#[cfg(test)]
#[path = "prepare_test.rs"]
mod tests;
