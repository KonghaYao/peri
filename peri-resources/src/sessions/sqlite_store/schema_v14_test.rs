//! schema v14：`threads` 的两列工作快照（见 `docs/design/session-durability.md` §2）。
//!
//! 断言以可观察事实为准：版本号、列声明与约束、旧行读到的默认值，以及升级前后
//! `threads` / `messages` / `thread_mq` 的行内容。夹具一律是临时文件，不解析真实库路径。

use super::*;
use crate::sessions::canonical::{
    CREATE_THREAD_MQ_ORDER_INDEX_SQL, CURRENT_SCHEMA_VERSION, THREADS_V14_COLUMNS,
};
use peri_acp_types::thread::WorkState;
use sqlx::{Connection, SqliteConnection};
use std::path::{Path, PathBuf};

async fn connect(path: &Path) -> SqliteConnection {
    // 夹具只关心形状与列值：关掉父行检查，避免为了插一行 thread 先造 workspace。
    SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(path)
            .foreign_keys(false),
    )
    .await
    .unwrap()
}

async fn user_version(connection: &mut SqliteConnection) -> i64 {
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&mut *connection)
        .await
        .unwrap();
    version
}

async fn thread_columns(connection: &mut SqliteConnection) -> Vec<String> {
    let rows: Vec<(String,)> = sqlx::query_as("SELECT name FROM pragma_table_info('threads')")
        .fetch_all(&mut *connection)
        .await
        .unwrap();
    rows.into_iter().map(|(name,)| name).collect()
}

async fn close(store: SqliteThreadStore) {
    store.database.pool.close().await;
}

/// 建一个当前版本的库，并写入一条 thread、一条 message、一条待消费的 MQ 行。
async fn seeded_database() -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("threads.db");
    close(SqliteThreadStore::new(path.clone()).await.unwrap()).await;
    let mut connection = connect(&path).await;
    sqlx::raw_sql(
        "INSERT INTO threads (id, title, created_at, updated_at, message_count, workspace_id)
         VALUES ('session', 'kept', '2020-01-01T00:00:00Z', '2020-01-01T00:00:00Z', 1, 'workspace');
         INSERT INTO messages (message_id, thread_id, role, content, truncated, excluded)
         VALUES ('message', 'session', 'user', 'kept content', 0, 0);
         INSERT INTO thread_mq (thread_id, id, enqueued_at, kind, source, policy_json, payload_json)
         VALUES ('session', 'queued', '2020-01-01T00:00:00Z', 'input', 'reminder', '{}', '{}');",
    )
    .execute(&mut connection)
    .await
    .unwrap();
    connection.close().await.unwrap();
    (directory, path)
}

/// 把当前库退回指定代际：先删两列工作快照，再按目标版本删 `thread_mq` / 派生索引表。
async fn downgrade(path: &Path, version: i64, drop_derived: bool) {
    let mut connection = connect(path).await;
    let mut statements = String::from(
        "ALTER TABLE threads DROP COLUMN context_usage_tokens;
         ALTER TABLE threads DROP COLUMN work_state;",
    );
    if version <= 12 {
        statements.push_str(" DROP TABLE thread_mq;");
    }
    if drop_derived {
        statements.push_str(
            " DROP TABLE inherited_payload_index;
              DROP TABLE inherited_snapshot_index;",
        );
    }
    statements.push_str(&format!(" PRAGMA user_version = {version};"));
    sqlx::raw_sql(sqlx::AssertSqlSafe(statements))
        .execute(&mut connection)
        .await
        .unwrap();
    connection.close().await.unwrap();
}

/// 新库：两列追加在末尾、默认值与 CHECK 约束都是设计契约。
#[tokio::test]
async fn fresh_database_declares_the_work_snapshot_columns() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("threads.db");
    close(SqliteThreadStore::new(path.clone()).await.unwrap()).await;

    let mut connection = connect(&path).await;
    assert_eq!(user_version(&mut connection).await, CURRENT_SCHEMA_VERSION);
    let columns = thread_columns(&mut connection).await;
    let tail: Vec<&str> = columns
        .iter()
        .rev()
        .take(THREADS_V14_COLUMNS.len())
        .rev()
        .map(String::as_str)
        .collect();
    assert_eq!(
        tail,
        THREADS_V14_COLUMNS
            .iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>(),
        "新列只能追加在末尾：迁移按同一列序 ALTER，形状判定逐项比对"
    );

    sqlx::raw_sql(
        "INSERT INTO threads (id, created_at, updated_at, workspace_id)
         VALUES ('session', '2020-01-01T00:00:00Z', '2020-01-01T00:00:00Z', 'workspace')",
    )
    .execute(&mut connection)
    .await
    .unwrap();
    let (usage, state): (Option<i64>, String) =
        sqlx::query_as("SELECT context_usage_tokens, work_state FROM threads WHERE id = 'session'")
            .fetch_one(&mut connection)
            .await
            .unwrap();
    assert_eq!(usage, None, "未知占用不是零占用");
    assert_eq!(state, "idle", "新行不得落进自动续跑状态");

    assert!(
        sqlx::query("UPDATE threads SET work_state = 'busy'")
            .execute(&mut connection)
            .await
            .is_err(),
        "work_state 只接受 idle/working/stopped"
    );
    assert!(
        sqlx::query("UPDATE threads SET context_usage_tokens = -1")
            .execute(&mut connection)
            .await
            .is_err(),
        "负的上下文占用必须被拒绝"
    );
    sqlx::query("UPDATE threads SET context_usage_tokens = 0, work_state = 'working'")
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query("UPDATE threads SET context_usage_tokens = NULL, work_state = 'stopped'")
        .execute(&mut connection)
        .await
        .unwrap();
}

