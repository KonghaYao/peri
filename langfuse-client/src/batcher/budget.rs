//! 队列与快照的字节记账口径（契约 §4）。
//!
//! 三种口径各自独立、互不代偿：
//! - [`event_bytes`]：冻结后事件对象自身的原文 JSON 字节（准入与批聚合用）；
//! - [`queued_bytes`]：队列记账，文件引用按其**描述**内存计费，不按文件原文；
//! - [`snapshot_allowance`]：冻结快照的输出 allowance（spool 阈值前缀 / 纯内存事件原文上限）。

use std::io::{self, Write};

use crate::config::TelemetryConfig;
use crate::snapshot::TelemetryEvent;
use crate::{IngestionEvent, LangfuseError};

/// 队列中每个冻结来源（文件/内存句柄）的保守描述内存账单。
///
/// 口径来自契约 §4「文件引用按描述内存计费而非文件原文」：句柄、元数据与注册凭证的
/// `size_of + heap capacity` 上界；保守取整，不按文件或正文长度计费。
pub(super) const SOURCE_DESCRIPTOR_BYTES: usize = 512;

struct ByteCounter {
    bytes: usize,
    limit: usize,
    exceeded: bool,
}

impl Write for ByteCounter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let remaining = self.limit.saturating_sub(self.bytes);
        if buffer.len() > remaining {
            self.exceeded = true;
            return Err(io::Error::other("event byte budget exceeded"));
        }
        self.bytes += buffer.len();
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) fn event_bytes(event: &IngestionEvent, limit: usize) -> Result<usize, LangfuseError> {
    let mut counter = ByteCounter {
        bytes: 0,
        limit,
        exceeded: false,
    };
    let result = serde_json::to_writer(&mut counter, event);
    if counter.exceeded {
        return Err(LangfuseError::PayloadTooLarge { limit_bytes: limit });
    }
    result?;
    Ok(counter.bytes)
}

/// 队列记账字节：事件原文 + 每个冻结来源的描述账单。
pub(super) fn queued_bytes(event: &TelemetryEvent, limit: usize) -> Result<usize, LangfuseError> {
    let mut bytes = event_bytes(&event.event, limit)?;
    for source in [event.input.as_ref(), event.output.as_ref()]
        .into_iter()
        .flatten()
    {
        let _ = source;
        bytes = bytes.saturating_add(SOURCE_DESCRIPTOR_BYTES);
    }
    Ok(bytes)
}

/// 冻结快照的输出 allowance（契约 §4）：spool 可用时按阈值前缀 capacity，纯内存时按
/// 受事件逻辑限额约束的 JSON capacity 上界。
///
/// allowance 在 `try_freeze` 时一次预留（先槽后额），Inline 完成时转成 `BudgetedBytes` 的
/// permit；Spooled 完成才归还。冻结 JSON 超过 allowance 按超限显式失败，不向下取整。
pub(super) fn snapshot_allowance(telemetry: &TelemetryConfig, max_event_bytes: usize) -> usize {
    if telemetry.spool_enabled {
        telemetry.input_spool_threshold_bytes
    } else {
        max_event_bytes
    }
}
