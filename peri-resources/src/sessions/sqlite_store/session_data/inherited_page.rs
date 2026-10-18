//! inherited 冻结前缀的窗口页读取。
//!
//! canonical 信封只按 [`InheritedLayout`] 的数组下标与原始 span 定位：条目身份、逻辑长度
//! 与正文都经同一个 blob 句柄增量读取，不执行 `json_each`/`length(CAST(...))`。非 canonical
//! 信封（`Unsupported`）保留原 SQL 语义作为唯一回退，不引入第二份 JSON 语义权威。

use super::blob_reader::{is_transient_blob_error, BlobReadError};
use super::history::{
    HISTORY_WINDOW_INHERITED_AFTER_SQL, HISTORY_WINDOW_INHERITED_ANCHOR_SQL,
    HISTORY_WINDOW_INHERITED_BEFORE_SQL, HISTORY_WINDOW_INHERITED_BODY_SQL,
    HISTORY_WINDOW_INHERITED_ROWID_SQL,
};
use super::history_window::{empty_window_page, window_anchor_failure};
use super::inherited_blob::{map_inherited_blob_error, InheritedFactsRead, InheritedLayout};
use super::*;
use peri_acp_types::history_page::HistoryPagePlan;
use peri_acp_types::history_window::{HistoryInheritedSnapshot, HistorySource, HistoryWindowError};
use peri_acp_types::session_resources::{
    HistoryWindowPagePlan, HistoryWindowRows, InheritedHistoryEntry,
};

impl SqliteSessionData {
    pub(super) async fn read_inherited_history_window_page(
        &self,
        id: &ThreadId,
        plan: &HistoryWindowPagePlan,
    ) -> SessionResourceResult<HistoryWindowRows> {
        let HistorySource::Inherited {
            frozen_snapshot_id,
            ancestor_thread_id,
        } = &plan.source
        else {
            return Err(SessionResourceError::new(
                SessionResourceErrorKind::InvalidInput {
                    detail: "inherited reader requires an inherited source".to_owned(),
                },
            ));
        };
        let mut connection = self
            .database
            .pool
            .acquire()
            .await
            .map_err(|error| map_sqlx(&error))?;
        // 冻结前缀是 write-once：事实先于页读取取得，canonical 路径不物化首尾正文。
        let facts = self
            .read_inherited_facts_cached(&mut connection, id)
            .await?;
        match facts {
            InheritedFactsRead::Facts {
                snapshot: Some(snapshot),
                layout: Some(layout),
            } => {
                verify_inherited_source(
                    &snapshot,
                    frozen_snapshot_id,
                    ancestor_thread_id.as_ref(),
                )?;
                self.read_inherited_page_from_layout(&mut connection, id, plan, &snapshot, &layout)
                    .await
            }
            InheritedFactsRead::Facts {
                snapshot: Some(_),
                layout: None,
            } => Err(corrupt("inherited layout is missing")),
            InheritedFactsRead::Facts { snapshot: None, .. } => {
                Err(HistoryWindowError::window_invalidated("inherited prefix is absent").into())
            }
            InheritedFactsRead::Unsupported => {
                self.read_inherited_page_sql(&mut connection, id, plan)
                    .await
            }
        }
    }

