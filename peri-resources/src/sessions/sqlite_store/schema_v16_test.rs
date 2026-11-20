//! schema v16：Swarm 自有事实的四表与一条索引。
//!
//! 覆盖本阶段要求的可观察事实：新库建出 `swarm_*` 四表与索引；v15 → v16 只追加这五个对象，
//! 既有会话行、历史、MQ、凭证与索引原样保留；v15 形状不符时拒绝升级且版本保持 15。
//! 全部使用临时文件，不触碰真实数据库。
//!
//! v15 夹具按「本代只追加对象」的同一事实构造：九张会话表与**非 Swarm** 索引取自 canonical
//! 清单（本代没有改动它们），`swarm_*` 一概不建。

use super::SqliteThreadStore;

use crate::sessions::canonical;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{Connection, SqliteConnection};
use std::path::Path;

/// v15 的会话表快照（与 v16 相同：本代只追加对象，不改既有表）。
const V15_DDL: &str = "CREATE TABLE machines (
    id TEXT PRIMARY KEY, name TEXT NOT NULL, identity_kind TEXT NOT NULL);
CREATE TABLE projects (
    id TEXT PRIMARY KEY, locator TEXT NOT NULL UNIQUE);
CREATE TABLE workspaces (
    id TEXT PRIMARY KEY, machine_id TEXT NOT NULL REFERENCES machines(id),
    path TEXT NOT NULL, path_source TEXT NOT NULL, project_id TEXT REFERENCES projects(id),
    discovery TEXT, UNIQUE(machine_id, path));
CREATE TABLE threads (
    id TEXT PRIMARY KEY, title TEXT, cwd TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL, updated_at TEXT NOT NULL, message_count INTEGER NOT NULL DEFAULT 0,
    parent_thread_id TEXT, snapshot_at_message_id TEXT, hidden BOOLEAN NOT NULL DEFAULT 0,
    cancel_policy TEXT NOT NULL DEFAULT 'cascade', config TEXT,
    frozen_context TEXT, inherited_context TEXT, agent_status TEXT NOT NULL DEFAULT 'active',
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    archived BOOLEAN NOT NULL DEFAULT 0 CHECK(archived IN (0, 1)),
    context_usage_tokens INTEGER CHECK(context_usage_tokens IS NULL OR context_usage_tokens >= 0),
    work_state TEXT NOT NULL DEFAULT 'idle' CHECK(work_state IN ('idle', 'working', 'stopped')));
CREATE TABLE messages (
    message_id TEXT PRIMARY KEY, thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    role TEXT NOT NULL, content TEXT NOT NULL,
    truncated BOOLEAN NOT NULL DEFAULT 0, excluded BOOLEAN NOT NULL DEFAULT 0, projection TEXT);
CREATE TABLE session_bindings (
    thread_id TEXT PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
    schema_version INTEGER NOT NULL,
    project_id TEXT NOT NULL, workspace_id TEXT NOT NULL, relative_cwd TEXT NOT NULL,
    discovery_snapshot TEXT);
CREATE TABLE mcp_oauth_credentials (
    principal_id TEXT NOT NULL, workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    server_key TEXT NOT NULL, credentials_blob TEXT NOT NULL, updated_at TEXT NOT NULL,
    PRIMARY KEY (principal_id, workspace_id, server_key));
CREATE TABLE session_close_intents (
    thread_id TEXT PRIMARY KEY REFERENCES threads(id), requested_at TEXT NOT NULL);
CREATE TABLE thread_mq (
    thread_id TEXT NOT NULL REFERENCES threads(id), id TEXT NOT NULL,
    enqueued_at TEXT NOT NULL, kind TEXT NOT NULL, source TEXT NOT NULL,
    policy_json TEXT NOT NULL, payload_json TEXT NOT NULL, original_draft TEXT,
    PRIMARY KEY(thread_id, id), UNIQUE(id)) STRICT";

async fn connect(path: &Path) -> SqliteConnection {
    SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(false),
    )
    .await
    .unwrap()
}

/// 建一个 v15 库：一条会话、一条历史、一条绑定、一条待发 MQ、一条关闭意图与一条凭证。
async fn v15_fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let mut connection = connect(&path).await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "{V15_DDL};
         INSERT INTO machines VALUES ('machine', '本机', 'known');
         INSERT INTO projects VALUES ('project', '/work');
         INSERT INTO workspaces VALUES ('workspace', 'machine', '/work', 'discovered',
             'project', '{{}}');
         INSERT INTO threads (id, cwd, created_at, updated_at, message_count, workspace_id,
             work_state) VALUES ('session', '/work', '2020-01-01T00:00:00Z',
             '2020-01-01T00:00:00Z', 1, 'workspace', 'working');
         INSERT INTO messages (message_id, thread_id, role, content)
             VALUES ('message', 'session', 'user', 'kept');
         INSERT INTO session_bindings VALUES ('session', 1, 'project', 'workspace', '', NULL);
         INSERT INTO thread_mq VALUES ('session', 'mq-row', '2020-01-01T00:00:01Z', 'message',
             '{{\"agentId\":\"root\"}}', '{{}}', '{{\"text\":\"pending\"}}', NULL);
         INSERT INTO session_close_intents VALUES ('session', '2020-01-01T00:00:02Z');
         INSERT INTO mcp_oauth_credentials VALUES
             ('principal', 'workspace', 'server', 'credential bytes', '2020-01-01T00:00:03Z');
         PRAGMA user_version = 15;"
    )))
    .execute(&mut connection)
    .await
    .unwrap();
    // v15 的索引集 = canonical 索引集去掉 Swarm 索引（本代没有改动其余索引文本）。
    for statement in canonical::CREATE_INDEXES
        .iter()
        .filter(|sql| !sql.contains("swarm_"))
    {
        // 语句文本来自 `canonical` 的静态清单，不含外部输入。
        sqlx::raw_sql(sqlx::AssertSqlSafe((*statement).to_owned()))
            .execute(&mut connection)
            .await
            .unwrap();
    }
    connection.close().await.unwrap();
    (directory, path)
}

