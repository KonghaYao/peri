use super::*;
use crate::sessions::canonical::{CREATE_INDEXES, V10_CREATE_TABLES};
use crate::sessions::schema_cleanup::{
    LEGACY_BOUND_EXECUTION_SQL, LEGACY_EXECUTION_SQL, LEGACY_GOALS_SQL,
};
use peri_acp_types::messages::BaseMessage;
use peri_acp_types::store::{serialize_persisted_payload, PersistedPayload};
use peri_acp_types::workspace::WorkspaceError;
use sqlx::{AssertSqlSafe, Connection, SqliteConnection};

async fn old_database() -> (tempfile::TempDir, SqliteConnection) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let mut connection = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .foreign_keys(true),
    )
    .await
    .unwrap();
    for statement in V10_CREATE_TABLES {
        sqlx::query(*statement)
            .execute(&mut connection)
            .await
            .unwrap();
    }
    sqlx::raw_sql("ALTER TABLE threads ADD COLUMN cached_context TEXT;
        ALTER TABLE threads ADD COLUMN context_cache_epoch INTEGER NOT NULL DEFAULT 0;
        PRAGMA user_version = 10;
        CREATE TABLE extension_state (id TEXT PRIMARY KEY, value TEXT NOT NULL);
        INSERT INTO extension_state VALUES ('extension', 'must survive');
        INSERT INTO threads (id, title, cwd, created_at, updated_at, message_count, config, cached_context, context_cache_epoch)
        VALUES ('older', 'older', '/tmp', '2020-01-01T00:00:00Z', '2020-01-01T00:00:00Z', 1, '{\"model\":\"keep\"}', 'obsolete cache', 7);
        INSERT INTO threads (id, title, cwd, created_at, updated_at, message_count)
        VALUES ('newer', 'newer', '/tmp', '2020-02-01T00:00:00Z', '2020-02-01T00:00:00Z', 1);")
        .execute(&mut connection).await.unwrap();
    sqlx::query(LEGACY_GOALS_SQL)
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query(LEGACY_EXECUTION_SQL)
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query("INSERT INTO execution_runs VALUES ('older', 7, 0), ('orphan', 9, 0)")
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query("INSERT INTO thread_goals VALUES ('older', 'goal', 'legacy goal', 'active', 100, 10, 2, 1, 2)")
        .execute(&mut connection).await.unwrap();
    for id in ["older", "newer"] {
        let message = BaseMessage::human(id);
        sqlx::query("INSERT INTO messages (message_id, thread_id, role, content, truncated, excluded) VALUES (?1, ?2, 'user', ?3, 1, 1)")
            .bind(message.id().as_uuid().to_string()).bind(id)
            .bind(serialize_persisted_payload(&PersistedPayload::Message(message)).unwrap())
            .execute(&mut connection).await.unwrap();
    }
    (directory, connection)
}

async fn history(
    connection: &mut SqliteConnection,
) -> Vec<(i64, String, String, String, bool, bool, Option<String>)> {
    sqlx::query_as("SELECT rowid, message_id, thread_id, content, truncated, excluded, projection FROM messages ORDER BY rowid")
        .fetch_all(connection).await.unwrap()
}

