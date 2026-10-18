//! `peri.historyWindowV1` 的 SQLite 有界读取：边界、双向 keyset、正文块。
//!
//! SQL 原文与旧分页/段读取同处 `session_data/history.rs`；本文件只做解码、事务编排与
//! 失败分类。任何读取都不得退化成「先全量取回再过滤」。

use super::history::{
    HISTORY_WINDOW_AFTER_SQL, HISTORY_WINDOW_ANCHOR_SQL, HISTORY_WINDOW_BEFORE_SQL,
    HISTORY_WINDOW_BODY_CHUNK_SQL, HISTORY_WINDOW_BOUNDS_SQL, HISTORY_WINDOW_CHECKPOINT_SQL,
    HISTORY_WINDOW_INHERITED_AFTER_SQL, HISTORY_WINDOW_INHERITED_ANCHOR_SQL,
    HISTORY_WINDOW_INHERITED_BEFORE_SQL, HISTORY_WINDOW_INHERITED_BODY_SQL,
    HISTORY_WINDOW_INHERITED_BOUNDS_SQL, HISTORY_WINDOW_INHERITED_CHUNK_SQL,
    HISTORY_WINDOW_LATEST_SQL, HISTORY_WINDOW_TAIL_CHECK_SQL,
};
use super::*;
use peri_acp_types::history_window::{
    HistoryBodyChunk, HistoryCheckpoint, HistoryCommittedTail, HistoryEntryId,
    HistoryInheritedSnapshot, HistoryInheritedWindow, HistoryReadTicket, HistorySource,
    HistoryWindowError, HISTORY_WINDOW_MAX_BYTE_BUDGET, HISTORY_WINDOW_MAX_CHUNK_SIZE,
    HISTORY_WINDOW_MAX_LIMIT,
};
use peri_acp_types::session_resources::{
    HistoryWindowEntry, HistoryWindowPagePlan, HistoryWindowReadIntent, HistoryWindowRows,
    HistoryWindowTailExpectation, InheritedHistoryEntry, SessionHistoryWindowBounds,
};
use sha2::{Digest, Sha256};

/// `HISTORY_WINDOW_BOUNDS_SQL` 的行形状：(最旧 id, 最新 id, 最新 position, 继承行数)。
type HistoryWindowBoundsRow = (Option<String>, Option<String>, Option<i64>, i64);

/// `HISTORY_WINDOW_INHERITED_BOUNDS_SQL` 的行形状。
type InheritedHistoryBoundsRow = (
    Option<String>,
    Option<i64>,
    i64,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<i64>,
);

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
    /// 冻结前缀存在性。语句不取 `content`；会话不存在是 `NotFound`。
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
        let mut tx = sqlx::Connection::begin(&mut *connection)
            .await
            .map_err(|error| map_sqlx(&error))?;
        self.require_session(&mut tx, id).await?;
        let row: Option<HistoryWindowBoundsRow> = sqlx::query_as(HISTORY_WINDOW_BOUNDS_SQL)
            .bind(id.as_str())
            .fetch_optional(&mut *tx)
            .await
            .map_err(|error| map_sqlx(&error))?;
        let (oldest, latest, latest_position, _inherited) = row.ok_or_else(not_found)?;
        let checkpoint: Option<(String, i64)> = sqlx::query_as(HISTORY_WINDOW_CHECKPOINT_SQL)
            .bind(id.as_str())
            .fetch_optional(&mut *tx)
            .await
            .map_err(|error| map_sqlx(&error))?;
        let inherited_snapshot = Self::read_inherited_facts(&mut tx, id).await?;
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
            let candidates: Vec<(String, i64, i64)> = sqlx::query_as(HISTORY_WINDOW_BEFORE_SQL)
                .bind(id.as_str())
                .bind(tail_position)
                .bind(anchor_position_i64)
                .bind(i64::from(plan.include_anchor))
                .bind((plan.before_limit + 1) as i64)
                .fetch_all(&mut *tx)
                .await
                .map_err(|error| map_sqlx(&error))?;
            let candidates = decode_window_candidates(candidates)?;
            Some(HistoryPagePlan::plan(
                id,
                "",
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
                let candidates: Vec<(String, i64, i64)> = sqlx::query_as(HISTORY_WINDOW_AFTER_SQL)
                    .bind(id.as_str())
                    .bind(tail_position)
                    .bind(anchor)
                    .bind((limit + 1) as i64)
                    .fetch_all(&mut *tx)
                    .await
                    .map_err(|error| map_sqlx(&error))?;
                let candidates = decode_window_candidates(candidates)?;
                Some(HistoryPagePlan::plan_after(
                    id,
                    "",
                    candidates,
                    limit,
                    after_budget,
                ))
            }
        };

        let before_cursor_anchor = before_plan
            .as_ref()
            .and_then(|plan| plan.has_more().then(|| plan.anchor()).flatten());
        let after_cursor_anchor = after_plan
            .as_ref()
            .and_then(|plan| plan.has_more().then(|| plan.anchor()).flatten());
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

    /// 有界读取超大条目的正文块：`substr` 直接按字节区间取，不先物化完整正文。
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
        let offset = i64::try_from(offset).map_err(|_| {
            HistoryWindowError::invalid_request("body chunk offset is out of range")
        })?;
        let committed_tail = committed_tail.ok_or_else(|| {
            HistoryWindowError::invalid_request("own history body requires a committed tail")
        })?;
        let tail_position = i64::try_from(committed_tail.position)
            .map_err(|_| corrupt("history window tail position exceeds the supported range"))?;
        let row: Option<(i64, Vec<u8>)> = sqlx::query_as(HISTORY_WINDOW_BODY_CHUNK_SQL)
            .bind(id.as_str())
            .bind(entry_id.as_uuid().to_string())
            .bind(offset)
            .bind(i64::from(max_bytes))
            .bind(tail_position)
            .fetch_optional(&self.database.pool)
            .await
            .map_err(|error| map_sqlx(&error))?;
        let Some((total, bytes)) = row else {
            return Err(HistoryWindowError::anchor_unavailable(
                "history body entry is not present in this session",
            )
            .into());
        };
        let total = u64::try_from(total).map_err(|_| corrupt("history body length is negative"))?;
        let offset =
            u64::try_from(offset).map_err(|_| corrupt("history body offset is negative"))?;
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

