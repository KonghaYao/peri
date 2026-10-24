//! `peri.historyWindowV1` 的 SQLite 有界读取：边界、双向 keyset、正文块。
//!
//! SQL 原文与旧分页/段读取同处 `session_data/history.rs`；本文件只做解码、事务编排与
//! 失败分类。任何读取都不得退化成「先全量取回再过滤」。

use super::blob_reader::{is_transient_blob_error, BlobReadError};
use super::history::{
    read_content_length, read_window_candidates, HISTORY_WINDOW_AFTER_SQL,
    HISTORY_WINDOW_ANCHOR_SQL, HISTORY_WINDOW_BEFORE_SQL, HISTORY_WINDOW_BODY_ROWID_SQL,
    HISTORY_WINDOW_BOUNDS_SQL, HISTORY_WINDOW_CHECKPOINT_FAST_SQL, HISTORY_WINDOW_CHECKPOINT_SQL,
    HISTORY_WINDOW_ENTRY_METADATA_SQL, HISTORY_WINDOW_INHERITED_ANCHOR_SQL,
    HISTORY_WINDOW_INHERITED_LENGTH_SQL, HISTORY_WINDOW_INHERITED_ROWID_SQL,
    HISTORY_WINDOW_LATEST_SQL, HISTORY_WINDOW_NEWER_EXISTS_SQL, HISTORY_WINDOW_OLDER_EXISTS_SQL,
    HISTORY_WINDOW_TAIL_CHECK_SQL,
};
use super::inherited_blob::{map_inherited_blob_error, InheritedFactsRead, InheritedSummaryRead};
use super::inherited_facts::{InheritedLayout, InheritedSummary};
use super::inherited_page::verify_inherited_source;
use super::*;
use peri_acp_types::history_common::{
    HistoryBodyChunk, HistoryCheckpoint, HistoryCommittedTail, HistoryEntryId,
    HistoryInheritedSnapshot, HistoryInheritedWindow, HistoryReadTicket, HistorySource,
    HistoryWindowError, HISTORY_WINDOW_MAX_BYTE_BUDGET, HISTORY_WINDOW_MAX_CHUNK_SIZE,
    HISTORY_WINDOW_MAX_LIMIT,
};
use peri_acp_types::history_page::HistoryEntryRole;
use peri_acp_types::session_resources::{
    HistoryEntryMetadata, HistoryWindowEntry, HistoryWindowPagePlan, HistoryWindowReadIntent,
    HistoryWindowRows, HistoryWindowTailExpectation, SessionHistoryWindowBounds,
};
use sha2::{Digest, Sha256};

/// `HISTORY_WINDOW_BOUNDS_SQL` 的行形状：(最旧 id, 最新 id, 最新 position)。
type HistoryWindowBoundsRow = (Option<String>, Option<String>, Option<i64>);

// ─── 窗口读取（peri.historyWindowV1） ─────────────────────────────────────────

impl SqliteSessionData {
    /// 按稳定条目身份 seek：构造双向计划后复用同一份有界读取。
    #[allow(clippy::too_many_arguments)] // 与 trait 契约同口径，逐项显式传递
    pub(super) async fn read_history_window_seek(
        &self,
        id: &ThreadId,
        source: HistorySource,
        anchor: MessageId,
        before_limit: usize,
        after_limit: usize,
        byte_budget: usize,
        committed_tail: &HistoryCommittedTail,
    ) -> SessionResourceResult<HistoryWindowRows> {
        let plan = HistoryWindowPagePlan::seek(
            source,
            anchor,
            before_limit,
            after_limit,
            byte_budget,
            committed_tail.clone(),
        );
        self.read_history_window_page(id, &plan).await
    }

