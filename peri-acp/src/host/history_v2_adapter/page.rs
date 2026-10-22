//! page/seek 的跨区组合与有界正文读取。

use peri_acp_types::history_v2::{
    HistoryBackendError, HistoryDirection, HistoryEntryAnchor, HistoryPageData, HistorySnapshotRef,
};
use peri_acp_types::history_window::{HistorySource as WindowSource, HistoryWindowDirection};
use peri_acp_types::messages::MessageId;
use peri_acp_types::session_resources::{
    HistoryWindowPagePlan, HistoryWindowReadIntent, HistoryWindowTailExpectation,
};
use peri_acp_types::thread::ThreadId;

use super::refs::{encode_inherited_ref, encode_own_ref, InheritedRef};
use super::{
    anchor_limits, inherited_seek_plan, parse_entry_id, snapshot_tail, tail_refs, target_bytes,
    HistoryV2BackendAdapter, PageParts,
};

impl HistoryV2BackendAdapter {
    pub(super) async fn page_own(
        &self,
        thread_id: &ThreadId,
        snapshot: &HistorySnapshotRef,
        anchor: MessageId,
        direction: HistoryDirection,
        limit: u32,
    ) -> Result<HistoryPageData, HistoryBackendError> {
        let tail = snapshot_tail(snapshot, thread_id)?;
        let window_direction = match direction {
            HistoryDirection::Before => HistoryWindowDirection::Before,
            HistoryDirection::After => HistoryWindowDirection::After,
        };
        let plan = HistoryWindowPagePlan::page(
            WindowSource::Own,
            anchor,
            window_direction,
            limit as usize,
            target_bytes(snapshot)?,
            tail,
        );
        let own_rows = self.load_rows(thread_id, &plan).await?;
        let own = self.own_view(thread_id, own_rows).await;
        let own_oldest = own.oldest;

        // 只有 before 方向会在 own 头之后继续进入 inherited；after 方向 own 已是逻辑尾部。
        let inherited_view = if window_direction == HistoryWindowDirection::Before
            && !own.before_more
            && own.raw_count < limit as usize
        {
            let bounds = self.bounds(thread_id).await?;
            match Self::inherited_ref(&bounds)? {
                Some(source) => {
                    let remaining = limit as usize - own.raw_count;
                    let budget = target_bytes(snapshot)?.saturating_sub(own.used_bytes);
                    if budget == 0 {
                        None
                    } else {
                        let plan = HistoryWindowPagePlan::open(
                            source.window_source(),
                            None,
                            HistoryWindowDirection::Before,
                            remaining,
                            0,
                            budget,
                        );
                        let rows = self.load_rows(thread_id, &plan).await?;
                        let view = self.inherited_view(thread_id, &source, rows).await;
                        Some((source, view))
                    }
                }
                None => None,
            }
        } else {
            None
        };

        let before_cursor = if let Some((source, view)) = inherited_view.as_ref() {
            // inherited 是最旧 region：其 before 边界即全局 before 边界。
            view.before_anchor
                .map(|entry_id| encode_inherited_ref(source, entry_id))
        } else if let Some(anchor_id) = own.before_anchor {
            Some(encode_own_ref(anchor_id))
        } else if self.inherited_tail_cursor(thread_id).await.is_some() {
            // own 头已到但本页未纳入 inherited：用本页最旧 own（已消费）作为游标，
            // 下一页 before 才能跨入 inherited 而非跳过其末条。
            Some(encode_own_ref(own_oldest.unwrap_or(anchor)))
        } else {
            None
        };

        let after_cursor = if own.raw_count > 0 {
            own.after_anchor.map(encode_own_ref)
        } else if let Some((source, view)) = inherited_view.as_ref() {
            // 本页只有 inherited：own 首条尚未返回，用本页最新 inherited（已消费）
            // 或输入 own 锚点（已消费）作为 after 游标。
            view.after_anchor
                .map(|entry_id| encode_inherited_ref(source, entry_id))
                .or_else(|| Some(encode_own_ref(anchor)))
        } else {
            None
        };

        let entries = if let Some((_, view)) = inherited_view {
            let mut entries = view.entries;
            entries.extend(own.entries);
            entries
        } else {
            own.entries
        };
        let (committed_tail, history_revision) =
            tail_refs(thread_id.as_str(), own.committed_tail.as_ref());
        Ok(self.assemble_page(PageParts {
            entries,
            before_cursor: before_cursor.clone(),
            after_cursor: after_cursor.clone(),
            before_more: before_cursor.is_some(),
            after_more: after_cursor.is_some(),
            committed_tail,
            history_revision,
        }))
    }