fn window_anchor_failure(plan: &HistoryWindowPagePlan, detail: &str) -> HistoryWindowError {
    match plan.intent {
        HistoryWindowReadIntent::Seek => HistoryWindowError::anchor_unavailable(detail),
        HistoryWindowReadIntent::Open | HistoryWindowReadIntent::Page => {
            HistoryWindowError::window_invalidated(detail)
        }
    }
}

fn decode_window_candidates(
    candidates: Vec<(String, i64, i64)>,
) -> SessionResourceResult<Vec<HistoryPageCandidate>> {
    candidates
        .into_iter()
        .map(|(message_id, position, encoded_bytes)| {
            HistoryPageCandidate::new(&message_id, position, encoded_bytes)
        })
        .collect()
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

fn window_oversized_entries(
    entries: Vec<HistoryPageCandidate>,
) -> impl Iterator<Item = HistoryWindowEntry> {
    entries
        .into_iter()
        .map(|entry| HistoryWindowEntry::Oversized {
            entry_id: entry.message_id,
            position: entry.position,
            total_bytes: entry.encoded_bytes as u64,
        })
}

fn empty_window_page() -> HistoryPage {
    HistoryPage {
        items: Vec::new(),
        next_cursor: None,
    }
}

// ─── inherited 冻结前缀读取（S7.4） ──────────────────────────────────────────

impl SqliteSessionData {
    /// 从 `threads.inherited_context` 读取冻结快照事实；不把整列正文带回 Rust。
    async fn read_inherited_facts(
        connection: &mut SqliteConnection,
        id: &ThreadId,
    ) -> SessionResourceResult<Option<HistoryInheritedSnapshot>> {
        let row: Option<InheritedHistoryBoundsRow> =
            sqlx::query_as(HISTORY_WINDOW_INHERITED_BOUNDS_SQL)
                .bind(id.as_str())
                .fetch_optional(&mut *connection)
                .await
                .map_err(|error| map_sqlx(&error))?;
        let Some((parent, bytes, count, first, last, payloads_type, version)) = row else {
            return Err(not_found());
        };
        if count < 0 {
            return Err(corrupt("inherited context is not valid JSON"));
        }
        if count == 0 {
            return Ok(None);
        }
        if payloads_type.as_deref() != Some("array") || version != Some(1) {
            return Err(corrupt("inherited context has an unsupported shape"));
        }
        let first_payload =
            first.ok_or_else(|| corrupt("inherited context first payload is missing"))?;
        let last_payload =
            last.ok_or_else(|| corrupt("inherited context last payload is missing"))?;
        let first_id = deserialize_persisted_payload(&first_payload)
            .map_err(|_| corrupt("inherited context first payload is not decodable"))?
            .id();
        let last_id = deserialize_persisted_payload(&last_payload)
            .map_err(|_| corrupt("inherited context last payload is not decodable"))?
            .id();
        let snapshot_id = inherited_snapshot_id(
            id,
            bytes.unwrap_or(0),
            count as usize,
            parent.as_deref(),
            first_id,
            last_id,
        );
        Ok(Some(HistoryInheritedSnapshot {
            snapshot_id,
            ancestor_thread_id: parent,
            first_entry: Some(first_id),
            last_entry: Some(last_id),
            entry_count: count as usize,
        }))
    }

    async fn read_inherited_history_window_page(
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
        let mut tx = sqlx::Connection::begin(&mut *connection)
            .await
            .map_err(|error| map_sqlx(&error))?;
        self.require_session(&mut tx, id).await?;
        let snapshot = Self::read_inherited_facts(&mut tx, id)
            .await?
            .ok_or_else(|| HistoryWindowError::window_invalidated("inherited prefix is absent"))?;
        if snapshot.snapshot_id != *frozen_snapshot_id {
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
        let after_plan = match (plan.after_limit, anchor_key) {
            (0, _) | (_, None) => None,
            (limit, Some(anchor)) => {
                let candidates: Vec<(i64, Option<String>, i64)> =
                    sqlx::query_as(HISTORY_WINDOW_INHERITED_AFTER_SQL)
                        .bind(id.as_str())
                        .bind(anchor)
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

        let before_cursor_anchor = before_plan
            .as_ref()
            .and_then(|plan| plan.has_more().then(|| plan.anchor()).flatten());
        let after_cursor_anchor = after_plan
            .as_ref()
            .and_then(|plan| plan.has_more().then(|| plan.anchor()).flatten());
        let before_rows = Self::read_inherited_bodies(&mut tx, id, before_plan.as_ref()).await?;
        let after_rows = Self::read_inherited_bodies(&mut tx, id, after_plan.as_ref()).await?;
        tx.commit().await.map_err(|error| map_sqlx(&error))?;

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
        inherited_entries.extend(before_oversized.into_iter().map(|entry| {
            InheritedHistoryEntry::Oversized {
                entry_id: entry.message_id,
                total_bytes: entry.encoded_bytes as u64,
            }
        }));
        inherited_entries.extend(
            after_page
                .items
                .into_iter()
                .map(InheritedHistoryEntry::Loaded),
        );
        inherited_entries.extend(after_oversized.into_iter().map(|entry| {
            InheritedHistoryEntry::Oversized {
                entry_id: entry.message_id,
                total_bytes: entry.encoded_bytes as u64,
            }
        }));
        Ok(HistoryWindowRows {
            committed_tail: None,
            entries: Vec::new(),
            inherited_entries,
            inherited_snapshot_id: Some(snapshot.snapshot_id),
            before_cursor_anchor,
            after_cursor_anchor,
        })
    }

    async fn read_inherited_bodies(
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
        let mut tx = sqlx::Connection::begin(&mut *connection)
            .await
            .map_err(|error| map_sqlx(&error))?;
        self.require_session(&mut tx, id).await?;
        let snapshot = Self::read_inherited_facts(&mut tx, id)
            .await?
            .ok_or_else(|| HistoryWindowError::window_invalidated("inherited prefix is absent"))?;
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
        let row: Option<(i64, Vec<u8>)> = sqlx::query_as(HISTORY_WINDOW_INHERITED_CHUNK_SQL)
            .bind(id.as_str())
            .bind(key)
            .bind(offset)
            .bind(i64::from(max_bytes))
            .fetch_optional(&mut *tx)
            .await
            .map_err(|error| map_sqlx(&error))?;
        tx.commit().await.map_err(|error| map_sqlx(&error))?;
        let Some((total, bytes)) = row else {
            return Err(HistoryWindowError::anchor_unavailable(
                "inherited history entry is not present in this frozen snapshot",
            )
            .into());
        };
        let total = u64::try_from(total)
            .map_err(|_| corrupt("inherited history body length is negative"))?;
        let offset = u64::try_from(offset)
            .map_err(|_| corrupt("inherited history body offset is negative"))?;
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

fn inherited_snapshot_id(
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
