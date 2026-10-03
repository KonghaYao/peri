//! 消息生命周期持久化：删除、flags、回滚与原子 compaction 事务。

use super::failure::commit_failure;
use super::{database::SqliteSessionDatabase, row_mapping::role_of};
use anyhow::Result;
use chrono::Utc;
use peri_acp_types::{
    store::{CompactionChange, MessageFlags},
    thread::ThreadId,
};
use sqlx::SqliteConnection;
use std::collections::HashMap;

pub(super) async fn delete_messages(
    database: &SqliteSessionDatabase,
    thread_id: &ThreadId,
    message_ids: &[peri_acp_types::messages::MessageId],
) -> Result<()> {
    if message_ids.is_empty() {
        return Ok(());
    }
    let mut tx = database.pool.begin().await?;
    for mid in message_ids {
        let uuid_str = mid.as_uuid().to_string();
        sqlx::query("DELETE FROM messages WHERE message_id = ?1 AND thread_id = ?2")
            .bind(&uuid_str)
            .bind(thread_id.as_str())
            .execute(&mut *tx)
            .await?;
    }
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "UPDATE threads SET updated_at = ?1,
                message_count = (SELECT COUNT(*) FROM messages WHERE thread_id = ?2)
             WHERE id = ?2",
    )
    .bind(&now)
    .bind(thread_id.as_str())
    .execute(&mut *tx)
    .await?;
    tx.commit()
        .await
        .map_err(|_| commit_failure(Some(thread_id.clone())))?;
    Ok(())
}

pub(super) async fn update_message_flags(
    database: &SqliteSessionDatabase,
    message_id: &peri_acp_types::messages::MessageId,
    flags: &MessageFlags,
) -> Result<()> {
    let id_str = message_id.as_uuid().to_string();
    let projection_json = if let Some(ref directive) = flags.projection {
        Some(serde_json::to_string(directive)?)
    } else {
        None
    };
    sqlx::query(
        "UPDATE messages SET truncated = ?, excluded = ?, projection = ? WHERE message_id = ?",
    )
    .bind(flags.truncated)
    .bind(flags.excluded)
    .bind(&projection_json)
    .bind(&id_str)
    .execute(&database.pool)
    .await?;

    Ok(())
}

pub(super) async fn commit_compaction_lifecycle(
    database: &SqliteSessionDatabase,
    thread_id: &ThreadId,
    lifecycle: &CompactionChange,
) -> Result<()> {
    let mut tx = database.pool.begin().await?;
    commit_compaction_lifecycle_on(&mut tx, thread_id, lifecycle).await?;
    tx.commit()
        .await
        .map_err(|_| commit_failure(Some(thread_id.clone())))?;
    Ok(())
}

pub(super) async fn commit_compaction_lifecycle_on(
    tx: &mut SqliteConnection,
    thread_id: &ThreadId,
    lifecycle: &CompactionChange,
) -> Result<()> {
    for (message_id, flags) in &lifecycle.flag_updates {
        let projection_json = flags
            .projection
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let result = sqlx::query(
            "UPDATE messages
                 SET truncated = ?1, excluded = ?2, projection = ?3
                 WHERE message_id = ?4 AND thread_id = ?5",
        )
        .bind(flags.truncated)
        .bind(flags.excluded)
        .bind(&projection_json)
        .bind(message_id.as_uuid().to_string())
        .bind(thread_id.as_str())
        .execute(&mut *tx)
        .await?;
        anyhow::ensure!(
            result.rows_affected() == 1,
            "message {} not found in thread {}",
            message_id.as_uuid(),
            thread_id.as_str()
        );
    }

    for message in &lifecycle.appended_messages {
        let message_id = message.id().as_uuid().to_string();
        let content = serde_json::to_string(message)?;
        sqlx::query(
            "INSERT INTO messages (message_id, thread_id, role, content)
                 VALUES (?1, ?2, ?3, ?4)",
        )
        .bind(&message_id)
        .bind(thread_id.as_str())
        .bind(role_of(message))
        .bind(&content)
        .execute(&mut *tx)
        .await?;
    }

    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "UPDATE threads
             SET updated_at = ?1,
                 message_count = (SELECT COUNT(*) FROM messages WHERE thread_id = ?2)
             WHERE id = ?2",
    )
    .bind(&now)
    .bind(thread_id.as_str())
    .execute(&mut *tx)
    .await?;

    Ok(())
}

pub(super) async fn load_message_flags(
    database: &SqliteSessionDatabase,
    thread_id: &ThreadId,
) -> Result<HashMap<peri_acp_types::messages::MessageId, MessageFlags>> {
    let mut connection = database.pool.acquire().await?;
    load_flags_on(&mut connection, thread_id).await
}

/// flags 是派生视图：只返回非默认标记。
///
/// 损坏的 `message_id` 或无法解释的 projection 是数据故障，不是「没有标记」：
/// 这里直接失败，避免调用方把损坏当成空值继续覆盖写入。
pub(super) async fn load_flags_on(
    connection: &mut SqliteConnection,
    thread_id: &ThreadId,
) -> Result<HashMap<peri_acp_types::messages::MessageId, MessageFlags>> {
    let rows: Vec<(String, bool, bool, Option<String>)> = sqlx::query_as(
        "SELECT message_id, truncated, excluded, projection FROM messages \
             WHERE thread_id = ?1 AND (truncated = 1 OR excluded = 1 OR projection IS NOT NULL)",
    )
    .bind(thread_id.as_str())
    .fetch_all(&mut *connection)
    .await?;

    let mut flags = HashMap::with_capacity(rows.len());
    for (id_str, truncated, excluded, projection_json) in rows {
        let id = uuid::Uuid::parse_str(&id_str)
            .map_err(|_| anyhow::anyhow!("stored message id is not a uuid"))?;
        let projection = projection_json
            .map(|json| serde_json::from_str(&json))
            .transpose()
            .map_err(|_| anyhow::anyhow!("stored message projection is not readable"))?;
        flags.insert(
            id.into(),
            MessageFlags {
                truncated,
                excluded,
                projection,
            },
        );
    }
    Ok(flags)
}

pub(super) async fn delete_messages_since(
    database: &SqliteSessionDatabase,
    thread_id: &ThreadId,
    message_id: &peri_acp_types::messages::MessageId,
) -> Result<()> {
    // 通过 rowid 定位目标消息在时间线上的位置
    let target_rowid: Option<(i64,)> =
        sqlx::query_as("SELECT rowid FROM messages WHERE thread_id = ?1 AND message_id = ?2")
            .bind(thread_id.as_str())
            .bind(message_id.as_uuid().to_string())
            .fetch_optional(&database.pool)
            .await?;

    if let Some((rowid,)) = target_rowid {
        let mut tx = database.pool.begin().await?;
        sqlx::query("DELETE FROM messages WHERE thread_id = ?1 AND rowid > ?2")
            .bind(thread_id.as_str())
            .bind(rowid)
            .execute(&mut *tx)
            .await?;
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "UPDATE threads SET updated_at = ?1,
                    message_count = (SELECT COUNT(*) FROM messages WHERE thread_id = ?2)
                 WHERE id = ?2",
        )
        .bind(&now)
        .bind(thread_id.as_str())
        .execute(&mut *tx)
        .await?;
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(thread_id.clone())))?;
    }
    Ok(())
}
