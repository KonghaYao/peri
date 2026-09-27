//! 祖先 payload 边界、上下文缓存与线程树读取。
//!
//! 读取分两层：`*_on(connection, …)` 是连接作用域原语，供需要「一次读取视图」的
//! 调用方（一致 snapshot、事务内复核）使用；`impl SqliteSessionDatabase` 上的方法
//! 自行取一条连接，供单条读取使用。

use super::{
    database::SqliteSessionDatabase,
    row_mapping::{meta_from_row, ThreadRow, THREAD_META_COLUMNS},
};
use anyhow::Result;
use chrono::Utc;
use peri_acp_types::{
    messages::BaseMessage,
    store::{deserialize_persisted_payload, InheritedContext, PersistedPayload},
    thread::{ThreadId, ThreadMeta},
};
use sqlx::{AssertSqlSafe, SqliteConnection};
use std::collections::HashSet;

impl SqliteSessionDatabase {
    /// 小型 metadata 投影：不含 `cached_context`（派生缓存不是 metadata 事实）。
    pub(super) async fn load_meta(&self, id: &ThreadId) -> Result<ThreadMeta> {
        let mut connection = self.pool.acquire().await?;
        load_meta_on(&mut connection, id).await
    }

    pub(super) async fn load_payloads(&self, id: &ThreadId) -> Result<Vec<PersistedPayload>> {
        let mut connection = self.pool.acquire().await?;
        load_payloads_on(&mut connection, id).await
    }

    /// 视图读取：继承区在前、自有 payload 在后。
    pub(super) async fn load_context_payloads(
        &self,
        thread_id: &ThreadId,
    ) -> Result<Vec<PersistedPayload>> {
        let mut connection = self.pool.acquire().await?;
        load_context_payloads_on(&mut connection, thread_id).await
    }

    pub(super) async fn load_inherited_context(
        &self,
        thread_id: &ThreadId,
    ) -> Result<InheritedContext> {
        let mut connection = self.pool.acquire().await?;
        load_inherited_context_on(&mut connection, thread_id).await
    }

    pub(super) async fn load_context(&self, thread_id: &ThreadId) -> Result<Vec<BaseMessage>> {
        let messages = self
            .load_context_payloads(thread_id)
            .await?
            .into_iter()
            .filter_map(|payload| payload.as_message().cloned())
            .collect::<Vec<_>>();
        if !messages.is_empty() {
            self.save_context_cache(thread_id, &messages).await?;
        }
        Ok(messages)
    }

    /// 将消息序列化为 JSON 并保存到 cached_context 列
    async fn save_context_cache(
        &self,
        thread_id: &ThreadId,
        messages: &[BaseMessage],
    ) -> Result<()> {
        // Read APIs must remain usable without an execution owner and never dirty a bound session.
        if self.read_only || self.load_session_binding_impl(thread_id).await?.is_some() {
            return Ok(());
        }
        let cached = serde_json::to_string(messages)?;
        let now = Utc::now().to_rfc3339();
        sqlx::query("UPDATE threads SET cached_context = ?1, updated_at = ?2 WHERE id = ?3")
            .bind(&cached)
            .bind(&now)
            .bind(thread_id.as_str())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub(super) async fn list_child_threads(&self, parent_id: &ThreadId) -> Result<Vec<ThreadMeta>> {
        let rows: Vec<ThreadRow> =
            sqlx::query_as(AssertSqlSafe(format!(
                "SELECT {THREAD_META_COLUMNS} FROM threads t WHERE t.parent_thread_id = ?1 ORDER BY t.created_at ASC"
            )))
            .bind(parent_id.as_str())
            .fetch_all(&self.pool)
            .await?;

        decode_meta_rows(rows)
    }

    pub(super) async fn list_session_threads(&self, root_id: &ThreadId) -> Result<Vec<ThreadMeta>> {
        let rows: Vec<ThreadRow> = sqlx::query_as(AssertSqlSafe(format!(
            "WITH RECURSIVE session_tree AS (
                    SELECT * FROM threads WHERE id = ?1
                    UNION ALL
                    SELECT t.* FROM threads t
                    INNER JOIN session_tree st ON t.parent_thread_id = st.id
                )
                SELECT {THREAD_META_COLUMNS} FROM session_tree t ORDER BY t.created_at ASC"
        )))
        .bind(root_id.as_str())
        .fetch_all(&self.pool)
        .await?;

        decode_meta_rows(rows)
    }

