use super::*;
use sqlx::Connection;
use std::collections::BTreeMap;
use std::error::Error as _;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::tempdir;

#[derive(Debug, PartialEq, Eq)]
struct DatabaseSnapshot {
    schema_version: i64,
    schema: Vec<(String, String, String)>,
    thread_rows: Vec<String>,
    message_rows: Vec<String>,
}

async fn database_snapshot(path: &std::path::Path) -> DatabaseSnapshot {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .read_only(true)
        .create_if_missing(false);
    let mut connection = sqlx::SqliteConnection::connect_with(&options)
        .await
        .unwrap();
    let schema_version = sqlx::query_scalar("PRAGMA schema_version")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    let schema = sqlx::query_as(
        "SELECT type, name, COALESCE(sql, '') FROM sqlite_master ORDER BY type, name",
    )
    .fetch_all(&mut connection)
    .await
    .unwrap();
    let thread_rows = sqlx::query_scalar(
        "SELECT printf('%Q|%Q|%Q|%Q|%Q|%Q|%Q|%Q|%Q|%Q|%Q|%Q|%Q|%Q',
            id, title, cwd, created_at, updated_at, message_count, parent_thread_id,
            snapshot_at_message_id, hidden, cancel_policy, config,
            frozen_context, agent_status, typeof(message_count))
         FROM threads ORDER BY id",
    )
    .fetch_all(&mut connection)
    .await
    .unwrap_or_default();
    let message_rows = sqlx::query_scalar(
        "SELECT printf('%Q|%Q|%Q|%Q|%Q|%Q|%Q', message_id, thread_id, role, content,
            truncated, excluded, projection) FROM messages ORDER BY message_id",
    )
    .fetch_all(&mut connection)
    .await
    .unwrap_or_default();
    connection.close().await.unwrap();
    DatabaseSnapshot {
        schema_version,
        schema,
        thread_rows,
        message_rows,
    }
}

fn directory_snapshot(path: &std::path::Path) -> BTreeMap<std::ffi::OsString, Vec<u8>> {
    std::fs::read_dir(path)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let file_type = entry.file_type().unwrap();
            assert!(file_type.is_file(), "fixture directory only contains files");
            let name = entry.file_name();
            let bytes = if name.to_string_lossy().ends_with("-wal")
                || name.to_string_lossy().ends_with("-shm")
            {
                Vec::new()
            } else {
                std::fs::read(entry.path()).unwrap()
            };
            (name, bytes)
        })
        .collect()
}

async fn create_schema_without(path: &std::path::Path, omitted_table: &str, omitted_column: &str) {
    let thread_definitions = [
        ("id", "id TEXT PRIMARY KEY"),
        ("title", "title TEXT"),
        ("cwd", "cwd TEXT NOT NULL DEFAULT ''"),
        ("created_at", "created_at TEXT NOT NULL"),
        ("updated_at", "updated_at TEXT NOT NULL"),
        ("message_count", "message_count INTEGER NOT NULL DEFAULT 0"),
        ("parent_thread_id", "parent_thread_id TEXT"),
        ("snapshot_at_message_id", "snapshot_at_message_id TEXT"),
        ("hidden", "hidden BOOLEAN NOT NULL DEFAULT 0"),
        (
            "cancel_policy",
            "cancel_policy TEXT NOT NULL DEFAULT 'cascade'",
        ),
        ("config", "config TEXT"),
        (
            "agent_status",
            "agent_status TEXT NOT NULL DEFAULT 'active'",
        ),
    ];
    let message_definitions = [
        ("thread_id", "thread_id TEXT NOT NULL"),
        ("content", "content TEXT NOT NULL"),
    ];
    let columns = |table: &str, definitions: &[(&str, &str)]| {
        definitions
            .iter()
            .filter(|(name, _)| !(table == omitted_table && *name == omitted_column))
            .map(|(_, definition)| *definition)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let schema = format!(
        "CREATE TABLE threads ({}); CREATE TABLE messages ({})",
        columns("threads", &thread_definitions),
        columns("messages", &message_definitions)
    );
    let mut connection = sqlx::SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true),
    )
    .await
    .unwrap();
    sqlx::query(AssertSqlSafe(schema))
        .execute(&mut connection)
        .await
        .unwrap();
    connection.close().await.unwrap();
}