    pub(super) async fn page_inherited(
        &self,
        thread_id: &ThreadId,
        snapshot: &HistorySnapshotRef,
        source: &InheritedRef,
        anchor: MessageId,
        direction: HistoryDirection,
        limit: u32,
    ) -> Result<HistoryPageData, HistoryBackendError> {
        self.ensure_snapshot_tail(thread_id, snapshot).await?;
        let bounds = self.bounds(thread_id).await?;
        let own_present = bounds.own_latest.is_some();
        let window_direction = match direction {
            HistoryDirection::Before => HistoryWindowDirection::Before,
            HistoryDirection::After => HistoryWindowDirection::After,
        };
        let plan = HistoryWindowPagePlan::page(
            source.window_source(),
            anchor,
            window_direction,
            limit as usize,
            target_bytes(snapshot)?,
            None,
        );
        let rows = self.load_rows(thread_id, &plan).await?;
        let view = self.inherited_view(thread_id, source, rows).await;
        let view_before = view.before_anchor;
        let view_after = view.after_anchor;
        let view_newest = view.newest;

        let own_fill = if window_direction == HistoryWindowDirection::After
            && !view.after_more
            && view.raw_count < limit as usize
            && own_present
        {
            let remaining = limit as usize - view.raw_count;
            let budget = target_bytes(snapshot)?.saturating_sub(view.used_bytes);
            match (bounds.own_oldest, budget) {
                (Some(own_oldest), budget) if budget > 0 => {
                    let plan = HistoryWindowPagePlan {
                        intent: HistoryWindowReadIntent::Page,
                        source: WindowSource::Own,
                        anchor: Some(own_oldest),
                        direction: HistoryWindowDirection::After,
                        include_anchor: true,
                        before_limit: 0,
                        after_limit: remaining,
                        byte_budget: budget,
                        tail: HistoryWindowTailExpectation::Fixed(snapshot_tail(
                            snapshot, thread_id,
                        )?),
                    };
                    let rows = self.load_rows(thread_id, &plan).await?;
                    Some(self.own_view(thread_id, rows).await)
                }
                _ => None,
            }
        } else {
            None
        };

        // before 游标指向本页最旧条目（已消费）。页内只有 own 时，游标取 own 最旧
        // 条目，下一页 before 才会跨回 inherited 而不是跳过其末条。
        let before_cursor = if view.entries.is_empty() {
            match own_fill.as_ref().and_then(|own| own.oldest) {
                Some(own_oldest) => Some(encode_own_ref(own_oldest)),
                None => Some(encode_inherited_ref(source, anchor)),
            }
        } else {
            view_before.map(|entry_id| encode_inherited_ref(source, entry_id))
        };
        let after_cursor = if let Some(own) = own_fill.as_ref() {
            own.after_anchor.map(encode_own_ref)
        } else if let Some(anchor_id) = view_after {
            Some(encode_inherited_ref(source, anchor_id))
        } else if own_present {
            // 冻结前缀已读完，下一段是 own：用本页最新 inherited（已消费）
            // 或输入锚点（已消费）续页，避免跳过 own 首条。
            Some(encode_inherited_ref(source, view_newest.unwrap_or(anchor)))
        } else {
            None
        };

        let entries = if let Some(own) = own_fill {
            let mut entries = view.entries;
            entries.extend(own.entries);
            entries
        } else {
            view.entries
        };
        Ok(self.assemble_page(PageParts {
            entries,
            before_cursor: before_cursor.clone(),
            after_cursor: after_cursor.clone(),
            before_more: before_cursor.is_some(),
            after_more: after_cursor.is_some(),
            committed_tail: snapshot.committed_tail.clone(),
            history_revision: snapshot.history_revision,
        }))
    }

