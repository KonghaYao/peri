//! schema v15：首个删列表代的升级面。
//!
//! 覆盖计划要求的可观察事实：新库没有已删列/表、`projects` 以 `UNIQUE(locator)` 为项目
//! 身份、v14 → v15 删列删表并保留行、重复 locator 合并保留最早一行、旧完整快照被投影、
//! 损坏快照拒绝整库升级、缺失证据保持 NULL。全部使用临时文件，不触碰真实数据库。

use super::*;
use crate::sessions::canonical;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{Connection, SqliteConnection};
use std::path::Path;

/// v14 的完整建表形状（手工快照，不引用当前 DDL）：`projects` 带 `object_identity`、
/// `workspaces` 带 `identity`、`session_bindings` 带 `evidence_origin`，两张派生索引表在位。
const V14_DDL: &str = "CREATE TABLE machines (
    id TEXT PRIMARY KEY, name TEXT NOT NULL, identity_kind TEXT NOT NULL);
CREATE TABLE projects (
    id TEXT PRIMARY KEY, locator TEXT NOT NULL, object_identity TEXT NOT NULL,
    UNIQUE(locator, object_identity));
CREATE TABLE workspaces (
    id TEXT PRIMARY KEY, machine_id TEXT NOT NULL REFERENCES machines(id),
    path TEXT NOT NULL, path_source TEXT NOT NULL CHECK(path_source IN ('discovered', 'derived_legacy', 'unverified')),
    project_id TEXT REFERENCES projects(id), identity TEXT, discovery TEXT,
    UNIQUE(machine_id, path));
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
    discovery_snapshot TEXT, evidence_origin TEXT NOT NULL);
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
    PRIMARY KEY(thread_id, id), UNIQUE(id)) STRICT;
CREATE TABLE inherited_snapshot_index (
    snapshot_id TEXT PRIMARY KEY, thread_id TEXT NOT NULL, payload_bytes INTEGER NOT NULL,
    entry_count INTEGER NOT NULL, prepared_at TEXT NOT NULL);
CREATE TABLE inherited_payload_index (
    snapshot_id TEXT NOT NULL, ordinal INTEGER NOT NULL, entry_id TEXT NOT NULL,
    raw_start INTEGER NOT NULL, raw_len INTEGER NOT NULL, logical_len INTEGER,
    PRIMARY KEY(snapshot_id, ordinal), UNIQUE(snapshot_id, entry_id));";

const FULL_DISCOVERY: &str = r#"{"root":"/work","root_identity":{"device":1,"inode":2},"common_dir":"/work/.git","common_identity":{"device":1,"inode":3},"private_dir":"/work/.git","private_identity":{"device":1,"inode":4}}"#;

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

/// 建一个 v14 库：两个项目行（同 locator）、一条归属行、一条会话、两条绑定（一条完整
/// 快照、一条缺失证据），两张派生索引表各一行。
async fn v14_fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let mut connection = connect(&path).await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "{V14_DDL}
         INSERT INTO machines VALUES ('machine', '机器', 'known');
         INSERT INTO projects VALUES ('keep-project', '/work', '{{\"device\":1,\"inode\":2}}');
         INSERT INTO projects VALUES ('drop-project', '/work', '{{\"device\":9,\"inode\":9}}');
         INSERT INTO workspaces VALUES ('workspace', 'machine', '/work', 'discovered',
             'drop-project', '{{\"device\":1,\"inode\":2}}', '{FULL_DISCOVERY}');
         INSERT INTO threads (id, cwd, created_at, updated_at, message_count, workspace_id, inherited_context)
         VALUES ('session', '/work', '2020-01-01T00:00:00Z', '2020-01-01T00:00:00Z', 1,
                 'workspace', 'inherited-bytes');
         INSERT INTO messages (message_id, thread_id, role, content)
         VALUES ('message', 'session', 'user', 'kept');
         INSERT INTO threads (id, cwd, created_at, updated_at, message_count, workspace_id)
         VALUES ('without-evidence', '/work', '2020-01-01T00:00:00Z', '2020-01-01T00:00:00Z', 0,
                 'workspace');
         INSERT INTO session_bindings VALUES ('session', 1, 'drop-project', 'workspace', '',
             '{FULL_DISCOVERY}', 'creation_snapshot');
         INSERT INTO session_bindings VALUES ('without-evidence', 1, 'drop-project', 'workspace',
             '', NULL, 'creation_snapshot');
         INSERT INTO inherited_snapshot_index VALUES ('snapshot', 'session', 10, 1, 'now');
         INSERT INTO inherited_payload_index VALUES ('snapshot', 0, 'entry', 0, 10, NULL);
         PRAGMA user_version = 14;"
    )))
    .execute(&mut connection)
    .await
    .unwrap();
    connection.close().await.unwrap();
    (directory, path)
}