async fn readonly_error_kind(path: &std::path::Path) -> ReadOnlyStoreErrorKind {
    match SqliteThreadStore::open_existing_read_only(path).await {
        Ok(_) => panic!("只读打开应失败"),
        Err(error) => error.kind(),
    }
}

fn readonly_load_error_kind(error: &anyhow::Error) -> ReadOnlyStoreErrorKind {
    error
        .downcast_ref::<ReadOnlyThreadStoreError>()
        .expect("只读错误应保持可 downcast")
        .kind()
}

#[tokio::test]
async fn test_readonly_open_missing_database_creates_nothing() {
    let dir = tempdir().unwrap();
    let parent = dir.path().join("missing-parent");
    let db_path = parent.join("threads.db");
    let kind = readonly_error_kind(&db_path).await;
    assert_eq!(kind, ReadOnlyStoreErrorKind::DatabaseNotFound);
    assert!(!parent.exists(), "只读打开不得创建父目录");
}

#[tokio::test]
async fn test_readonly_open_directory_and_non_sqlite_are_typed() {
    let dir = tempdir().unwrap();
    let directory_kind = readonly_error_kind(dir.path()).await;
    assert_eq!(directory_kind, ReadOnlyStoreErrorKind::DatabaseUnreadable);
    let file = dir.path().join("not-sqlite.db");
    std::fs::write(&file, "not a sqlite database").unwrap();
    let file_kind = readonly_error_kind(&file).await;
    assert_eq!(file_kind, ReadOnlyStoreErrorKind::SchemaIncompatible);
}

#[tokio::test]
async fn test_readonly_open_rejects_missing_table_and_required_column_without_migration() {
    let dir = tempdir().unwrap();
    let missing_table = dir.path().join("missing-table.db");
    let mut connection = sqlx::SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(&missing_table)
            .create_if_missing(true),
    )
    .await
    .unwrap();
    sqlx::query("CREATE TABLE unrelated (id TEXT)")
        .execute(&mut connection)
        .await
        .unwrap();
    connection.close().await.unwrap();
    let database_before = database_snapshot(&missing_table).await;
    let before = directory_snapshot(dir.path());
    assert_eq!(
        readonly_error_kind(&missing_table).await,
        ReadOnlyStoreErrorKind::SchemaIncompatible
    );
    assert_eq!(database_snapshot(&missing_table).await, database_before);
    assert_eq!(directory_snapshot(dir.path()), before);
    let missing_column = dir.path().join("missing-column.db");
    let mut connection = sqlx::SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(&missing_column)
            .create_if_missing(true),
    )
    .await
    .unwrap();
    sqlx::query(
        "CREATE TABLE threads (id TEXT); CREATE TABLE messages (thread_id TEXT, content TEXT)",
    )
    .execute(&mut connection)
    .await
    .unwrap();
    connection.close().await.unwrap();
    let database_before = database_snapshot(&missing_column).await;
    let before = directory_snapshot(dir.path());
    assert_eq!(
        readonly_error_kind(&missing_column).await,
        ReadOnlyStoreErrorKind::SchemaIncompatible
    );
    assert_eq!(database_snapshot(&missing_column).await, database_before);
    assert_eq!(directory_snapshot(dir.path()), before);
}