    /// 一次有界读取窗口边界：own 最旧/最新身份、durable tail、可重建 checkpoint、
    /// 冻结前缀事实。边界语句不取 `content`，也不触碰 `inherited_context`
    /// （由增量 blob 扫描独立给出）；会话不存在是 `NotFound`。
    pub(super) async fn read_history_window_bounds(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<SessionHistoryWindowBounds> {
        let mut connection = self
            .database
            .pool
            .acquire()
            .await
            .map_err(|error| map_sqlx(&error))?;
        // inherited 是 write-once 冻结列：bounds 只需要轻量摘要（首尾身份/计数/字节），
        // 不构造全量布局。非 canonical 信封回退原 SQL 路径。
        let inherited_snapshot = match self
            .read_inherited_summary_cached(&mut connection, id)
            .await?
        {
            InheritedSummaryRead::Facts { snapshot } => snapshot,
            InheritedSummaryRead::Unsupported => {
                Self::read_inherited_facts_sql(&mut connection, id).await?
            }
        };
        let mut tx = sqlx::Connection::begin(&mut *connection)
            .await
            .map_err(|error| map_sqlx(&error))?;
        self.require_session(&mut tx, id).await?;
        let row: Option<HistoryWindowBoundsRow> = sqlx::query_as(HISTORY_WINDOW_BOUNDS_SQL)
            .bind(id.as_str())
            .fetch_optional(&mut *tx)
            .await
            .map_err(|error| map_sqlx(&error))?;
        let (oldest, latest, latest_position) = row.ok_or_else(not_found)?;
        let checkpoint: Option<(String, i64)> = sqlx::query_as(HISTORY_WINDOW_CHECKPOINT_FAST_SQL)
            .bind(id.as_str())
            .fetch_optional(&mut *tx)
            .await
            .map_err(|error| map_sqlx(&error))?;
        // 快路径只读最新归档行；它给不出有效指针时（legacy/悬挂/损坏）才回到全归档推导。
        let checkpoint = match checkpoint {
            Some(row) => Some(row),
            None => sqlx::query_as(HISTORY_WINDOW_CHECKPOINT_SQL)
                .bind(id.as_str())
                .fetch_optional(&mut *tx)
                .await
                .map_err(|error| map_sqlx(&error))?,
        };
        tx.commit().await.map_err(|error| map_sqlx(&error))?;

        let own_oldest = oldest.as_deref().map(message_id_of).transpose()?;
        let own_latest = match (latest.as_deref(), latest_position) {
            (Some(entry), Some(position)) => Some(HistoryCommittedTail::new(
                id,
                message_id_of(entry)?,
                u64::try_from(position)
                    .map_err(|_| corrupt("history window tail position is negative"))?,
            )),
            (None, None) => None,
            _ => return Err(corrupt("history window bounds are inconsistent")),
        };
        let checkpoint = checkpoint
            .map(|(entry, position)| {
                let message_id = message_id_of(&entry)?;
                let position = u64::try_from(position)
                    .map_err(|_| corrupt("history checkpoint position is negative"))?;
                Ok::<_, SessionResourceError>(HistoryCheckpoint {
                    checkpoint_id: HistoryEntryId::new(id, message_id).encode(),
                    summary_entry_id: Some(message_id),
                    position,
                })
            })
            .transpose()?;
        Ok(SessionHistoryWindowBounds {
            own_oldest,
            own_latest,
            checkpoint,
            inherited: match inherited_snapshot {
                Some(snapshot) => HistoryInheritedWindow::Present(snapshot),
                None => HistoryInheritedWindow::Absent,
            },
        })
    }

    /// 有界读取窗口页：固定 tail 复核、锚点身份复核、before/after keyset 候选、
    /// 只取选中行的正文。所有事实在同一只读事务内完成；超预算条目只返回 descriptor
    /// 候选，不取正文。
    pub(super) async fn read_history_window_page(
        &self,
        id: &ThreadId,
        plan: &HistoryWindowPagePlan,
    ) -> SessionResourceResult<HistoryWindowRows> {
        validate_history_window_plan(plan)?;
        if !plan.source.is_own() {
            return self.read_inherited_history_window_page(id, plan).await;
        }
        let mut connection = self
            .database
            .pool
            .acquire()
            .await
            .map_err(|error| map_sqlx(&error))?;
        let mut tx = sqlx::Connection::begin(&mut *connection)
            .await
            .map_err(|error| map_sqlx(&error))?;
        self.require_session(&mut tx, id).await?;

        let tail = match &plan.tail {
            HistoryWindowTailExpectation::Current => Self::read_window_tail(&mut tx, id).await?,
            HistoryWindowTailExpectation::Fixed(None) => None,
            HistoryWindowTailExpectation::Fixed(Some(expected)) => {
                if expected.thread_id != *id {
                    return Err(HistoryWindowError::CrossSession {
                        expected: id.clone(),
                        actual: expected.thread_id.clone(),
                    }
                    .into());
                }
                let exists: Option<(i64,)> = sqlx::query_as(HISTORY_WINDOW_TAIL_CHECK_SQL)
                    .bind(id.as_str())
                    .bind(expected.entry_id.as_uuid().to_string())
                    .bind(i64::try_from(expected.position).map_err(|_| {
                        corrupt("history window tail position exceeds the supported range")
                    })?)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|error| map_sqlx(&error))?;
                if exists.is_none() {
                    return Err(HistoryWindowError::window_invalidated(
                        "committed tail no longer matches the snapshot",
                    )
                    .into());
                }
                Some(expected.clone())
            }
        };
        let Some(tail) = tail else {
            tx.commit().await.map_err(|error| map_sqlx(&error))?;
            return Ok(HistoryWindowRows {
                committed_tail: None,
                entries: Vec::new(),
                inherited_entries: Vec::new(),
                inherited_snapshot_id: None,
                before_cursor_anchor: None,
                after_cursor_anchor: None,
            });
        };
        let tail_position = i64::try_from(tail.position)
            .map_err(|_| corrupt("history window tail position exceeds the supported range"))?;

        let anchor_position = match plan.anchor {
            Some(anchor) => {
                let row: Option<(i64,)> = sqlx::query_as(HISTORY_WINDOW_ANCHOR_SQL)
                    .bind(id.as_str())
                    .bind(anchor.as_uuid().to_string())
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|error| map_sqlx(&error))?;
                let Some((position,)) = row else {
                    return Err(window_anchor_failure(
                        plan,
                        "history anchor is not present in this snapshot",
                    )
                    .into());
                };
                let position = u64::try_from(position)
                    .map_err(|_| corrupt("history anchor position is negative"))?;
                if position > tail.position {
                    return Err(window_anchor_failure(
                        plan,
                        "history anchor is outside the fixed snapshot",
                    )
                    .into());
                }
                Some(position)
            }
            None => None,
        };

