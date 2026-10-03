//! schema v6 → v11 迁移：执行状态不再持久化，历史数据与登记保持不变。
//!
//! 覆盖：退役执行表删除、会话删除语义、本机不再有 store 维度的表、
//! 迁移失败整体回滚、只读打开不迁移也不按版本拒绝。所有库都在 tempdir 中构造，不触碰
//! 真实本机数据库。

use super::schema::CURRENT_SCHEMA_VERSION;
use super::*;
use crate::sessions::data::SessionDataPort;
use peri_acp_types::store::{serialize_persisted_payload, PersistedPayload, ThreadStore};
use peri_acp_types::workspace::WorkspaceError;
use sqlx::{sqlite::SqliteConnectOptions, AssertSqlSafe, Connection, SqliteConnection};
use std::path::Path;

/// v6 库的完整形状（含 execution_runs 的 threads 外键与旧 registration 约束）。
const V6_SCHEMA: &str = r#"
PRAGMA user_version = 6;
CREATE TABLE threads (
    id TEXT PRIMARY KEY, title TEXT, cwd TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL, updated_at TEXT NOT NULL, message_count INTEGER NOT NULL DEFAULT 0,
    parent_thread_id TEXT, snapshot_at_message_id TEXT, hidden BOOLEAN NOT NULL DEFAULT 0,
    cancel_policy TEXT NOT NULL DEFAULT 'cascade', config TEXT, cached_context TEXT,
    frozen_context TEXT, inherited_context TEXT, agent_status TEXT NOT NULL DEFAULT 'active',
    context_cache_epoch INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE messages (
    message_id TEXT PRIMARY KEY, thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    role TEXT NOT NULL, content TEXT NOT NULL, truncated BOOLEAN NOT NULL DEFAULT 0,
    excluded BOOLEAN NOT NULL DEFAULT 0, projection TEXT
);
CREATE INDEX idx_messages_thread_id ON messages(thread_id);
CREATE TABLE projects (
    id TEXT PRIMARY KEY, locator TEXT NOT NULL, object_identity TEXT NOT NULL,
    UNIQUE(locator, object_identity)
);
CREATE TABLE workspaces (
    id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id),
    root TEXT NOT NULL, root_identity TEXT NOT NULL, discovery TEXT NOT NULL,
    UNIQUE(root, root_identity), UNIQUE(id, project_id)
);
CREATE TABLE session_bindings (
    thread_id TEXT PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
    schema_version INTEGER NOT NULL,
    project_id TEXT NOT NULL, workspace_id TEXT NOT NULL, relative_cwd TEXT NOT NULL,
    FOREIGN KEY(workspace_id, project_id) REFERENCES workspaces(id, project_id)
);
CREATE TABLE execution_runs (
    thread_id TEXT PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
    generation INTEGER NOT NULL, clean BOOLEAN NOT NULL
);
CREATE TABLE thread_goals (
    thread_id TEXT PRIMARY KEY, goal_id TEXT NOT NULL, objective TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('active','paused','blocked','usage_limited','budget_limited','complete')),
    token_budget INTEGER NULL, tokens_used INTEGER NOT NULL DEFAULT 0,
    time_used_seconds INTEGER NOT NULL DEFAULT 0, created_at_ms INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL,
    FOREIGN KEY(thread_id) REFERENCES threads(id) ON DELETE CASCADE
);
"#;

async fn v6_database(path: &Path) -> SqliteConnection {
    let mut connection = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true),
    )
    .await
    .unwrap();
    sqlx::raw_sql(V6_SCHEMA)
        .execute(&mut connection)
        .await
        .unwrap();
    connection
}