/// 升级只重建 `threads` / `session_bindings` / `workspaces` / oauth：挂在**别的表**上的
/// 扩展对象（列、表、索引、视图）原样保留，历史行与 rowid 不变。
///
/// 重建表上不承载数据的挂载对象不随表搬运：索引由迁移末尾整表重放 canonical 集合补回
/// （见 `schema_v11_rebuild_replaces_the_index_set_on_threads`），触发器随重建消失。
/// 载有数据的未知列没有去处，一律拒绝升级（见 `unknown_thread_columns_refuse_the_upgrade`）。
#[tokio::test]
async fn old_versions_upgrade_preserving_goal_extensions_and_history_rowids() {
    for version in [10, 11] {
        let (directory, mut connection) = old_database().await;
        sqlx::raw_sql(
            "ALTER TABLE thread_goals ADD COLUMN extension TEXT;
            UPDATE thread_goals SET extension = 'goal extension';
            CREATE TABLE extension_goal_rows (id TEXT REFERENCES thread_goals(thread_id));
            INSERT INTO extension_goal_rows VALUES ('older');
            CREATE INDEX extension_goal_index ON thread_goals(objective);
            CREATE VIEW extension_goal_view AS SELECT objective FROM thread_goals;",
        )
        .execute(&mut connection)
        .await
        .unwrap();
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "PRAGMA user_version = {version}"
        )))
        .execute(&mut connection)
        .await
        .unwrap();
        let before = history(&mut connection).await;
        let objects: Vec<(String, Option<String>)> = sqlx::query_as(
            "SELECT name, sql FROM sqlite_schema WHERE name LIKE 'extension_%' ORDER BY name",
        )
        .fetch_all(&mut connection)
        .await
        .unwrap();
        connection.close().await.unwrap();
        let store = SqliteThreadStore::new(directory.path().join("threads.db"))
            .await
            .unwrap();
        let mut connection = store.database.pool.acquire().await.unwrap();
        assert_eq!(before, history(&mut connection).await);
        let after: Vec<(String, Option<String>)> = sqlx::query_as(
            "SELECT name, sql FROM sqlite_schema WHERE name LIKE 'extension_%' ORDER BY name",
        )
        .fetch_all(&mut *connection)
        .await
        .unwrap();
        assert_eq!(objects, after);
        let retained: (String,) = sqlx::query_as("SELECT extension FROM thread_goals")
            .fetch_one(&mut *connection)
            .await
            .unwrap();
        assert_eq!(retained.0, "goal extension");
        let version: i64 = sqlx::query_scalar("PRAGMA user_version")
            .fetch_one(&mut *connection)
            .await
            .unwrap();
        assert_eq!(version, CURRENT_SCHEMA_VERSION);
        drop(connection);
        let extension: String = sqlx::query_scalar("SELECT extension FROM thread_goals")
            .fetch_one(&store.database.pool)
            .await
            .unwrap();
        assert_eq!(extension, "goal extension");
    }
}

/// `threads` 被重建：表上不承载数据的挂载对象随重建消失，索引以 canonical 集合为准
/// （迁移末尾整表重放一次，补回 `DROP TABLE` 带走的那两条，也顺手清掉旧代留下的退役索引）。
#[tokio::test]
async fn schema_v11_rebuild_replaces_the_index_set_on_threads() {
    let (directory, mut connection) = old_database().await;
    sqlx::raw_sql(
        "CREATE INDEX idx_threads_parent_thread_id ON threads(parent_thread_id);
        CREATE TRIGGER extension_title_trigger AFTER UPDATE OF title ON threads
            BEGIN UPDATE thread_goals SET status = 'paused' WHERE thread_id = NEW.id; END;",
    )
    .execute(&mut connection)
    .await
    .unwrap();
    let before = history(&mut connection).await;
    assert_eq!(
        index_names(&mut connection, "threads").await,
        vec!["idx_threads_parent_thread_id".to_owned()]
    );
    connection.close().await.unwrap();
    let store = SqliteThreadStore::new(directory.path().join("threads.db"))
        .await
        .unwrap();
    let mut connection = store.database.pool.acquire().await.unwrap();
    let mut expected: Vec<String> = CREATE_INDEXES
        .iter()
        .filter(|statement| statement.contains(" ON threads("))
        .map(|statement| canonical_index_name(statement))
        .collect();
    expected.sort();
    assert_eq!(index_names(&mut connection, "threads").await, expected);
    let (trigger,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'trigger' AND tbl_name = 'threads'",
    )
    .fetch_one(&mut *connection)
    .await
    .unwrap();
    assert_eq!(trigger, 0);
    assert_eq!(before, history(&mut connection).await);
}

/// 本库已有的索引名（按名排序），用于断言迁移终点的索引集合。
async fn index_names(connection: &mut SqliteConnection, table: &str) -> Vec<String> {
    let rows: Vec<(String,)> = sqlx::query_as(AssertSqlSafe(format!(
        "SELECT name FROM sqlite_schema WHERE type = 'index' AND tbl_name = '{table}'
            AND name NOT LIKE 'sqlite_%' ORDER BY name"
    )))
    .fetch_all(connection)
    .await
    .unwrap();
    rows.into_iter().map(|(name,)| name).collect()
}

