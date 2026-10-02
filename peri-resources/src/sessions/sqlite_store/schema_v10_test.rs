//! schema v10 回退迁移：删除 v7..v9 写下的本机远程痕迹。
//!
//! 用户裁决撤销「远程存储在本机留有痕迹」的整套能力，本机表结构回到 remote 工作之前：
//! 不保留远程痕迹表或 store 维度；当前初始化补建环境表与 OAuth 凭据表。覆盖：
//!
//! - v7 / v8 / v9 三种来源库都收敛到同一形状（既有业务表与两张初始化表）；
//! - 表数据连同表一起消失，**业务表逐行不动**；
//! - schema 11 删除 `execution_runs`，不把其状态移入新的表或列；
//! - 同名但形状不符的表 → fail-closed 拒绝并整体回滚，不删不认识的数据；
//! - 没有那 5 张表的库是幂等的。
//!
//! 所有库都在 tempdir 中构造，不触碰真实本机数据库。

use super::schema::CURRENT_SCHEMA_VERSION;
use super::*;
use crate::sessions::canonical::{OAUTH_CREDENTIALS_TABLE, SESSION_ENVIRONMENTS_TABLE};
use peri_acp_types::workspace::WorkspaceError;
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

/// 五张本机远程痕迹表各一行：迁移要连表带行一起删掉。
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

/// 库内的表清单（不含 SQLite 内部表）。
async fn table_names(connection: &mut SqliteConnection) -> Vec<String> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
         ORDER BY name",
    )
    .fetch_all(&mut *connection)
    .await
    .unwrap();
    rows.into_iter().map(|(name,)| name).collect()
}

async fn preserved_table_definitions(connection: &mut SqliteConnection) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT name, sql FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
         ORDER BY name",
    )
    .fetch_all(&mut *connection)
    .await
    .unwrap();
    rows.retain(|(name, _)| {
        !DROPPED_TABLES.contains(&name.as_str())
            && name != "threads"
            && name != "execution_runs"
            && name != "thread_goals"
            && name != OAUTH_CREDENTIALS_TABLE
            && name != SESSION_ENVIRONMENTS_TABLE
            && name != "workspaces"
            && name != "session_bindings"
            && name != "legacy_execution_registrations"
            && name != "machines"
    });
    rows
}

async fn read_only(path: &Path) -> SqliteConnection {
    SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(path).read_only(true))
        .await
        .unwrap()
}

/// v7 / v8 / v9 收敛并补齐初始化表，既有业务表结构与数据保持不变。
#[tokio::test]
async fn test_v7_v8_v9_all_converge_and_drop_only_the_remote_tables() {
    for source_version in [7, 8, 9] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("threads.db");
        let mut connection = v9_database(&path, source_version).await;
        populate(&mut connection).await;
        let tables_before = table_names(&mut connection).await;
        let definitions_before = preserved_table_definitions(&mut connection).await;
        connection.close().await.unwrap();

        let store = SqliteThreadStore::new(&path).await.unwrap();
        let mut connection = read_only(&path).await;
        let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
            .fetch_one(&mut connection)
            .await
            .unwrap();
        assert_eq!(version, CURRENT_SCHEMA_VERSION, "来源版本 {source_version}");

        let tables_after = table_names(&mut connection).await;
        for table in DROPPED_TABLES {
            assert!(
                !tables_after.iter().any(|name| name == table),
                "来源版本 {source_version}：{table} 应当已被删除"
            );
            assert!(tables_before.iter().any(|name| name == table));
        }
        let mut expected: Vec<String> = tables_before
            .iter()
            .filter(|name| {
                !DROPPED_TABLES.contains(&name.as_str()) && name.as_str() != "execution_runs"
            })
            .cloned()
            .collect();
        expected.extend([
            OAUTH_CREDENTIALS_TABLE.to_owned(),
            SESSION_ENVIRONMENTS_TABLE.to_owned(),
            "legacy_execution_registrations".to_owned(),
            "machines".to_owned(),
        ]);
        expected.sort();
        assert_eq!(tables_after, expected, "来源版本 {source_version}");
        assert_eq!(
            preserved_table_definitions(&mut connection).await,
            definitions_before
        );

        // 业务表逐行保留。
        let (title,): (String,) =
            sqlx::query_as("SELECT title FROM threads WHERE id = 'local-root'")
                .fetch_one(&mut connection)
                .await
                .unwrap();
        assert_eq!(title, "本机会话");
        let (cwd,): (String,) = sqlx::query_as(
            "SELECT relative_cwd FROM session_bindings WHERE thread_id = 'local-root'",
        )
        .fetch_one(&mut connection)
        .await
        .unwrap();
        assert_eq!(cwd, "");
        let (locator,): (String,) = sqlx::query_as("SELECT locator FROM projects WHERE id = 'p1'")
            .fetch_one(&mut connection)
            .await
            .unwrap();
        assert_eq!(locator, "/work");

        assert!(!tables_after.iter().any(|name| name == "execution_runs"));
        connection.close().await.unwrap();

        // 迁移后的库可以正常写打开（门面与桥共用同一条连接真相）。
        store.close().await;
        let reopened = SqliteThreadStore::new(&path).await.unwrap();
        reopened.close().await;
    }
}

/// 没有远程痕迹表时仅补齐初始化表，既有业务表结构与数据保持不变。
#[tokio::test]
async fn test_database_without_remote_tables_is_idempotent() {
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
    let tables_before = table_names(&mut connection).await;
    let definitions_before = preserved_table_definitions(&mut connection).await;
    connection.close().await.unwrap();

    let store = SqliteThreadStore::new(&path).await.unwrap();
    let mut connection = read_only(&path).await;
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(version, CURRENT_SCHEMA_VERSION);
    let mut expected = tables_before;
    expected.retain(|name| name != "execution_runs");
    expected.extend([
        OAUTH_CREDENTIALS_TABLE.to_owned(),
        SESSION_ENVIRONMENTS_TABLE.to_owned(),
        "legacy_execution_registrations".to_owned(),
        "machines".to_owned(),
    ]);
    expected.sort();
    assert_eq!(table_names(&mut connection).await, expected);
    assert_eq!(
        preserved_table_definitions(&mut connection).await,
        definitions_before
    );
    let sessions: Vec<(String, String)> =
        sqlx::query_as("SELECT id, title FROM threads ORDER BY id")
            .fetch_all(&mut connection)
            .await
            .unwrap();
    assert_eq!(
        sessions,
        vec![("local-root".to_owned(), "本机会话".to_owned())]
    );
    connection.close().await.unwrap();
    store.close().await;
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
    assert!(matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::UnsupportedDatabaseSchema)
    ));

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