    /// canonical 路径：一次 blob 句柄完成锚点定位、双向候选长度与选中正文。
    async fn read_inherited_page_from_layout(
        &self,
        connection: &mut SqliteConnection,
        id: &ThreadId,
        plan: &HistoryWindowPagePlan,
        snapshot: &HistoryInheritedSnapshot,
        layout: &InheritedLayout,
    ) -> SessionResourceResult<HistoryWindowRows> {
        let anchor_key = match plan.anchor {
            Some(anchor) => Some(layout.position_of(anchor).ok_or_else(|| {
                window_anchor_failure(
                    plan,
                    "inherited history anchor is not present in this frozen snapshot",
                )
            })?),
            None => None,
        };
        let mut retried = false;
        loop {
            let mut tx = sqlx::Connection::begin(&mut *connection)
                .await
                .map_err(|error| map_sqlx(&error))?;
            self.require_session(&mut tx, id).await?;
            let row: Option<(i64,)> = sqlx::query_as(HISTORY_WINDOW_INHERITED_ROWID_SQL)
                .bind(id.as_str())
                .fetch_optional(&mut *tx)
                .await
                .map_err(|error| map_sqlx(&error))?;
            let Some((rowid,)) = row else {
                return Err(not_found());
            };
            let built = Self::inherited_blob_scope(&mut tx, rowid, |reader| {
                build_inherited_page(reader, id, plan, snapshot, layout, anchor_key)
            })
            .await?;
            tx.commit().await.map_err(|error| map_sqlx(&error))?;
            match built {
                Ok(rows) => return Ok(rows),
                // 冻结列被并发改写（write-once 不变量被绕过）：重试一次，再失败 fail-closed。
                Err(error) if !retried && is_transient_blob_error(&error) => {
                    retried = true;
                }
                Err(error) => return Err(map_inherited_blob_error(error)),
            }
        }
    }

    /// 非 canonical 信封的 SQL 回退：原语义（JSON1 定位 + `substr` 块读取）。
    async fn read_inherited_page_sql(
        &self,
        connection: &mut SqliteConnection,
        id: &ThreadId,
        plan: &HistoryWindowPagePlan,
    ) -> SessionResourceResult<HistoryWindowRows> {
        let HistorySource::Inherited {
            frozen_snapshot_id,
            ancestor_thread_id,
        } = &plan.source
        else {
            return Err(SessionResourceError::new(
                SessionResourceErrorKind::InvalidInput {
                    detail: "inherited reader requires an inherited source".to_owned(),
                },
            ));
        };
        let snapshot = Self::read_inherited_facts_sql(&mut *connection, id)
            .await?
            .ok_or_else(|| HistoryWindowError::window_invalidated("inherited prefix is absent"))?;
        verify_inherited_source(&snapshot, frozen_snapshot_id, ancestor_thread_id.as_ref())?;
        let mut tx = sqlx::Connection::begin(&mut *connection)
            .await
            .map_err(|error| map_sqlx(&error))?;
        self.require_session(&mut tx, id).await?;

        let anchor_key = match plan.anchor {
            Some(anchor) => {
                let key: Option<(i64,)> = sqlx::query_as(HISTORY_WINDOW_INHERITED_ANCHOR_SQL)
                    .bind(id.as_str())
                    .bind(anchor.as_uuid().to_string())
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|error| map_sqlx(&error))?;
                match key {
                    Some((key,)) => Some(key),
                    None => {
                        return Err(window_anchor_failure(
                            plan,
                            "inherited history anchor is not present in this frozen snapshot",
                        )
                        .into())
                    }
                }
            }
            None => None,
        };

        let before_plan = if plan.before_limit == 0 {
            None
        } else {
            let candidates: Vec<(i64, Option<String>, i64)> =
                sqlx::query_as(HISTORY_WINDOW_INHERITED_BEFORE_SQL)
                    .bind(id.as_str())
                    .bind(anchor_key)
                    .bind(i64::from(plan.include_anchor))
                    .bind((plan.before_limit + 1) as i64)
                    .fetch_all(&mut *tx)
                    .await
                    .map_err(|error| map_sqlx(&error))?;
            let candidates = decode_inherited_candidates(candidates)?;
            Some(HistoryPagePlan::plan(
                id,
                &snapshot.snapshot_id,
                candidates,
                plan.before_limit,
                plan.byte_budget,
            ))
        };
        let before_bytes = planned_bytes(before_plan.as_ref());
        let after_budget = plan.byte_budget.saturating_sub(before_bytes);
        let after_plan = match (plan.after_limit, anchor_key) {
            (0, _) | (_, None) => None,
            (limit, Some(anchor)) => {
                let candidates: Vec<(i64, Option<String>, i64)> =
                    sqlx::query_as(HISTORY_WINDOW_INHERITED_AFTER_SQL)
                        .bind(id.as_str())
                        .bind(anchor)
                        .bind(i64::from(plan.after_includes_anchor()))
                        .bind((limit + 1) as i64)
                        .fetch_all(&mut *tx)
                        .await
                        .map_err(|error| map_sqlx(&error))?;
                let candidates = decode_inherited_candidates(candidates)?;
                Some(HistoryPagePlan::plan_after(
                    id,
                    &snapshot.snapshot_id,
                    candidates,
                    limit,
                    after_budget,
                ))
            }
        };

