//! 按事件边界拆批：已计量事件序列 → 可发送批次与单条拒收项（契约 §3.4、§7 P3a）。
//!
//! 本模块是纯函数与纯数据结构：不做 IO、不读文件、不改写事件。**不截断 input、不伪造
//! 分片 span**：批次只覆盖完整事件；单条即超限的事件精确拒收，其原文与编码计量保持完整，
//! 不向下取整、不切割成多条「分片」事件冒充实测正文。
//!
//! 参与拆批的是两个按事件可加的量：冻结后事件原文（`max_event_bytes` / `max_batch_bytes`）
//! 与最终 OTLP 正文（`max_encoded_bytes`）。压缩后的 wire 上限**不是**拆批输入：gzip 字节
//! 不是按事件可加的，由编码 writer 在写入时强制（契约 §3.4），本模块不据此伪造批次边界。
use std::ops::Range;

use crate::error::LangfuseError;

/// 拆批与单条识别共用的限额；单条事件是最小批次，故两处规则完全一致。
///
/// 由部署配置映射：`max_event_bytes` / `max_batch_bytes` / `max_events` 取
/// `BatcherConfig` 同名字段，`max_encoded_bytes` 取最终 OTLP 正文上限
/// （`ExportConfig::max_request_bytes`）。限额自洽性由 [`split`] 校验。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BatchLimits {
    /// 单条事件原文上限；超出即单条拒收，不可通过拆批消化。
    pub max_event_bytes: usize,
    /// 单批原文聚合上限；必须覆盖单条原文上限。
    pub max_batch_bytes: usize,
    /// 单批最终编码（未压缩 OTLP 正文）上限；单条即等价单条编码上限。
    pub max_encoded_bytes: usize,
    /// 单批事件条数上限。
    pub max_events: usize,
}

/// 单条事件的双重计量（由 [`super::prepare`] 产出）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EventMeasure {
    /// 冻结后事件对象自身的 serde-JSON 字节数；不含被替代来源的正文。
    pub raw_bytes: u64,
    /// 该事件单独成批时的最终 OTLP 正文字节数（含信封与转义，未压缩）。
    pub encoded_bytes: u64,
}

/// 单条超限的原因与限额；错误映射见 [`RejectedEvent::error`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OversizeReason {
    /// 原文超过单事件原文上限（`max_event_bytes`）。
    Raw { limit_bytes: usize },
    /// 最终编码超过单批编码上限（`max_encoded_bytes`）：单条即不可拆。
    Encoded { limit_bytes: usize },
}

/// 单条事件的准备结果；三类都显式表达，不做隐式丢弃或补零计量。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreparedEvent {
    /// 已计量且单条即可成批。
    Measured(EventMeasure),
    /// 单条即在原文或编码维度超限：不能通过拆批消化。
    Oversized {
        measure: EventMeasure,
        reason: OversizeReason,
    },
    /// input/output 快照尚未收敛（`InputSource::Preparing`）：最终编码无法计量。
    Waiting,
}

/// 单条事件的超限判定；[`super::prepare`] 与 [`split`] 共用这一份权威规则。
///
/// 先判编码维度（不可发送的硬约束），再判原文维度；两者都超限时报告编码超限。
pub(crate) fn oversize_reason(
    measure: &EventMeasure,
    limits: &BatchLimits,
) -> Option<OversizeReason> {
    if measure.encoded_bytes > limits.max_encoded_bytes as u64 {
        return Some(OversizeReason::Encoded {
            limit_bytes: limits.max_encoded_bytes,
        });
    }
    if measure.raw_bytes > limits.max_event_bytes as u64 {
        return Some(OversizeReason::Raw {
            limit_bytes: limits.max_event_bytes,
        });
    }
    None
}

/// 被精确拒收的单条事件；计量保持完整，便于按事件计入失败账本。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RejectedEvent {
    /// 输入序列中的下标。
    pub index: usize,
    /// 完整（未截断）的双重计量。
    pub measure: EventMeasure,
    pub reason: OversizeReason,
}

impl RejectedEvent {
    /// 映射为既有错误变体：编码超限 → `SingleSpanTooLarge`，原文超限 → `PayloadTooLarge`。
    pub(crate) fn error(&self) -> LangfuseError {
        match self.reason {
            OversizeReason::Encoded { limit_bytes } => LangfuseError::SingleSpanTooLarge {
                limit_bytes,
                encoded_bytes: self.measure.encoded_bytes,
            },
            OversizeReason::Raw { limit_bytes } => LangfuseError::PayloadTooLarge { limit_bytes },
        }
    }
}

/// 一个可发送批次：输入序列中的连续区间，只含已计量且未超限的完整事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BatchWindow {
    /// 输入序列下标区间；区间内元素全部属于本批次。
    pub range: Range<usize>,
    /// 原文合计（Σ `raw_bytes`），与既有按原文聚合的口径一致。
    pub raw_bytes: u64,
    /// 编码上界（Σ `encoded_bytes`）。单条计量在独立正文中完成，n=1 时即精确值；
    /// 同一 emitter 的元素编码与邻居无关，批次正文 = 信封 + Σ元素 + 分隔符，故该和不低于
    /// 实际正文，据此拆批不会产出超限批次。
    pub encoded_bytes: u64,
}

