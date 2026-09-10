//! SQLite 连接、只读 schema 探测、迁移与安全错误分类。

use super::SqliteThreadStore;
use anyhow::{Context, Result};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    AssertSqlSafe,
};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::Duration,
};

const READ_ONLY_BUSY_TIMEOUT: Duration = Duration::from_millis(250);
pub(super) const REQUIRED_THREAD_COLUMNS: &[&str] = &[
    "id",
    "title",
    "cwd",
    "created_at",
    "updated_at",
    "message_count",
    "parent_thread_id",
    "snapshot_at_message_id",
    "hidden",
    "cancel_policy",
    "config",
    "cached_context",
    "agent_status",
];
pub(super) const REQUIRED_MESSAGE_COLUMNS: &[&str] = &["thread_id", "content"];

/// 只读 session 数据库访问的稳定失败分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOnlyStoreErrorKind {
    DatabaseNotFound,
    DatabaseUnreadable,
    SchemaIncompatible,
    SessionNotFound,
    CorruptSessionData,
    Internal,
}

/// 只读 session 数据库错误；所有公开格式及 source chain 均不携带数据库行值或 SQL 文本。
#[derive(Debug)]
pub enum ReadOnlyThreadStoreError {
    DatabaseNotFound,
    DatabaseUnreadable,
    SchemaIncompatible,
    SessionNotFound,
    CorruptSessionData,
    Internal,
}

impl ReadOnlyThreadStoreError {
    pub(super) fn from_kind(kind: ReadOnlyStoreErrorKind) -> Self {
        match kind {
            ReadOnlyStoreErrorKind::DatabaseNotFound => Self::DatabaseNotFound,
            ReadOnlyStoreErrorKind::DatabaseUnreadable => Self::DatabaseUnreadable,
            ReadOnlyStoreErrorKind::SchemaIncompatible => Self::SchemaIncompatible,
            ReadOnlyStoreErrorKind::SessionNotFound => Self::SessionNotFound,
            ReadOnlyStoreErrorKind::CorruptSessionData => Self::CorruptSessionData,
            ReadOnlyStoreErrorKind::Internal => Self::Internal,
        }
    }

    pub fn kind(&self) -> ReadOnlyStoreErrorKind {
        match self {
            Self::DatabaseNotFound => ReadOnlyStoreErrorKind::DatabaseNotFound,
            Self::DatabaseUnreadable => ReadOnlyStoreErrorKind::DatabaseUnreadable,
            Self::SchemaIncompatible => ReadOnlyStoreErrorKind::SchemaIncompatible,
            Self::SessionNotFound => ReadOnlyStoreErrorKind::SessionNotFound,
            Self::CorruptSessionData => ReadOnlyStoreErrorKind::CorruptSessionData,
            Self::Internal => ReadOnlyStoreErrorKind::Internal,
        }
    }
}

impl std::fmt::Display for ReadOnlyThreadStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self.kind() {
            ReadOnlyStoreErrorKind::DatabaseNotFound => "session database not found",
            ReadOnlyStoreErrorKind::DatabaseUnreadable => "session database is unreadable",
            ReadOnlyStoreErrorKind::SchemaIncompatible => "session database schema is incompatible",
            ReadOnlyStoreErrorKind::SessionNotFound => "session not found",
            ReadOnlyStoreErrorKind::CorruptSessionData => "session data is corrupt",
            ReadOnlyStoreErrorKind::Internal => "internal storage error",
        };
        f.write_str(message)
    }
}

impl std::error::Error for ReadOnlyThreadStoreError {}

impl SqliteThreadStore {
    /// 使用指定路径打开（或创建）数据库，并初始化 Schema
    pub async fn new(db_path: impl Into<PathBuf>) -> Result<Self> {
        let db_path = db_path.into();
        // 确保父目录存在
        if let Some(parent) = db_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
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
        let store = Self {
            pool,
            read_only: false,
        };
        store.init_schema().await?;
        Ok(store)
    }

    /// 以 SQLite read-only capability 打开已存在的数据库。
    ///
    /// 该路径不创建目录、数据库或 schema，也不执行 migration。
    pub async fn open_existing_read_only(
        db_path: impl AsRef<Path>,
    ) -> std::result::Result<Self, ReadOnlyThreadStoreError> {
        let db_path = db_path.as_ref();
        let metadata = tokio::fs::metadata(db_path).await.map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                ReadOnlyThreadStoreError::from_kind(ReadOnlyStoreErrorKind::DatabaseNotFound)
            } else {
                ReadOnlyThreadStoreError::from_kind(ReadOnlyStoreErrorKind::DatabaseUnreadable)
            }
        })?;
        if !metadata.is_file() {
            return Err(ReadOnlyThreadStoreError::from_kind(
                ReadOnlyStoreErrorKind::DatabaseUnreadable,
            ));
        }

        let options = SqliteConnectOptions::new()
            .filename(db_path)
            .read_only(true)
            .create_if_missing(false)
            .busy_timeout(READ_ONLY_BUSY_TIMEOUT);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .map_err(|_| {
                ReadOnlyThreadStoreError::from_kind(ReadOnlyStoreErrorKind::DatabaseUnreadable)
            })?;
        let store = Self {
            pool,
            read_only: true,
        };
        store.probe_load_meta_shape().await?;
        Ok(store)
    }

    async fn probe_load_meta_shape(&self) -> std::result::Result<(), ReadOnlyThreadStoreError> {
        for (table, required) in [
            ("threads", REQUIRED_THREAD_COLUMNS),
            ("messages", REQUIRED_MESSAGE_COLUMNS),
        ] {
            let rows: Vec<(String,)> = sqlx::query_as(AssertSqlSafe(format!(
                "SELECT name FROM pragma_table_info('{table}')"
            )))
            .fetch_all(&self.pool)
            .await
            .map_err(|_| {
                ReadOnlyThreadStoreError::from_kind(ReadOnlyStoreErrorKind::SchemaIncompatible)
            })?;
            let actual: HashSet<String> = rows.into_iter().map(|(name,)| name).collect();
            if !required.iter().all(|column| actual.contains(*column)) {
                return Err(ReadOnlyThreadStoreError::from_kind(
                    ReadOnlyStoreErrorKind::SchemaIncompatible,
                ));
            }
        }
        Ok(())
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
            "ALTER TABLE threads ADD COLUMN frozen_context TEXT",
            "ALTER TABLE threads ADD COLUMN agent_status TEXT NOT NULL DEFAULT 'active'",
            "ALTER TABLE messages ADD COLUMN truncated BOOLEAN NOT NULL DEFAULT 0",
            "ALTER TABLE messages ADD COLUMN excluded BOOLEAN NOT NULL DEFAULT 0",
            "ALTER TABLE messages ADD COLUMN projection TEXT",
            // H6: context cache 纪元，每次 compact 提交后递增
            "ALTER TABLE threads ADD COLUMN context_cache_epoch INTEGER NOT NULL DEFAULT 0",
        ];
        for sql in &alter_columns {
            // SQLite 返回 "duplicate column name" 时忽略
            // 常量数组（'static str）仅含 DDL 列名，无动态输入；sqlx 0.9 需显式断言
            if let Err(e) = sqlx::query(AssertSqlSafe(*sql)).execute(&self.pool).await {
                let msg = e.to_string();
                if !msg.contains("duplicate column name") {
                    return Err(e.into());
                }
            }
        }

        Ok(())
    }
}