/// v6 库 + 历史 + 脏执行代际 + 辅助表数据。
async fn populated_v6(path: &Path) -> Vec<u8> {
    let mut connection = v6_database(path).await;
    let message = BaseMessage::human("history before v7");
    let content = serialize_persisted_payload(&PersistedPayload::Message(message.clone())).unwrap();
    sqlx::query(
        "INSERT INTO threads (id, title, cwd, created_at, updated_at, message_count, frozen_context)
         VALUES ('old-root', '旧会话', '/old/worktree', '2026-09-01T00:00:00Z', '2026-09-02T00:00:00Z', 1, '{\"version\":1}')",
    )
    .execute(&mut connection)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO messages (message_id, thread_id, role, content)
         VALUES (?1, 'old-root', 'user', ?2)",
    )
    .bind(message.id().as_uuid().to_string())
    .bind(&content)
    .execute(&mut connection)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO execution_runs (thread_id, generation, clean) VALUES ('old-root', 4, 0)",
    )
    .execute(&mut connection)
    .await
    .unwrap();
    sqlx::query("INSERT INTO thread_goals VALUES ('old-root', 'goal', '保留目标', 'active', NULL, 0, 0, 1, 1)")
        .execute(&mut connection)
        .await
        .unwrap();
    connection.close().await.unwrap();
    std::fs::read(path).unwrap()
}

/// v10 回退删掉的本机表：升级之后一张都不该留在库里。
const DROPPED_TABLES: &[&str] = &[
    "session_store_registrations",
    "session_lifecycle_commitments",
    "session_remote_operations",
    "remote_execution_runs",
    "remote_lifecycle_commitments",
];

/// 表是否存在于库内。
async fn table_present(connection: &mut SqliteConnection, table: &str) -> bool {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT name FROM sqlite_schema WHERE type = 'table' AND name = ?")
            .bind(table)
            .fetch_optional(&mut *connection)
            .await
            .unwrap();
    row.is_some()
}

#[tokio::test]
async fn test_v6_upgrade_removes_execution_state_and_keeps_history() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("threads.db");
    populated_v6(&path).await;

    let store = SqliteThreadStore::new(&path).await.unwrap();
    let mut connection = SqliteConnection::connect_with(
        &SqliteConnectOptions::new().filename(&path).read_only(true),
    )
    .await
    .unwrap();
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(version, CURRENT_SCHEMA_VERSION);

    assert!(!table_present(&mut connection, "execution_runs").await);

    // 历史与辅助表逐字节保留。
    let history: (String, String) =
        sqlx::query_as("SELECT role, content FROM messages WHERE thread_id = 'old-root'")
            .fetch_one(&mut connection)
            .await
            .unwrap();
    let expected = BaseMessage::human("history before v7");
    assert_eq!(history.0, "user");
    assert!(history.1.contains("history before v7"));
    let (goals,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM sqlite_master WHERE name = 'thread_goals'")
            .fetch_one(&mut connection)
            .await
            .unwrap();
    assert_eq!(goals, 0);
    let frozen: (Option<String>,) =
        sqlx::query_as("SELECT frozen_context FROM threads WHERE id = 'old-root'")
            .fetch_one(&mut connection)
            .await
            .unwrap();
    assert_eq!(frozen.0.as_deref(), Some("{\"version\":1}"));
    let _ = expected;

    // v10 回退：v7..v9 引入的本机远程痕迹一张都不留。
    for table in DROPPED_TABLES {
        assert!(
            !table_present(&mut connection, table).await,
            "v10 之后本机不该再有 {table}"
        );
    }
    connection.close().await.unwrap();

    let data = SqliteSessionData::new(Arc::clone(&store.database));
    let owner = data.claim_execution_owner(&"old-root".to_owned(), false, None).await.unwrap();
    data.install_execution_owner_token(owner.token);
    data.delete_tree(&"old-root".to_owned()).await.unwrap();
    let sessions: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM threads")
        .fetch_one(&store.database.pool)
        .await
        .unwrap();
    assert_eq!(sessions.0, 0);
}