        let (before_cursor_anchor, after_cursor_anchor) = inherited_boundary_cursors(
            before_plan.as_ref(),
            after_plan.as_ref(),
            snapshot.entry_count,
        );
        let before_rows =
            Self::read_inherited_bodies_sql(&mut tx, id, before_plan.as_ref()).await?;
        let after_rows = Self::read_inherited_bodies_sql(&mut tx, id, after_plan.as_ref()).await?;
        tx.commit().await.map_err(|error| map_sqlx(&error))?;

        assemble_inherited_rows(
            snapshot.snapshot_id,
            before_plan,
            before_rows,
            after_plan,
            after_rows,
            before_cursor_anchor,
            after_cursor_anchor,
        )
    }

    async fn read_inherited_bodies_sql(
        connection: &mut SqliteConnection,
        id: &ThreadId,
        plan: Option<&HistoryPagePlan>,
    ) -> SessionResourceResult<Vec<HistoryPageRow>> {
        let Some(plan) = plan else {
            return Ok(Vec::new());
        };
        let mut rows = Vec::new();
        for entry in plan
            .entries()
            .iter()
            .filter(|entry| entry.encoded_bytes <= plan.byte_budget())
        {
            let key = i64::try_from(entry.position)
                .map_err(|_| corrupt("inherited history index exceeds the supported range"))?;
            let content: Option<(String,)> = sqlx::query_as(HISTORY_WINDOW_INHERITED_BODY_SQL)
                .bind(id.as_str())
                .bind(key)
                .fetch_optional(&mut *connection)
                .await
                .map_err(|error| map_sqlx(&error))?;
            let content = content
                .map(|(content,)| content)
                .ok_or_else(|| corrupt("inherited history body is missing"))?;
            rows.push(HistoryPageRow::new(
                &entry.message_id.as_uuid().to_string(),
                content,
            )?);
        }
        Ok(rows)
    }
}

/// canonical 布局的页构造：所有读取共享同一个 blob 句柄。
fn build_inherited_page(
    reader: &super::blob_reader::BlobReader,
    id: &ThreadId,
    plan: &HistoryWindowPagePlan,
    snapshot: &HistoryInheritedSnapshot,
    layout: &InheritedLayout,
    anchor_key: Option<usize>,
) -> Result<HistoryWindowRows, BlobReadError> {
    let before_plan = if plan.before_limit == 0 {
        None
    } else {
        let candidates = layout_before(
            reader,
            layout,
            anchor_key,
            plan.include_anchor,
            plan.before_limit + 1,
        )?;
        Some(HistoryPagePlan::plan(
            id,
            &snapshot.snapshot_id,
            candidates,
            plan.before_limit,
            plan.byte_budget,
        ))
    };
    let after_budget = plan
        .byte_budget
        .saturating_sub(planned_bytes(before_plan.as_ref()));
    let after_plan = match (plan.after_limit, anchor_key) {
        (0, _) | (_, None) => None,
        (limit, Some(anchor)) => {
            let candidates = layout_after(
                reader,
                layout,
                anchor,
                plan.after_includes_anchor(),
                limit + 1,
            )?;
            Some(HistoryPagePlan::plan_after(
                id,
                &snapshot.snapshot_id,
                candidates,
                limit,
                after_budget,
            ))
        }
    };

    let (before_cursor_anchor, after_cursor_anchor) = inherited_boundary_cursors(
        before_plan.as_ref(),
        after_plan.as_ref(),
        snapshot.entry_count,
    );
    let before_rows = read_layout_bodies(reader, layout, before_plan.as_ref())?;
    let after_rows = read_layout_bodies(reader, layout, after_plan.as_ref())?;

    assemble_inherited_rows(
        snapshot.snapshot_id.clone(),
        before_plan,
        before_rows,
        after_plan,
        after_rows,
        before_cursor_anchor,
        after_cursor_anchor,
    )
    .map_err(|_| BlobReadError::Corrupt("inherited page assembly failed"))
}

