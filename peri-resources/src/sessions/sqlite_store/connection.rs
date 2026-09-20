//! SQLite 连接、只读 schema 探测、单库升级与安全错误分类。

use super::SqliteThreadStore;
use anyhow::{Context, Result};
use peri_acp_types::workspace::WorkspaceError;
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    AssertSqlSafe, Connection,
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

/// 只读 shape probe 查询失败的分类。
///
/// 只有确定性的「不是 SQLite 数据库 / 镜像损坏」才能判定 schema 不兼容；锁竞争、
/// IO 故障、`-wal`/`-shm` 不可用等瞬时或环境故障必须保持可诊断，否则一次并发
/// checkpoint 会被误报成 schema 问题。SQLite primary result code：
/// `SQLITE_CORRUPT` = 11，`SQLITE_NOTADB` = 26。
pub(super) fn classify_shape_probe_failure(error: &sqlx::Error) -> ReadOnlyThreadStoreError {
    let damaged_image = match error {
        sqlx::Error::Database(db) => matches!(db.code().as_deref(), Some("11") | Some("26")),
        _ => false,
    };
    if damaged_image {
        ReadOnlyThreadStoreError::from_kind(ReadOnlyStoreErrorKind::SchemaIncompatible)
    } else {
        ReadOnlyThreadStoreError::from_kind(ReadOnlyStoreErrorKind::DatabaseUnreadable)
    }
}

impl SqliteThreadStore {
    /// 打开或创建会话数据库，原地升级已知旧 schema 并保留历史数据。
    pub async fn new(db_path: impl Into<PathBuf>) -> Result<Self> {
        let db_path = db_path.into();
        // 确保父目录存在
        if let Some(parent) = db_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("创建目录失败: {}", parent.display()))?;
        }
        let _schema_lock = lock_schema_open(&db_path).await?;
        // 在 WAL/DDL 写入前识别未知 schema；已知旧库交给事务升级。
        if tokio::fs::metadata(&db_path)
            .await
            .is_ok_and(|meta| meta.len() > 0)
        {
            let mut probe = sqlx::SqliteConnection::connect_with(
                &SqliteConnectOptions::new()
                    .filename(&db_path)
                    .read_only(true)
                    .create_if_missing(false),
            )
            .await?;
            let schema = super::schema::inspect(&mut probe).await;
            probe.close().await?;
            schema?;
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
            db_path: tokio::fs::canonicalize(&db_path).await?,
            execution_leases: Default::default(),
        };
        if let Err(error) = store.init_schema().await {
            store.close().await;
            return Err(error);
        }
        Ok(store)
    }

    /// 关闭连接池并等待全部连接释放。
    ///
    /// `Drop` 返回时不等待连接关闭完成，最后一次连接关闭触发的 WAL checkpoint 与
    /// `-wal`/`-shm` 清理因此可能晚于 `Drop` 返回，并与并发只读打开重叠。需要确定性
    /// 收尾时调用本方法：返回后本进程不再持有该数据库的连接，且侧车文件已完成收尾。
    pub async fn close(&self) {
        self.pool.close().await;
    }

    /// 只读打开的 store 不能写入：给出可诊断的原因，而不是让 SQL 层在写入时才报
    /// 「attempt to write a readonly database」。
    ///
    /// 与 `ExecutionLeaseRequired` 的分工：那个说的是「某条会话的执行所有权不在本
    /// 节点」（历史仍可按只读会话进入）；这里连会话都还没有，没有可降级的对象。
    pub(super) fn require_writable(&self) -> Result<()> {
        if self.read_only {
            return Err(WorkspaceError::ReadOnlyStore.into());
        }
        Ok(())
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
            db_path: db_path.to_path_buf(),
            execution_leases: Default::default(),
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
            .map_err(|error| classify_shape_probe_failure(&error))?;
            let actual: HashSet<String> = rows.into_iter().map(|(name,)| name).collect();
            if !required.iter().all(|column| actual.contains(*column)) {
                return Err(ReadOnlyThreadStoreError::from_kind(
                    ReadOnlyStoreErrorKind::SchemaIncompatible,
                ));
            }
        }
        Ok(())
    }

    /// 默认数据库位置 `~/.peri/threads/threads.db`；不创建目录、数据库或连接。
    pub(crate) fn default_database_path() -> Result<PathBuf> {
        super::super::default_database_path().context("无法获取 home 目录")
    }

    /// 使用默认路径 `~/.peri/threads/threads.db` 创建
    pub async fn default_path() -> Result<Self> {
        Self::new(Self::default_database_path()?).await
    }
}

/// SQLite's initial journal-mode switch can return BUSY despite busy_timeout when
/// two fresh connections upgrade together. Serialize writable opens before connecting.
async fn lock_schema_open(path: &Path) -> Result<std::fs::File> {
    let canonical = if tokio::fs::try_exists(path).await? {
        tokio::fs::canonicalize(path).await?
    } else {
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        tokio::fs::canonicalize(parent)
            .await?
            .join(path.file_name().context("database filename missing")?)
    };
    let mut lock_path = canonical.into_os_string();
    lock_path.push(".schema-lock");
    tokio::task::spawn_blocking(move || {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)?;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(file),
                Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    anyhow::bail!("session database initialization is busy")
                }
                Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
            }
        }
    })
    .await?
}
