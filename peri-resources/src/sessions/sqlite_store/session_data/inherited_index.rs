//! inherited 派生索引：写时记录快照与逐 payload 区间，读取时免去全列扫描。
//!
//! 索引是**派生对象**：事实全部来自 `threads.inherited_context` 的 canonical 信封，
//! 不从它独占任何数据。因此：
//!
//! - 索引缺表、缺行、行数不符或 `payload_bytes` 与列总字节数不一致时，读取回退到
//!   [`super::inherited_facts`] 的全列扫描（行为与引入索引前相同）。
//! - 旧库（schema 11）第一次读布局时按一次扫描的结果回填；回填只作用于当前快照，
//!   不启动后台任务、不跨线程累积内存。
//! - `logical_len` 允许为 NULL：legacy 回填不解码正文，读取时按需解码；新写入由
//!   [`peri_acp_types::store::InheritedContext::to_json_with_spans`] 直接给出。
//!
//! 与内存缓存的关系：缓存键仍是 `(ThreadId, 列总字节数)`。索引只替代「如何得到事实」，
//! 不改变 `snapshot_id` 的派生口径（[`super::history_window::inherited_snapshot_id`]）。

use std::collections::HashMap;
use std::sync::OnceLock;

use peri_acp_types::messages::MessageId;
use peri_acp_types::store::InheritedSerialization;
use peri_acp_types::thread::ThreadId;
use sqlx::{Connection, SqliteConnection};

use super::history_window::inherited_snapshot_id;
use super::inherited_facts::{InheritedLayout, InheritedSummary, LayoutPayload, PayloadSpan};
use super::*;

pub(super) const SNAPSHOT_SQL: &str = "SELECT snapshot_id, payload_bytes, entry_count
    FROM inherited_snapshot_index WHERE thread_id = ?1 ORDER BY rowid DESC LIMIT 1";
pub(super) const PAYLOADS_SQL: &str = "SELECT entry_id, raw_start, raw_len, logical_len
    FROM inherited_payload_index WHERE snapshot_id = ?1 ORDER BY ordinal";
const DELETE_PAYLOADS_SQL: &str =
    crate::sessions::canonical::DELETE_INHERITED_PAYLOADS_BY_THREAD_SQL;
const DELETE_SNAPSHOT_SQL: &str =
    crate::sessions::canonical::DELETE_INHERITED_SNAPSHOT_BY_THREAD_SQL;
const INSERT_SNAPSHOT_SQL: &str = "INSERT INTO inherited_snapshot_index
    (snapshot_id, thread_id, payload_bytes, entry_count, prepared_at)
    VALUES (?1, ?2, ?3, ?4, ?5)";
const INSERT_PAYLOAD_SQL: &str = "INSERT INTO inherited_payload_index
    (snapshot_id, ordinal, entry_id, raw_start, raw_len, logical_len)
    VALUES (?1, ?2, ?3, ?4, ?5, ?6)";

/// 一条已索引 payload 的定位事实。
#[derive(Clone)]
pub(super) struct IndexedPayload {
    pub(super) id: MessageId,
    pub(super) span: PayloadSpan,
    pub(super) logical_len: Option<u64>,
}

/// 一份已索引 inherited 快照。
#[derive(Clone)]
pub(super) struct IndexedSnapshot {
    pub(super) snapshot_id: String,
    pub(super) payload_bytes: u64,
    pub(super) entry_count: usize,
    pub(super) payloads: Vec<IndexedPayload>,
}

impl IndexedSnapshot {
    /// 索引行是否仍描述调用方刚读到的这一列。
    ///
    /// 校验的是**派生身份**而不是内容：列总字节数一致、条数一致、逐区间落在列内，
    /// 且按现有口径重算的 `snapshot_id` 与持久值相同。同长度改写无法被这些事实识别
    /// ——这是 write-once 不变量与 F12a 缓存共同承认的边界，不是索引新增的弱化。
    pub(super) fn validate(
        &self,
        thread: &ThreadId,
        parent: Option<&str>,
        total_bytes: u64,
    ) -> bool {
        if self.payload_bytes != total_bytes
            || self.entry_count == 0
            || self.payloads.len() != self.entry_count
        {
            return false;
        }
        let (Some(first), Some(last)) = (self.payloads.first(), self.payloads.last()) else {
            return false;
        };
        if self.payloads.iter().any(|payload| {
            payload
                .span
                .raw_start
                .checked_add(payload.span.raw_len)
                .is_none_or(|end| end > self.payload_bytes)
        }) {
            return false;
        }
        let Ok(bytes) = i64::try_from(self.payload_bytes) else {
            return false;
        };
        inherited_snapshot_id(thread, bytes, self.entry_count, parent, first.id, last.id)
            == self.snapshot_id
    }

