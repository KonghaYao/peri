//! 窗口条目的展示投影与 oversize 可见性判定。
//!
//! own 与 inherited 复用同一套可见性口径：已解码 payload 走 `dispatch` 投影；
//! 超预算条目只按 ≤1024 字节前缀判定，绝不读取整条正文。

use peri_acp_types::history_common::{HistoryCommittedTail, HistorySource as WindowSource};
use peri_acp_types::history_page::{HistoryEntryRole, HistoryPageItem};
use peri_acp_types::history_v2::{
    DecimalU64, HistoryBackendError, HistoryBodyDescriptor, HistoryDurability, HistoryEntry,
    HistoryOpaqueSubItem, HistoryOrder, HistoryRepresentation, HistorySource as V2Source,
    HistorySubItem,
};
use peri_acp_types::messages::MessageId;
use peri_acp_types::session_resources::{
    HistoryWindowEntry, HistoryWindowRows, InheritedHistoryEntry,
};
use peri_acp_types::store::deserialize_persisted_payload;
use peri_acp_types::thread::ThreadId;

use crate::dispatch;

use super::body::MAX_CANONICAL_BODY_BYTES;
use super::projection::{self, PrefixVisibility, VISIBILITY_PREFIX_BYTES};
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
    ) -> Result<RegionView, HistoryBackendError> {
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
                    match self
                        .oversized_visibility(
                            thread_id,
                            committed_tail.as_ref(),
                            entry_id,
                            role,
                            total_bytes,
                        )
                        .await?
                    {
                        PrefixVisibility::Visible => entries.push(self.opaque_entry(
                            session_id,
                            entry_id,
                            V2Source::Own,
                            own_durability(position),
                            own_order(position),
                            total_bytes,
                        )),
                        PrefixVisibility::Hidden => {}
                        PrefixVisibility::Undetermined => {
                            return Err(undetermined_visibility(thread_id, entry_id));
                        }
                    }
                }
            }
        }
        Ok(RegionView {
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
        })
    }

    pub(super) async fn inherited_view(
        &self,
        thread_id: &ThreadId,
        source: &InheritedRef,
        rows: HistoryWindowRows,
    ) -> Result<RegionView, HistoryBackendError> {
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
                    match self
                        .inherited_oversized_visibility(thread_id, source, entry_id, total_bytes)
                        .await?
                    {
                        PrefixVisibility::Visible => entries.push(self.opaque_entry(
                            session_id,
                            entry_id,
                            source.v2_source(),
                            imported_durability(),
                            inherited_order(ordinal),
                            total_bytes,
                        )),
                        PrefixVisibility::Hidden => {}
                        PrefixVisibility::Undetermined => {
                            return Err(undetermined_visibility(thread_id, entry_id));
                        }
                    }
                }
            }
        }
        Ok(RegionView {
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
        })
    }

    /// 不物化正文的 own oversize 可见性判定。page 过滤与 body 复核共用。
    ///
    /// role 列是明确的存储事实时不需要读正文；reminder/未知 role 与 inherited 使用
    /// 同一套结构化前缀判定。reminder 前缀通过后还要做一次有界完整读取，复用
    /// [`crate::dispatch::reminder_visible_to_client`] 的完整出口规则（`validate()`、
    /// `delivery` 等），因此 `diagnostic_only` 不会生成 opaque descriptor。
    /// 前缀读取失败原样返回错误，不转成隐藏。
    pub(super) async fn oversized_visibility(
        &self,
        thread_id: &ThreadId,
        tail: Option<&HistoryCommittedTail>,
        entry_id: MessageId,
        role: HistoryEntryRole,
        canonical_bytes: u64,
    ) -> Result<PrefixVisibility, HistoryBackendError> {
        match role {
            HistoryEntryRole::System => Ok(PrefixVisibility::Hidden),
            HistoryEntryRole::User | HistoryEntryRole::Assistant | HistoryEntryRole::Tool => {
                Ok(PrefixVisibility::Visible)
            }
            HistoryEntryRole::SystemReminder | HistoryEntryRole::Unknown => {
                let Some(tail) = tail else {
                    // 没有固定 tail 就无法在快照内定位前缀：这是「无法判定」，
                    // 不是「明确隐藏」。调用方会让整页失败。
                    tracing::warn!(
                        entry_id = %entry_id.as_uuid(),
                        "history v2 oversized entry has no committed tail for visibility"
                    );
                    return Ok(PrefixVisibility::Undetermined);
                };
                let prefix = self
                    .visibility_prefix(thread_id, &WindowSource::Own, Some(tail), entry_id)
                    .await?;
                self.resolve_verdict(
                    thread_id,
                    &WindowSource::Own,
                    Some(tail),
                    entry_id,
                    canonical_bytes,
                    projection::prefix_verdict(&prefix),
                )
                .await
            }
        }
    }

    /// inherited oversize 没有 role 列：前缀判定 + reminder 完整规则复核。
    pub(super) async fn inherited_oversized_visibility(
        &self,
        thread_id: &ThreadId,
        source: &InheritedRef,
        entry_id: MessageId,
        canonical_bytes: u64,
    ) -> Result<PrefixVisibility, HistoryBackendError> {
        let window_source = source.window_source();
        let prefix = self
            .visibility_prefix(thread_id, &window_source, None, entry_id)
            .await?;
        self.resolve_verdict(
            thread_id,
            &window_source,
            None,
            entry_id,
            canonical_bytes,
            projection::prefix_verdict(&prefix),
        )
        .await
    }

    /// 前缀结论 → 可见性。`ReminderCandidate` 用有界完整 payload 复核。
    async fn resolve_verdict(
        &self,
        thread_id: &ThreadId,
        source: &WindowSource,
        tail: Option<&HistoryCommittedTail>,
        entry_id: MessageId,
        canonical_bytes: u64,
        verdict: projection::PrefixVerdict,
    ) -> Result<PrefixVisibility, HistoryBackendError> {
        match verdict {
            projection::PrefixVerdict::Visible => Ok(PrefixVisibility::Visible),
            projection::PrefixVerdict::Hidden => Ok(PrefixVisibility::Hidden),
            projection::PrefixVerdict::Undetermined => Ok(PrefixVisibility::Undetermined),
            projection::PrefixVerdict::ReminderCandidate => {
                self.reminder_payload_visibility(thread_id, source, tail, entry_id, canonical_bytes)
                    .await
            }
        }
    }

    /// reminder 的完整出口规则复核：有界（≤8 MiB）读取 canonical payload，
    /// 反序列化后调用与页面/正文出口同一个 `displayable_to_client()`。
    /// 只用于判定，不返回正文、不写日志内容。
    async fn reminder_payload_visibility(
        &self,
        thread_id: &ThreadId,
        source: &WindowSource,
        tail: Option<&HistoryCommittedTail>,
        entry_id: MessageId,
        canonical_bytes: u64,
    ) -> Result<PrefixVisibility, HistoryBackendError> {
        if canonical_bytes > MAX_CANONICAL_BODY_BYTES as u64 {
            // reminder 的 body/metadata 上限远小于单条读取上限，超过 8 MiB 的
            // reminder 必然无法通过 `validate()`。这是确定的隐藏，不是猜测。
            return Ok(PrefixVisibility::Hidden);
        }
        let canonical = self
            .read_canonical_payload(thread_id, source, entry_id, tail, canonical_bytes)
            .await?;
        let Ok(raw) = std::str::from_utf8(&canonical) else {
            // 无法解码的正文不能按可见处理；正文路径会返回 source_unavailable。
            return Ok(PrefixVisibility::Undetermined);
        };
        let Ok(payload) = deserialize_persisted_payload(raw) else {
            return Ok(PrefixVisibility::Undetermined);
        };
        Ok(if payload.displayable_to_client() {
            PrefixVisibility::Visible
        } else {
            PrefixVisibility::Hidden
        })
    }

    /// 有界前缀读取；错误保持原始领域语义（由 `map_resource_error` 决定重试含义）。
    async fn visibility_prefix(
        &self,
        thread_id: &ThreadId,
        source: &WindowSource,
        tail: Option<&HistoryCommittedTail>,
        entry_id: MessageId,
    ) -> Result<Vec<u8>, HistoryBackendError> {
        #[cfg(test)]
        {
            self.visibility_reads.lock().push(VISIBILITY_PREFIX_BYTES);
            if self.visibility_read_fails() {
                return Err(HistoryBackendError::BackendUnavailable);
            }
        }
        let chunk = self
            .load_body(
                thread_id,
                source,
                entry_id,
                0,
                VISIBILITY_PREFIX_BYTES,
                tail,
            )
            .await?;
        Ok(chunk.bytes)
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
        for update in dispatch::project_payload_updates(&item.payload, &self.caps) {
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
            // 页面阶段不读正文：占位版本不用于冲突检测；body chunk 的
            // contentVersion 是 canonical hash，客户端以它自校验。
            content_version: "1".to_owned(),
            durability,
            order,
            sub_items: vec![HistorySubItem::Opaque(HistoryOpaqueSubItem {
                sub_item_id,
                body: HistoryBodyDescriptor {
                    body_ref,
                    // 契约（见 `HistoryBodyDescriptor::total_bytes`）：descriptor 与 body
                    // 的 representation/mediaType 必须一致；`total_bytes` 在页面阶段只是
                    // canonical payload 的字节数（不读正文即可得到的上界），**不是**展示
                    // 文本长度。展示长度只在 body 首块给出，并由客户端用 body 响应的
                    // `totalBytes` 修正。
                    representation: HistoryRepresentation::DisplayText,
                    media_type: super::body::DISPLAY_MEDIA_TYPE.to_owned(),
                    total_bytes: DecimalU64::new(total_bytes),
                    chunk_bytes: self.chunk_bytes(),
                },
            })],
        }
    }
}

/// 前缀内无法确定条目身份/受众：整页失败，不发布越过该条目的成功页。
///
/// 复用 [`HistoryBackendError::SourceUnavailable`]（`recovery = Retry`）：读取本身可能
/// 是瞬态的，允许客户端重试；若重试后仍无法在 1024 字节前缀内确定，则继续失败关闭，
/// **不会**把条目静默当成隐藏，也不会提交跨过该条目的游标。
pub(super) fn undetermined_visibility(
    thread_id: &ThreadId,
    entry_id: MessageId,
) -> HistoryBackendError {
    tracing::warn!(
        session_id = thread_id.as_str(),
        entry_id = %entry_id.as_uuid(),
        "history v2 failed the page: entry visibility is undetermined within the bounded prefix"
    );
    HistoryBackendError::SourceUnavailable
}
