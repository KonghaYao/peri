//! 窗口票据级的 oversize descriptor 记忆与预算。
//!
//! 记忆只回答「本票据是否已向客户端暴露过该 descriptor」，不构成读取授权；
//! 每条正文读取仍会重新校验 snapshot/tail 与受众可见性。集合与预算都属于单张
//! ticket，publish/revoke 即整体丢弃，不跨票据存活。
//!
//! 内存有界：身份数超过 [`OVERSIZE_DESCRIPTOR_MEMORY_MAX_ENTRIES`] 或预算耗尽后
//! 不再保留逐条身份，而是置 `overflowed`；此后正文读取必须按稳定 `entryId` 回存储
//! 重取校验，不能用位置/索引猜身份。

use std::collections::HashSet;

use peri_acp_types::messages::MessageId;

/// 已登记 descriptor 的身份上限：超过后清空集合并置 `overflowed`，内存不随页数增长。
pub(super) const OVERSIZE_DESCRIPTOR_MEMORY_MAX_ENTRIES: usize = 1024;

/// 票据级 descriptor 字节预算。计划 §9.1 未给出明确数值，沿用本仓历史窗口/TUI
/// 缓存的 8 MiB 量级；预算只限制登记新 descriptor，不限制已暴露条目的正文读取。
pub(super) const OVERSIZE_DESCRIPTOR_BYTE_BUDGET: u64 = 8 * 1024 * 1024;

#[derive(Debug)]
pub(super) struct OversizeMemory {
    entries: HashSet<MessageId>,
    overflowed: bool,
    /// 本票据剩余可登记的 descriptor 字节数，按登记条目的 `total_bytes` 扣减。
    descriptor_byte_budget: u64,
}

impl OversizeMemory {
    pub(super) fn new() -> Self {
        Self {
            entries: HashSet::new(),
            overflowed: false,
            descriptor_byte_budget: OVERSIZE_DESCRIPTOR_BYTE_BUDGET,
        }
    }

    /// 登记本页即将向客户端暴露的 descriptor（`entryId` 与正文总字节数）。
    ///
    /// 同一身份重复登记（例如 seek 再次返回同一 descriptor）不重复扣预算。
    /// 预算耗尽后停止登记新身份并置 `overflowed`；已登记身份保留在集合中。
    pub(super) fn remember(&mut self, entries: &[(MessageId, u64)]) {
        if self.overflowed {
            return;
        }
        for (entry_id, total_bytes) in entries {
            if self.entries.contains(entry_id) {
                continue;
            }
            if *total_bytes > self.descriptor_byte_budget {
                // 预算耗尽：不再登记新 descriptor；读取已暴露条目改走重取校验。
                self.overflowed = true;
                return;
            }
            self.descriptor_byte_budget -= total_bytes;
            self.entries.insert(*entry_id);
            if self.entries.len() > OVERSIZE_DESCRIPTOR_MEMORY_MAX_ENTRIES {
                // 身份数超限：整集合清空，避免内存随页数增长；后续按稳定身份重取。
                self.entries.clear();
                self.overflowed = true;
                return;
            }
        }
    }

    /// `overflowed` 时返回 `true`：集合已不可作为权威，调用方必须按稳定身份向
    /// Store 重取校验，不能仅凭「不在集合里」拒绝。
    pub(super) fn remembers(&self, entry_id: MessageId) -> bool {
        self.overflowed || self.entries.contains(&entry_id)
    }

    pub(super) fn overflowed(&self) -> bool {
        self.overflowed
    }

    #[cfg(test)]
    pub(super) fn force_overflow(&mut self) {
        self.entries.clear();
        self.overflowed = true;
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }
}
