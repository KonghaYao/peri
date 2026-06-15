use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    SqlitePool,
};

use crate::{
    messages::BaseMessage,
    thread::{AgentStatus, CancelPolicy, ThreadId, ThreadMeta, ThreadStore},
};

/// SELECT 所有 thread 列的统一常量（含 cached_context，仅 load_context 等需要完整数据的场景使用）
const THREAD_COLUMNS: &str = "t.id, t.title, t.cwd, t.created_at, t.updated_at, t.message_count,
    (SELECT COALESCE(SUM(LENGTH(m.content)), 0) FROM messages m WHERE m.thread_id = t.id) as content_size,
    t.parent_thread_id, t.snapshot_at_message_id, t.hidden, t.cancel_policy, t.config, t.cached_context, t.agent_status";

/// SELECT thread 元数据列（不含 cached_context），用于 list_threads 等列表场景。
/// cached_context 包含完整消息历史 JSON，加载所有线程时会占用大量内存（~1MB/线程）。
const THREAD_META_COLUMNS: &str = "t.id, t.title, t.cwd, t.created_at, t.updated_at, t.message_count,
    (SELECT COALESCE(SUM(LENGTH(m.content)), 0) FROM messages m WHERE m.thread_id = t.id) as content_size,
    t.parent_thread_id, t.snapshot_at_message_id, t.hidden, t.cancel_policy, t.config, NULL as cached_context, t.agent_status";

/// 基于 SQLite 的 ThreadStore 实现
///
/// 使用 WAL 模式提升并发读性能，sqlx SqlitePool 连接池管理并发。
pub struct SqliteThreadStore {
    pool: SqlitePool,
}

impl SqliteThreadStore {
    /// 使用指定路径打开（或创建）数据库，并初始化 Schema
    pub async fn new(db_path: impl Into<PathBuf>) -> Result<Self> {
        let db_path = db_path.into();
        // 确保父目录存在
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("创建目录失败: {}", parent.display()))?;
        }
        let options = SqliteConnectOptions::new()
            .filename(&db_path)
            .create_if_missing(true)
            .pragma("journal_mode", "WAL")
            .pragma("synchronous", "NORMAL")
            .pragma("foreign_keys", "ON");
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await?;
        let store = Self { pool };
        store.init_schema().await?;
        Ok(store)
    }

    /// 使用默认路径 `~/.peri/threads/threads.db` 创建
    pub async fn default_path() -> Result<Self> {
        let db_path = dirs_next::home_dir()
            .context("无法获取 home 目录")?
            .join(".peri")
            .join("threads")
            .join("threads.db");
        Self::new(db_path).await
    }

    /// 初始化 Schema（幂等，可重复调用）
    async fn init_schema(&self) -> Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS threads (
                id          TEXT PRIMARY KEY,
                title       TEXT,
                cwd         TEXT NOT NULL DEFAULT '',
                created_at  TEXT NOT NULL,
                updated_at  TEXT NOT NULL,
                message_count INTEGER NOT NULL DEFAULT 0
            )",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS messages (
                message_id  TEXT PRIMARY KEY,
                thread_id   TEXT NOT NULL,
                role        TEXT NOT NULL,
                content     TEXT NOT NULL,
                FOREIGN KEY (thread_id) REFERENCES threads(id) ON DELETE CASCADE
            )",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_messages_thread_id ON messages (thread_id ASC)",
        )
        .execute(&self.pool)
        .await?;

        // 迁移：为已有表添加新列（忽略 "duplicate column" 错误实现幂等）
        let alter_columns = [
            "ALTER TABLE threads ADD COLUMN parent_thread_id TEXT",
            "ALTER TABLE threads ADD COLUMN snapshot_at_message_id TEXT",
            "ALTER TABLE threads ADD COLUMN hidden BOOLEAN NOT NULL DEFAULT 0",
            "ALTER TABLE threads ADD COLUMN cancel_policy TEXT NOT NULL DEFAULT 'cascade'",
            "ALTER TABLE threads ADD COLUMN config TEXT",
            "ALTER TABLE threads ADD COLUMN cached_context TEXT",
            "ALTER TABLE threads ADD COLUMN agent_status TEXT NOT NULL DEFAULT 'active'",
        ];
        for sql in &alter_columns {
            // SQLite 返回 "duplicate column name" 时忽略
            if let Err(e) = sqlx::query(sql).execute(&self.pool).await {
                let msg = e.to_string();
                if !msg.contains("duplicate column name") {
                    return Err(e.into());
                }
            }
        }

        Ok(())
    }

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

    /// 加载指定 thread 中 rowid <= 目标消息 rowid 的所有消息
    async fn load_messages_up_to(
        &self,
        thread_id: &ThreadId,
        message_id: &str,
    ) -> Result<Vec<BaseMessage>> {
        // 先查找目标消息的 rowid
        let target_row: Option<(i64,)> =
            sqlx::query_as("SELECT rowid FROM messages WHERE message_id = ?1")
                .bind(message_id)
                .fetch_optional(&self.pool)
                .await?;

        let target_rowid = match target_row {
            Some((rid,)) => rid,
            None => {
                // 消息不存在，返回空
                return Ok(vec![]);
            }
        };

        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT content FROM messages WHERE thread_id = ?1 AND rowid <= ?2 ORDER BY rowid",
        )
        .bind(thread_id.as_str())
        .bind(target_rowid)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|(content,)| serde_json::from_str(&content).map_err(Into::into))
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

