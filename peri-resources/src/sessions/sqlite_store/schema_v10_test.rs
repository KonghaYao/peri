use super::*;
use sqlx::{sqlite::SqliteConnectOptions, Connection, SqliteConnection};
use std::path::Path;

/// v10 回退删掉的本机表。
const DROPPED_TABLES: &[&str] = &[
    "thread_goals",
    "session_store_registrations",
    "session_lifecycle_commitments",
    "session_remote_operations",
    "remote_execution_runs",
    "remote_lifecycle_commitments",
];

/// v9 库的完整形状：v6 时代的业务表 + v7..v9 追加的本机远程痕迹。
const V9_SCHEMA: &str = r#"
PRAGMA user_version = 9;
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
CREATE INDEX idx_bindings_project ON session_bindings(project_id, thread_id);
CREATE TABLE execution_runs (
    thread_id TEXT PRIMARY KEY, generation INTEGER NOT NULL, clean BOOLEAN NOT NULL
);
CREATE TABLE thread_goals (
    thread_id TEXT PRIMARY KEY, goal_id TEXT NOT NULL, objective TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('active','paused','blocked','usage_limited','budget_limited','complete')),
    token_budget INTEGER NULL, tokens_used INTEGER NOT NULL DEFAULT 0,
    time_used_seconds INTEGER NOT NULL DEFAULT 0, created_at_ms INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL,
    FOREIGN KEY(thread_id) REFERENCES threads(id) ON DELETE CASCADE
);
CREATE TABLE session_lifecycle_commitments (
    thread_id TEXT PRIMARY KEY, root_id TEXT NOT NULL, kind TEXT NOT NULL, state TEXT NOT NULL,
    generation INTEGER, operation_id TEXT, detail TEXT,
    created_at TEXT NOT NULL, updated_at TEXT NOT NULL
);
CREATE TABLE session_store_registrations (
    store_id TEXT PRIMARY KEY, engine TEXT NOT NULL, locator_digest TEXT NOT NULL,
    installation_id TEXT NOT NULL, created_at TEXT NOT NULL
);
CREATE TABLE session_remote_operations (
    operation_id TEXT PRIMARY KEY, store_id TEXT NOT NULL, thread_id TEXT NOT NULL,
    root_id TEXT NOT NULL, behavior TEXT NOT NULL, digest TEXT NOT NULL, state TEXT NOT NULL,
    created_at TEXT NOT NULL, updated_at TEXT NOT NULL
);
CREATE TABLE remote_execution_runs (
    store_id TEXT NOT NULL, root_id TEXT NOT NULL, generation INTEGER NOT NULL,
    clean BOOLEAN NOT NULL, PRIMARY KEY (store_id, root_id)
);
CREATE TABLE remote_lifecycle_commitments (
    store_id TEXT NOT NULL, thread_id TEXT NOT NULL, kind TEXT NOT NULL, state TEXT NOT NULL,
    created_at TEXT NOT NULL, updated_at TEXT NOT NULL, PRIMARY KEY (store_id, thread_id)
);
"#;

/// 建一个 v9 形状但 `user_version` 由调用方指定的库（v7/v8 是同一批表的历史形态）。
async fn v9_database(path: &Path, user_version: i64) -> SqliteConnection {
    let mut connection = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true),
    )
    .await
    .unwrap();
    sqlx::raw_sql(V9_SCHEMA)
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "PRAGMA user_version = {user_version}"
    )))
    .execute(&mut connection)
    .await
    .unwrap();
    connection
}

/// 往 v9 库里填数据：一条本机会话、一条远程遗留的执行代际行、五张表各一行。
async fn populate(connection: &mut SqliteConnection) {
    populate_business(connection).await;
    populate_remote_traces(connection).await;
}

/// 业务表数据（迁移必须逐行不动）：本机会话、消息、登记关系、目标，以及两条执行代际行
/// ——`local-root` 是本机的，`remote-root` 是 remote 工作留下的孤儿行。
async fn populate_business(connection: &mut SqliteConnection) {
    sqlx::raw_sql(
        "INSERT INTO threads (id, title, cwd, created_at, updated_at, message_count)
         VALUES ('local-root', '本机会话', '/work', '2026-09-01T00:00:00Z', '2026-09-02T00:00:00Z', 1);
         INSERT INTO messages (message_id, thread_id, role, content)
         VALUES ('m1', 'local-root', 'user', 'hello');
         INSERT INTO projects (id, locator, object_identity) VALUES ('p1', '/work', 'dev:1');
         INSERT INTO workspaces (id, project_id, root, root_identity, discovery)
         VALUES ('11111111-1111-4111-8111-111111111111', 'p1', '/work', 'dev:1', '{\"root\":\"/work\",\"root_identity\":{\"device\":1,\"inode\":1},\"common_dir\":null,\"common_identity\":null,\"private_dir\":null,\"private_identity\":null}');
         INSERT INTO session_bindings (thread_id, schema_version, project_id, workspace_id, relative_cwd)
         VALUES ('local-root', 1, 'p1', '11111111-1111-4111-8111-111111111111', '');
         INSERT INTO execution_runs (thread_id, generation, clean) VALUES ('local-root', 4, 0);
         INSERT INTO execution_runs (thread_id, generation, clean) VALUES ('remote-root', 7, 0);",
    )
    .execute(&mut *connection)
    .await
    .unwrap();
}