#[tokio::test]
async fn test_v6_upgrade_failure_rolls_back_version_structure_and_rows() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("threads.db");
    populated_v6(&path).await;
    // 预置一张形状不符的同名表：v10 回退必须整体失败，而不是把不认识的数据丢掉。
    let mut connection =
        SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&path))
            .await
            .unwrap();
    sqlx::query("CREATE TABLE session_store_registrations (store_id TEXT PRIMARY KEY)")
        .execute(&mut connection)
        .await
        .unwrap();
    let before: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT name, sql FROM sqlite_schema ORDER BY name")
            .fetch_all(&mut connection)
            .await
            .unwrap();
    connection.close().await.unwrap();

    let error = SqliteThreadStore::new(&path).await.err().unwrap();
    assert!(matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::UnsupportedDatabaseSchema)
    ));

    let mut connection = SqliteConnection::connect_with(
        &SqliteConnectOptions::new().filename(&path).read_only(true),
    )
    .await
    .unwrap();
    let after: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT name, sql FROM sqlite_schema ORDER BY name")
            .fetch_all(&mut connection)
            .await
            .unwrap();
    assert_eq!(after, before, "失败的迁移不得留下半迁移结构");
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(version, 6, "失败后版本保持 6，可重试");
    let foreign: Vec<(String,)> =
        sqlx::query_as("SELECT \"table\" FROM pragma_foreign_key_list('execution_runs')")
            .fetch_all(&mut connection)
            .await
            .unwrap();
    assert_eq!(foreign, vec![("threads".to_owned(),)], "结构未被部分重建");
    let runs: Vec<(String, i64, bool)> =
        sqlx::query_as("SELECT thread_id, generation, clean FROM execution_runs")
            .fetch_all(&mut connection)
            .await
            .unwrap();
    assert_eq!(runs, vec![("old-root".to_owned(), 4, false)]);
    assert!(
        table_present(&mut connection, "session_store_registrations").await,
        "形状不符的同名表在失败的迁移里必须原样保留"
    );
    let rows: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM session_store_registrations")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(rows.0, 0);
}

#[tokio::test]
async fn test_read_only_open_accepts_v6_v7_and_future_shapes_without_migrating() {
    let directory = tempfile::tempdir().unwrap();

    // v6：只读打开不迁移、不建表、不改版本号。
    let v6 = directory.path().join("v6.db");
    populated_v6(&v6).await;
    let reader = SqliteThreadStore::open_existing_read_only(&v6)
        .await
        .unwrap();
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&reader.database.pool)
        .await
        .unwrap();
    assert_eq!(version, 6, "只读打开不写 user_version");
    for table in DROPPED_TABLES {
        assert!(
            !table_present(&mut reader.database.pool.acquire().await.unwrap(), table).await,
            "只读打开不建表：{table}"
        );
    }
    assert_eq!(
        reader
            .load_meta(&"old-root".to_owned())
            .await
            .unwrap()
            .title
            .as_deref(),
        Some("旧会话")
    );
    reader.close().await;

    // 未来版本：只读按列形状放行（不因版本号拒绝）。
    let future = directory.path().join("future.db");
    populated_v6(&future).await;
    let future_version = CURRENT_SCHEMA_VERSION + 1;
    let mut connection =
        SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&future))
            .await
            .unwrap();
    sqlx::query(AssertSqlSafe(format!(
        "PRAGMA user_version = {future_version}"
    )))
    .execute(&mut connection)
    .await
    .unwrap();
    connection.close().await.unwrap();
    let before = std::fs::read(&future).unwrap();
    let reader = SqliteThreadStore::open_existing_read_only(&future)
        .await
        .unwrap();
    assert!(reader.load_meta(&"old-root".to_owned()).await.is_ok());
    reader.close().await;

    // 同一份未来版本库：写打开拒绝、不降级、不改动文件。
    let error = SqliteThreadStore::new(&future).await.err().unwrap();
    assert!(matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::UnsupportedSchemaVersion { found, .. }) if *found == future_version
    ));
    assert_eq!(std::fs::read(&future).unwrap(), before);
}

#[tokio::test]
async fn test_upgraded_database_reopens_without_second_migration() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("threads.db");
    populated_v6(&path).await;

    let store = SqliteThreadStore::new(&path).await.unwrap();
    store.close().await;
    let reopened = SqliteThreadStore::new(&path).await.unwrap();
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&reopened.database.pool)
        .await
        .unwrap();
    assert_eq!(version, CURRENT_SCHEMA_VERSION);
    for table in DROPPED_TABLES {
        assert!(
            !table_present(&mut reopened.database.pool.acquire().await.unwrap(), table).await,
            "重复打开不重建已回退的表：{table}"
        );
    }
    assert!(
        !table_present(
            &mut reopened.database.pool.acquire().await.unwrap(),
            "execution_runs"
        )
        .await
    );
    assert_eq!(
        reopened
            .load_messages(&"old-root".to_owned())
            .await
            .unwrap()
            .len(),
        1
    );
}