        let anchor_position_i64 = anchor_position
            .map(|position| {
                i64::try_from(position)
                    .map_err(|_| corrupt("history anchor position exceeds the supported range"))
            })
            .transpose()?;

        let before_plan = if plan.before_limit == 0 {
            None
        } else {
            let candidates: Vec<(String, i64, String)> = sqlx::query_as(HISTORY_WINDOW_BEFORE_SQL)
                .bind(id.as_str())
                .bind(tail_position)
                .bind(anchor_position_i64)
                .bind(i64::from(plan.include_anchor))
                .bind((plan.before_limit + 1) as i64)
                .fetch_all(&mut *tx)
                .await
                .map_err(|error| map_sqlx(&error))?;
            let candidates = read_window_candidates(&mut tx, candidates).await?;
            Some(HistoryPagePlan::plan(
                candidates,
                plan.before_limit,
                plan.byte_budget,
            ))
        };
        let before_bytes = before_plan
            .as_ref()
            .map(|plan| {
                plan.entries()
                    .iter()
                    .map(|entry| entry.encoded_bytes)
                    .sum::<usize>()
            })
            .unwrap_or(0);
        let after_budget = plan.byte_budget.saturating_sub(before_bytes);
        let after_plan = match (plan.after_limit, anchor_position_i64) {
            (0, _) | (_, None) => None,
            (limit, Some(anchor)) => {
                let candidates: Vec<(String, i64, String)> =
                    sqlx::query_as(HISTORY_WINDOW_AFTER_SQL)
                        .bind(id.as_str())
                        .bind(tail_position)
                        .bind(anchor)
                        .bind(i64::from(plan.after_includes_anchor()))
                        .bind((limit + 1) as i64)
                        .fetch_all(&mut *tx)
                        .await
                        .map_err(|error| map_sqlx(&error))?;
                let candidates = read_window_candidates(&mut tx, candidates).await?;
                Some(HistoryPagePlan::plan_after(candidates, limit, after_budget))
            }
        };

        // 两个游标都锚定**本页**行集的两端，并由固定 snapshot 内的邻接存在性决定是否
        // 铸造。不能只看请求方向：更早页也必须暴露更晚方向的内侧锚点，否则淘汰尾段后
        // 无法向下补回。
        let (before_cursor_anchor, after_cursor_anchor) = window_boundary_cursors(
            &mut tx,
            id,
            before_plan.as_ref(),
            after_plan.as_ref(),
            tail_position,
        )
        .await?;
        let before_body_ids = window_body_ids(before_plan.as_ref());
        let after_body_ids = window_body_ids(after_plan.as_ref());
        let before_rows = Self::read_page_bodies(&mut tx, id, &before_body_ids).await?;
        let after_rows = Self::read_page_bodies(&mut tx, id, &after_body_ids).await?;
        tx.commit().await.map_err(|error| map_sqlx(&error))?;

