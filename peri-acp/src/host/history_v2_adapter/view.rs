//! 窗口条目的展示投影与 oversize 可见性判定。
//!
//! own 与 inherited 复用同一套可见性口径：已解码 payload 走 `dispatch` 投影；
//! 超预算条目只按 ≤1024 字节前缀判定，绝不读取整条正文。

use peri_acp_types::history_page::{HistoryEntryRole, HistoryPageItem};
use peri_acp_types::history_v2::{
    DecimalU64, HistoryBodyDescriptor, HistoryDurability, HistoryEntry, HistoryOpaqueSubItem,
    HistoryOrder, HistoryRepresentation, HistorySource as V2Source, HistorySubItem,
};
use peri_acp_types::history_window::{HistoryCommittedTail, HistorySource as WindowSource};
use peri_acp_types::messages::MessageId;
use peri_acp_types::session_resources::{
    HistoryWindowEntry, HistoryWindowRows, InheritedHistoryEntry,
};
use peri_acp_types::thread::ThreadId;

use crate::dispatch;

use super::projection::{self, VISIBILITY_PREFIX_BYTES};
use super::refs::{encode_inherited_ref, encode_own_ref, InheritedRef};
use super::{
    imported_durability, inherited_entry_bytes, inherited_order, own_durability, own_order,
    window_entry_bytes, HistoryV2BackendAdapter, RegionView,
};

impl HistoryV2BackendAdapter {
    pub(super) async fn own_view(
        &self,
        thread_id: &ThreadId,
        rows: HistoryWindowRows,
    ) -> RegionView {
        let raw_count = rows.entries.len();
        let used_bytes = rows.entries.iter().map(window_entry_bytes).sum();
        let before_anchor = rows.before_cursor_anchor;
        let after_anchor = rows.after_cursor_anchor;
        let committed_tail = rows.committed_tail;
        let session_id = thread_id.as_str();
        let mut entries = Vec::new();
        let mut oldest = None;
        let mut newest = None;
        for entry in rows.entries {
            match entry {
                HistoryWindowEntry::Loaded(item) => {
                    let position = item.position;
                    let message_id = item.id;
                    oldest.get_or_insert(message_id);
                    newest = Some(message_id);
                    if let Some(entry) = self.loaded_entry(
                        session_id,
                        item,
                        V2Source::Own,
                        own_durability(position),
                        own_order(position),
                    ) {
                        entries.push(entry);
                    }
                }
                HistoryWindowEntry::Oversized {
                    entry_id,
                    position,
                    total_bytes,
                    role,
                } => {
                    oldest.get_or_insert(entry_id);
                    newest = Some(entry_id);
                    if self
                        .oversized_visible(thread_id, committed_tail.as_ref(), entry_id, role)
                        .await
                    {
                        entries.push(self.opaque_entry(
                            session_id,
                            entry_id,
                            V2Source::Own,
                            own_durability(position),
                            own_order(position),
                            total_bytes,
                        ));
                    }
                }
            }
        }
        RegionView {
            entries,
            before_anchor,
            after_anchor,
            oldest,
            newest,
            before_more: before_anchor.is_some(),
            after_more: after_anchor.is_some(),
            raw_count,
            used_bytes,
            committed_tail,
        }
    }

    pub(super) async fn inherited_view(
        &self,
        thread_id: &ThreadId,
        source: &InheritedRef,
        rows: HistoryWindowRows,
    ) -> RegionView {
        let raw_count = rows.inherited_entries.len();
        let used_bytes = rows
            .inherited_entries
            .iter()
            .map(inherited_entry_bytes)
            .sum();
        let before_anchor = rows.before_cursor_anchor;
        let after_anchor = rows.after_cursor_anchor;
        let session_id = thread_id.as_str();
        let mut entries = Vec::new();
        let mut oldest = None;
        let mut newest = None;
        for entry in rows.inherited_entries {
            let ordinal = entry.ordinal();
            match entry {
                InheritedHistoryEntry::Loaded(item) => {
                    let message_id = item.id;
                    oldest.get_or_insert(message_id);
                    newest = Some(message_id);
                    if let Some(entry) = self.loaded_entry(
                        session_id,
                        item,
                        source.v2_source(),
                        imported_durability(),
                        inherited_order(ordinal),
                    ) {
                        entries.push(entry);
                    }
                }
                InheritedHistoryEntry::Oversized {
                    entry_id,
                    total_bytes,
                    ..
                } => {
                    oldest.get_or_insert(entry_id);
                    newest = Some(entry_id);
                    if self
                        .inherited_oversized_visible(thread_id, source, entry_id)
                        .await
                    {
                        entries.push(self.opaque_entry(
                            session_id,
                            entry_id,
                            source.v2_source(),
                            imported_durability(),
                            inherited_order(ordinal),
                            total_bytes,
                        ));
                    }
                }
            }
        }
        RegionView {
            entries,
            before_anchor,
            after_anchor,
            oldest,
            newest,
            before_more: before_anchor.is_some(),
            after_more: after_anchor.is_some(),
            raw_count,
            used_bytes,
            committed_tail: None,
        }
    }