impl BatchWindow {
    /// 本批次的事件条数。
    pub(crate) fn len(&self) -> usize {
        self.range.len()
    }

    /// 是否为空批次；拆分不产出空批次，仅用于调用方断言（`batcher/split_test.rs`）。
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.range.is_empty()
    }
}

/// 拆批结果：批次 + 单条拒收 + 未就绪停止点。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SplitPlan {
    /// 可发送批次；按 FIFO 顺序、互不重叠，区间之间只可能是被拒收或未就绪的事件。
    pub batches: Vec<BatchWindow>,
    /// 单条超限事件；按输入顺序，不进入任何批次。
    pub rejected: Vec<RejectedEvent>,
    /// 未就绪停止点：`Some(index)` 表示 `index`（含）起的输入本轮未规划，保持 FIFO。
    pub deferred: Option<usize>,
}

impl SplitPlan {
    /// 本轮已规划的输入条数：批次与拒收事件覆盖 `[0, planned_len)`；消费方为拆分回归断言。
    #[cfg(test)]
    pub(crate) fn planned_len(&self, input_len: usize) -> usize {
        self.deferred.unwrap_or(input_len)
    }

    /// 批次与拒收项覆盖的事件条数；与 [`Self::planned_len`] 相等即无丢事件。
    #[cfg(test)]
    pub(crate) fn accounted_len(&self) -> usize {
        self.batches.iter().map(BatchWindow::len).sum::<usize>() + self.rejected.len()
    }
}

/// 按事件边界把已计量事件序列切成可发送批次。
///
/// 规则：顺序不变；非超限事件连续累积，直到条数或两类字节上限之一无法继续；遇到单条超限
/// 事件即精确拒收并继续规划其后的事件；遇到未就绪事件即停止规划（其后输入保持 FIFO）。
pub(crate) fn split(
    prepared: &[PreparedEvent],
    limits: &BatchLimits,
) -> Result<SplitPlan, LangfuseError> {
    validate_limits(limits)?;
    let mut plan = SplitPlan {
        batches: Vec::new(),
        rejected: Vec::new(),
        deferred: None,
    };
    let mut current: Option<BatchWindow> = None;
    for (index, item) in prepared.iter().enumerate() {
        let measure = match item {
            PreparedEvent::Waiting => {
                close(&mut current, &mut plan);
                plan.deferred = Some(index);
                break;
            }
            PreparedEvent::Oversized { measure, reason } => {
                close(&mut current, &mut plan);
                plan.rejected.push(RejectedEvent {
                    index,
                    measure: *measure,
                    reason: *reason,
                });
                continue;
            }
            PreparedEvent::Measured(measure) => *measure,
        };
        // 调用方可能用不同限额分类；单条即无法成批时精确拒收，而不是产出超限批次。
        if let Some(reason) = oversize_reason(&measure, limits) {
            close(&mut current, &mut plan);
            plan.rejected.push(RejectedEvent {
                index,
                measure,
                reason,
            });
            continue;
        }
        if let Some(batch) = current.as_mut() {
            if fits(batch, &measure, limits) {
                extend(batch, index, &measure);
                continue;
            }
        }
        close(&mut current, &mut plan);
        current = Some(BatchWindow {
            range: index..index + 1,
            raw_bytes: measure.raw_bytes,
            encoded_bytes: measure.encoded_bytes,
        });
    }
    close(&mut current, &mut plan);
    Ok(plan)
}

/// 该事件能否并入已累积的批次。
fn fits(batch: &BatchWindow, measure: &EventMeasure, limits: &BatchLimits) -> bool {
    batch.len() < limits.max_events
        && batch.raw_bytes.saturating_add(measure.raw_bytes) <= limits.max_batch_bytes as u64
        && batch.encoded_bytes.saturating_add(measure.encoded_bytes)
            <= limits.max_encoded_bytes as u64
}

/// 追加事件到当前批次；只接收相邻下标，保持区间连续。
fn extend(batch: &mut BatchWindow, index: usize, measure: &EventMeasure) {
    batch.range.end = index + 1;
    batch.raw_bytes = batch.raw_bytes.saturating_add(measure.raw_bytes);
    batch.encoded_bytes = batch.encoded_bytes.saturating_add(measure.encoded_bytes);
}

/// 结束当前批次（若有）；空批次不入结果。
fn close(current: &mut Option<BatchWindow>, plan: &mut SplitPlan) {
    if let Some(batch) = current.take() {
        plan.batches.push(batch);
    }
}

/// 限额自洽性：条数为零，或单批限额小于单条限额时，非超限单条无法保证成批。
fn validate_limits(limits: &BatchLimits) -> Result<(), LangfuseError> {
    if limits.max_events == 0
        || limits.max_event_bytes == 0
        || limits.max_batch_bytes == 0
        || limits.max_encoded_bytes == 0
    {
        return Err(LangfuseError::Config(
            "batch split limits must be nonzero".into(),
        ));
    }
    if limits.max_event_bytes > limits.max_batch_bytes {
        return Err(LangfuseError::Config(
            "batch raw limit must cover the single-event raw limit".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "split_test.rs"]
mod tests;
