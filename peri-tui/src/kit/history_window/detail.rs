//! 超大条目的持久身份与详情描述符（不持正文）。

use super::{HistorySource, PersistentEntryKey};
use crate::kit::tui_render_unit::tui_hash_str;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetailDescriptor {
    pub key: PersistentEntryKey,
    pub total_bytes: u64,
    pub chunk_size: u32,
    pub snapshot_token: String,
    pub read_ticket: String,
}

impl super::HistoryWindow {
    pub fn register_detail_descriptor(&mut self, descriptor: DetailDescriptor) {
        self.detail_descriptors
            .insert(descriptor.key.clone(), descriptor);
    }

    pub fn detail_descriptor(&self, key: &PersistentEntryKey) -> Option<&DetailDescriptor> {
        self.detail_descriptors.get(key)
    }

    pub fn clear_detail_descriptors(&mut self) {
        self.detail_descriptors.clear();
    }
}

pub fn publish_detail_targets(targets: Vec<HistoryDetailTarget>) {
    let handle = crate::kit::atoms::HISTORY_DETAIL_TARGETS.state();
    let mut all = handle.write();
    for target in targets {
        all.retain(|existing| existing.descriptor.key != target.descriptor.key);
        all.push(target);
    }
}

pub fn clear_detail_targets() {
    crate::kit::atoms::HISTORY_DETAIL_TARGETS.set(Vec::new());
}

pub fn remove_detail_targets(keys: &std::collections::HashSet<PersistentEntryKey>) {
    let handle = crate::kit::atoms::HISTORY_DETAIL_TARGETS.state();
    handle
        .write()
        .retain(|target| !keys.contains(&target.descriptor.key));
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryDetailTarget {
    pub content_hash: u64,
    pub descriptor: DetailDescriptor,
}

/// 占位 note 的缓存键：把持久身份混入 hash，避免同尺寸占位互相命中。
pub fn placeholder_hash(key: &PersistentEntryKey, total_bytes: u64) -> u64 {
    tui_hash_str(&format!(
        "{}|{}|{}",
        key.source.frozen_snapshot_id().unwrap_or("own"),
        key.entry_id,
        total_bytes
    ))
}

impl DetailDescriptor {
    pub fn new(
        source: HistorySource,
        entry_id: impl Into<String>,
        total_bytes: u64,
        chunk_size: u32,
        snapshot_token: impl Into<String>,
        read_ticket: impl Into<String>,
    ) -> Self {
        let key = PersistentEntryKey::new(source, entry_id);
        Self {
            key,
            total_bytes,
            chunk_size,
            snapshot_token: snapshot_token.into(),
            read_ticket: read_ticket.into(),
        }
    }
}