        let (before_page, before_oversized) = match before_plan {
            Some(plan) => plan.materialize_omitting_oversize(before_rows)?,
            None => (empty_window_page(), Vec::new()),
        };
        let (after_page, after_oversized) = match after_plan {
            Some(plan) => plan.materialize_omitting_oversize(after_rows)?,
            None => (empty_window_page(), Vec::new()),
        };
        let mut entries = Vec::new();
        entries.extend(
            before_page
                .items
                .into_iter()
                .map(HistoryWindowEntry::Loaded),
        );
        entries.extend(window_oversized_entries(before_oversized));
        entries.extend(after_page.items.into_iter().map(HistoryWindowEntry::Loaded));
        entries.extend(window_oversized_entries(after_oversized));
        entries.sort_by_key(HistoryWindowEntry::position);
        Ok(HistoryWindowRows {
            committed_tail: Some(tail),
            entries,
            inherited_entries: Vec::new(),
            inherited_snapshot_id: None,
            before_cursor_anchor,
            after_cursor_anchor,
        })
    }

    /// 有界读取超大条目的正文块：先按主键取 `rowid`，再由 blob 句柄读取请求区间。
    ///
    /// 总长度只来自 `sqlite3_blob_bytes`，区间只来自 `sqlite3_blob_read`；不在 SQL 里
    /// `length(CAST(...))`/`substr(CAST(...))`（那会在 SQLite 内物化整条正文）。
    #[allow(clippy::too_many_arguments)] // 与 trait 契约同口径，逐项显式传递
    pub(super) async fn read_history_body_chunk(
        &self,
        id: &ThreadId,
        source: HistorySource,
        entry_id: MessageId,
        offset: u64,
        max_bytes: u32,
        committed_tail: Option<&HistoryCommittedTail>,
        _read_ticket: &HistoryReadTicket,
    ) -> SessionResourceResult<HistoryBodyChunk> {
        if let HistorySource::Inherited {
            frozen_snapshot_id,
            ancestor_thread_id,
        } = &source
        {
            return self
                .read_inherited_body_chunk(
                    id,
                    frozen_snapshot_id,
                    ancestor_thread_id.as_ref(),
                    entry_id,
                    offset,
                    max_bytes,
                    _read_ticket,
                )
                .await;
        }
        if max_bytes == 0 || max_bytes > HISTORY_WINDOW_MAX_CHUNK_SIZE {
            return Err(
                HistoryWindowError::invalid_request("body chunk size is out of range").into(),
            );
        }
        let committed_tail = committed_tail.ok_or_else(|| {
            HistoryWindowError::invalid_request("own history body requires a committed tail")
        })?;
        let tail_position = i64::try_from(committed_tail.position)
            .map_err(|_| corrupt("history window tail position exceeds the supported range"))?;
        let expected = entry_id.as_uuid().to_string();
        let mut connection = self
            .database
            .pool
            .acquire()
            .await
            .map_err(|error| map_sqlx(&error))?;
        let mut retried = false;
        let (total, bytes) = loop {
            let mut tx = sqlx::Connection::begin(&mut *connection)
                .await
                .map_err(|error| map_sqlx(&error))?;
            let row: Option<(i64,)> = sqlx::query_as(HISTORY_WINDOW_BODY_ROWID_SQL)
                .bind(id.as_str())
                .bind(&expected)
                .bind(tail_position)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|error| map_sqlx(&error))?;
            let Some((rowid,)) = row else {
                return Err(HistoryWindowError::anchor_unavailable(
                    "history body entry is not present in this session",
                )
                .into());
            };
            let read = Self::blob_scope(&mut tx, b"messages\0", b"content\0", rowid, |reader| {
                Ok((reader.total(), reader.read(offset, max_bytes as usize)?))
            })
            .await?;
            tx.commit().await.map_err(|error| map_sqlx(&error))?;
            match read {
                Ok(read) => break read,
                // 并发更新可能让句柄失效；重开一次，不把瞬态竞争当持久损坏。
                Err(error) if !retried && is_transient_blob_error(&error) => {
                    retried = true;
                }
                Err(error) => return Err(map_inherited_blob_error(error)),
            }
        };
        if offset > total {
            return Err(HistoryWindowError::invalid_request(
                "body chunk offset is beyond the entry length",
            )
            .into());
        }
        let end = offset.checked_add(bytes.len() as u64).ok_or_else(|| {
            HistoryWindowError::ReadFailed {
                detail: "body chunk offset overflow".into(),
            }
        })?;
        let complete = end >= total;
        if !complete && bytes.is_empty() {
            return Err(HistoryWindowError::ReadFailed {
                detail: "body chunk made no progress".into(),
            }
            .into());
        }
        Ok(HistoryBodyChunk {
            entry_id: HistoryEntryId::new(id, entry_id).encode(),
            offset,
            bytes,
            next_offset: (!complete).then_some(end),
            complete,
        })
    }

    /// body 定位的独立元数据读取：单条主键 + 固定 tail，长度来自 blob 句柄。
    ///
    /// 与页读取完全解耦：不构造 `HistoryWindowPagePlan`，也不读取任何正文。页上的
    /// `Oversized`/`Loaded` 只是当页交付方式，不能作为正文存在性的依据。
    pub(super) async fn read_history_entry_metadata_from_store(
        &self,
        id: &ThreadId,
        source: HistorySource,
        entry_id: MessageId,
        committed_tail: Option<&HistoryCommittedTail>,
        _read_ticket: &HistoryReadTicket,
    ) -> SessionResourceResult<HistoryEntryMetadata> {
        if let HistorySource::Inherited {
            frozen_snapshot_id,
            ancestor_thread_id,
        } = &source
        {
            return self
                .read_inherited_entry_metadata(
                    id,
                    frozen_snapshot_id,
                    ancestor_thread_id.as_ref(),
                    entry_id,
                )
                .await;
        }
        let committed_tail = committed_tail.ok_or_else(|| {
            HistoryWindowError::invalid_request("own history metadata requires a committed tail")
        })?;
        let tail_position = i64::try_from(committed_tail.position)
            .map_err(|_| corrupt("history window tail position exceeds the supported range"))?;
        let mut connection = self
            .database
            .pool
            .acquire()
            .await
            .map_err(|error| map_sqlx(&error))?;
        let mut tx = sqlx::Connection::begin(&mut *connection)
            .await
            .map_err(|error| map_sqlx(&error))?;
        self.require_session(&mut tx, id).await?;
        let row: Option<(i64, String)> = sqlx::query_as(HISTORY_WINDOW_ENTRY_METADATA_SQL)
            .bind(id.as_str())
            .bind(entry_id.as_uuid().to_string())
            .bind(tail_position)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|error| map_sqlx(&error))?;
        let Some((rowid, role)) = row else {
            return Err(HistoryWindowError::anchor_unavailable(
                "history body entry is not present in this session",
            )
            .into());
        };
        let canonical_bytes = read_content_length(&mut tx, rowid).await?;
        tx.commit().await.map_err(|error| map_sqlx(&error))?;
        Ok(HistoryEntryMetadata {
            canonical_bytes,
            role: HistoryEntryRole::from_stored(&role),
        })
    }

    async fn read_inherited_entry_metadata(
        &self,
        id: &ThreadId,
        frozen_snapshot_id: &str,
        ancestor_thread_id: Option<&ThreadId>,
        entry_id: MessageId,
    ) -> SessionResourceResult<HistoryEntryMetadata> {
        let mut connection = self
            .database
            .pool
            .acquire()
            .await
            .map_err(|error| map_sqlx(&error))?;
        let read = self
            .read_inherited_layout_cached(&mut connection, id)
            .await?;
        match read {
            InheritedFactsRead::Unsupported => {
                self.read_inherited_entry_metadata_sql(
                    &mut connection,
                    id,
                    frozen_snapshot_id,
                    ancestor_thread_id,
                    entry_id,
                )
                .await
            }
            InheritedFactsRead::Facts { snapshot: None, .. } => {
                Err(HistoryWindowError::window_invalidated("inherited prefix is absent").into())
            }
            InheritedFactsRead::Facts {
                snapshot: Some(snapshot),
                layout: Some(layout),
            } => {
                verify_inherited_source(&snapshot, frozen_snapshot_id, ancestor_thread_id)?;
                let index = layout.position_of(entry_id).ok_or_else(|| {
                    HistoryWindowError::anchor_unavailable(
                        "inherited history entry is not present in this frozen snapshot",
                    )
                })?;
                let row: Option<(i64,)> = sqlx::query_as(HISTORY_WINDOW_INHERITED_ROWID_SQL)
                    .bind(id.as_str())
                    .fetch_optional(&mut *connection)
                    .await
                    .map_err(|error| map_sqlx(&error))?;
                let Some((rowid,)) = row else {
                    return Err(not_found());
                };
                let read = Self::inherited_blob_scope(&mut connection, rowid, |reader| {
                    layout.logical_len(reader, index)
                })
                .await?;
                match read {
                    Ok(canonical_bytes) => Ok(HistoryEntryMetadata {
                        canonical_bytes,
                        // inherited 没有 role 列：可见性只能由前缀判定。
                        role: HistoryEntryRole::Unknown,
                    }),
                    // 冻结列被并发改写：回退 SQL 语义，避免把瞬态竞争当损坏。
                    Err(error) if is_transient_blob_error(&error) => {
                        self.read_inherited_entry_metadata_sql(
                            &mut connection,
                            id,
                            frozen_snapshot_id,
                            ancestor_thread_id,
                            entry_id,
                        )
                        .await
                    }
                    Err(error) => Err(map_inherited_blob_error(error)),
                }
            }
            InheritedFactsRead::Facts {
                snapshot: Some(_),
                layout: None,
            } => Err(corrupt("inherited layout is missing")),
        }
    }

    async fn read_inherited_entry_metadata_sql(
        &self,
        connection: &mut SqliteConnection,
        id: &ThreadId,
        frozen_snapshot_id: &str,
        ancestor_thread_id: Option<&ThreadId>,
        entry_id: MessageId,
    ) -> SessionResourceResult<HistoryEntryMetadata> {
        let mut tx = sqlx::Connection::begin(&mut *connection)
            .await
            .map_err(|error| map_sqlx(&error))?;
        self.require_session(&mut tx, id).await?;
        let snapshot = Self::read_inherited_facts_sql(&mut tx, id)
            .await?
            .ok_or_else(|| HistoryWindowError::window_invalidated("inherited prefix is absent"))?;
        verify_inherited_source(&snapshot, frozen_snapshot_id, ancestor_thread_id)?;
        let key: Option<(i64,)> = sqlx::query_as(HISTORY_WINDOW_INHERITED_ANCHOR_SQL)
            .bind(id.as_str())
            .bind(entry_id.as_uuid().to_string())
            .fetch_optional(&mut *tx)
            .await
            .map_err(|error| map_sqlx(&error))?;
        let Some((key,)) = key else {
            return Err(HistoryWindowError::anchor_unavailable(
                "inherited history entry is not present in this frozen snapshot",
            )
            .into());
        };
        let length: Option<(i64,)> = sqlx::query_as(HISTORY_WINDOW_INHERITED_LENGTH_SQL)
            .bind(id.as_str())
            .bind(key)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|error| map_sqlx(&error))?;
        tx.commit().await.map_err(|error| map_sqlx(&error))?;
        let Some((length,)) = length else {
            return Err(HistoryWindowError::anchor_unavailable(
                "inherited history entry is not present in this frozen snapshot",
            )
            .into());
        };
        let canonical_bytes = u64::try_from(length)
            .map_err(|_| corrupt("inherited history body length is negative"))?;
        Ok(HistoryEntryMetadata {
            canonical_bytes,
            role: HistoryEntryRole::Unknown,
        })
    }

    async fn read_window_tail(
        connection: &mut SqliteConnection,
        id: &ThreadId,
    ) -> SessionResourceResult<Option<HistoryCommittedTail>> {
        let row: Option<(String, i64)> = sqlx::query_as(HISTORY_WINDOW_LATEST_SQL)
            .bind(id.as_str())
            .fetch_optional(&mut *connection)
            .await
            .map_err(|error| map_sqlx(&error))?;
        row.map(|(entry, position)| {
            Ok(HistoryCommittedTail::new(
                id,
                message_id_of(&entry)?,
                u64::try_from(position)
                    .map_err(|_| corrupt("history window tail position is negative"))?,
            ))
        })
        .transpose()
    }
}

