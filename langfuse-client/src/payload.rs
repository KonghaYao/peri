//! 遥测正文的所有权类型：受预算约束的内存字节与已完成封装的 payload（契约 §3.3）。
//!
//! [`InputSnapshot`] 保存冻结后的紧凑 JSON 字节或暂存对象句柄，不存在把文件 ID 塞进 wire
//! 的变体；[`ExportPayload`] 是已封装完成、可重读的标准 OTLP 正文，绑定部署预算身份。
//!
//! 这些类型不实现 `Debug`：正文与凭据不得因日志/断言而外泄。

use std::sync::Arc;

use bytes::Bytes;

use crate::config::Compression;
use crate::error::LangfuseError;
use crate::memory::{MemoryBudget, MemoryPermit};
use crate::spool::SpoolObject;

/// 受预算约束的内存字节：`Bytes` 与其 permit 同寿。
///
/// HTTP 共享内存正文时必须整体包在私有 `Arc` 内（`BudgetedBytes::into_shared`），不能只
/// clone `Bytes` 后提前归还 permit；字段按声明顺序销毁，正文先于 permit 释放。
pub struct BudgetedBytes {
    pub(crate) bytes: Bytes,
    pub(crate) permit: MemoryPermit,
}

impl BudgetedBytes {
    /// 绑定一份已记账正文；permit 未覆盖 `bytes` 时按预算身份不一致拒收。
    ///
    /// 同步无 IO；permit 是 crate 内受控凭证，正文字节数不得超过其预约额度。
    pub(crate) fn new(bytes: Bytes, permit: MemoryPermit) -> Result<Self, LangfuseError> {
        if permit.bytes() < bytes.len() {
            return Err(LangfuseError::BudgetMismatch);
        }
        Ok(Self { bytes, permit })
    }

    /// 正文切片；借用期内 permit 不释放。
    pub(crate) fn bytes(&self) -> &Bytes {
        &self.bytes
    }

    /// 正文长度（受限额比较与 HTTP `Content-Length` 使用）。
    pub(crate) fn len(&self) -> usize {
        self.bytes.len()
    }

    /// 整体 `Arc` 化以在多个 owner 间共享同一份 permit；引用清零才释放。仅测试构造使用。
    #[cfg(test)]
    pub(crate) fn into_shared(self) -> Arc<Self> {
        Arc::new(self)
    }

    /// 本次正文的预算身份与额度；用于逐项 `Arc::ptr_eq` 核对。
    pub(crate) fn permit(&self) -> &MemoryPermit {
        &self.permit
    }
}

/// 冻结的观测 input：小对象留内存，大对象保存为暂存对象句柄。
pub enum InputSnapshot {
    Inline(BudgetedBytes),
    Spooled(SpoolObject),
}

/// payload 正文来源：受预算约束的内存前缀，或已原子发布的暂存文件。
pub enum PayloadBody {
    Memory(BudgetedBytes),
    File(SpoolObject),
}

/// 已完成封装、可重读的标准 OTLP 正文。
///
/// `raw_len` 是解压后的 JSON 字节数，`wire_len` 是实际发送的字节数（含内容编码）；
/// `sha256` 针对 `wire` 字节序列。
pub struct ExportPayload {
    pub(crate) body: PayloadBody,
    pub encoding: Compression,
    pub raw_len: u64,
    pub wire_len: u64,
    pub sha256: [u8; 32],
    pub event_count: u64,
    /// 部署预算身份，由 encoder 绑定；HTTP owner 从此预约工作区，不在 client 内另建预算。
    pub(crate) budget: Arc<MemoryBudget>,
}

impl ExportPayload {
    /// 正文的实际落盘/上线字节数；与 `wire_len` 交叉核对用，消费方为回归断言。
    #[cfg(test)]
    pub(crate) fn body_len(&self) -> u64 {
        match &self.body {
            PayloadBody::Memory(bytes) => bytes.len() as u64,
            PayloadBody::File(object) => object.meta().stored_len,
        }
    }

    /// 正文是否驻留内存（HTTP 走内存内核还是文件读泵由发送 owner 决定）。
    pub(crate) fn is_memory(&self) -> bool {
        matches!(self.body, PayloadBody::Memory(_))
    }

    /// 部署预算身份；发送 owner 只能从这里取得工作区额度来源。
    pub(crate) fn budget(&self) -> &Arc<MemoryBudget> {
        &self.budget
    }
}

#[cfg(test)]
#[path = "payload_test.rs"]
mod tests;
