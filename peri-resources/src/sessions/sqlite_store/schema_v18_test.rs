use super::super::absolute_test_path;
use super::*;
use peri_acp_types::{messages::BaseMessage, store::ThreadStore, thread::ThreadMeta};

const RECOVERY_TABLES: &[&str] = &[
    "session_work_commands",
    "session_work_state",
    "session_work_events",
    "session_work_receipts",
    "session_control_state",
    "session_control_receipts",
];

async fn assert_no_recovery_tables(pool: &sqlx::SqlitePool) {
    for table in RECOVERY_TABLES {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_schema WHERE name = ?1")
            .bind(table)
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(count, 0, "{table}");
    }
}

#[tokio::test]
async fn schema_v19_new_store_has_eight_business_tables_and_normal_history() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let store = SqliteThreadStore::new(&path).await.unwrap();
    assert_no_recovery_tables(&store.database.pool).await;
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
    )
    .fetch_one(&store.database.pool)
    .await
    .unwrap();
    // v19 起执行登记并入 `workspaces`，新建库不再有登记表。
    assert_eq!(count, 8);
    let registrations: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name = 'legacy_execution_registrations'",
    )
    .fetch_one(&store.database.pool)
    .await
    .unwrap();
    assert_eq!(registrations, 0);
    let session = store
        .create_thread(ThreadMeta::new_at(
            absolute_test_path("tmp"),
            peri_time::now_wall(),
        ))
        .await
        .unwrap();
    store
        .append_message(&session, BaseMessage::human("first"))
        .await
        .unwrap();
    store.close().await;
    let reopened = SqliteThreadStore::new(&path).await.unwrap();
    assert_eq!(
        reopened.load_context(&session).await.unwrap()[0].content(),
        "first"
    );
    reopened
        .append_message(&session, BaseMessage::human("next"))
        .await
        .unwrap();
    let history = reopened.load_context(&session).await.unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[1].content(), "next");
}

#[tokio::test]
async fn schema_v18_from_v17_preserves_business_pages_extensions_goals_and_rowid() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let store = SqliteThreadStore::new(&path).await.unwrap();
    let session = store
        .create_thread(ThreadMeta::new_at(
            absolute_test_path("tmp"),
            peri_time::now_wall(),
        ))
        .await
        .unwrap();
    store
        .append_message(&session, BaseMessage::human("history"))
        .await
        .unwrap();
    for (_, definition) in crate::sessions::schema_cleanup::RETIRED_EXECUTION_TABLES {
        sqlx::query(*definition)
            .execute(&store.database.pool)
            .await
            .unwrap();
    }
    sqlx::raw_sql(
        // v17 的形状里还有执行登记表（v12 起由旧 `workspaces` 改名而来，v19 删除）；
        // 夹具从新建的库里出发，必须把它补回来，否则「声明为 17 的库」并不具备 v17 形状。
        "CREATE TABLE legacy_execution_registrations (
            id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id),
            root TEXT NOT NULL, root_identity TEXT NOT NULL, discovery TEXT NOT NULL,
            UNIQUE(root, root_identity), UNIQUE(id, project_id));
         CREATE TABLE thread_goals (thread_id TEXT, objective TEXT, extension TEXT);
         INSERT INTO thread_goals VALUES ('session', 'goal', 'keep');
         CREATE TABLE extension_data (id TEXT REFERENCES threads(id), payload TEXT);
         CREATE INDEX extension_message_index ON messages(role);
         CREATE TRIGGER extension_message_trigger AFTER INSERT ON messages
         BEGIN INSERT INTO extension_data VALUES (NEW.thread_id, 'appended'); END;
         UPDATE messages SET rowid = 71;
         PRAGMA user_version = 17;",
    )
    .execute(&store.database.pool)
    .await
    .unwrap();
    let before: Vec<(String, i64, Option<String>)> = sqlx::query_as(
        "SELECT name, rootpage, sql FROM sqlite_schema WHERE tbl_name NOT LIKE 'session_work_%'
         AND tbl_name NOT LIKE 'session_control_%'
         AND tbl_name NOT IN ('legacy_execution_registrations', 'session_bindings')
         ORDER BY name",
    )
    .fetch_all(&store.database.pool)
    .await
    .unwrap();
    store.close().await;
    let reopened = SqliteThreadStore::new(&path).await.unwrap();
    assert_no_recovery_tables(&reopened.database.pool).await;
    // v19 有意动两张表：登记表整体删除，`session_bindings` 重建以去掉指向它的外键
    // （`workspace_id` 同时收敛到归属行）。其余对象的页面与 SQL 必须原样保留。
    let after: Vec<(String, i64, Option<String>)> = sqlx::query_as(
        "SELECT name, rootpage, sql FROM sqlite_schema WHERE tbl_name NOT IN ('legacy_execution_registrations', 'session_bindings')
         ORDER BY name",
    )
    .fetch_all(&reopened.database.pool)
    .await
    .unwrap();
    assert_eq!(before, after);
    let registrations: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_schema WHERE tbl_name = 'legacy_execution_registrations'",
    )
    .fetch_one(&reopened.database.pool)
    .await
    .unwrap();
    assert_eq!(registrations, 0);
    let (bindings,): (String,) = sqlx::query_as(
        "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = 'session_bindings'",
    )
    .fetch_one(&reopened.database.pool)
    .await
    .unwrap();
    assert!(
        !bindings.contains("legacy_execution_registrations"),
        "{bindings}"
    );
    let rowid: i64 = sqlx::query_scalar("SELECT rowid FROM messages")
        .fetch_one(&reopened.database.pool)
        .await
        .unwrap();
    assert_eq!(rowid, 71);
    let goal: (String, String) = sqlx::query_as("SELECT objective, extension FROM thread_goals")
        .fetch_one(&reopened.database.pool)
        .await
        .unwrap();
    assert_eq!(goal, ("goal".into(), "keep".into()));
    reopened
        .append_message(&session, BaseMessage::human("continued"))
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM extension_data")
        .fetch_one(&reopened.database.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&reopened.database.pool)
        .await
        .unwrap();
    assert_eq!(version, CURRENT_SCHEMA_VERSION);
}

