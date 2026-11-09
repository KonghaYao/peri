//! open/seek 的固定快照重取与初始窗口组装。

use peri_acp_types::history_common::{HistorySource as WindowSource, HistoryWindowDirection};
use peri_acp_types::history_v2::{
    HistoryBackendError, HistoryEntryAnchor, HistoryOpenedSnapshot, HistorySource as V2Source,
};
use peri_acp_types::session_resources::HistoryWindowPagePlan;
use peri_acp_types::thread::ThreadId;

use super::refs::{encode_inherited_ref, encode_own_ref, InheritedRef};
use super::{
    anchor_limits, inherited_seek_plan, parse_entry_id, tail_refs, HistoryV2BackendAdapter,
    PageParts,
};

impl HistoryV2BackendAdapter {
    pub(super) async fn open_latest(
        &self,
        thread_id: &ThreadId,
        before: usize,
        target_bytes: usize,
    ) -> Result<HistoryOpenedSnapshot, HistoryBackendError> {
        let bounds = self.bounds(thread_id).await?;
        let inherited = Self::inherited_ref(&bounds)?;
        let own_plan = HistoryWindowPagePlan::open(
            WindowSource::Own,
            None,
            HistoryWindowDirection::Before,
            before,
            0,
            target_bytes,
        );
        let own_rows = self.load_rows(thread_id, &own_plan).await?;
        let own = self.own_view(thread_id, own_rows).await?;
        let own_oldest = own.oldest;
        let own_entries_empty = own.entries.is_empty();

        let inherited_view = if !own.before_more && own.raw_count < before {
            match inherited.as_ref() {
                Some(source) => {
                    let remaining = before - own.raw_count;
                    let budget = target_bytes.saturating_sub(own.used_bytes);
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
                        let view = self.inherited_view(thread_id, source, rows).await?;
                        Some((source.clone(), view))
                    }
                }
                None => None,
            }
        } else {
            None
        };

        // 页内顺序固定为 inherited → own；before 游标必须指向本页最旧条目（已消费），
        // 否则下一页会重复或跳过 inherited 边界。
        let before_cursor = if own.before_more {
            own.before_anchor.map(encode_own_ref)
        } else if let Some((source, view)) = inherited_view.as_ref() {
            view.before_anchor
                .map(|entry_id| encode_inherited_ref(source, entry_id))
        } else if inherited.is_some() {
            own_oldest.map(encode_own_ref)
        } else {
            None
        };

        let (entries, after_more) = match inherited_view {
            Some((_, view)) => {
                let after_more = if own_entries_empty {
                    view.after_more
                } else {
                    own.after_more
                };
                let mut entries = view.entries;
                entries.extend(own.entries);
                (entries, after_more)
            }
            None => {
                let after_more = own.after_more;
                (own.entries, after_more)
            }
        };

        let (committed_tail, history_revision) =
            tail_refs(thread_id.as_str(), own.committed_tail.as_ref());
        let before_more = before_cursor.is_some();
        let page = self.assemble_page(PageParts {
            entries,
            before_cursor,
            after_cursor: None,
            before_more,
            after_more,
            committed_tail,
            history_revision,
        });
        Ok(HistoryOpenedSnapshot {
            history_revision: page.history_revision,
            committed_tail: page.committed_tail.clone(),
            projection_version: page.projection_version,
            page,
        })
    }

    pub(super) async fn open_entry(
        &self,
        thread_id: &ThreadId,
        anchor: &HistoryEntryAnchor,
        before: u32,
        after: u32,
        target_bytes: usize,
    ) -> Result<HistoryOpenedSnapshot, HistoryBackendError> {
        let entry_id = parse_entry_id(&anchor.entry_id, thread_id)?;
        let (before_limit, after_limit) = anchor_limits(before, after);
        match &anchor.source {
            V2Source::Own => {
                let plan = HistoryWindowPagePlan::open(
                    WindowSource::Own,
                    Some(entry_id),
                    HistoryWindowDirection::Around,
                    before_limit,
                    after_limit,
                    target_bytes,
                );
                let own_rows = self.load_rows(thread_id, &plan).await?;
                let own = self.own_view(thread_id, own_rows).await?;
                let inherited = if own.before_more {
                    None
                } else {
                    Self::inherited_ref(&self.bounds(thread_id).await?)?
                };
                let inherited_present = inherited.is_some();
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
                let page = self.assemble_page(PageParts {
                    entries: own.entries,
                    before_cursor: before_cursor.clone(),
                    after_cursor: after_cursor.clone(),
                    before_more: before_cursor.is_some(),
                    after_more: after_cursor.is_some(),
                    committed_tail,
                    history_revision,
                });
                Ok(HistoryOpenedSnapshot {
                    history_revision: page.history_revision,
                    committed_tail: page.committed_tail.clone(),
                    projection_version: page.projection_version,
                    page,
                })
            }
            V2Source::Inherited(source) => {
                let source =
                    InheritedRef::from_v2(source).ok_or(HistoryBackendError::SourceUnavailable)?;
                let bounds = self.bounds(thread_id).await?;
                let plan =
                    inherited_seek_plan(&source, entry_id, before_limit, after_limit, target_bytes);
                let rows = self.load_rows(thread_id, &plan).await?;
                let view = self.inherited_view(thread_id, &source, rows).await?;
                let own_present = bounds.own_latest.is_some();
                let before_cursor = view
                    .before_anchor
                    .map(|entry_id| encode_inherited_ref(&source, entry_id));
                // inherited 之后紧接 own：本区没有更新的 inherited 时，用本页最新
                // inherited 条目（已消费）作为 after 游标，下一页才能返回 own 首条。
                let after_cursor = if let Some(anchor_id) = view.after_anchor {
                    Some(encode_inherited_ref(&source, anchor_id))
                } else if own_present {
                    Some(encode_inherited_ref(
                        &source,
                        view.newest.unwrap_or(entry_id),
                    ))
                } else {
                    None
                };
                let (committed_tail, history_revision) =
                    tail_refs(thread_id.as_str(), bounds.own_latest.as_ref());
                let page = self.assemble_page(PageParts {
                    entries: view.entries,
                    before_cursor: before_cursor.clone(),
                    after_cursor: after_cursor.clone(),
                    before_more: before_cursor.is_some(),
                    after_more: after_cursor.is_some(),
                    committed_tail,
                    history_revision,
                });
                Ok(HistoryOpenedSnapshot {
                    history_revision: page.history_revision,
                    committed_tail: page.committed_tail.clone(),
                    projection_version: page.projection_version,
                    page,
                })
            }
        }
    }
}
