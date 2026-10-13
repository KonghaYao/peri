//! 导出配置与 OTLP 正文编码入口（契约 §3.1、§3.4）。
//!
//! 编码不再物化整份正文：事件直接流进受预算约束的 `BudgetedWriter`，跨阈值转盘；
//! `none` 与 `gzip` 使用同一签名、由 [`encode`] 按配置分派，共用同一份 OTLP 映射
//! （`crate::encoding::write_otlp`）。两个字节上限互相独立：解压后的 JSON 由 raw 守卫计量，
//! 压缩后的线包由 writer 自己计量。

use std::io::Write;
use std::time::Duration;

use serde::Serialize;

use crate::config::Compression;
use crate::encoding::json_string::write_failure;
use crate::encoding::{BudgetedWriter, SinkFailure};
use crate::error::LangfuseError;
use crate::snapshot::TelemetryEvent;

/// HTTP 导出的内存与重试预算，与批处理队列容量独立。
#[derive(Debug, Clone)]
pub struct ExportConfig {
    /// 最终解压 JSON 的字节上限，默认 4 MiB；direct ingest 的既有事件预检继续执行。
    pub max_request_bytes: usize,
    /// 成功响应的字节上限，默认 64 KiB；失败响应正文不读取。
    pub max_response_bytes: usize,
    /// 包括首次发送、正文读取与重试等待的累计期限，默认 60 秒。
    pub retry_budget: Duration,
    /// 指数退避初始上限，默认 1 秒；实际等待带 50%..=100% jitter。
    pub initial_retry_delay: Duration,
    /// 指数退避上限，默认 30 秒；Retry-After 作为最短等待另行遵守。
    pub max_retry_delay: Duration,
    /// 上报正文的内容编码，默认标准 gzip。
    pub compression: Compression,
    /// 压缩后 wire 正文字节上限，默认 4 MiB；必须非零。
    pub max_wire_bytes: usize,
}

impl Default for ExportConfig {
    fn default() -> Self {
        Self {
            max_request_bytes: 4 * 1024 * 1024,
            max_response_bytes: 64 * 1024,
            retry_budget: Duration::from_secs(60),
            initial_retry_delay: Duration::from_secs(1),
            max_retry_delay: Duration::from_secs(30),
            compression: Compression::Gzip,
            max_wire_bytes: 4 * 1024 * 1024,
        }
    }
}

impl ExportConfig {
    pub(super) fn validate(&self) -> Result<(), LangfuseError> {
        if self.max_request_bytes == 0 || self.max_response_bytes == 0 {
            return Err(LangfuseError::Config(
                "OTLP byte limits must be nonzero".into(),
            ));
        }
        if self.max_wire_bytes == 0 {
            return Err(LangfuseError::Config(
                "OTLP wire byte limit must be nonzero".into(),
            ));
        }
        if self.retry_budget.is_zero()
            || self.initial_retry_delay.is_zero()
            || self.max_retry_delay < self.initial_retry_delay
            || peri_time::monotonic_now()
                .checked_add(self.retry_budget)
                .is_none()
        {
            return Err(LangfuseError::Config(
                "invalid OTLP retry time budget".into(),
            ));
        }
        Ok(())
    }
}

pub(super) fn preflight(payload: &impl Serialize, limit: usize) -> Result<(), LangfuseError> {
    let mut writer = CountingWriter {
        count: 0,
        limit,
        exceeded: false,
    };
    if serde_json::to_writer(&mut writer, payload).is_err() {
        return Err(if writer.exceeded {
            LangfuseError::PayloadTooLarge { limit_bytes: limit }
        } else {
            LangfuseError::IngestionApi("OTLP event preflight serialization failed".into())
        });
    }
    Ok(())
}

struct CountingWriter {
    count: usize,
    limit: usize,
    exceeded: bool,
}

impl Write for CountingWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.count) {
            self.exceeded = true;
            return Err(std::io::ErrorKind::FileTooLarge.into());
        }
        self.count += bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// 按配置选择内容编码，写出完整 OTLP 正文。
