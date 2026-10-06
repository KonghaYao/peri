#![cfg(not(target_os = "emscripten"))]

use peri_acp_types::messages::BaseMessage;
use peri_acp_types::store::{serialize_persisted_payload, PersistedPayload};
use peri_resources::sessions::{
    migrate_stopped_work_store, SqliteThreadStore, StoppedWriterApproval,
};
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{Connection, SqliteConnection};

#[tokio::test]
async fn new_schema17_opens_with_reference_messages_only() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("new.db");
    let store = SqliteThreadStore::new(&path).await.unwrap();
    store.close().await;
    let mut connection = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(false),
    )
    .await
    .unwrap();
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(version, 18);
    let columns: Vec<String> = sqlx::query_scalar("SELECT name FROM pragma_table_info('messages')")
        .fetch_all(&mut connection)
        .await
        .unwrap();
    assert!(columns.iter().any(|column| column == "content_ref"));
    assert!(columns.iter().any(|column| column == "transcript_seq"));
    assert!(!columns.iter().any(|column| column == "content"));
    let old_table: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='session_work_state')",
    )
    .fetch_one(&mut connection)
    .await
    .unwrap();
    assert!(!old_table);
    connection.close().await.unwrap();
}

async fn old_fixture(path: &std::path::Path, valid_message: bool) -> String {
    let mut connection = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true),
    )
    .await
    .unwrap();
    for statement in [
        "PRAGMA user_version=17",
        "CREATE TABLE threads(id TEXT PRIMARY KEY)",
        "CREATE TABLE messages(message_id TEXT PRIMARY KEY,thread_id TEXT NOT NULL,role TEXT NOT NULL,content TEXT NOT NULL,truncated BOOLEAN NOT NULL DEFAULT 0,excluded BOOLEAN NOT NULL DEFAULT 0,projection TEXT)",
        "CREATE TABLE session_control_state(session_id TEXT PRIMARY KEY,state_json TEXT NOT NULL)",
        "CREATE TABLE session_control_receipts(command_id TEXT PRIMARY KEY,session_id TEXT NOT NULL,digest TEXT NOT NULL,resolution_json TEXT NOT NULL)",
        "CREATE TABLE session_work_state(session_id TEXT PRIMARY KEY,state_json TEXT NOT NULL)",
        "CREATE TABLE session_work_commands(mutation_id TEXT PRIMARY KEY,session_id TEXT NOT NULL,digest TEXT NOT NULL,command_json TEXT NOT NULL,reconciled INTEGER NOT NULL DEFAULT 0)",
        "CREATE TABLE session_work_receipts(mutation_id TEXT PRIMARY KEY,session_id TEXT NOT NULL,digest TEXT NOT NULL,resolution_json TEXT NOT NULL)",
        "CREATE TABLE session_work_events(event_key TEXT PRIMARY KEY,event_json TEXT NOT NULL)",
        "INSERT INTO threads VALUES ('legacy-session')",
        "INSERT INTO session_work_state VALUES ('legacy-session','{}')",
        "INSERT INTO session_work_commands VALUES ('original','legacy-session','original-digest','original unknown command bytes',0)",
        "INSERT INTO session_work_events VALUES ('orphan-event','original orphan event bytes')",
    ] {
        sqlx::query(statement).execute(&mut connection).await.unwrap();
    }
    let payload = PersistedPayload::Message(BaseMessage::human("原始正文 🦀"));
    let content = if valid_message {
        serialize_persisted_payload(&payload).unwrap()
    } else {
        "invalid canonical JSON".into()
    };
    sqlx::query("INSERT INTO messages(message_id,thread_id,role,content,truncated) VALUES (?1,'legacy-session','user',?2,1)")
        .bind(payload.id().as_uuid().to_string()).bind(&content).execute(&mut connection).await.unwrap();
    connection.close().await.unwrap();
    content
}

fn approval() -> StoppedWriterApproval {
    StoppedWriterApproval {
        source_version: 17,
        writers_stopped: true,
        backup_verified: true,
    }
}

#[tokio::test]
async fn stopped_migration_preserves_raw_unknown_evidence_and_canonical_message() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("legacy.db");
    let content = old_fixture(&path, true).await;
    std::fs::copy(&path, directory.path().join("verified-backup.db")).unwrap();
    let report = migrate_stopped_work_store(&path, &approval())
        .await
        .unwrap();
    assert_eq!(report.messages, 1);
    assert_eq!(report.sessions, 1);
    assert_eq!(report.legacy_unknown_sessions, 1);
    assert_eq!(report.retained_evidence, 3);
    let mut connection = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(false),
    )
    .await
    .unwrap();
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(version, 18);
    let (reference, truncated): (String, bool) =
        sqlx::query_as("SELECT content_ref,truncated FROM messages")
            .fetch_one(&mut connection)
            .await
            .unwrap();
    assert!(truncated);
    let bytes: Vec<u8> = sqlx::query_scalar("SELECT bytes FROM session_payloads WHERE storage_scope=json_extract(?1,'$.storageScope') AND payload_id=json_extract(?1,'$.payloadId')").bind(reference).fetch_one(&mut connection).await.unwrap();
    assert_eq!(bytes, content.as_bytes());
    let retained: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM session_payloads WHERE kind='legacyOriginal'")
            .fetch_one(&mut connection)
            .await
            .unwrap();
    assert_eq!(retained, 3);
    connection.close().await.unwrap();
}

#[tokio::test]
async fn failed_stopped_migration_rolls_back_schema_and_all_original_rows() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("invalid.db");
    let content = old_fixture(&path, false).await;
    assert!(migrate_stopped_work_store(&path, &approval())
        .await
        .is_err());
    let mut connection = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(false),
    )
    .await
    .unwrap();
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(version, 17);
    let saved: String = sqlx::query_scalar("SELECT content FROM messages")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(saved, content);
    let old: String = sqlx::query_scalar("SELECT state_json FROM session_work_state")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(old, "{}");
    let payload_table: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='session_payloads')",
    )
    .fetch_one(&mut connection)
    .await
    .unwrap();
    assert!(!payload_table);
    connection.close().await.unwrap();
}
