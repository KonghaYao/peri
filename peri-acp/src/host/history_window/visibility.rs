//! 窗口 ticket 级的受众可见性缓存。
//!
//! 可见性判定的输入是持久化正文；同一 ticket 内对同一 entry 的多次分块请求不应重复
//! 从头解析。缓存键 = `(entry_id, SHA-256(正文前 64 KiB))`：canonical 序列化把
//! `role`/`type` 与 `audiences` 放在正文之前，因此前缀摘要变化足以让旧结论失效；
//! 条目正文的长度本身不参与键（同长度改写也会被摘要识别）。
//!
//! 缓存归属 [`super::IssuedWindowTicket`]：publish/revoke 即整体丢弃，不跨票据、
//! 不跨 connection/session 存活。缓存命中只影响性能，不改变任何交付或授权语义。

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use peri_acp_types::messages::MessageId;
use sha2::{Digest, Sha256};

/// 条目上限：窗口内已登记的 oversize descriptor 实际很少；超过上限整体清空，
/// 不引入按条目淘汰的额外状态。
const MAX_ENTRIES: usize = 4096;

/// 与 ticket 同生命周期的可见性结论缓存。
#[derive(Clone, Default)]
pub(super) struct VisibilityCache {
    state: Arc<VisibilityState>,
}

#[derive(Default)]
struct VisibilityState {
    entries: Mutex<HashMap<(MessageId, [u8; 32]), bool>>,
    /// 完整流式解析次数；仅测试用于证明分块请求没有重扫全文。
    full_scans: AtomicUsize,
}

impl VisibilityCache {
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// A single storage snapshot supplies both the cache key and the JSON. No
    /// answer is recorded until the canonical visibility parser reaches EOF.
    pub(super) fn inspect(
        &self,
        entry_id: MessageId,
        reader: &mut dyn std::io::BufRead,
    ) -> peri_acp_types::session_resources::SessionResourceResult<bool> {
        use peri_acp_types::history_window::HistoryWindowError;
        let prefix = reader
            .fill_buf()
            .map_err(|error| HistoryWindowError::ReadFailed {
                detail: format!("history audience prefix could not be read: {error}"),
            })?;
        let key = (entry_id, digest(prefix));
        if let Some(visible) = self.lookup_key(&key) {
            return Ok(visible);
        }
        self.note_full_scan();
        let visible =
            peri_acp_types::history_page_visibility::history_body_displayable_to_client_buffered(
                reader,
            )
            .map_err(|error| {
                if error.is_io() {
                    HistoryWindowError::ReadFailed {
                        detail: "history audience body could not be read".to_owned(),
                    }
                } else {
                    HistoryWindowError::anchor_unavailable("history audience could not be verified")
                }
            })?;
        self.store_key(key, visible);
        Ok(visible)
    }

    /// 前缀摘要与条目身份都匹配时返回已缓存的可见性结论。
    pub(super) fn lookup(&self, entry_id: MessageId, prefix: &[u8]) -> Option<bool> {
        self.lookup_key(&(entry_id, digest(prefix)))
    }

    fn lookup_key(&self, key: &(MessageId, [u8; 32])) -> Option<bool> {
        match self.state.entries.lock() {
            Ok(entries) => entries.get(key).copied(),
            Err(_) => panic!("visibility cache mutex must remain unpoisoned"),
        }
    }

    /// 记录一次完整解析的结论；容量到上限时整体清空。
    pub(super) fn store(&self, entry_id: MessageId, prefix: &[u8], visible: bool) {
        self.store_key((entry_id, digest(prefix)), visible);
    }

    fn store_key(&self, key: (MessageId, [u8; 32]), visible: bool) {
        let mut entries = match self.state.entries.lock() {
            Ok(entries) => entries,
            Err(_) => panic!("visibility cache mutex must remain unpoisoned"),
        };
        if entries.len() >= MAX_ENTRIES {
            entries.clear();
        }
        entries.insert(key, visible);
    }

    /// 未命中缓存、即将完整解析时计数。
    pub(super) fn note_full_scan(&self) {
        self.state.full_scans.fetch_add(1, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(super) fn full_scans(&self) -> usize {
        self.state.full_scans.load(Ordering::Relaxed)
    }
}

fn digest(prefix: &[u8]) -> [u8; 32] {
    let digest = Sha256::digest(prefix);
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&digest);
    bytes
}
