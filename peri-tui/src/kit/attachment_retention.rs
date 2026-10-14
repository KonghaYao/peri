//! 已提交附件的载荷保留（**有界** LRU）——消息区附件记录行的浮动预览数据源。
//!
//! **为什么需要**：粘贴图片走上传式附件（base64 直上 wire），提交后 TUI 只保留
//! 展示记录（`UserAttachment`：字节数 + 载荷身份，不含 base64），消息区的
//! `[图片] 图片 N` 记录行因此没有像素来源、无法预览。这里保留最近提交的少量载荷。
//!
//! **上限与取舍**（保守默认，可调）：
//! - 条数上限 [`MAX_RETAINED_ATTACHMENTS`] = 4：浮层一次只看一张，保留最近几张
//!   足够覆盖「刚发完想再看一眼」的用法；
//! - 字节上限 [`MAX_RETAINED_BYTES`] = 16 MiB（base64 文本长度）：典型截图
//!   0.5–2 MiB/张，16 MiB 可容纳数张且远小于浮层解码本就要承担的峰值
//!   （4K 图解码 ≈ 33 MiB，见 `image_safety::MAX_TOTAL_PIXELS`）；
//! - 单张载荷超过字节上限 → 不保留（发送不受影响，仅该图没有 hover 预览）；
//! - 超出任一上限即从**最旧**开始淘汰（LRU，靠重新提交同图刷新位置）；
//! - 会话边界（`session_boundary::project_session_boundary`）清空，不跨会话堆积。
//!
//! 保留的是提交时的原始 base64，不重编码、不落盘（沿用「不落盘」的既有取舍）。

use std::collections::VecDeque;

/// 保留条数上限（最近提交的附件张数）。
pub const MAX_RETAINED_ATTACHMENTS: usize = 4;
/// 保留载荷总量上限（base64 文本字节数，16 MiB）。
pub const MAX_RETAINED_BYTES: usize = 16 * 1024 * 1024;

/// 单条已提交附件的载荷。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetainedAttachment {
    /// 载荷身份（与 `PendingAttachment::payload_id`、`UserAttachment.payload_id` 同源）。
    pub payload_id: u64,
    pub media_type: String,
    pub base64_data: String,
}

/// 有界 LRU：条目按提交顺序排列，队首最旧。
#[derive(Debug, Default)]
pub struct RetainedAttachments {
    entries: VecDeque<RetainedAttachment>,
    total_bytes: usize,
}

impl RetainedAttachments {
    /// 保留一份已提交载荷。同一 `payload_id` 重复提交视为刷新（移到队尾）。
    pub fn retain(&mut self, media_type: &str, base64_data: &str, payload_id: u64) {
        if base64_data.len() > MAX_RETAINED_BYTES {
            return;
        }
        self.remove(payload_id);
        self.entries.push_back(RetainedAttachment {
            payload_id,
            media_type: media_type.to_string(),
            base64_data: base64_data.to_string(),
        });
        self.total_bytes += base64_data.len();
        self.evict();
    }

    /// 按载荷身份取回载荷；未保留（未提交、被淘汰或超上限）→ None。
    pub fn get(&self, payload_id: u64) -> Option<&RetainedAttachment> {
        self.entries
            .iter()
            .find(|entry| entry.payload_id == payload_id)
    }

    /// 会话边界释放（不跨会话堆积）。
    pub fn clear(&mut self) {
        self.entries.clear();
        self.total_bytes = 0;
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    fn remove(&mut self, payload_id: u64) {
        if let Some(index) = self
            .entries
            .iter()
            .position(|entry| entry.payload_id == payload_id)
            && let Some(removed) = self.entries.remove(index)
        {
            self.total_bytes = self.total_bytes.saturating_sub(removed.base64_data.len());
        }
    }

    /// 从最旧开始淘汰，直到同时满足条数与字节两个上限。
    fn evict(&mut self) {
        while self.entries.len() > MAX_RETAINED_ATTACHMENTS || self.total_bytes > MAX_RETAINED_BYTES
        {
            let Some(removed) = self.entries.pop_front() else {
                break;
            };
            self.total_bytes = self.total_bytes.saturating_sub(removed.base64_data.len());
        }
    }
}

#[cfg(test)]
#[path = "attachment_retention_test.rs"]
mod tests;