    pub(super) fn summary(&self) -> InheritedSummary {
        InheritedSummary {
            total_bytes: self.payload_bytes,
            count: self.entry_count,
            first_id: self.payloads.first().map(|payload| payload.id),
            last_id: self.payloads.last().map(|payload| payload.id),
        }
    }

    /// 索引 → 读路径布局。重复身份与全列扫描同一口径（保留首次出现的下标）。
    pub(super) fn to_layout(&self) -> InheritedLayout {
        let mut positions = HashMap::with_capacity(self.payloads.len());
        let payloads = self
            .payloads
            .iter()
            .enumerate()
            .map(|(index, payload)| {
                positions.entry(payload.id).or_insert(index);
                LayoutPayload {
                    span: payload.span,
                    id: payload.id,
                    indexed_logical_len: payload.logical_len,
                    decoded: OnceLock::new(),
                }
            })
            .collect();
        InheritedLayout {
            total_bytes: self.payload_bytes,
            payloads,
            positions,
        }
    }
}

/// 读取线程当前的派生索引；缺表/缺行/坏行一律返回 `None`，交给全列扫描。
pub(super) async fn read(
    connection: &mut SqliteConnection,
    thread: &ThreadId,
) -> SessionResourceResult<Option<IndexedSnapshot>> {
    let row: Option<(String, i64, i64)> = match sqlx::query_as(SNAPSHOT_SQL)
        .bind(thread.as_str())
        .fetch_optional(&mut *connection)
        .await
    {
        Ok(row) => row,
        Err(error) if is_missing_table(&error) => return Ok(None),
        Err(error) => return Err(map_sqlx(&error)),
    };
    let Some((snapshot_id, payload_bytes, entry_count)) = row else {
        return Ok(None);
    };
    let (Ok(payload_bytes), Ok(entry_count)) =
        (u64::try_from(payload_bytes), usize::try_from(entry_count))
    else {
        return Ok(None);
    };
    if entry_count == 0 {
        return Ok(None);
    }
    let rows: Vec<(String, i64, i64, Option<i64>)> = match sqlx::query_as(PAYLOADS_SQL)
        .bind(&snapshot_id)
        .fetch_all(&mut *connection)
        .await
    {
        Ok(rows) => rows,
        Err(error) if is_missing_table(&error) => return Ok(None),
        Err(error) => return Err(map_sqlx(&error)),
    };
    let mut payloads = Vec::with_capacity(rows.len());
    for (entry_id, raw_start, raw_len, logical_len) in rows {
        let (Ok(uuid), Ok(raw_start), Ok(raw_len)) = (
            uuid::Uuid::parse_str(&entry_id),
            u64::try_from(raw_start),
            u64::try_from(raw_len),
        ) else {
            return Ok(None);
        };
        let logical_len = match logical_len {
            Some(value) => match u64::try_from(value) {
                Ok(value) => Some(value),
                Err(_) => return Ok(None),
            },
            None => None,
        };
        payloads.push(IndexedPayload {
            id: MessageId::from(uuid),
            span: PayloadSpan { raw_start, raw_len },
            logical_len,
        });
    }
    Ok(Some(IndexedSnapshot {
        snapshot_id,
        payload_bytes,
        entry_count,
        payloads,
    }))
}

/// 一份待落库的索引事实。
pub(in crate::sessions::sqlite_store) struct SnapshotWrite {
    thread_id: ThreadId,
    snapshot_id: String,
    payload_bytes: u64,
    payloads: Vec<(MessageId, PayloadSpan, Option<u64>)>,
}