fn layout_before(
    reader: &super::blob_reader::BlobReader,
    layout: &InheritedLayout,
    anchor: Option<usize>,
    include_anchor: bool,
    max: usize,
) -> Result<Vec<HistoryPageCandidate>, BlobReadError> {
    let mut candidates = Vec::new();
    let mut index = match anchor {
        None => layout.len().checked_sub(1),
        Some(anchor) if include_anchor => Some(anchor),
        Some(anchor) => anchor.checked_sub(1),
    };
    while let Some(current) = index {
        if candidates.len() >= max {
            break;
        }
        candidates.push(layout_candidate(reader, layout, current)?);
        index = current.checked_sub(1);
    }
    Ok(candidates)
}

fn layout_after(
    reader: &super::blob_reader::BlobReader,
    layout: &InheritedLayout,
    anchor: usize,
    include_anchor: bool,
    max: usize,
) -> Result<Vec<HistoryPageCandidate>, BlobReadError> {
    let mut candidates = Vec::new();
    let start = if include_anchor {
        anchor
    } else {
        anchor.saturating_add(1)
    };
    for index in start..layout.len() {
        if candidates.len() >= max {
            break;
        }
        candidates.push(layout_candidate(reader, layout, index)?);
    }
    Ok(candidates)
}

fn layout_candidate(
    reader: &super::blob_reader::BlobReader,
    layout: &InheritedLayout,
    index: usize,
) -> Result<HistoryPageCandidate, BlobReadError> {
    let message_id = layout
        .id_at(index)
        .ok_or(BlobReadError::Corrupt("inherited payload index is missing"))?;
    let position = u64::try_from(index).map_err(|_| {
        BlobReadError::Corrupt("inherited history index exceeds the supported range")
    })?;
    let encoded_bytes = usize::try_from(layout.logical_len(reader, index)?).map_err(|_| {
        BlobReadError::Corrupt("inherited history length exceeds the supported range")
    })?;
    Ok(HistoryPageCandidate {
        message_id,
        position,
        encoded_bytes,
    })
}

fn read_layout_bodies(
    reader: &super::blob_reader::BlobReader,
    layout: &InheritedLayout,
    plan: Option<&HistoryPagePlan>,
) -> Result<Vec<HistoryPageRow>, BlobReadError> {
    let Some(plan) = plan else {
        return Ok(Vec::new());
    };
    let mut rows = Vec::new();
    for entry in plan
        .entries()
        .iter()
        .filter(|entry| entry.encoded_bytes <= plan.byte_budget())
    {
        let index = usize::try_from(entry.position).map_err(|_| {
            BlobReadError::Corrupt("inherited history index exceeds the supported range")
        })?;
        if layout.id_at(index) != Some(entry.message_id) {
            return Err(BlobReadError::Corrupt(
                "inherited history body identity does not match the layout",
            ));
        }
        let content = String::from_utf8(layout.read_body(reader, index)?)
            .map_err(|_| BlobReadError::Corrupt("inherited history body is not valid UTF-8"))?;
        rows.push(HistoryPageRow {
            message_id: entry.message_id,
            content,
        });
    }
    Ok(rows)
}

fn planned_bytes(plan: Option<&HistoryPagePlan>) -> usize {
    plan.map(|plan| {
        plan.entries()
            .iter()
            .map(|entry| entry.encoded_bytes)
            .sum::<usize>()
    })
    .unwrap_or(0)
}