#[tokio::test]
async fn schema_v18_refuses_non_table_without_changing_six_objects_or_version() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let store = SqliteThreadStore::new(&path).await.unwrap();
    for (table, definition) in crate::sessions::schema_cleanup::RETIRED_EXECUTION_TABLES {
        if *table == "session_control_receipts" {
            continue;
        }
        sqlx::query(*definition)
            .execute(&store.database.pool)
            .await
            .unwrap();
    }
    sqlx::raw_sql("CREATE VIEW session_control_receipts AS SELECT 1; PRAGMA user_version = 17")
        .execute(&store.database.pool)
        .await
        .unwrap();
    store.close().await;
    assert!(SqliteThreadStore::new(&path).await.is_err());
    let mut connection = SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&path)
            .read_only(true),
    )
    .await
    .unwrap();
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(version, 17);
    for table in RECOVERY_TABLES {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_schema WHERE name = ?1")
            .bind(table)
            .fetch_one(&mut connection)
            .await
            .unwrap();
        assert_eq!(count, 1);
    }
}

#[tokio::test]
async fn schema_v18_refuses_unknown_shapes_and_external_references_without_data_loss() {
    for setup in [
        "ALTER TABLE session_work_state ADD COLUMN extension TEXT",
        "CREATE VIEW extension_view AS SELECT * FROM session_work_state",
        "CREATE TABLE extension_fk (id TEXT REFERENCES session_work_state(session_id))",
        "CREATE TRIGGER extension_trigger AFTER UPDATE ON threads BEGIN DELETE FROM session_work_state; END",
        "CREATE TRIGGER attached_trigger AFTER INSERT ON session_work_state BEGIN SELECT 1; END",
        "CREATE INDEX extension_index ON session_work_state(state_json)",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("threads.db");
        let store = SqliteThreadStore::new(&path).await.unwrap();
        for (_, definition) in crate::sessions::schema_cleanup::RETIRED_EXECUTION_TABLES {
            sqlx::query(*definition).execute(&store.database.pool).await.unwrap();
        }
        sqlx::query("INSERT INTO session_work_state VALUES ('session', 'keep')")
            .execute(&store.database.pool).await.unwrap();
        sqlx::raw_sql(sqlx::AssertSqlSafe(setup.to_owned()))
            .execute(&store.database.pool).await.unwrap();
        sqlx::query("PRAGMA user_version = 17").execute(&store.database.pool).await.unwrap();
        let before: Vec<(String, String, Option<String>)> = sqlx::query_as(
            "SELECT type, name, sql FROM sqlite_schema ORDER BY name",
        ).fetch_all(&store.database.pool).await.unwrap();
        store.close().await;
        assert!(SqliteThreadStore::new(&path).await.is_err(), "{setup}");
        let mut connection = SqliteConnection::connect_with(
            &sqlx::sqlite::SqliteConnectOptions::new().filename(&path).read_only(true),
        ).await.unwrap();
        let after: Vec<(String, String, Option<String>)> = sqlx::query_as(
            "SELECT type, name, sql FROM sqlite_schema ORDER BY name",
        ).fetch_all(&mut connection).await.unwrap();
        assert_eq!(before, after);
        let retained: String = sqlx::query_scalar("SELECT state_json FROM session_work_state")
            .fetch_one(&mut connection).await.unwrap();
        assert_eq!(retained, "keep");
        let version: i64 = sqlx::query_scalar("PRAGMA user_version")
            .fetch_one(&mut connection).await.unwrap();
        assert_eq!(version, 17);
    }
}