async fn scalar_i64(connection: &mut SqliteConnection, sql: String) -> i64 {
    let (value,): (i64,) = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .fetch_one(&mut *connection)
        .await
        .unwrap();
    value
}

async fn table_exists(connection: &mut SqliteConnection, table: &str) -> bool {
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT name FROM sqlite_schema WHERE type = 'table' AND name = ?1")
            .bind(table)
            .fetch_all(&mut *connection)
            .await
            .unwrap();
    !rows.is_empty()
}

#[tokio::test]
async fn fresh_database_has_the_swarm_tables_and_index() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("threads.db");
    SqliteThreadStore::try_new(&path)
        .await
        .unwrap()
        .close()
        .await;
    let mut connection = connect(&path).await;
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(version, 16);
    for table in canonical::SWARM_TABLES {
        assert!(table_exists(&mut connection, table).await, "{table}");
    }
    let indexes: Vec<(String,)> =
        sqlx::query_as("SELECT name FROM sqlite_schema WHERE type = 'index' AND name = ?1")
            .bind("swarm_items_by_id")
            .fetch_all(&mut connection)
            .await
            .unwrap();
    assert_eq!(indexes.len(), 1);
}

/// v15 → v16：只追加 `swarm_*` 四表与索引，既有会话事实逐项保留。
#[tokio::test]
async fn v15_to_v16_appends_swarm_objects_and_keeps_existing_rows() {
    let (_directory, path) = v15_fixture().await;
    {
        let mut connection = connect(&path).await;
        assert!(!table_exists(&mut connection, "swarm_sessions").await);
        assert_eq!(
            scalar_i64(&mut connection, "PRAGMA user_version".to_owned()).await,
            15
        );
    }
    SqliteThreadStore::try_new(&path)
        .await
        .unwrap()
        .close()
        .await;

    let mut connection = connect(&path).await;
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(version, 16);
    for table in canonical::SWARM_TABLES {
        assert!(table_exists(&mut connection, table).await, "{table}");
    }
    // 既有行原样保留（会话、历史、归属、项目、绑定、MQ、关闭意图、凭证）。
    for (table, expected) in [
        ("machines", 1),
        ("projects", 1),
        ("workspaces", 1),
        ("threads", 1),
        ("messages", 1),
        ("session_bindings", 1),
        ("thread_mq", 1),
        ("session_close_intents", 1),
        ("mcp_oauth_credentials", 1),
    ] {
        assert_eq!(
            scalar_i64(&mut connection, format!("SELECT COUNT(*) FROM {table}")).await,
            expected,
            "{table}"
        );
    }
    let (content, state): (String, String) = sqlx::query_as(
        "SELECT m.content, t.work_state FROM messages m JOIN threads t ON t.id = m.thread_id",
    )
    .fetch_one(&mut connection)
    .await
    .unwrap();
    assert_eq!(content, "kept");
    assert_eq!(state, "working");
    let (pending,): (String,) = sqlx::query_as("SELECT payload_json FROM thread_mq")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(pending, r#"{"text":"pending"}"#);
    // 既有索引仍在（升级不重建、不删任何既有对象）。
    let existing_index: Vec<(String,)> =
        sqlx::query_as("SELECT name FROM sqlite_schema WHERE type = 'index' AND name = ?1")
            .bind("idx_messages_thread_id")
            .fetch_all(&mut connection)
            .await
            .unwrap();
    assert_eq!(existing_index.len(), 1);
    // 外键补齐校验。
    let violations: Vec<(String, i64, String, i64)> = sqlx::query_as("PRAGMA foreign_key_check")
        .fetch_all(&mut connection)
        .await
        .unwrap();
    assert!(violations.is_empty(), "{violations:?}");
}

/// v15 版本号配错的形状：拒绝打开，版本保持 15，不产生任何 Swarm 对象。
#[tokio::test]
async fn a_v15_store_with_a_drifted_shape_is_refused() {
    let (_directory, path) = v15_fixture().await;
    {
        let mut connection = connect(&path).await;
        sqlx::query("ALTER TABLE session_bindings DROP COLUMN discovery_snapshot")
            .execute(&mut connection)
            .await
            .unwrap();
        connection.close().await.unwrap();
    }
    let error = SqliteThreadStore::try_new(&path).await.err().unwrap();
    assert!(
        error.to_string().contains("schema"),
        "拒绝必须能回答版本/形状问题: {error}"
    );
    let mut connection = connect(&path).await;
    assert_eq!(
        scalar_i64(&mut connection, "PRAGMA user_version".to_owned()).await,
        15
    );
    assert!(!table_exists(&mut connection, "swarm_sessions").await);
}