#[tokio::test]
async fn test_readonly_open_rejects_every_required_column_without_mutation() {
    for (table, columns) in [
        ("threads", REQUIRED_THREAD_COLUMNS),
        ("messages", REQUIRED_MESSAGE_COLUMNS),
    ] {
        for column in columns {
            let dir = tempdir().unwrap();
            let db_path = dir.path().join(format!("missing-{table}-{column}.db"));
            create_schema_without(&db_path, table, column).await;
            let before = directory_snapshot(dir.path());
            assert_eq!(
                readonly_error_kind(&db_path).await,
                ReadOnlyStoreErrorKind::SchemaIncompatible,
                "missing {table}.{column} must fail at shape probe"
            );
            assert_eq!(directory_snapshot(dir.path()), before);
        }
    }
}

#[tokio::test]
async fn test_readonly_store_loads_exact_meta_and_distinguishes_missing_session() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("threads.db");
    let writer = SqliteThreadStore::new(&db_path).await.unwrap();
    let mut expected = ThreadMeta::new_at(absolute_test_path("workspace"), peri_time::now_wall());
    expected.title = Some("title".into());
    expected.agent_status = AgentStatus::Done;
    let id = writer.create_thread(expected.clone()).await.unwrap();
    writer.database.pool.close().await;
    let database_before = database_snapshot(&db_path).await;
    let directory_before = directory_snapshot(dir.path());
    let reader = SqliteThreadStore::open_existing_read_only(&db_path)
        .await
        .unwrap();
    let loaded = reader.load_meta(&id).await.unwrap();
    assert_eq!(loaded.id, expected.id);
    assert_eq!(loaded.title, expected.title);
    assert_eq!(loaded.cwd, expected.cwd);
    assert_eq!(loaded.agent_status, expected.agent_status);
    let error = reader
        .load_meta(&"00000000-0000-0000-0000-000000000000".into())
        .await
        .unwrap_err();
    assert_eq!(
        readonly_load_error_kind(&error),
        ReadOnlyStoreErrorKind::SessionNotFound
    );
    drop(reader);
    assert_eq!(database_snapshot(&db_path).await, database_before);
    assert_eq!(directory_snapshot(dir.path()), directory_before);
}