    pub(super) async fn invalidate_context_cache(&self, thread_id: &ThreadId) -> Result<()> {
        sqlx::query("UPDATE threads SET cached_context = NULL WHERE id = ?1")
            .bind(thread_id.as_str())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub(super) async fn get_context_cache_epoch(&self, thread_id: &ThreadId) -> Result<u64> {
        let row: Option<(i64,)> =
            sqlx::query_as("SELECT context_cache_epoch FROM threads WHERE id = ?1")
                .bind(thread_id.as_str())
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.map(|(e,)| e as u64).unwrap_or(0))
    }
}

fn decode_meta_rows(rows: Vec<ThreadRow>) -> Result<Vec<ThreadMeta>> {
    rows.into_iter()
        .map(|row| {
            meta_from_row(
                row.0, row.1, row.2, row.3, row.4, row.5, row.6, row.7, row.8, row.9, row.10,
                row.11, row.12, row.13,
            )
        })
        .collect()
}

/// 单条 metadata 读取（不含 `cached_context`）。
pub(super) async fn load_meta_on(
    connection: &mut SqliteConnection,
    id: &ThreadId,
) -> Result<ThreadMeta> {
    let row: ThreadRow = sqlx::query_as(AssertSqlSafe(format!(
        "SELECT {THREAD_META_COLUMNS} FROM threads t WHERE t.id = ?1"
    )))
    .bind(id.as_str())
    .fetch_one(&mut *connection)
    .await?;
    meta_from_row(
        row.0, row.1, row.2, row.3, row.4, row.5, row.6, row.7, row.8, row.9, row.10, row.11,
        row.12, row.13,
    )
}

/// 自有 payload 读取：`rowid` 顺序即 canonical 顺序，并复核行内 ID 与主键一致。
pub(super) async fn load_payloads_on(
    connection: &mut SqliteConnection,
    id: &ThreadId,
) -> Result<Vec<PersistedPayload>> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT message_id, content FROM messages WHERE thread_id = ?1 ORDER BY rowid",
    )
    .bind(id.as_str())
    .fetch_all(&mut *connection)
    .await?;
    rows.into_iter()
        .map(|(row_id, content)| {
            let payload = deserialize_persisted_payload(&content)?;
            if payload.id().as_uuid().to_string() != row_id {
                anyhow::bail!("persisted payload message id mismatch");
            }
            Ok(payload)
        })
        .collect()
}

pub(super) async fn load_context_payloads_on(
    connection: &mut SqliteConnection,
    thread_id: &ThreadId,
) -> Result<Vec<PersistedPayload>> {
    let mut payloads = load_inherited_context_on(connection, thread_id)
        .await?
        .payloads;
    payloads.extend(load_payloads_on(connection, thread_id).await?);
    Ok(payloads)
}