/// 从 `CREATE INDEX IF NOT EXISTS <name> ON ...` 里取出索引名（canonical 的语句形态固定）。
fn canonical_index_name(statement: &str) -> String {
    statement
        .split_whitespace()
        .skip_while(|token| *token != "EXISTS")
        .nth(1)
        .expect("canonical index statement must name the index")
        .to_owned()
}

/// 未知列载有数据、没有去处：升级必须**拒绝**（库保持原版本、字节不变），而不是把值搬丢。
#[tokio::test]
async fn unknown_thread_columns_refuse_the_upgrade() {
    for version in [10, 11] {
        let (directory, mut connection) = old_database().await;
        sqlx::raw_sql(
            "ALTER TABLE threads ADD COLUMN extension_value TEXT;
            UPDATE threads SET extension_value = 'thread extension' WHERE id = 'older';",
        )
        .execute(&mut connection)
        .await
        .unwrap();
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "PRAGMA user_version = {version}"
        )))
        .execute(&mut connection)
        .await
        .unwrap();
        let before = history(&mut connection).await;
        connection.close().await.unwrap();
        let error = match SqliteThreadStore::new(directory.path().join("threads.db")).await {
            Ok(_) => panic!("unknown columns on a rebuilt table must refuse the upgrade"),
            Err(error) => error,
        };
        let message = format!("{error:#}");
        // 版本 10 走搬迁路径：拒绝文案点名未知列（见 `storage_v11_migration`）。版本 11 由
        // 写打开的探测直接判定形状不认识（错误类型只有分类），逐项原因落在日志里。
        if version == 10 {
            assert!(message.contains("extension_value"), "{message}");
        }
        let mut connection = SqliteConnection::connect_with(
            &SqliteConnectOptions::new()
                .filename(directory.path().join("threads.db"))
                .foreign_keys(true),
        )
        .await
        .unwrap();
        let (stored,): (i64,) = sqlx::query_as("PRAGMA user_version")
            .fetch_one(&mut connection)
            .await
            .unwrap();
        assert_eq!(stored, version);
        assert_eq!(before, history(&mut connection).await);
        let (kept,): (String,) =
            sqlx::query_as("SELECT extension_value FROM threads WHERE id = 'older'")
                .fetch_one(&mut connection)
                .await
                .unwrap();
        assert_eq!(kept, "thread extension");
    }
}

#[tokio::test]
async fn environment_backfill_failure_rolls_back_schema_and_version() {
    let (directory, mut connection) = old_database().await;
    sqlx::query("DROP TABLE session_environments")
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query("CREATE TABLE session_environments (thread_id TEXT PRIMARY KEY)")
        .execute(&mut connection)
        .await
        .unwrap();
    connection.close().await.unwrap();

    let path = directory.path().join("threads.db");
    assert!(SqliteThreadStore::new(&path).await.is_err());
    let mut connection = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(&path)
            .foreign_keys(true),
    )
    .await
    .unwrap();
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(version, 10);
    let (cached_column,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM pragma_table_info('threads') WHERE name = 'cached_context'",
    )
    .fetch_one(&mut connection)
    .await
    .unwrap();
    assert_eq!(cached_column, 1);
    let (goals_table,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM sqlite_master WHERE name = 'thread_goals'")
            .fetch_one(&mut connection)
            .await
            .unwrap();
    assert_eq!(goals_table, 1);
}