#[test]
fn test_meta_decoder_rejects_negative_derived_content_size() {
    let error = meta_from_row(
        "550e8400-e29b-41d4-a716-446655440000".into(),
        None,
        absolute_test_path("tmp").into(),
        "2026-09-04T00:00:00Z".into(),
        "2026-09-04T00:00:00Z".into(),
        0,
        -1,
        None,
        None,
        false,
        "cascade".into(),
        None,
        "active".into(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("content_size is negative"));
}

#[tokio::test]
async fn test_readonly_store_rejects_corrupt_enum_time_type_and_negative_counts_without_values() {
    for (column, value) in [
        ("agent_status", "'secret-invalid-status'"),
        ("cancel_policy", "'secret-invalid-policy'"),
        ("created_at", "'secret-invalid-time'"),
        ("updated_at", "'secret-invalid-updated-time'"),
        ("cwd", "X'80'"),
        ("title", "X'80'"),
        ("parent_thread_id", "X'80'"),
        ("snapshot_at_message_id", "X'80'"),
        ("hidden", "'secret-invalid-hidden-type'"),
        ("config", "X'80'"),
        ("message_count", "-1"),
        ("message_count", "'secret-invalid-type'"),
    ] {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("threads.db");
        let writer = SqliteThreadStore::new(&db_path).await.unwrap();
        let id = writer
            .create_thread(ThreadMeta::new_at(
                absolute_test_path("tmp"),
                peri_time::now_wall(),
            ))
            .await
            .unwrap();
        sqlx::query(AssertSqlSafe(format!(
            "UPDATE threads SET {column} = {value} WHERE id = ?1"
        )))
        .bind(&id)
        .execute(&writer.database.pool)
        .await
        .unwrap();
        writer.database.pool.close().await;
        let database_before = database_snapshot(&db_path).await;
        let reader = SqliteThreadStore::open_existing_read_only(&db_path)
            .await
            .unwrap();
        let before = directory_snapshot(dir.path());
        let error = reader.load_meta(&id).await.unwrap_err();
        assert_eq!(
            readonly_load_error_kind(&error),
            ReadOnlyStoreErrorKind::CorruptSessionData
        );
        assert!(
            !error.to_string().contains("secret-invalid"),
            "稳定错误不得泄露存储原值"
        );
        assert!(!format!("{error:#}").contains("secret-invalid"));
        assert!(!format!("{error:?}").contains("secret-invalid"));
        let typed = error.downcast_ref::<ReadOnlyThreadStoreError>().unwrap();
        assert!(typed.source().is_none(), "公开错误 source chain 必须脱敏");
        drop(reader);
        assert_eq!(database_snapshot(&db_path).await, database_before);
        assert_eq!(directory_snapshot(dir.path()), before);
    }
}

#[tokio::test]
async fn test_readonly_backed_trait_rejects_mutation_and_preserves_row() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("threads.db");
    let writer = SqliteThreadStore::new(&db_path).await.unwrap();
    let expected = ThreadMeta::new_at(absolute_test_path("before"), peri_time::now_wall());
    let id = writer.create_thread(expected.clone()).await.unwrap();
    writer.database.pool.close().await;
    let database_before = database_snapshot(&db_path).await;
    let reader: Arc<dyn ThreadStore> = Arc::new(
        SqliteThreadStore::open_existing_read_only(&db_path)
            .await
            .unwrap(),
    );
    let before = directory_snapshot(dir.path());
    let mut changed = expected.clone();
    changed.cwd = "/after".into();
    assert!(
        reader.update_meta(&id, changed).await.is_err(),
        "SQLite read-only capability 必须拒绝写入"
    );
    let loaded = reader.load_meta(&id).await.unwrap();
    assert_eq!(loaded.id, expected.id);
    assert_eq!(loaded.cwd, expected.cwd);
    assert_eq!(loaded.created_at, expected.created_at);
    assert_eq!(loaded.updated_at, expected.updated_at);
    assert_eq!(loaded.message_count, expected.message_count);
    assert_eq!(loaded.agent_status, expected.agent_status);
    drop(reader);
    assert_eq!(database_snapshot(&db_path).await, database_before);
    assert_eq!(directory_snapshot(dir.path()), before);
}

#[tokio::test]
async fn test_readonly_store_observes_wal_commit_but_not_uncommitted_update() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("threads.db");
    let writer = SqliteThreadStore::new(&db_path).await.unwrap();
    let id = writer
        .create_thread(ThreadMeta::new_at(
            absolute_test_path("baseline"),
            peri_time::now_wall(),
        ))
        .await
        .unwrap();
    let mut transaction = writer.database.pool.begin().await.unwrap();
    sqlx::query("UPDATE threads SET cwd = '/pending' WHERE id = ?1")
        .bind(&id)
        .execute(&mut *transaction)
        .await
        .unwrap();
    let sidecars_before_open = directory_snapshot(dir.path());
    let reader = SqliteThreadStore::open_existing_read_only(&db_path)
        .await
        .unwrap();
    let sidecars_after_open = directory_snapshot(dir.path());
    assert_eq!(sidecars_after_open, sidecars_before_open);
    assert_eq!(
        reader.load_meta(&id).await.unwrap().cwd,
        absolute_test_path("baseline")
    );
    let sidecars_after_load = directory_snapshot(dir.path());
    assert_eq!(sidecars_after_load, sidecars_before_open);
    drop(reader);
    assert_eq!(directory_snapshot(dir.path()), sidecars_before_open);
    transaction.commit().await.unwrap();
    let sidecars_before_committed_open = directory_snapshot(dir.path());
    let reader = SqliteThreadStore::open_existing_read_only(&db_path)
        .await
        .unwrap();
    assert_eq!(
        directory_snapshot(dir.path()),
        sidecars_before_committed_open
    );
    assert_eq!(reader.load_meta(&id).await.unwrap().cwd, "/pending");
    assert_eq!(
        directory_snapshot(dir.path()),
        sidecars_before_committed_open
    );
    drop(reader);
    assert_eq!(
        directory_snapshot(dir.path()),
        sidecars_before_committed_open
    );
}