    pub(super) async fn seek_own(
        &self,
        thread_id: &ThreadId,
        snapshot: &HistorySnapshotRef,
        anchor: &HistoryEntryAnchor,
        before: u32,
        after: u32,
    ) -> Result<HistoryPageData, HistoryBackendError> {
        let entry_id = parse_entry_id(&anchor.entry_id, thread_id)?;
        let tail =
            snapshot_tail(snapshot, thread_id)?.ok_or(HistoryBackendError::AnchorUnavailable)?;
        let (before_limit, after_limit) = anchor_limits(before, after);
        let plan = HistoryWindowPagePlan::seek(
            WindowSource::Own,
            entry_id,
            before_limit,
            after_limit,
            target_bytes(snapshot)?,
            tail,
        );
        let own_rows = self.load_rows(thread_id, &plan).await?;
        let own = self.own_view(thread_id, own_rows).await;
        let inherited_present = if own.before_more {
            false
        } else {
            Self::inherited_ref(&self.bounds(thread_id).await?)?.is_some()
        };
        let before_cursor = if let Some(anchor) = own.before_anchor {
            Some(encode_own_ref(anchor))
        } else if inherited_present {
            Some(encode_own_ref(own.oldest.unwrap_or(entry_id)))
        } else {
            None
        };
        let after_cursor = own.after_anchor.map(encode_own_ref);
        let (committed_tail, history_revision) =
            tail_refs(thread_id.as_str(), own.committed_tail.as_ref());
        Ok(self.assemble_page(PageParts {
            entries: own.entries,
            before_cursor: before_cursor.clone(),
            after_cursor: after_cursor.clone(),
            before_more: before_cursor.is_some(),
            after_more: after_cursor.is_some(),
            committed_tail,
            history_revision,
        }))
    }

    pub(super) async fn seek_inherited(
        &self,
        thread_id: &ThreadId,
        snapshot: &HistorySnapshotRef,
        source: &InheritedRef,
        anchor: &HistoryEntryAnchor,
        before: u32,
        after: u32,
    ) -> Result<HistoryPageData, HistoryBackendError> {
        self.ensure_snapshot_tail(thread_id, snapshot).await?;
        let entry_id = parse_entry_id(&anchor.entry_id, thread_id)?;
        let (before_limit, after_limit) = anchor_limits(before, after);
        let bounds = self.bounds(thread_id).await?;
        let own_present = bounds.own_latest.is_some();
        let plan = inherited_seek_plan(
            source,
            entry_id,
            before_limit,
            after_limit,
            target_bytes(snapshot)?,
        );
        let rows = self.load_rows(thread_id, &plan).await?;
        let view = self.inherited_view(thread_id, source, rows).await;
        let before_cursor = view
            .before_anchor
            .map(|entry_id| encode_inherited_ref(source, entry_id));
        let after_cursor = if let Some(anchor_id) = view.after_anchor {
            Some(encode_inherited_ref(source, anchor_id))
        } else if own_present {
            Some(encode_inherited_ref(
                source,
                view.newest.unwrap_or(entry_id),
            ))
        } else {
            None
        };
        Ok(self.assemble_page(PageParts {
            entries: view.entries,
            before_cursor: before_cursor.clone(),
            after_cursor: after_cursor.clone(),
            before_more: before_cursor.is_some(),
            after_more: after_cursor.is_some(),
            committed_tail: snapshot.committed_tail.clone(),
            history_revision: snapshot.history_revision,
        }))
    }

    async fn inherited_tail_cursor(&self, thread_id: &ThreadId) -> Option<String> {
        let bounds = self.bounds(thread_id).await.ok()?;
        let source = Self::inherited_ref(&bounds).ok()??;
        let last = bounds.inherited.snapshot()?.last_entry?;
        Some(encode_inherited_ref(&source, last))
    }
}