async fn columns(connection: &mut SqliteConnection, table: &str) -> Vec<String> {
    let rows: Vec<(String,)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT name FROM pragma_table_info('{table}')"
    )))
    .fetch_all(&mut *connection)
    .await
    .unwrap();
    rows.into_iter().map(|(name,)| name).collect()
}

#[tokio::test]
async fn fresh_database_has_no_retired_columns_or_tables() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("threads.db");
    SqliteThreadStore::new(&path).await.unwrap().close().await;
    let mut connection = connect(&path).await;
    for (table, column) in [
        ("projects", "object_identity"),
        ("workspaces", "identity"),
        ("session_bindings", "evidence_origin"),
    ] {
        assert!(
            !columns(&mut connection, table)
                .await
                .contains(&column.to_owned()),
            "{table}.{column} 不得出现在新库"
        );
    }
    let (retired,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table'
         AND name IN ('inherited_snapshot_index', 'inherited_payload_index')",
    )
    .fetch_one(&mut connection)
    .await
    .unwrap();
    assert_eq!(retired, 0);
}

/// 项目身份就是 locator：唯一键收窄后同 locator 的第二行必须被拒绝。
#[tokio::test]
async fn projects_unique_locator() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("threads.db");
    SqliteThreadStore::new(&path).await.unwrap().close().await;
    let mut connection = connect(&path).await;
    sqlx::query("INSERT INTO projects (id, locator) VALUES ('p1', '/work')")
        .execute(&mut connection)
        .await
        .unwrap();
    assert!(
        sqlx::query("INSERT INTO projects (id, locator) VALUES ('p2', '/work')")
            .execute(&mut connection)
            .await
            .is_err(),
        "v15 的唯一键是 UNIQUE(locator)"
    );
}

/// v14 → v15：列与表删除、行保留、重复 locator 合并到最早一行、引用先改绑。
#[tokio::test]
async fn v14_to_v15_drops_columns_and_tables_and_keeps_rows() {
    let (_directory, path) = v14_fixture().await;
    SqliteThreadStore::new(&path).await.unwrap().close().await;
    let mut connection = connect(&path).await;
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(version, canonical::CURRENT_SCHEMA_VERSION);

    // 两行同 locator 合并为一行：保留 rowid 最早的项目（'keep-project'）。
    let projects: Vec<(String, String)> =
        sqlx::query_as("SELECT id, locator FROM projects ORDER BY id")
            .fetch_all(&mut connection)
            .await
            .unwrap();
    assert_eq!(
        projects,
        vec![("keep-project".to_owned(), "/work".to_owned())]
    );
    // 引用先改绑再删行：归属行与两个绑定都指向保留行。
    let (workspace_project,): (String,) =
        sqlx::query_as("SELECT project_id FROM workspaces WHERE id = 'workspace'")
            .fetch_one(&mut connection)
            .await
            .unwrap();
    assert_eq!(workspace_project, "keep-project");
    let binding_projects: Vec<(String, String)> =
        sqlx::query_as("SELECT thread_id, project_id FROM session_bindings ORDER BY thread_id")
            .fetch_all(&mut connection)
            .await
            .unwrap();
    assert_eq!(
        binding_projects,
        vec![
            ("session".to_owned(), "keep-project".to_owned()),
            ("without-evidence".to_owned(), "keep-project".to_owned()),
        ]
    );
    // 行数与历史、inherited 字节不变。
    for (table, expected) in [("threads", 2), ("messages", 1), ("workspaces", 1)] {
        let (count,): (i64,) =
            sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
                .fetch_one(&mut connection)
                .await
                .unwrap();
        assert_eq!(count, expected, "{table}");
    }
    let (inherited,): (String,) =
        sqlx::query_as("SELECT inherited_context FROM threads WHERE id = 'session'")
            .fetch_one(&mut connection)
            .await
            .unwrap();
    assert_eq!(inherited, "inherited-bytes");
    // 退役表消失。
    let (retired,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table'
         AND name IN ('inherited_snapshot_index', 'inherited_payload_index')",
    )
    .fetch_one(&mut connection)
    .await
    .unwrap();
    assert_eq!(retired, 0);
    // 外键补齐校验。
    let violations: Vec<(String, i64, String, i64)> = sqlx::query_as("PRAGMA foreign_key_check")
        .fetch_all(&mut connection)
        .await
        .unwrap();
    assert!(violations.is_empty(), "{violations:?}");
}