// ── 辅助函数 ──────────────────────────────────────────────────────────────────

fn role_of(msg: &BaseMessage) -> &'static str {
    match msg {
        BaseMessage::Human { .. } => "user",
        BaseMessage::Ai { .. } => "assistant",
        BaseMessage::System { .. } => "system",
        BaseMessage::Tool { .. } => "tool",
    }
}

#[allow(clippy::too_many_arguments)]
fn meta_from_row(
    id: String,
    title: Option<String>,
    cwd: String,
    created_at: String,
    updated_at: String,
    message_count: i64,
    content_size: i64,
    parent_thread_id: Option<String>,
    snapshot_at_message_id: Option<String>,
    hidden: bool,
    cancel_policy: String,
    config: Option<String>,
    cached_context: Option<String>,
    agent_status: String,
) -> Result<ThreadMeta> {
    // 关键约束：DB 字符串必须经 FromStr 解析为强类型枚举；非法值不静默 fallback
    let cancel_policy = CancelPolicy::from_str(&cancel_policy)
        .with_context(|| format!("解析 cancel_policy 失败（thread_id={}）", id))?;
    let agent_status = AgentStatus::from_str(&agent_status)
        .with_context(|| format!("解析 agent_status 失败（thread_id={}）", id))?;
    Ok(ThreadMeta {
        id,
        title,
        cwd,
        created_at: created_at.parse::<DateTime<Utc>>()?,
        updated_at: updated_at.parse::<DateTime<Utc>>()?,
        message_count: message_count as usize,
        content_size: content_size as u64,
        parent_thread_id,
        snapshot_at_message_id,
        hidden,
        cancel_policy,
        config,
        cached_context,
        agent_status,
    })
}