/// v13 → v14：只补两列，历史、MQ 行与派生对象都保持原样。
#[tokio::test]
async fn upgrading_from_v13_adds_the_columns_without_touching_history() {
    let (_directory, path) = seeded_database().await;
    downgrade(&path, 13, false).await;
    close(SqliteThreadStore::new(path.clone()).await.unwrap()).await;

    let mut connection = connect(&path).await;
    assert_eq!(user_version(&mut connection).await, CURRENT_SCHEMA_VERSION);
    let (title, count, usage, state): (String, i64, Option<i64>, String) = sqlx::query_as(
        "SELECT title, message_count, context_usage_tokens, work_state
         FROM threads WHERE id = 'session'",
    )
    .fetch_one(&mut connection)
    .await
    .unwrap();
    assert_eq!((title.as_str(), count), ("kept", 1), "会话事实不得被改写");
    assert_eq!(usage, None);
    assert_eq!(state, "idle", "旧行升级后不得自动续跑");

    let (content,): (String,) =
        sqlx::query_as("SELECT content FROM messages WHERE message_id = 'message'")
            .fetch_one(&mut connection)
            .await
            .unwrap();
    assert_eq!(content, "kept content");
    let (queued,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM thread_mq WHERE thread_id = 'session'")
            .fetch_one(&mut connection)
            .await
            .unwrap();
    assert_eq!(queued, 1, "v13 已有的 MQ 行必须保留");
}

/// v12 → v14：链式补齐 `thread_mq` 与两列，MQ 排序索引一并建立。
#[tokio::test]
async fn upgrading_from_v12_creates_thread_mq_and_the_columns() {
    let (_directory, path) = seeded_database().await;
    downgrade(&path, 12, false).await;
    close(SqliteThreadStore::new(path.clone()).await.unwrap()).await;

    let mut connection = connect(&path).await;
    assert_eq!(user_version(&mut connection).await, CURRENT_SCHEMA_VERSION);
    let (mq_table,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name = 'thread_mq'",
    )
    .fetch_one(&mut connection)
    .await
    .unwrap();
    assert_eq!(mq_table, 1, "v12 → v14 必须补齐 thread_mq");
    let (mq_index,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'index' AND name = 'idx_thread_mq_order'",
    )
    .fetch_one(&mut connection)
    .await
    .unwrap();
    assert_eq!(
        mq_index, 1,
        "{CREATE_THREAD_MQ_ORDER_INDEX_SQL} 必须重放一次"
    );
    let (usage, state): (Option<i64>, String) =
        sqlx::query_as("SELECT context_usage_tokens, work_state FROM threads WHERE id = 'session'")
            .fetch_one(&mut connection)
            .await
            .unwrap();
    assert_eq!((usage, state.as_str()), (None, "idle"));
}

/// v11 → v14：派生索引表、`thread_mq` 与两列一起补齐，历史不动。
#[tokio::test]
async fn upgrading_from_v11_restores_derived_objects_and_the_columns() {
    let (_directory, path) = seeded_database().await;
    downgrade(&path, 11, true).await;
    close(SqliteThreadStore::new(path.clone()).await.unwrap()).await;

    let mut connection = connect(&path).await;
    assert_eq!(user_version(&mut connection).await, CURRENT_SCHEMA_VERSION);
    let (derived,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table'
         AND name IN ('inherited_snapshot_index', 'inherited_payload_index', 'thread_mq')",
    )
    .fetch_one(&mut connection)
    .await
    .unwrap();
    assert_eq!(derived, 3);
    let (content,): (String,) =
        sqlx::query_as("SELECT content FROM messages WHERE message_id = 'message'")
            .fetch_one(&mut connection)
            .await
            .unwrap();
    assert_eq!(content, "kept content");
    let (state,): (String,) = sqlx::query_as("SELECT work_state FROM threads WHERE id = 'session'")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(state, "idle");
}

/// 写入面：工作意图与上下文占用经 store 写回，读路径必须如实反映（含清回 NULL）。
#[tokio::test]
async fn work_state_and_context_usage_round_trip_through_the_store() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = std::fs::canonicalize(directory.path()).unwrap();
    let store = SqliteThreadStore::new(directory.path().join("threads.db"))
        .await
        .unwrap();
    store.resolve_workspace(&cwd).await.unwrap();
    let id = store
        .create_thread(ThreadMeta::new_at(
            cwd.to_str().unwrap(),
            peri_time::now_wall(),
        ))
        .await
        .unwrap();

    let meta = store.load_meta(&id).await.unwrap();
    assert_eq!(meta.work_state, WorkState::Idle);
    assert_eq!(meta.context_usage_tokens, None);

    store.set_work_state(&id, WorkState::Working).await.unwrap();
    store.set_context_usage(&id, Some(4096)).await.unwrap();
    let meta = store.load_meta(&id).await.unwrap();
    assert_eq!(meta.work_state, WorkState::Working);
    assert_eq!(meta.context_usage_tokens, Some(4096));

    store.set_work_state(&id, WorkState::Stopped).await.unwrap();
    store.set_context_usage(&id, None).await.unwrap();
    let meta = store.load_meta(&id).await.unwrap();
    assert_eq!(meta.work_state, WorkState::Stopped);
    assert_eq!(meta.context_usage_tokens, None, "未知占用必须写回 NULL");
}