/// 旧完整快照在重建时投影为最小执行根；缺失证据保持 NULL（不伪造）。
#[tokio::test]
async fn legacy_snapshots_are_projected_and_missing_evidence_stays_null() {
    let (_directory, path) = v14_fixture().await;
    SqliteThreadStore::new(&path).await.unwrap().close().await;
    let mut connection = connect(&path).await;
    let (snapshot,): (String,) = sqlx::query_as(
        "SELECT discovery_snapshot FROM session_bindings WHERE thread_id = 'session'",
    )
    .fetch_one(&mut connection)
    .await
    .unwrap();
    assert_eq!(snapshot, r#"{"root":"/work"}"#);
    let (missing,): (Option<String>,) = sqlx::query_as(
        "SELECT discovery_snapshot FROM session_bindings WHERE thread_id = 'without-evidence'",
    )
    .fetch_one(&mut connection)
    .await
    .unwrap();
    assert_eq!(missing, None, "旧远端缺证据的行保持 NULL，不伪造执行根");
    // 归属行的完整发现快照不被清洗（诊断依据与路径来源判定仍在用它）。
    let (discovery,): (String,) =
        sqlx::query_as("SELECT discovery FROM workspaces WHERE id = 'workspace'")
            .fetch_one(&mut connection)
            .await
            .unwrap();
    assert_eq!(discovery, FULL_DISCOVERY);
}

/// 损坏的创建证据拒绝整库升级：版本保持 14、旧二进制仍可打开。
#[tokio::test]
async fn corrupted_snapshot_refuses_the_upgrade() {
    let (_directory, path) = v14_fixture().await;
    {
        let mut connection = connect(&path).await;
        sqlx::query("UPDATE session_bindings SET discovery_snapshot = 'not json' WHERE thread_id = 'session'")
            .execute(&mut connection)
            .await
            .unwrap();
        connection.close().await.unwrap();
    }
    let error = SqliteThreadStore::new(&path).await.err().unwrap();
    assert!(
        error.to_string().contains("creation snapshot"),
        "错误必须指出无法投影创建证据: {error}"
    );
    let mut connection = connect(&path).await;
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(version, 14, "失败回滚后版本保持 14");
    let (bindings,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM session_bindings")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(bindings, 2, "失败不丢行");
}

/// 旧行仍带 `root_identity` 等键：读取端天然兼容（serde 忽略未知键），不要求清洗字节。
#[tokio::test]
async fn legacy_identity_json_stays_readable() {
    let (_directory, path) = v14_fixture().await;
    SqliteThreadStore::new(&path).await.unwrap().close().await;
    let mut connection = connect(&path).await;
    // 归属行的 discovery 仍是完整 v14 形态；按当前结构反序列化必须成功且 root 正确。
    let (discovery,): (String,) =
        sqlx::query_as("SELECT discovery FROM workspaces WHERE id = 'workspace'")
            .fetch_one(&mut connection)
            .await
            .unwrap();
    let parsed: crate::sessions::discovery::Discovery = serde_json::from_str(&discovery).unwrap();
    assert_eq!(parsed.root, std::path::PathBuf::from("/work"));
    assert_eq!(
        parsed.common_dir,
        Some(std::path::PathBuf::from("/work/.git"))
    );
}

/// 退役列删除前的形状守卫：同名异形的 `inherited_*` 表不是迁移认识的对象。
///
/// 与计划 §3.6 一致：存在但形状不符 → 拒绝、回滚、库保持 14，原表与数据不动。
#[tokio::test]
async fn foreign_shaped_inherited_tables_refuse_the_upgrade() {
    let (_directory, path) = v14_fixture().await;
    {
        let mut connection = connect(&path).await;
        sqlx::raw_sql(
            "DROP TABLE inherited_payload_index;
             DROP TABLE inherited_snapshot_index;
             CREATE TABLE inherited_payload_index (
                 snapshot_id TEXT PRIMARY KEY, totally_other_column TEXT);
             CREATE TABLE inherited_snapshot_index (whatever TEXT);",
        )
        .execute(&mut connection)
        .await
        .unwrap();
        connection.close().await.unwrap();
    }
    let error = SqliteThreadStore::new(&path).await.err().unwrap();
    let message = error.to_string();
    assert!(
        message.contains("refuses to drop a foreign inherited table"),
        "错误必须说明拒绝删除同名异形表: {message}"
    );
    assert!(
        message.contains("inherited_"),
        "错误必须报出具体表名: {message}"
    );

    // 失败回滚：版本 14、会话数据与两张表原样保留。
    let mut connection = connect(&path).await;
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(version, 14, "拒绝后版本必须保持 14");
    let tables: Vec<(String,)> = sqlx::query_as(
        "SELECT name FROM sqlite_schema WHERE type = 'table'
         AND name IN ('inherited_payload_index', 'inherited_snapshot_index') ORDER BY name",
    )
    .fetch_all(&mut connection)
    .await
    .unwrap();
    assert_eq!(
        tables
            .iter()
            .map(|(name,)| name.as_str())
            .collect::<Vec<_>>(),
        vec!["inherited_payload_index", "inherited_snapshot_index"],
        "同名异形表不得被删除"
    );
    let (threads,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM threads")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(threads, 2, "会话行不得因回滚丢失");
    let (messages,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM messages")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(messages, 1, "历史不得因回滚丢失");
    let violations: Vec<(String, i64, String, i64)> = sqlx::query_as("PRAGMA foreign_key_check")
        .fetch_all(&mut connection)
        .await
        .unwrap();
    assert!(violations.is_empty(), "{violations:?}");
}

/// 缺表合法：派生索引在 v14 即可选，升级照常完成。
#[tokio::test]
async fn missing_inherited_tables_upgrade_cleanly() {
    let (_directory, path) = v14_fixture().await;
    {
        let mut connection = connect(&path).await;
        sqlx::raw_sql(
            "DROP TABLE inherited_payload_index;
             DROP TABLE inherited_snapshot_index;",
        )
        .execute(&mut connection)
        .await
        .unwrap();
        connection.close().await.unwrap();
    }
    SqliteThreadStore::new(&path).await.unwrap().close().await;
    let mut connection = connect(&path).await;
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(version, canonical::CURRENT_SCHEMA_VERSION);
    let (threads,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM threads")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(threads, 2);
}

/// 正确 v14 形状 → 两表被删除、版本推进（与 `v14_to_v15_...` 共同覆盖成功路径）。
#[tokio::test]
async fn correctly_shaped_inherited_tables_are_dropped() {
    let (_directory, path) = v14_fixture().await;
    SqliteThreadStore::new(&path).await.unwrap().close().await;
    let mut connection = connect(&path).await;
    let tables: Vec<(String,)> = sqlx::query_as(
        "SELECT name FROM sqlite_schema WHERE type = 'table'
         AND name IN ('inherited_payload_index', 'inherited_snapshot_index')",
    )
    .fetch_all(&mut connection)
    .await
    .unwrap();
    assert!(tables.is_empty(), "正确形状的派生表必须被删除: {tables:?}");
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(version, canonical::CURRENT_SCHEMA_VERSION);
}
