use super::*;
use crate::sessions::canonical::CREATE_TABLES;
use crate::sessions::schema_cleanup::{
    LEGACY_BOUND_EXECUTION_SQL, LEGACY_EXECUTION_SQL, LEGACY_GOALS_SQL,
};
use peri_acp_types::messages::BaseMessage;
use peri_acp_types::store::{serialize_persisted_payload, PersistedPayload};
use sqlx::{Connection, SqliteConnection};

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
    for statement in CREATE_TABLES {
        sqlx::query(
            if statement.contains("CREATE TABLE IF NOT EXISTS messages") {
                LEGACY_MESSAGES_SQL
            } else {
                *statement
            },
        )
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
async fn schema_v11_refusal_preserves_retired_state_config_and_history() {
    let (directory, connection) = old_database().await;
    let path = directory.path().join("threads.db");
    connection.close().await.unwrap();
    assert_legacy_rejected(&path).await;
}

#[tokio::test]
async fn schema_v11_refusal_preserves_context_and_timestamps() {
    let (directory, connection) = old_database().await;
    let path = directory.path().join("threads.db");
    connection.close().await.unwrap();
    assert_legacy_rejected(&path).await;
}

#[tokio::test]
async fn schema_v11_readonly_inline_history_is_refused_without_writes() {
    let (directory, connection) = old_database().await;
    let path = directory.path().join("threads.db");
    connection.close().await.unwrap();
    assert_legacy_rejected(&path).await;
}

#[tokio::test]
async fn schema_v11_unknown_goals_and_external_dependencies_refuse_without_data_loss() {
    let cases = [
        "ALTER TABLE execution_runs ADD COLUMN extension_data TEXT",
        "DROP TABLE execution_runs; CREATE TABLE execution_runs (thread_id TEXT PRIMARY KEY, generation INTEGER NOT NULL, clean BOOLEAN NOT NULL CHECK(clean = 1))",
        "DROP TABLE execution_runs; CREATE VIEW execution_runs AS SELECT id AS thread_id FROM threads",
        "CREATE TABLE extension_execution (id TEXT REFERENCES execution_runs(thread_id) ON DELETE CASCADE); INSERT INTO extension_execution VALUES ('older')",
        "CREATE TABLE sqlitex_execution (id TEXT REFERENCES 'execution_runs'(thread_id))",
        "CREATE VIEW execution_view AS SELECT * FROM execution_runs",
        "CREATE TRIGGER execution_trigger AFTER UPDATE ON execution_runs BEGIN UPDATE threads SET title='extension' WHERE id=NEW.thread_id; END",
        "CREATE TRIGGER thread_execution_trigger AFTER UPDATE ON threads BEGIN DELETE FROM execution_runs WHERE thread_id=NEW.id; END",
        "ALTER TABLE thread_goals ADD COLUMN extension TEXT",
        "CREATE TABLE extension_goals (id TEXT PRIMARY KEY REFERENCES 'thread_goals'(thread_id) ON DELETE CASCADE); INSERT INTO extension_goals VALUES ('older')",
        "CREATE TABLE extension_goals (id TEXT PRIMARY KEY REFERENCES 'THREAD_GOALS'(thread_id) ON DELETE CASCADE); INSERT INTO extension_goals VALUES ('older')",
        "CREATE TABLE sqlitex_extension_goals (id TEXT PRIMARY KEY REFERENCES thread_goals(thread_id) ON DELETE CASCADE); INSERT INTO sqlitex_extension_goals VALUES ('older')",
        "CREATE VIEW extension_view AS SELECT objective FROM thread_goals",
        "CREATE TRIGGER extension_trigger AFTER UPDATE ON threads BEGIN DELETE FROM thread_goals WHERE thread_id = NEW.id; END",
        "CREATE VIEW extension_cache AS SELECT cached_context FROM threads",
        "CREATE VIEW extension_goals AS SELECT * FROM 'thread_goals'",
        "CREATE VIEW extension_threads AS SELECT * FROM 'threads'",
        "ALTER TABLE threads ADD COLUMN extension_goal TEXT REFERENCES thread_goals(thread_id) ON DELETE CASCADE; UPDATE threads SET extension_goal='older' WHERE id='older'",
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
        assert!(error
            .to_string()
            .contains("explicit stopped-writer offline migration"));
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
async fn schema_v11_failed_column_drop_rolls_back_the_prior_goal_drop() {
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
async fn schema_v11_refusal_preserves_prior_development_runtime_tables() {
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
        connection.close().await.unwrap();
        let path = directory.path().join("threads.db");
        for _ in 0..2 {
            assert_legacy_rejected(&path).await;
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

const LEGACY_MESSAGES_SQL: &str = r#"CREATE TABLE IF NOT EXISTS messages (
    message_id TEXT PRIMARY KEY, thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    role TEXT NOT NULL, content TEXT NOT NULL,
    truncated BOOLEAN NOT NULL DEFAULT 0, excluded BOOLEAN NOT NULL DEFAULT 0, projection TEXT
)"#;

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

#[tokio::test]
async fn reference_history_read_preserves_timestamps_recent_order_config_and_flags() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("reference.db");
    let store = SqliteThreadStore::new(&path).await.unwrap();
    let mut ids = Vec::new();
    for title in ["older", "newer"] {
        let id = store
            .create_thread(peri_acp_types::thread::ThreadMeta::new_at(
                "/tmp",
                peri_time::now_wall(),
            ))
            .await
            .unwrap();
        store
            .append_message(&id, BaseMessage::human(title))
            .await
            .unwrap();
        ids.push(id);
    }
    sqlx::query("UPDATE threads SET updated_at='2020-01-01T00:00:00Z',config='{\"model\":\"keep\"}' WHERE id=?1").bind(&ids[0]).execute(&store.database.pool).await.unwrap();
    sqlx::query("UPDATE threads SET updated_at='2020-02-01T00:00:00Z' WHERE id=?1")
        .bind(&ids[1])
        .execute(&store.database.pool)
        .await
        .unwrap();
    let before = store.load_meta(&ids[0]).await.unwrap().updated_at;
    let order = store
        .list_thread_entries("/tmp")
        .await
        .unwrap()
        .into_iter()
        .map(|entry| entry.id)
        .collect::<Vec<_>>();
    let flags = serde_json::to_value(store.load_message_flags(&ids[0]).await.unwrap()).unwrap();
    for _ in 0..2 {
        assert_eq!(
            store.load_context(&ids[0]).await.unwrap()[0].content(),
            "older"
        );
    }
    assert_eq!(store.load_meta(&ids[0]).await.unwrap().updated_at, before);
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
    assert_eq!(
        store.load_meta(&ids[0]).await.unwrap().config.as_deref(),
        Some(r#"{"model":"keep"}"#)
    );
    store.close().await;
    let reader = SqliteThreadStore::open_existing_read_only(&path)
        .await
        .unwrap();
    assert_eq!(
        reader.load_context(&ids[0]).await.unwrap()[0].content(),
        "older"
    );
    assert_eq!(reader.load_meta(&ids[0]).await.unwrap().updated_at, before);
    assert_eq!(
        serde_json::to_value(reader.load_message_flags(&ids[0]).await.unwrap()).unwrap(),
        flags
    );
    reader.close().await;
    let store = SqliteThreadStore::new(&path).await.unwrap();
    store.update_title(&ids[0], "renamed").await.unwrap();
    assert!(store.load_meta(&ids[0]).await.unwrap().updated_at > before);
    assert_eq!(
        store.list_thread_entries("/tmp").await.unwrap()[0].id,
        ids[0]
    );
    store.close().await;
}