fn validate_history_window_plan(plan: &HistoryWindowPagePlan) -> SessionResourceResult<()> {
    if plan.before_limit > HISTORY_WINDOW_MAX_LIMIT as usize
        || plan.after_limit > HISTORY_WINDOW_MAX_LIMIT as usize
    {
        return Err(
            HistoryWindowError::invalid_request("history window limit is out of range").into(),
        );
    }
    if plan.byte_budget == 0 || plan.byte_budget > HISTORY_WINDOW_MAX_BYTE_BUDGET as usize {
        return Err(HistoryWindowError::invalid_request(
            "history window byte budget is out of range",
        )
        .into());
    }
    Ok(())
}

pub(super) fn window_anchor_failure(
    plan: &HistoryWindowPagePlan,
    detail: &str,
) -> HistoryWindowError {
    match plan.intent {
        HistoryWindowReadIntent::Seek => HistoryWindowError::anchor_unavailable(detail),
        HistoryWindowReadIntent::Open | HistoryWindowReadIntent::Page => {
            HistoryWindowError::window_invalidated(detail)
        }
    }
}

fn window_body_ids(plan: Option<&HistoryPagePlan>) -> Vec<MessageId> {
    plan.map(|plan| {
        plan.entries()
            .iter()
            .filter(|entry| entry.encoded_bytes <= plan.byte_budget())
            .map(|entry| entry.message_id)
            .collect()
    })
    .unwrap_or_default()
}

