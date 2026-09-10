//! 祖先 payload 边界、上下文缓存与线程树读取。

use super::{
    row_mapping::{meta_from_row, ThreadRow, THREAD_META_COLUMNS},
    SqliteThreadStore,
};
use anyhow::Result;
use chrono::Utc;
use peri_acp_types::{
    messages::BaseMessage,
    store::{deserialize_persisted_payload, PersistedPayload, ThreadStore},
    thread::{ThreadId, ThreadMeta},
};
use sqlx::AssertSqlSafe;

impl SqliteThreadStore {
    /// 沿 parent_thread_id 链向上回溯，返回从根到自身的有序列表
    async fn resolve_ancestor_chain(&self, thread_id: &ThreadId) -> Result<Vec<ThreadId>> {
        let mut chain = vec![thread_id.clone()];
        let mut current = thread_id.clone();
        loop {
            let row: Option<(Option<String>,)> =
                sqlx::query_as("SELECT parent_thread_id FROM threads WHERE id = ?1")
                    .bind(current.as_str())
                    .fetch_optional(&self.pool)
                    .await?;
            match row {
                Some((Some(parent),)) => {
                    chain.push(parent.clone());
                    current = parent;
                }
                _ => break,
            }
        }
        chain.reverse();
        Ok(chain)
    }

    async fn load_payloads_up_to(
        &self,
        thread_id: &ThreadId,
        message_id: &str,
    ) -> Result<Vec<PersistedPayload>> {
        let target_row: Option<(i64,)> =
            sqlx::query_as("SELECT rowid FROM messages WHERE thread_id = ?1 AND message_id = ?2")
                .bind(thread_id.as_str())
                .bind(message_id)
                .fetch_optional(&self.pool)
                .await?;
        let Some((target_rowid,)) = target_row else {
            return Ok(vec![]);
        };
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT message_id, content FROM messages WHERE thread_id = ?1 AND rowid <= ?2 ORDER BY rowid",
        )
        .bind(thread_id.as_str())
        .bind(target_rowid)
        .fetch_all(&self.pool)
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

    /// 将消息序列化为 JSON 并保存到 cached_context 列
    async fn save_context_cache(
        &self,
        thread_id: &ThreadId,
        messages: &[BaseMessage],
    ) -> Result<()> {
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
}

pub(super) async fn load_context_payloads(
    store: &SqliteThreadStore,
    thread_id: &ThreadId,
) -> Result<Vec<PersistedPayload>> {
    let chain = store.resolve_ancestor_chain(thread_id).await?;
    let mut payloads = Vec::new();
    for (index, tid) in chain.iter().enumerate() {
        if index == chain.len() - 1 {
            payloads.extend(store.load_payloads(tid).await?);
        } else if let Some(snapshot_id) = store.load_meta(tid).await?.snapshot_at_message_id {
            payloads.extend(store.load_payloads_up_to(tid, &snapshot_id).await?);
        }
    }
    Ok(payloads)
}

pub(super) async fn load_context(
    store: &SqliteThreadStore,
    thread_id: &ThreadId,
) -> Result<Vec<BaseMessage>> {
    let messages = store
        .load_context_payloads(thread_id)
        .await?
        .into_iter()
        .filter_map(|payload| payload.as_message().cloned())
        .collect::<Vec<_>>();
    if !messages.is_empty() {
        store.save_context_cache(thread_id, &messages).await?;
    }
    Ok(messages)
}

pub(super) async fn list_child_threads(
    store: &SqliteThreadStore,
    parent_id: &ThreadId,
) -> Result<Vec<ThreadMeta>> {
    let rows: Vec<ThreadRow> =
            sqlx::query_as(AssertSqlSafe(format!(
                "SELECT {THREAD_META_COLUMNS} FROM threads t WHERE t.parent_thread_id = ?1 ORDER BY t.created_at ASC"
            )))
            .bind(parent_id.as_str())
            .fetch_all(&store.pool)
            .await?;

    rows.into_iter()
        .map(|row| {
            meta_from_row(
                row.0, row.1, row.2, row.3, row.4, row.5, row.6, row.7, row.8, row.9, row.10,
                row.11, row.12, row.13,
            )
        })
        .collect()
}

pub(super) async fn list_session_threads(
    store: &SqliteThreadStore,
    root_id: &ThreadId,
) -> Result<Vec<ThreadMeta>> {
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
    .fetch_all(&store.pool)
    .await?;

    rows.into_iter()
        .map(|row| {
            meta_from_row(
                row.0, row.1, row.2, row.3, row.4, row.5, row.6, row.7, row.8, row.9, row.10,
                row.11, row.12, row.13,
            )
        })
        .collect()
}

pub(super) async fn invalidate_context_cache(
    store: &SqliteThreadStore,
    thread_id: &ThreadId,
) -> Result<()> {
    sqlx::query("UPDATE threads SET cached_context = NULL WHERE id = ?1")
        .bind(thread_id.as_str())
        .execute(&store.pool)
        .await?;
    Ok(())
}

pub(super) async fn get_context_cache_epoch(
    store: &SqliteThreadStore,
    thread_id: &ThreadId,
) -> Result<u64> {
    let row: Option<(i64,)> =
        sqlx::query_as("SELECT context_cache_epoch FROM threads WHERE id = ?1")
            .bind(thread_id.as_str())
            .fetch_optional(&store.pool)
            .await?;
    Ok(row.map(|(e,)| e as u64).unwrap_or(0))
}