/// 继承区页行集的两端游标：锚点分别是本页最旧/最新条目，只有冻结快照内确实还有
/// 邻接条目时才铸造。`entry_count` 是冻结数组长度，方向无关，无需额外查询。
fn inherited_boundary_cursors(
    before_plan: Option<&HistoryPagePlan>,
    after_plan: Option<&HistoryPagePlan>,
    entry_count: usize,
) -> (Option<MessageId>, Option<MessageId>) {
    let mut oldest: Option<(u64, MessageId)> = None;
    let mut newest: Option<(u64, MessageId)> = None;
    for entry in before_plan
        .into_iter()
        .flat_map(HistoryPagePlan::entries)
        .chain(after_plan.into_iter().flat_map(HistoryPagePlan::entries))
    {
        if oldest.is_none_or(|(position, _)| entry.position < position) {
            oldest = Some((entry.position, entry.message_id));
        }
        if newest.is_none_or(|(position, _)| entry.position > position) {
            newest = Some((entry.position, entry.message_id));
        }
    }
    let (Some((oldest_position, oldest_id)), Some((newest_position, newest_id))) = (oldest, newest)
    else {
        return (None, None);
    };
    let has_older = oldest_position > 0;
    let has_newer = usize::try_from(newest_position)
        .ok()
        .is_some_and(|position| position.saturating_add(1) < entry_count);
    (
        has_older.then_some(oldest_id),
        has_newer.then_some(newest_id),
    )
}

fn decode_inherited_candidates(
    candidates: Vec<(i64, Option<String>, i64)>,
) -> SessionResourceResult<Vec<HistoryPageCandidate>> {
    candidates
        .into_iter()
        .map(|(position, message_id, encoded_bytes)| {
            let message_id =
                message_id.ok_or_else(|| corrupt("inherited history entry id is missing"))?;
            HistoryPageCandidate::new(&message_id, position, encoded_bytes)
        })
        .collect()
}

/// 把双向计划装配成窗口行；加载项与 descriptor 的顺序与原实现一致。
fn assemble_inherited_rows(
    snapshot_id: String,
    before_plan: Option<HistoryPagePlan>,
    before_rows: Vec<HistoryPageRow>,
    after_plan: Option<HistoryPagePlan>,
    after_rows: Vec<HistoryPageRow>,
    before_cursor_anchor: Option<MessageId>,
    after_cursor_anchor: Option<MessageId>,
) -> SessionResourceResult<HistoryWindowRows> {
    let (before_page, before_oversized) = match before_plan {
        Some(plan) => plan.materialize_omitting_oversize(before_rows)?,
        None => (empty_window_page(), Vec::new()),
    };
    let (after_page, after_oversized) = match after_plan {
        Some(plan) => plan.materialize_omitting_oversize(after_rows)?,
        None => (empty_window_page(), Vec::new()),
    };
    let mut inherited_entries: Vec<InheritedHistoryEntry> = before_page
        .items
        .into_iter()
        .map(InheritedHistoryEntry::Loaded)
        .collect();
    inherited_entries.extend(before_oversized.into_iter().map(oversized_entry));
    inherited_entries.extend(
        after_page
            .items
            .into_iter()
            .map(InheritedHistoryEntry::Loaded),
    );
    inherited_entries.extend(after_oversized.into_iter().map(oversized_entry));
    Ok(HistoryWindowRows {
        committed_tail: None,
        entries: Vec::new(),
        inherited_entries,
        inherited_snapshot_id: Some(snapshot_id),
        before_cursor_anchor,
        after_cursor_anchor,
    })
}

fn oversized_entry(entry: HistoryPageCandidate) -> InheritedHistoryEntry {
    InheritedHistoryEntry::Oversized {
        entry_id: entry.message_id,
        total_bytes: entry.encoded_bytes as u64,
    }
}

fn verify_inherited_source(
    snapshot: &HistoryInheritedSnapshot,
    frozen_snapshot_id: &str,
    ancestor_thread_id: Option<&ThreadId>,
) -> SessionResourceResult<()> {
    if snapshot.snapshot_id != frozen_snapshot_id {
        return Err(HistoryWindowError::SnapshotMismatch {
            detail: "inherited snapshot id no longer matches the frozen prefix".into(),
        }
        .into());
    }
    if let Some(expected) = ancestor_thread_id {
        if snapshot.ancestor_thread_id.as_ref() != Some(expected) {
            return Err(HistoryWindowError::SnapshotMismatch {
                detail: "inherited ancestor identity does not match".into(),
            }
            .into());
        }
    }
    Ok(())
}