/// 由页行集两端与 fixed snapshot 内的邻接存在性计算双侧游标锚点。
///
/// `before_cursor_anchor` = 本页最旧行（更早方向还存在可读行时）；
/// `after_cursor_anchor` = 本页最新行（更晚方向还存在未越过 tail 的可读行时）。
/// 空页两侧均为 `None`，不铸造无行可锚的游标。
async fn window_boundary_cursors(
    connection: &mut SqliteConnection,
    id: &ThreadId,
    before_plan: Option<&HistoryPagePlan>,
    after_plan: Option<&HistoryPagePlan>,
    tail_position: i64,
) -> SessionResourceResult<(Option<MessageId>, Option<MessageId>)> {
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
        return Ok((None, None));
    };
    let oldest_position = i64::try_from(oldest_position)
        .map_err(|_| corrupt("history window position exceeds the supported range"))?;
    let newest_position = i64::try_from(newest_position)
        .map_err(|_| corrupt("history window position exceeds the supported range"))?;
    let has_older: Option<(i64,)> = sqlx::query_as(HISTORY_WINDOW_OLDER_EXISTS_SQL)
        .bind(id.as_str())
        .bind(oldest_position)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| map_sqlx(&error))?;
    let has_newer: Option<(i64,)> = sqlx::query_as(HISTORY_WINDOW_NEWER_EXISTS_SQL)
        .bind(id.as_str())
        .bind(newest_position)
        .bind(tail_position)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| map_sqlx(&error))?;
    Ok((
        has_older.is_some().then_some(oldest_id),
        has_newer.is_some().then_some(newest_id),
    ))
}