/// 从消息列表中提取标题（取第一条 Human 消息的前 50 字符）
fn extract_title(msgs: &[BaseMessage]) -> Option<String> {
    use crate::messages::{ContentBlock, MessageContent};
    for msg in msgs {
        if let BaseMessage::Human { content, .. } = msg {
            let text = match content {
                MessageContent::Text(t) => t.clone(),
                MessageContent::Blocks(blocks) => blocks
                    .iter()
                    .filter_map(|b| {
                        if let ContentBlock::Text { text } = b {
                            Some(text.as_str())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
                MessageContent::Raw(_) => continue,
            };
            let title: String = text.chars().take(50).collect();
            if !title.is_empty() {
                return Some(title);
            }
        }
    }
    None
}

// ── ThreadStore impl ───────────────────────────────────────────────────────────

#[async_trait]
impl ThreadStore for SqliteThreadStore {
    async fn create_thread(&self, meta: ThreadMeta) -> Result<ThreadId> {
        let id = meta.id.clone();
        sqlx::query(
            "INSERT INTO threads (id, title, cwd, created_at, updated_at, message_count,
                parent_thread_id, snapshot_at_message_id, hidden, cancel_policy, config, cached_context, agent_status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        )
        .bind(&meta.id)
        .bind(&meta.title)
        .bind(&meta.cwd)
        .bind(meta.created_at.to_rfc3339())
        .bind(meta.updated_at.to_rfc3339())
        .bind(meta.message_count as i64)
        .bind(&meta.parent_thread_id)
        .bind(&meta.snapshot_at_message_id)
        .bind(meta.hidden)
        .bind(meta.cancel_policy.as_str())
        .bind(&meta.config)
        .bind(&meta.cached_context)
        .bind(meta.agent_status.as_str())
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    async fn append_messages(&self, id: &ThreadId, msgs: &[BaseMessage]) -> Result<()> {
        if msgs.is_empty() {
            return Ok(());
        }
        let mut tx = self.pool.begin().await?;
        for msg in msgs {
            let message_id = msg.id().as_uuid().to_string();
            let role = role_of(msg);
            let content = serde_json::to_string(msg)?;
            sqlx::query(
                "INSERT OR IGNORE INTO messages (message_id, thread_id, role, content)
                 VALUES (?1, ?2, ?3, ?4)",
            )
            .bind(&message_id)
            .bind(id.as_str())
            .bind(role)
            .bind(&content)
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
        .bind(id.as_str())
        .execute(&mut *tx)
        .await?;

        if let Some(title) = extract_title(msgs) {
            sqlx::query("UPDATE threads SET title = ?1 WHERE id = ?2 AND title IS NULL")
                .bind(&title)
                .bind(id.as_str())
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn load_messages(&self, id: &ThreadId) -> Result<Vec<BaseMessage>> {
        let rows: Vec<(String,)> =
            sqlx::query_as("SELECT content FROM messages WHERE thread_id = ?1 ORDER BY rowid")
                .bind(id.as_str())
                .fetch_all(&self.pool)
                .await?;

        rows.into_iter()
            .map(|(content,)| serde_json::from_str(&content).map_err(Into::into))
            .collect()
    }

    async fn load_meta(&self, id: &ThreadId) -> Result<ThreadMeta> {
        let row: (
            String,
            Option<String>,
            String,
            String,
            String,
            i64,
            i64,
            Option<String>,
            Option<String>,
            bool,
            String,
            Option<String>,
            Option<String>,
            String,
        ) = sqlx::query_as(&format!(
            "SELECT {THREAD_COLUMNS} FROM threads t WHERE t.id = ?1"
        ))
        .bind(id.as_str())
        .fetch_one(&self.pool)
        .await?;

        meta_from_row(
            row.0, row.1, row.2, row.3, row.4, row.5, row.6, row.7, row.8, row.9, row.10, row.11,
            row.12, row.13,
        )
    }

    async fn update_meta(&self, id: &ThreadId, meta: ThreadMeta) -> Result<()> {
        sqlx::query(
            "UPDATE threads SET title = ?1, cwd = ?2, updated_at = ?3, message_count = ?4,
                parent_thread_id = ?5, snapshot_at_message_id = ?6, hidden = ?7,
                cancel_policy = ?8, config = ?9, cached_context = ?10, agent_status = ?11
             WHERE id = ?12",
        )
        .bind(&meta.title)
        .bind(&meta.cwd)
        .bind(meta.updated_at.to_rfc3339())
        .bind(meta.message_count as i64)
        .bind(&meta.parent_thread_id)
        .bind(&meta.snapshot_at_message_id)
        .bind(meta.hidden)
        .bind(meta.cancel_policy.as_str())
        .bind(&meta.config)
        .bind(&meta.cached_context)
        .bind(meta.agent_status.as_str())
        .bind(id.as_str())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list_threads(&self) -> Result<Vec<ThreadMeta>> {
        let rows: Vec<(
            String,
            Option<String>,
            String,
            String,
            String,
            i64,
            i64,
            Option<String>,
            Option<String>,
            bool,
            String,
            Option<String>,
            Option<String>,
            String,
        )> = sqlx::query_as(&format!(
            "SELECT {THREAD_META_COLUMNS} FROM threads t WHERE t.hidden = 0 ORDER BY t.updated_at DESC"
        ))
        .fetch_all(&self.pool)
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

    async fn delete_thread(&self, id: &ThreadId) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM threads WHERE id = ?1")
            .bind(id.as_str())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn update_title(&self, id: &ThreadId, title: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        sqlx::query("UPDATE threads SET title = ?1, updated_at = ?2 WHERE id = ?3")
            .bind(title)
            .bind(&now)
            .bind(id.as_str())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn load_context(&self, thread_id: &ThreadId) -> Result<Vec<BaseMessage>> {
        // 先尝试从 cached_context 读取
        let cache_row: Option<(Option<String>,)> =
            sqlx::query_as("SELECT cached_context FROM threads WHERE id = ?1")
                .bind(thread_id.as_str())
                .fetch_optional(&self.pool)
                .await?;

        let cached = cache_row.and_then(|(c,)| c);

        if let Some(json) = cached {
            let mut cached_msgs: Vec<BaseMessage> = serde_json::from_str(&json)?;
            // 检查是否有新消息追加到缓存之后
            let cached_count = cached_msgs.len();
            let rows: Vec<(String,)> = sqlx::query_as(
                "SELECT content FROM messages WHERE thread_id = ?1 ORDER BY rowid LIMIT -1 OFFSET ?2"
            )
            .bind(thread_id.as_str())
            .bind(cached_count as i64)
            .fetch_all(&self.pool)
            .await?;

            if rows.is_empty() {
                return Ok(cached_msgs);
            }

            let new_msgs: Vec<BaseMessage> = rows
                .into_iter()
                .map(|(content,)| serde_json::from_str(&content).map_err(Into::into))
                .collect::<Result<Vec<_>>>()?;
            cached_msgs.extend(new_msgs);

            // 更新缓存
            self.save_context_cache(thread_id, &cached_msgs).await?;
            return Ok(cached_msgs);
        }

        // 缓存未命中：解析祖先链 + 各级消息
        let chain = self.resolve_ancestor_chain(thread_id).await?;
        let mut all_msgs = Vec::new();

        for (i, tid) in chain.iter().enumerate() {
            let is_last = i == chain.len() - 1;

            if is_last {
                // 自身线程：加载全部消息
                let msgs = self.load_messages(tid).await?;
                all_msgs.extend(msgs);
            } else {
                // 祖先线程：只加载到 snapshot_at_message_id
                let meta = self.load_meta(tid).await?;
                if let Some(ref snap_id) = meta.snapshot_at_message_id {
                    let msgs = self.load_messages_up_to(tid, snap_id).await?;
                    all_msgs.extend(msgs);
                }
            }
        }

        // 保存缓存
        if !all_msgs.is_empty() {
            self.save_context_cache(thread_id, &all_msgs).await?;
        }

        Ok(all_msgs)
    }

    async fn list_child_threads(&self, parent_id: &ThreadId) -> Result<Vec<ThreadMeta>> {
        let rows: Vec<(String, Option<String>, String, String, String, i64, i64,
                       Option<String>, Option<String>, bool, String, Option<String>, Option<String>, String)> =
            sqlx::query_as(&format!(
                "SELECT {THREAD_META_COLUMNS} FROM threads t WHERE t.parent_thread_id = ?1 ORDER BY t.created_at ASC"
            ))
            .bind(parent_id.as_str())
            .fetch_all(&self.pool)
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

    async fn list_session_threads(&self, root_id: &ThreadId) -> Result<Vec<ThreadMeta>> {
        let rows: Vec<(
            String,
            Option<String>,
            String,
            String,
            String,
            i64,
            i64,
            Option<String>,
            Option<String>,
            bool,
            String,
            Option<String>,
            Option<String>,
            String,
        )> = sqlx::query_as(&format!(
            "WITH RECURSIVE session_tree AS (
                    SELECT * FROM threads WHERE id = ?1
                    UNION ALL
                    SELECT t.* FROM threads t
                    INNER JOIN session_tree st ON t.parent_thread_id = st.id
                )
                SELECT {THREAD_META_COLUMNS} FROM session_tree t ORDER BY t.created_at ASC"
        ))
        .bind(root_id.as_str())
        .fetch_all(&self.pool)
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

    async fn update_thread_status(&self, id: &ThreadId, status: &str) -> Result<()> {
        // 关键约束：参数字符串必须经 FromStr 解析，非法值直接返回错误，不静默 fallback
        let status = AgentStatus::from_str(status)
            .with_context(|| format!("非法 agent_status 值: {status:?}"))?;
        let now = Utc::now().to_rfc3339();
        sqlx::query("UPDATE threads SET agent_status = ?1, updated_at = ?2 WHERE id = ?3")
            .bind(status.as_str())
            .bind(&now)
            .bind(id.as_str())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn invalidate_context_cache(&self, thread_id: &ThreadId) -> Result<()> {
        sqlx::query("UPDATE threads SET cached_context = NULL WHERE id = ?1")
            .bind(thread_id.as_str())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn delete_messages(
        &self,
        thread_id: &ThreadId,
        message_ids: &[crate::messages::MessageId],
    ) -> Result<()> {
        if message_ids.is_empty() {
            return Ok(());
        }
        let mut tx = self.pool.begin().await?;
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
        tx.commit().await?;
        self.invalidate_context_cache(thread_id).await?;
        Ok(())
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    include!("sqlite_store_test.rs");
}