pub(super) async fn load_inherited_context_on(
    connection: &mut SqliteConnection,
    thread_id: &ThreadId,
) -> Result<InheritedContext> {
    // 已保存的继承快照是权威值：父会话之后的 compact/rewind 都不改变它。
    let stored: (Option<String>,) =
        sqlx::query_as("SELECT inherited_context FROM threads WHERE id = ?1")
            .bind(thread_id)
            .fetch_one(&mut *connection)
            .await?;
    if let Some(snapshot) = stored.0 {
        return InheritedContext::from_json(&snapshot);
    }
    let chain = resolve_ancestor_chain_on(connection, thread_id).await?;
    let mut context = InheritedContext::default();
    // Each edge's cutoff belongs to its child. A stored snapshot replaces all
    // inherited state, so later parent compaction/rewind cannot change it.
    for (index, tid) in chain.iter().enumerate() {
        let stored: (Option<String>,) =
            sqlx::query_as("SELECT inherited_context FROM threads WHERE id = ?1")
                .bind(tid)
                .fetch_one(&mut *connection)
                .await?;
        if let Some(snapshot) = stored.0 {
            context = InheritedContext::from_json(&snapshot)?;
            continue;
        }
        if index == 0 {
            continue;
        }
        let meta = load_meta_on(connection, tid).await?;
        let Some(cutoff) = meta.snapshot_at_message_id else {
            context = InheritedContext::default();
            continue;
        };
        let parent = &chain[index - 1];
        let own = load_payloads_up_to_on(connection, parent, &cutoff).await?;
        if own.is_empty() {
            // A parent with no own entries can point into its inherited region.
            if let Some(position) = context
                .payloads
                .iter()
                .position(|payload| payload.id().as_uuid().to_string() == cutoff)
            {
                context.payloads.truncate(position + 1);
            } else {
                anyhow::bail!("inherited context cutoff is missing");
            }
        } else {
            let own_ids = own.iter().map(PersistedPayload::id).collect::<HashSet<_>>();
            context.flags.extend(
                super::compaction::load_flags_on(connection, parent)
                    .await?
                    .into_iter()
                    .filter(|(id, _)| own_ids.contains(id)),
            );
            context.payloads.extend(own);
        }
        let ids = context
            .payloads
            .iter()
            .map(PersistedPayload::id)
            .collect::<HashSet<_>>();
        context.flags.retain(|id, _| ids.contains(id));
    }
    Ok(context)
}

/// 沿 parent_thread_id 链向上回溯，返回从根到自身的有序列表
async fn resolve_ancestor_chain_on(
    connection: &mut SqliteConnection,
    thread_id: &ThreadId,
) -> Result<Vec<ThreadId>> {
    let mut chain = vec![thread_id.clone()];
    let mut current = thread_id.clone();
    loop {
        let row: Option<(Option<String>,)> =
            sqlx::query_as("SELECT parent_thread_id FROM threads WHERE id = ?1")
                .bind(current.as_str())
                .fetch_optional(&mut *connection)
                .await?;
        match row {
            Some((Some(parent),)) => {
                if chain.contains(&parent) {
                    anyhow::bail!("cyclic thread ancestry");
                }
                chain.push(parent.clone());
                current = parent;
            }
            _ => break,
        }
    }
    chain.reverse();
    Ok(chain)
}

async fn load_payloads_up_to_on(
    connection: &mut SqliteConnection,
    thread_id: &ThreadId,
    message_id: &str,
) -> Result<Vec<PersistedPayload>> {
    let target_row: Option<(i64,)> =
        sqlx::query_as("SELECT rowid FROM messages WHERE thread_id = ?1 AND message_id = ?2")
            .bind(thread_id.as_str())
            .bind(message_id)
            .fetch_optional(&mut *connection)
            .await?;
    let Some((target_rowid,)) = target_row else {
        return Ok(vec![]);
    };
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT message_id, content FROM messages WHERE thread_id = ?1 AND rowid <= ?2 ORDER BY rowid",
    )
    .bind(thread_id.as_str())
    .bind(target_rowid)
    .fetch_all(&mut *connection)
    .await?;
    rows.into_iter()
        .map(|(row_id, content)| {
            let payload = deserialize_persisted_payload(&content)?;
            if payload.id().as_uuid().to_string() != row_id {
                anyhow::bail!("persisted payload message id mismatch");
            }
            Ok(payload)
        })
        .collect()
}

impl SqliteSessionDatabase {
    /// 保存 child 的继承区：只在缺失时写入，已有值不被覆盖。
    pub(super) async fn store_inherited_context(
        &self,
        thread_id: &ThreadId,
        context: &InheritedContext,
    ) -> Result<()> {
        let snapshot = context.to_json()?;
        // Validate before publishing, including the message/flag reference boundary.
        InheritedContext::from_json(&snapshot)?;
        let result = sqlx::query(
            "UPDATE threads SET inherited_context = ?1 WHERE id = ?2 AND inherited_context IS NULL",
        )
        .bind(snapshot)
        .bind(thread_id)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() != 1 {
            anyhow::bail!("inherited context already exists or thread is missing");
        }
        Ok(())
    }
}