/// 确定性 test double：只用于覆盖 probe 失败的分类分支，不经过真实 SQLite。
#[derive(Debug)]
struct ShapeProbeError {
    code: Option<&'static str>,
    message: &'static str,
}

impl std::fmt::Display for ShapeProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message)
    }
}

impl std::error::Error for ShapeProbeError {}

impl sqlx::error::DatabaseError for ShapeProbeError {
    fn message(&self) -> &str {
        self.message
    }

    fn code(&self) -> Option<std::borrow::Cow<'_, str>> {
        self.code.map(std::borrow::Cow::Borrowed)
    }

    fn as_error(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
        self
    }

    fn as_error_mut(&mut self) -> &mut (dyn std::error::Error + Send + Sync + 'static) {
        self
    }

    fn into_error(self: Box<Self>) -> Box<dyn std::error::Error + Send + Sync + 'static> {
        self
    }

    fn kind(&self) -> sqlx::error::ErrorKind {
        sqlx::error::ErrorKind::Other
    }
}

#[test]
fn test_shape_probe_failure_classification_never_fakes_schema_verdict() {
    // 只有确定性损坏/非数据库镜像才可判定 schema 不兼容。
    for code in ["11", "26"] {
        let error = sqlx::Error::Database(Box::new(ShapeProbeError {
            code: Some(code),
            message: "shape probe failed",
        }));
        assert_eq!(
            classify_shape_probe_failure(&error).kind(),
            ReadOnlyStoreErrorKind::SchemaIncompatible,
            "SQLite primary result code {code} 是确定性 schema/镜像判定"
        );
    }

    // 瞬时或环境故障（锁竞争、IO、连接池超时等）必须保持可诊断。
    let transient_codes = [
        Some("5"),   // SQLITE_BUSY：并发 checkpoint / lock 未释放
        Some("6"),   // SQLITE_LOCKED：共享缓存表锁
        Some("10"),  // SQLITE_IOERR
        Some("14"),  // SQLITE_CANTOPEN：-wal/-shm 不可用
        Some("266"), // SQLITE_IOERR_READ 扩展码
        None,
    ];
    for code in transient_codes {
        let error = sqlx::Error::Database(Box::new(ShapeProbeError {
            code,
            message: "shape probe failed",
        }));
        assert_eq!(
            classify_shape_probe_failure(&error).kind(),
            ReadOnlyStoreErrorKind::DatabaseUnreadable,
            "code {code:?} 不得伪装成 schema 判定"
        );
    }
    for error in [sqlx::Error::PoolTimedOut, sqlx::Error::RowNotFound] {
        assert_eq!(
            classify_shape_probe_failure(&error).kind(),
            ReadOnlyStoreErrorKind::DatabaseUnreadable
        );
    }
}

#[tokio::test]
async fn test_readonly_open_lock_contention_is_bounded() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("threads.db");
    let writer = SqliteThreadStore::new(&db_path).await.unwrap();
    drop(writer);
    let mut lock =
        sqlx::SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&db_path))
            .await
            .unwrap();
    sqlx::query("PRAGMA locking_mode=EXCLUSIVE")
        .execute(&mut lock)
        .await
        .unwrap();
    sqlx::query("BEGIN EXCLUSIVE")
        .execute(&mut lock)
        .await
        .unwrap();
    let started = Instant::now();
    let kind = readonly_error_kind(&db_path).await;
    let elapsed = started.elapsed();
    assert_eq!(
        kind,
        ReadOnlyStoreErrorKind::DatabaseUnreadable,
        "锁竞争必须报成不可读，不得伪装成 schema 判定: {kind:?}"
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "锁等待必须有界: {elapsed:?}"
    );
    sqlx::query("ROLLBACK").execute(&mut lock).await.unwrap();
}