///
/// `out` 由发送 owner 按 `TelemetryConfig`、部署预算与 `ExportContext` 构造；两个上限
/// （解压后 / 线包）分别由 raw 守卫与 writer 独立执行。
pub(crate) fn encode(
    events: &[TelemetryEvent],
    out: BudgetedWriter,
    config: &ExportConfig,
) -> Result<crate::payload::ExportPayload, LangfuseError> {
    match config.compression {
        Compression::Gzip => encode_gzip(events, out, config),
        Compression::None => encode_none(events, out, config),
    }
}

/// 未压缩编码：raw 与 wire 是同一串字节，两个上限各自独立生效。
pub(crate) fn encode_none(
    events: &[TelemetryEvent],
    mut out: BudgetedWriter,
    config: &ExportConfig,
) -> Result<crate::payload::ExportPayload, LangfuseError> {
    let (outcome, raw_failure) = {
        let mut guard = RawLimitWriter::new(&mut out, config.max_request_bytes);
        let outcome = crate::encoding::write_otlp_counted(events, &mut guard);
        (outcome, guard.take_failure())
    };
    match outcome {
        Ok((raw_len, spans)) => out.finish(raw_len, Compression::None, spans),
        Err(error) => Err(classify(raw_failure, out.take_failure(), error)),
    }
}

/// 标准 gzip 编码：固定 level 6、mtime 0、无自定义 wire 扩展；trailer 计入线包上限。
///
/// raw 守卫在压缩器**之前**计量解压后字节，writer 在 gzip **之后**计量线包字节：两个上限
/// 一个管未压缩正文，一个管实际发出去的字节，互不代偿。
pub(crate) fn encode_gzip(
    events: &[TelemetryEvent],
    mut out: BudgetedWriter,
    config: &ExportConfig,
) -> Result<crate::payload::ExportPayload, LangfuseError> {
    let mut encoder = flate2::write::GzEncoder::new(&mut out, flate2::Compression::new(6));
    let (outcome, raw_failure) = {
        let mut guard = RawLimitWriter::new(&mut encoder, config.max_request_bytes);
        let outcome = crate::encoding::write_otlp_counted(events, &mut guard);
        (outcome, guard.take_failure())
    };
    let outcome = match (outcome, encoder.finish()) {
        (Ok(counted), Ok(_)) => Ok(counted),
        (Ok(_), Err(error)) => Err(write_failure(error)),
        (Err(error), _) => Err(error),
    };
    match outcome {
        Ok((raw_len, spans)) => out.finish(raw_len, Compression::Gzip, spans),
        Err(error) => Err(classify(raw_failure, out.take_failure(), error)),
    }
}

/// 失败分类：先看守卫与 writer 记录的失败（它们是精确的），否则沿用原错误。
///
/// 两个 sink 都在收到超限字节前拒绝写入，因此同一时刻最多只有一个记录了失败。
fn classify(
    raw_failure: Option<SinkFailure>,
    wire_failure: Option<SinkFailure>,
    fallback: LangfuseError,
) -> LangfuseError {
    raw_failure
        .or(wire_failure)
        .map_or(fallback, SinkFailure::into_langfuse)
}

/// 解压前（raw）字节上限守卫：与 writer 的线包上限各自独立生效。
struct RawLimitWriter<'a> {
    inner: &'a mut dyn Write,
    limit: usize,
    written: usize,
    failure: Option<SinkFailure>,
}

impl<'a> RawLimitWriter<'a> {
    fn new(inner: &'a mut dyn Write, limit: usize) -> Self {
        Self {
            inner,
            limit,
            written: 0,
            failure: None,
        }
    }

    fn take_failure(&mut self) -> Option<SinkFailure> {
        self.failure.take()
    }
}

impl Write for RawLimitWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if let Some(failure) = &self.failure {
            return Err(std::io::Error::other(failure.describe()));
        }
        if bytes.len() > self.limit.saturating_sub(self.written) {
            let failure = SinkFailure::Limit {
                limit_bytes: self.limit,
            };
            let error = std::io::Error::other(failure.describe());
            self.failure = Some(failure);
            return Err(error);
        }
        let written = self.inner.write(bytes)?;
        self.written = self.written.saturating_add(written);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
#[path = "client_export_test.rs"]
mod tests;