    /// 不物化正文的 own oversize 可见性判定。page 过滤与 body 复核共用。
    pub(super) async fn oversized_visible(
        &self,
        thread_id: &ThreadId,
        tail: Option<&HistoryCommittedTail>,
        entry_id: MessageId,
        role: HistoryEntryRole,
    ) -> bool {
        match role {
            HistoryEntryRole::System => false,
            HistoryEntryRole::User | HistoryEntryRole::Assistant | HistoryEntryRole::Tool => true,
            HistoryEntryRole::SystemReminder => {
                let Some(tail) = tail else {
                    tracing::warn!(
                        entry_id = %entry_id.as_uuid(),
                        "history v2 omitted an oversized reminder without a committed tail"
                    );
                    return false;
                };
                let Some(prefix) = self
                    .visibility_prefix(thread_id, &WindowSource::Own, Some(tail), entry_id)
                    .await
                else {
                    tracing::warn!(
                        entry_id = %entry_id.as_uuid(),
                        "history v2 omitted an oversized reminder: prefix read failed"
                    );
                    return false;
                };
                match projection::reminder_allows_tui(&prefix) {
                    Some(true) => true,
                    Some(false) => {
                        tracing::warn!(
                            entry_id = %entry_id.as_uuid(),
                            "history v2 omitted an oversized model-only reminder"
                        );
                        false
                    }
                    None => {
                        tracing::warn!(
                            entry_id = %entry_id.as_uuid(),
                            "history v2 omitted an oversized reminder: audiences undetermined"
                        );
                        false
                    }
                }
            }
            HistoryEntryRole::Unknown => {
                let Some(tail) = tail else {
                    return false;
                };
                let Some(prefix) = self
                    .visibility_prefix(thread_id, &WindowSource::Own, Some(tail), entry_id)
                    .await
                else {
                    tracing::warn!(
                        entry_id = %entry_id.as_uuid(),
                        "history v2 omitted an oversized entry with an unreadable prefix"
                    );
                    return false;
                };
                projection::unknown_role_visible(&prefix)
            }
        }
    }

    /// inherited oversize 没有 role 列：只读 1024 字节前缀判定受众。
    pub(super) async fn inherited_oversized_visible(
        &self,
        thread_id: &ThreadId,
        source: &InheritedRef,
        entry_id: MessageId,
    ) -> bool {
        let Some(prefix) = self
            .visibility_prefix(thread_id, &source.window_source(), None, entry_id)
            .await
        else {
            tracing::warn!(
                entry_id = %entry_id.as_uuid(),
                "history v2 omitted an inherited oversized entry: prefix read failed"
            );
            return false;
        };
        projection::inherited_prefix_visible(&prefix)
    }

    async fn visibility_prefix(
        &self,
        thread_id: &ThreadId,
        source: &WindowSource,
        tail: Option<&HistoryCommittedTail>,
        entry_id: MessageId,
    ) -> Option<Vec<u8>> {
        #[cfg(test)]
        self.visibility_reads.lock().push(VISIBILITY_PREFIX_BYTES);
        self.load_body(
            thread_id,
            source,
            entry_id,
            0,
            VISIBILITY_PREFIX_BYTES,
            tail,
        )
        .await
        .ok()
        .map(|chunk| chunk.bytes)
    }

    /// 一条已解码 payload 的 V2 条目；没有可见子项时按 V1 `page_item` 口径省略。
    fn loaded_entry(
        &self,
        session_id: &str,
        item: HistoryPageItem,
        source: V2Source,
        durability: HistoryDurability,
        order: HistoryOrder,
    ) -> Option<HistoryEntry> {
        if !item.payload.displayable_to_client() {
            return None;
        }
        let entry_id = dispatch::entry_id(session_id, item.id);
        let mut sub_items = Vec::new();
        for update in dispatch::project_payload_updates(&item.payload, &self.caps, None) {
            if let Some(sub_item) =
                projection::projected_sub_item(update, session_id, &entry_id, sub_items.len())
            {
                sub_items.push(sub_item);
            }
        }
        if sub_items.is_empty() {
            return None;
        }
        Some(HistoryEntry {
            entry_id,
            source,
            content_version: "1".to_owned(),
            durability,
            order,
            sub_items,
        })
    }

    fn opaque_entry(
        &self,
        session_id: &str,
        message_id: MessageId,
        source: V2Source,
        durability: HistoryDurability,
        order: HistoryOrder,
        total_bytes: u64,
    ) -> HistoryEntry {
        let entry_id = dispatch::entry_id(session_id, message_id);
        let sub_item_id = format!("{entry_id}:0");
        let body_ref = match &source {
            V2Source::Own => encode_own_ref(message_id),
            V2Source::Inherited(inherited) => match InheritedRef::from_v2(inherited) {
                Some(source) => encode_inherited_ref(&source, message_id),
                // 来源在进入映射前已经校验；这里兜底时保持 own 编码会误路由，
                // 因此使用一个不可能被解析成 own 的前缀，body 读取会 fail-closed。
                None => format!(
                    "i:invalid:{}:{}",
                    message_id.as_uuid(),
                    message_id.as_uuid()
                ),
            },
        };
        HistoryEntry {
            entry_id,
            source,
            content_version: "1".to_owned(),
            durability,
            order,
            sub_items: vec![HistorySubItem::Opaque(HistoryOpaqueSubItem {
                sub_item_id,
                body: HistoryBodyDescriptor {
                    body_ref,
                    // body 返回的是展示投影文本；page 阶段只给 canonical 长度作为
                    // 预估，权威总长以 body 响应的 totalBytes 为准。
                    representation: HistoryRepresentation::DisplayText,
                    media_type: super::body::DISPLAY_MEDIA_TYPE.to_owned(),
                    total_bytes: DecimalU64::new(total_bytes),
                    chunk_bytes: self.chunk_bytes(),
                },
            })],
        }
    }
}
