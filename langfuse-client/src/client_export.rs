use std::{io::Write, time::Duration};

use serde::Serialize;

use crate::error::LangfuseError;

/// HTTP 导出的内存与重试预算，与批处理队列容量独立。
#[derive(Debug, Clone)]
pub struct ExportConfig {
    /// 原始事件 JSON 总字节及编码后请求的上限，默认 4 MiB。
    pub max_request_bytes: usize,
    /// 成功响应的字节上限，默认 64 KiB；失败响应正文不读取。
    pub max_response_bytes: usize,
    /// 包括首次发送、正文读取与重试等待的累计期限，默认 60 秒。
    pub retry_budget: Duration,
    /// 指数退避初始上限，默认 1 秒；实际等待带 50%..=100% jitter。
    pub initial_retry_delay: Duration,
    /// 指数退避上限，默认 30 秒；Retry-After 作为最短等待另行遵守。
    pub max_retry_delay: Duration,
}

impl Default for ExportConfig {
    fn default() -> Self {
        Self {
            max_request_bytes: 4 * 1024 * 1024,
            max_response_bytes: 64 * 1024,
            retry_budget: Duration::from_secs(60),
            initial_retry_delay: Duration::from_secs(1),
            max_retry_delay: Duration::from_secs(30),
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

pub(super) fn encode(payload: &impl Serialize, limit: usize) -> Result<Vec<u8>, LangfuseError> {
    let mut writer = LimitedWriter {
        bytes: Vec::new(),
        limit,
        exceeded: false,
    };
    if serde_json::to_writer(&mut writer, payload).is_err() {
        return Err(if writer.exceeded {
            LangfuseError::PayloadTooLarge { limit_bytes: limit }
        } else {
            LangfuseError::IngestionApi("OTLP request serialization failed".into())
        });
    }
    Ok(writer.bytes)
}

struct LimitedWriter {
    bytes: Vec<u8>,
    limit: usize,
    exceeded: bool,
}

impl Write for LimitedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            self.exceeded = true;
            return Err(std::io::Error::other(
                "OTLP encoded request exceeds byte limit",
            ));
        }
        let required = self.bytes.len() + bytes.len();
        if required > self.bytes.capacity() {
            let capacity = self
                .bytes
                .capacity()
                .saturating_mul(2)
                .max(4096)
                .max(required)
                .min(self.limit);
            self.bytes.reserve_exact(capacity - self.bytes.len());
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
#[path = "client_export_test.rs"]
mod tests;