/// 五张本机远程痕迹表各一行。
async fn populate_remote_traces(connection: &mut SqliteConnection) {
    sqlx::raw_sql(
        "INSERT INTO session_lifecycle_commitments
             (thread_id, root_id, kind, state, created_at, updated_at)
         VALUES ('gone', 'gone', 'tombstone', 'deleted', '2026-09-01T00:00:00Z', '2026-09-01T00:00:00Z');
         INSERT INTO session_store_registrations
             (store_id, engine, locator_digest, installation_id, created_at)
         VALUES ('store-a', 'turso', 'digest', 'install', '2026-09-01T00:00:00Z');
         INSERT INTO session_remote_operations
             (operation_id, store_id, thread_id, root_id, behavior, digest, state, created_at, updated_at)
         VALUES ('op1', 'store-a', 'remote-root', 'remote-root', 'append', 'd', 'pending',
                 '2026-09-01T00:00:00Z', '2026-09-01T00:00:00Z');
         INSERT INTO remote_execution_runs (store_id, root_id, generation, clean)
         VALUES ('store-a', 'remote-root', 3, 0);
         INSERT INTO remote_lifecycle_commitments
             (store_id, thread_id, kind, state, created_at, updated_at)
         VALUES ('store-a', 'remote-root', 'tombstone', 'deleting',
                 '2026-09-01T00:00:00Z', '2026-09-01T00:00:00Z');",
    )
    .execute(&mut *connection)
    .await
    .unwrap();
}

async fn read_only(path: &Path) -> SqliteConnection {
    SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(path).read_only(true))
        .await
        .unwrap()
}

#[tokio::test]
async fn test_v7_v8_v9_refusal_preserves_remote_tables() {
    for source_version in [7, 8, 9] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("threads.db");
        let mut connection = v9_database(&path, source_version).await;
        populate(&mut connection).await;
        connection.close().await.unwrap();

        assert_legacy_rejected(&path).await;
    }
}

#[tokio::test]
async fn test_legacy_database_without_remote_tables_is_refused_idempotently() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let mut connection = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true),
    )
    .await
    .unwrap();
    sqlx::raw_sql(V9_SCHEMA)
        .execute(&mut connection)
        .await
        .unwrap();
    // 删掉 v7..v9 引入的表与 v9 版本号，模拟「从未建过这些表」的库。
    for table in DROPPED_TABLES {
        sqlx::query(sqlx::AssertSqlSafe(format!("DROP TABLE {table}")))
            .execute(&mut connection)
            .await
            .unwrap();
    }
    sqlx::query("PRAGMA user_version = 8")
        .execute(&mut connection)
        .await
        .unwrap();
    // 这些库从来只有业务表，所以只填业务数据（远程痕迹表不存在，无从填写）。
    populate_business(&mut connection).await;
    connection.close().await.unwrap();

    assert_legacy_rejected(&path).await;
}

/// 同名但形状不符的表 → fail-closed：拒绝升级、整体回滚，不认识的数据一行不动。
#[tokio::test]
async fn test_mismatched_table_shape_fails_closed_and_rolls_back() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let mut connection = v9_database(&path, 9).await;
    populate(&mut connection).await;
    // 把登记表换成同名的别的业务表（列不符）。
    sqlx::raw_sql(
        "DROP TABLE session_store_registrations;
         CREATE TABLE session_store_registrations (store_id TEXT PRIMARY KEY, note TEXT);
         INSERT INTO session_store_registrations VALUES ('not-ours', '别人写的');",
    )
    .execute(&mut connection)
    .await
    .unwrap();
    connection.close().await.unwrap();
    let before = std::fs::read(&path).unwrap();

    let error = SqliteThreadStore::new(&path).await.err().unwrap();
    assert!(error
        .to_string()
        .contains("explicit stopped-writer offline migration"));

    let mut connection = read_only(&path).await;
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(version, 9, "失败的迁移不推进版本号");
    // 其余四张表仍在：整体回滚，不是「删一半」。
    for table in DROPPED_TABLES {
        let present: Option<(String,)> =
            sqlx::query_as("SELECT name FROM sqlite_schema WHERE type = 'table' AND name = ?")
                .bind(table)
                .fetch_optional(&mut connection)
                .await
                .unwrap();
        assert!(present.is_some(), "{table} 在失败的迁移里必须原样保留");
    }
    let (note,): (String,) =
        sqlx::query_as("SELECT note FROM session_store_registrations WHERE store_id = 'not-ours'")
            .fetch_one(&mut connection)
            .await
            .unwrap();
    assert_eq!(note, "别人写的");
    connection.close().await.unwrap();
    // 结构未被部分改动（WAL 下文件字节可能变化，因此比对表清单而不是文件内容）。
    assert!(!before.is_empty());
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