#[tokio::test]
async fn schema_v11_upgrade_removes_only_retired_state_and_keeps_config_and_history() {
    let (directory, mut connection) = old_database().await;
    let before = history(&mut connection).await;
    let path = directory.path().join("threads.db");
    connection.close().await.unwrap();
    let store = SqliteThreadStore::new(&path).await.unwrap();
    let mut connection = store.database.pool.acquire().await.unwrap();
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&mut *connection)
        .await
        .unwrap();
    assert_eq!(version, super::schema::CURRENT_SCHEMA_VERSION);
    let (retired,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM sqlite_master WHERE name IN ('thread_goals', 'execution_runs')",
    )
    .fetch_one(&mut *connection)
    .await
    .unwrap();
    assert_eq!(retired, 1);
    let (columns,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM pragma_table_xinfo('threads') WHERE name IN ('cached_context', 'context_cache_epoch')").fetch_one(&mut *connection).await.unwrap();
    assert_eq!(columns, 0);
    assert_eq!(history(&mut connection).await, before);
    let (extension,): (String,) = sqlx::query_as("SELECT value FROM extension_state")
        .fetch_one(&mut *connection)
        .await
        .unwrap();
    assert_eq!(extension, "must survive");
    drop(connection);
    let mut meta = store.load_meta(&"older".into()).await.unwrap();
    assert_eq!(meta.config.as_deref(), Some(r#"{"model":"keep"}"#));
    meta.config = Some(r#"{"model":"changed"}"#.into());
    store.update_meta(&"older".into(), meta).await.unwrap();
    let reopened = SqliteThreadStore::new(&path).await.unwrap();
    assert_eq!(
        reopened
            .load_meta(&"older".into())
            .await
            .unwrap()
            .config
            .as_deref(),
        Some(r#"{"model":"changed"}"#)
    );
    assert_eq!(
        reopened
            .load_message_flags(&"older".into())
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn schema_v11_reading_context_preserves_timestamp_and_recent_order() {
    let (directory, connection) = old_database().await;
    let path = directory.path().join("threads.db");
    connection.close().await.unwrap();
    let store = SqliteThreadStore::new(&path).await.unwrap();
    let before = store.load_meta(&"older".into()).await.unwrap().updated_at;
    let order = store
        .list_thread_entries("/tmp")
        .await
        .unwrap()
        .into_iter()
        .map(|entry| entry.id)
        .collect::<Vec<_>>();
    for _ in 0..2 {
        let context = store.load_context(&"older".into()).await.unwrap();
        assert_eq!(context.len(), 1);
        assert_eq!(context[0].content(), "older");
    }
    assert_eq!(
        store.load_meta(&"older".into()).await.unwrap().updated_at,
        before
    );
    assert_eq!(
        store
            .list_thread_entries("/tmp")
            .await
            .unwrap()
            .into_iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        order
    );
    store
        .update_title(&"older".into(), "renamed")
        .await
        .unwrap();
    assert!(store.load_meta(&"older".into()).await.unwrap().updated_at > before);
    assert_eq!(
        store.list_thread_entries("/tmp").await.unwrap()[0].id,
        "older"
    );
}

#[tokio::test]
async fn schema_v11_readonly_old_and_new_databases_never_migrate_or_write_on_read() {
    let (directory, mut connection) = old_database().await;
    let path = directory.path().join("threads.db");
    let before = history(&mut connection).await;
    connection.close().await.unwrap();
    let reader = SqliteThreadStore::open_existing_read_only(&path)
        .await
        .unwrap();
    assert_eq!(
        reader.load_context(&"older".into()).await.unwrap()[0].content(),
        "older"
    );
    let mut connection = reader.database.pool.acquire().await.unwrap();
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&mut *connection)
        .await
        .unwrap();
    assert_eq!(version, 10);
    let (goals,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM thread_goals")
        .fetch_one(&mut *connection)
        .await
        .unwrap();
    assert_eq!(goals, 1);
    assert_eq!(history(&mut connection).await, before);
    drop(connection);
    drop(reader);
    let writer = SqliteThreadStore::new(&path).await.unwrap();
    drop(writer);
    let reader = SqliteThreadStore::open_existing_read_only(&path)
        .await
        .unwrap();
    assert_eq!(
        reader
            .load_meta(&"older".into())
            .await
            .unwrap()
            .config
            .as_deref(),
        Some(r#"{"model":"keep"}"#)
    );
    assert_eq!(
        reader.load_context(&"older".into()).await.unwrap()[0].content(),
        "older"
    );
}

#[tokio::test]
async fn schema_v11_unknown_retired_state_and_external_dependencies_refuse_without_data_loss() {
    let cases = [
        "ALTER TABLE execution_runs ADD COLUMN extension_data TEXT",
        "DROP TABLE execution_runs; CREATE TABLE execution_runs (thread_id TEXT PRIMARY KEY, generation INTEGER NOT NULL, clean BOOLEAN NOT NULL CHECK(clean = 1))",
        "DROP TABLE execution_runs; CREATE VIEW execution_runs AS SELECT id AS thread_id FROM threads",
        "CREATE TABLE extension_execution (id TEXT REFERENCES execution_runs(thread_id) ON DELETE CASCADE); INSERT INTO extension_execution VALUES ('older')",
        "CREATE TABLE sqlitex_execution (id TEXT REFERENCES 'execution_runs'(thread_id))",
        "CREATE VIEW execution_view AS SELECT * FROM execution_runs",
        "CREATE TRIGGER execution_trigger AFTER UPDATE ON execution_runs BEGIN UPDATE threads SET title='extension' WHERE id=NEW.thread_id; END",
        "CREATE TRIGGER thread_execution_trigger AFTER UPDATE ON threads BEGIN DELETE FROM execution_runs WHERE thread_id=NEW.id; END",
        "CREATE VIEW extension_cache AS SELECT cached_context FROM threads",
        "CREATE VIEW extension_threads AS SELECT * FROM 'threads'",
        "ALTER TABLE threads RENAME COLUMN cached_context TO retained_cache; ALTER TABLE threads ADD COLUMN cached_context INTEGER; UPDATE threads SET cached_context=retained_cache",
        "CREATE VIEW extension_all AS SELECT * FROM threads",
    ];
    for setup in cases {
        let (directory, mut connection) = old_database().await;
        sqlx::raw_sql(setup).execute(&mut connection).await.unwrap();
        let before: Vec<(String, String, Option<String>)> =
            sqlx::query_as("SELECT type, name, sql FROM sqlite_master ORDER BY type, name")
                .fetch_all(&mut connection)
                .await
                .unwrap();
        connection.close().await.unwrap();
        let error = match SqliteThreadStore::new(directory.path().join("threads.db")).await {
            Ok(_) => panic!("unexpected upgrade: {setup}"),
            Err(error) => error,
        };
        assert!(matches!(
            error.downcast_ref::<WorkspaceError>(),
            Some(WorkspaceError::UnsupportedDatabaseSchema)
        ));
        let mut connection = SqliteConnection::connect_with(
            &SqliteConnectOptions::new()
                .filename(directory.path().join("threads.db"))
                .read_only(true),
        )
        .await
        .unwrap();
        let after: Vec<(String, String, Option<String>)> =
            sqlx::query_as("SELECT type, name, sql FROM sqlite_master ORDER BY type, name")
                .fetch_all(&mut connection)
                .await
                .unwrap();
        assert_eq!(after, before);
        let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
            .fetch_one(&mut connection)
            .await
            .unwrap();
        assert_eq!(version, 10);
        let (execution,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM sqlite_schema WHERE name = 'execution_runs'")
                .fetch_one(&mut connection)
                .await
                .unwrap();
        assert_eq!(execution, 1);
        let (goals,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM thread_goals")
            .fetch_one(&mut connection)
            .await
            .unwrap();
        assert_eq!(goals, 1);
        let (config, cache): (String, String) =
            sqlx::query_as("SELECT config, cached_context FROM threads WHERE id = 'older'")
                .fetch_one(&mut connection)
                .await
                .unwrap();
        assert_eq!(config, r#"{"model":"keep"}"#);
        assert_eq!(cache, "obsolete cache");
    }
}

#[tokio::test]
async fn schema_v11_failed_column_drop_rolls_back_the_prior_execution_drop() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let mut connection = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true),
    )
    .await
    .unwrap();
    let schema = include_str!("fixtures/legacy_with_goals.sql").replace(
        "config TEXT",
        "config TEXT CHECK(cached_context IS NULL OR config IS NOT NULL)",
    );
    sqlx::raw_sql(AssertSqlSafe(schema))
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query(LEGACY_EXECUTION_SQL)
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query("INSERT INTO execution_runs VALUES ('session', 17, 0)")
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::raw_sql("PRAGMA user_version = 10; INSERT INTO threads (id, cwd, created_at, updated_at) VALUES ('session', '/tmp', '2020-01-01T00:00:00Z', '2020-01-01T00:00:00Z'); INSERT INTO thread_goals VALUES ('session', 'goal', 'keep on failure', 'active', NULL, 0, 0, 1, 1)").execute(&mut connection).await.unwrap();
    connection.close().await.unwrap();
    assert!(SqliteThreadStore::new(&path).await.is_err());
    let mut connection = SqliteConnection::connect_with(
        &SqliteConnectOptions::new().filename(&path).read_only(true),
    )
    .await
    .unwrap();
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(version, 10);
    let (objective,): (String,) = sqlx::query_as("SELECT objective FROM thread_goals")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(objective, "keep on failure");
    let execution: (String, i64, bool) =
        sqlx::query_as("SELECT thread_id, generation, clean FROM execution_runs")
            .fetch_one(&mut connection)
            .await
            .unwrap();
    assert_eq!(execution, ("session".to_owned(), 17, false));
    let (columns,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM pragma_table_xinfo('threads') WHERE name IN ('cached_context', 'context_cache_epoch')").fetch_one(&mut connection).await.unwrap();
    assert_eq!(columns, 2);
}

#[tokio::test]
async fn schema_v11_cleans_prior_development_eleven_and_reopens_without_runtime_tables() {
    for definition in [LEGACY_EXECUTION_SQL, LEGACY_BOUND_EXECUTION_SQL] {
        let (directory, mut connection) = old_database().await;
        sqlx::raw_sql(
            "DROP TABLE execution_runs; DROP TABLE thread_goals;
            ALTER TABLE threads DROP COLUMN cached_context;
            ALTER TABLE threads DROP COLUMN context_cache_epoch;
            PRAGMA user_version = 11",
        )
        .execute(&mut connection)
        .await
        .unwrap();
        sqlx::query(definition)
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::raw_sql(
            "INSERT INTO execution_runs VALUES ('older', 7, 0);
            CREATE INDEX idx_execution_generation ON execution_runs(generation)",
        )
        .execute(&mut connection)
        .await
        .unwrap();
        let before = history(&mut connection).await;
        let meta: (String, String, String) =
            sqlx::query_as("SELECT config, created_at, updated_at FROM threads WHERE id='older'")
                .fetch_one(&mut connection)
                .await
                .unwrap();
        connection.close().await.unwrap();
        let path = directory.path().join("threads.db");
        for _ in 0..2 {
            let store = SqliteThreadStore::new(&path).await.unwrap();
            let mut connection = store.database.pool.acquire().await.unwrap();
            assert_eq!(history(&mut connection).await, before);
            let after: (String, String, String) = sqlx::query_as(
                "SELECT config, created_at, updated_at FROM threads WHERE id='older'",
            )
            .fetch_one(&mut *connection)
            .await
            .unwrap();
            assert_eq!(after, meta);
            let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
                .fetch_one(&mut *connection)
                .await
                .unwrap();
            assert_eq!(version, super::schema::CURRENT_SCHEMA_VERSION);
            let (retired,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM sqlite_schema WHERE name IN ('execution_runs', 'idx_execution_generation')")
                .fetch_one(&mut *connection).await.unwrap();
            assert_eq!(retired, 0);
            drop(connection);
            let (retired,): (i64,) =
                sqlx::query_as("SELECT COUNT(*) FROM sqlite_schema WHERE name = 'execution_runs'")
                    .fetch_one(&store.database.pool)
                    .await
                    .unwrap();
            assert_eq!(retired, 0);
            store.close().await;
        }
    }
}

#[tokio::test]
async fn schema_v11_same_version_unknown_execution_table_is_preserved() {
    let (directory, mut connection) = old_database().await;
    sqlx::raw_sql(
        "PRAGMA user_version = 11;
        ALTER TABLE execution_runs ADD COLUMN extension_data TEXT;
        UPDATE execution_runs SET extension_data='must survive'",
    )
    .execute(&mut connection)
    .await
    .unwrap();
    connection.close().await.unwrap();
    let path = directory.path().join("threads.db");
    assert!(SqliteThreadStore::new(&path).await.is_err());
    let mut connection = SqliteConnection::connect_with(
        &SqliteConnectOptions::new().filename(&path).read_only(true),
    )
    .await
    .unwrap();
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(version, 11);
    let (value,): (String,) =
        sqlx::query_as("SELECT extension_data FROM execution_runs WHERE thread_id='older'")
            .fetch_one(&mut connection)
            .await
            .unwrap();
    assert_eq!(value, "must survive");
    let (cache,): (String,) = sqlx::query_as("SELECT cached_context FROM threads WHERE id='older'")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(cache, "obsolete cache");
}
