use super::schema::CURRENT_SCHEMA_VERSION;
use super::*;
use peri_acp_types::store::{serialize_persisted_payload, PersistedPayload};
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
async fn test_v6_refusal_preserves_execution_state_and_history() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("threads.db");
    populated_v6(&path).await;

    assert_legacy_rejected(&path).await;
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
    assert!(error
        .to_string()
        .contains("explicit stopped-writer offline migration"));

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
async fn test_read_only_open_refuses_inline_v6_v7_and_future_shapes_without_writes() {
    let directory = tempfile::tempdir().unwrap();
    for version in [6, 7, CURRENT_SCHEMA_VERSION + 1] {
        let path = directory.path().join(format!("v{version}.db"));
        populated_v6(&path).await;
        let mut connection =
            SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&path))
                .await
                .unwrap();
        sqlx::query(AssertSqlSafe(format!("PRAGMA user_version = {version}")))
            .execute(&mut connection)
            .await
            .unwrap();
        connection.close().await.unwrap();
        let before = legacy_evidence(&path).await;
        let error = SqliteThreadStore::open_existing_read_only(&path)
            .await
            .err()
            .unwrap();
        assert_eq!(
            error.kind(),
            super::connection::ReadOnlyStoreErrorKind::SchemaIncompatible
        );
        if version > CURRENT_SCHEMA_VERSION {
            let error = SqliteThreadStore::new(&path).await.err().unwrap();
            assert!(
                matches!(error.downcast_ref::<WorkspaceError>(), Some(WorkspaceError::UnsupportedSchemaVersion { found, .. }) if *found == version)
            );
        } else {
            assert_legacy_rejected(&path).await;
        }
        assert_eq!(legacy_evidence(&path).await, before);
    }
}

#[tokio::test]
async fn test_legacy_database_repeated_opens_never_migrate() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("threads.db");
    populated_v6(&path).await;

    for _ in 0..2 {
        assert_legacy_rejected(&path).await;
    }
}

async fn assert_legacy_rejected(path: &std::path::Path) {
    let before = legacy_evidence(path).await;
    let backup = path.with_extension("verified-backup");
    std::fs::copy(path, &backup).unwrap();
    assert_eq!(
        std::fs::read(&backup).unwrap(),
        std::fs::read(path).unwrap()
    );
    let mut connection =
        SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(path).read_only(true))
            .await
            .unwrap();
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    connection.close().await.unwrap();
    if version != 17 {
        let approval = crate::sessions::StoppedWriterApproval {
            source_version: version,
            writers_stopped: true,
            backup_verified: true,
        };
        assert!(crate::sessions::migrate_stopped_work_store(path, &approval)
            .await
            .is_err());
    }
    let error = SqliteThreadStore::new(path)
        .await
        .err()
        .expect("legacy writer must refuse");
    assert!(
        error
            .to_string()
            .contains("explicit stopped-writer offline migration"),
        "{error:#}"
    );
    let error = SqliteThreadStore::open_existing_read_only(path)
        .await
        .err()
        .expect("inline history is not reference history");
    assert!(matches!(
        error.kind(),
        super::connection::ReadOnlyStoreErrorKind::SchemaIncompatible
    ));
    assert_eq!(
        legacy_evidence(path).await,
        before,
        "refusal must preserve every schema object and row"
    );
}

async fn legacy_evidence(path: &std::path::Path) -> Vec<(String, Vec<String>)> {
    let mut connection =
        SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(path).read_only(true))
            .await
            .unwrap();
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    let schema: Vec<String> = sqlx::query_scalar("SELECT json_array(type,name,tbl_name,sql) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%' ORDER BY type,name").fetch_all(&mut connection).await.unwrap();
    let tables: Vec<String> = sqlx::query_scalar("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").fetch_all(&mut connection).await.unwrap();
    let mut evidence = vec![
        ("version".into(), vec![version.to_string()]),
        ("schema".into(), schema),
    ];
    for table in tables {
        let columns: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info(?1) ORDER BY cid")
                .bind(&table)
                .fetch_all(&mut connection)
                .await
                .unwrap();
        let expressions = columns
            .iter()
            .map(|column| format!("quote(\"{}\")", column.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" || ',' || ");
        let statement = format!(
            "SELECT quote(rowid) || ':' || {expressions} FROM \"{}\" ORDER BY rowid",
            table.replace('"', "\"\"")
        );
        let rows = sqlx::query_scalar(sqlx::AssertSqlSafe(statement))
            .fetch_all(&mut connection)
            .await
            .unwrap();
        evidence.push((table, rows));
    }
    connection.close().await.unwrap();
    evidence
}