fn window_oversized_entries(
    entries: Vec<HistoryPageCandidate>,
) -> impl Iterator<Item = HistoryWindowEntry> {
    entries
        .into_iter()
        .map(|entry| HistoryWindowEntry::Oversized {
            entry_id: entry.message_id,
            position: entry.position,
            total_bytes: entry.encoded_bytes as u64,
            role: entry.role,
        })
}

pub(super) fn empty_window_page() -> HistoryPage {
    HistoryPage { items: Vec::new() }
}

impl SqliteSessionData {
    #[allow(clippy::too_many_arguments)] // 与 trait 契约同口径，逐项显式传递
    async fn read_inherited_body_chunk(
        &self,
        id: &ThreadId,
        frozen_snapshot_id: &str,
        ancestor_thread_id: Option<&ThreadId>,
        entry_id: MessageId,
        offset: u64,
        max_bytes: u32,
        _read_ticket: &HistoryReadTicket,
    ) -> SessionResourceResult<HistoryBodyChunk> {
        if max_bytes == 0 || max_bytes > HISTORY_WINDOW_MAX_CHUNK_SIZE {
            return Err(
                HistoryWindowError::invalid_request("body chunk size is out of range").into(),
            );
        }
        let offset = i64::try_from(offset).map_err(|_| {
            HistoryWindowError::invalid_request("body chunk offset is out of range")
        })?;
        let mut connection = self
            .database
            .pool
            .acquire()
            .await
            .map_err(|error| map_sqlx(&error))?;
        let read = self
            .read_inherited_layout_cached(&mut connection, id)
            .await?;
        match read {
            InheritedFactsRead::Unsupported => {
                self.read_inherited_body_chunk_sql(
                    &mut connection,
                    id,
                    frozen_snapshot_id,
                    ancestor_thread_id,
                    entry_id,
                    offset,
                    max_bytes,
                )
                .await
            }
            InheritedFactsRead::Facts { snapshot: None, .. } => {
                Err(HistoryWindowError::window_invalidated("inherited prefix is absent").into())
            }
            InheritedFactsRead::Facts {
                snapshot: Some(snapshot),
                layout: Some(layout),
            } => {
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
                let index = layout.position_of(entry_id).ok_or_else(|| {
                    HistoryWindowError::anchor_unavailable(
                        "inherited history entry is not present in this frozen snapshot",
                    )
                })?;
                let blob_offset = u64::try_from(offset)
                    .map_err(|_| corrupt("inherited history body offset is negative"))?;
                let chunk = Self::read_inherited_chunk_blob(
                    &mut connection,
                    id,
                    &layout,
                    index,
                    blob_offset,
                    max_bytes,
                )
                .await?;
                let chunk = match chunk {
                    Ok(chunk) => chunk,
                    // 并发更新可能让 blob 句柄失效；用 SQL 语义重读，避免把瞬态竞争
                    // 当成持久损坏。
                    Err(error) if is_transient_blob_error(&error) => {
                        return self
                            .read_inherited_body_chunk_sql(
                                &mut connection,
                                id,
                                frozen_snapshot_id,
                                ancestor_thread_id,
                                entry_id,
                                offset,
                                max_bytes,
                            )
                            .await;
                    }
                    Err(error) => return Err(map_inherited_blob_error(error)),
                };
                Ok(HistoryBodyChunk {
                    entry_id: HistoryEntryId::new(id, entry_id).encode(),
                    offset: chunk.offset,
                    bytes: chunk.bytes,
                    next_offset: chunk.next_offset,
                    complete: chunk.complete,
                })
            }
            InheritedFactsRead::Facts {
                snapshot: Some(_),
                layout: None,
            } => Err(corrupt("inherited layout is missing")),
        }
    }