impl SnapshotWrite {
    /// 新写入：区间与逻辑长度直接来自信封序列化。
    pub(in crate::sessions::sqlite_store) fn from_serialization(
        thread: &ThreadId,
        parent: Option<&str>,
        serialization: &InheritedSerialization,
    ) -> Option<Self> {
        if serialization.payloads.is_empty() {
            return None;
        }
        let payloads: Vec<(MessageId, PayloadSpan, Option<u64>)> = serialization
            .payloads
            .iter()
            .map(|payload| {
                (
                    payload.id,
                    PayloadSpan {
                        raw_start: payload.raw_start,
                        raw_len: payload.raw_len,
                    },
                    Some(payload.logical_len),
                )
            })
            .collect();
        let first = payloads.first()?.0;
        let last = payloads.last()?.0;
        let bytes = u64::try_from(serialization.json.len()).ok()?;
        let snapshot_id = inherited_snapshot_id(
            thread,
            i64::try_from(bytes).ok()?,
            payloads.len(),
            parent,
            first,
            last,
        );
        Some(Self {
            thread_id: thread.clone(),
            snapshot_id,
            payload_bytes: bytes,
            payloads,
        })
    }

    /// legacy 回填：布局扫描已经给出区间与身份，正文逻辑长度留空按需解码。
    fn from_layout(
        thread: &ThreadId,
        parent: Option<&str>,
        layout: &InheritedLayout,
    ) -> Option<Self> {
        let first = layout.first_id()?;
        let last = layout.last_id()?;
        let snapshot_id = inherited_snapshot_id(
            thread,
            i64::try_from(layout.total_bytes).ok()?,
            layout.len(),
            parent,
            first,
            last,
        );
        Some(Self {
            thread_id: thread.clone(),
            snapshot_id,
            payload_bytes: layout.total_bytes,
            payloads: layout
                .payloads
                .iter()
                .map(|payload| (payload.id, payload.span, payload.indexed_logical_len))
                .collect(),
        })
    }

    /// 用当前快照替换该线程的索引行（同一线程最多一份快照）。
    pub(in crate::sessions::sqlite_store) async fn replace(
        &self,
        connection: &mut SqliteConnection,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(DELETE_PAYLOADS_SQL)
            .bind(self.thread_id.as_str())
            .execute(&mut *connection)
            .await?;
        sqlx::query(DELETE_SNAPSHOT_SQL)
            .bind(self.thread_id.as_str())
            .execute(&mut *connection)
            .await?;
        sqlx::query(INSERT_SNAPSHOT_SQL)
            .bind(&self.snapshot_id)
            .bind(self.thread_id.as_str())
            .bind(i64::try_from(self.payload_bytes).unwrap_or(i64::MAX))
            .bind(i64::try_from(self.payloads.len()).unwrap_or(i64::MAX))
            .bind(peri_time::now_utc_rfc3339())
            .execute(&mut *connection)
            .await?;
        for (ordinal, (id, span, logical_len)) in self.payloads.iter().enumerate() {
            sqlx::query(INSERT_PAYLOAD_SQL)
                .bind(&self.snapshot_id)
                .bind(i64::try_from(ordinal).unwrap_or(i64::MAX))
                .bind(id.as_uuid().to_string())
                .bind(i64::try_from(span.raw_start).unwrap_or(i64::MAX))
                .bind(i64::try_from(span.raw_len).unwrap_or(i64::MAX))
                .bind(logical_len.and_then(|value| i64::try_from(value).ok()))
                .execute(&mut *connection)
                .await?;
        }
        Ok(())
    }
}

/// 用一次布局扫描的结果回填索引；失败只记录日志，读取路径继续按扫描事实工作。
pub(super) async fn backfill_from_layout(
    connection: &mut SqliteConnection,
    thread: &ThreadId,
    parent: Option<&str>,
    layout: &InheritedLayout,
) {
    let Some(write) = SnapshotWrite::from_layout(thread, parent, layout) else {
        return;
    };
    let result = async {
        let mut tx = connection.begin().await?;
        write.replace(&mut tx).await?;
        tx.commit().await
    }
    .await;
    if let Err(error) = result {
        tracing::warn!(
            thread = %thread.as_str(),
            %error,
            "failed to backfill the inherited derived index; reads fall back to scanning"
        );
    }
}

fn is_missing_table(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::Database(database) if database.message().contains("no such table"))
}