    /// 增量块读取：只定位 rowid，再从 blob 读取请求区间；总长度由 blob 句柄直接给出，
    /// 不查询 `length(CAST(...))`（那会物化整列）。
    async fn read_inherited_chunk_blob(
        connection: &mut SqliteConnection,
        id: &ThreadId,
        layout: &InheritedLayout,
        index: usize,
        offset: u64,
        max_bytes: u32,
    ) -> SessionResourceResult<Result<super::inherited_blob::PayloadChunk, BlobReadError>> {
        let row: Option<(i64,)> = sqlx::query_as(HISTORY_WINDOW_INHERITED_ROWID_SQL)
            .bind(id.as_str())
            .fetch_optional(&mut *connection)
            .await
            .map_err(|error| map_sqlx(&error))?;
        let Some((rowid,)) = row else {
            return Err(not_found());
        };
        Self::inherited_blob_scope(connection, rowid, |reader| {
            layout.read_chunk(reader, index, offset, max_bytes)
        })
        .await
    }
}

/// 由摘要构造冻结快照事实；条目为空时与 SQL 路径一致地返回 `None`。
pub(super) fn inherited_snapshot_of(
    id: &ThreadId,
    parent: Option<String>,
    summary: &InheritedSummary,
) -> Option<HistoryInheritedSnapshot> {
    let first = summary.first_id?;
    let last = summary.last_id?;
    let count = summary.count;
    let snapshot_id = inherited_snapshot_id(
        id,
        summary.total_bytes as i64,
        count,
        parent.as_deref(),
        first,
        last,
    );
    Some(HistoryInheritedSnapshot {
        snapshot_id,
        ancestor_thread_id: parent,
        first_entry: Some(first),
        last_entry: Some(last),
        entry_count: count,
    })
}

pub(super) fn inherited_snapshot_id(
    child: &ThreadId,
    bytes: i64,
    count: usize,
    ancestor: Option<&str>,
    first: MessageId,
    last: MessageId,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(child.as_bytes());
    hasher.update(b"\0");
    hasher.update(bytes.to_string().as_bytes());
    hasher.update(b"\0");
    hasher.update(count.to_string().as_bytes());
    hasher.update(b"\0");
    hasher.update(ancestor.unwrap_or(""));
    hasher.update(b"\0");
    hasher.update(first.as_uuid().to_string().as_bytes());
    hasher.update(b"\0");
    hasher.update(last.as_uuid().to_string().as_bytes());
    let digest = hasher.finalize();
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("inherited:{hex}")
}
