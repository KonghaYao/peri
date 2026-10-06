use super::*;
use peri_acp_types::messages::BaseMessage;
use sqlx::sqlite::SqlitePoolOptions;

#[tokio::test]
async fn canonical_messages_round_trip_references_and_reject_identity_mismatch() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    for statement in crate::sessions::work_store::schema::initialization_sql() {
        sqlx::query(statement)
            .execute(&mut *transaction)
            .await
            .unwrap();
    }
    sqlx::query("CREATE TABLE messages(message_id TEXT PRIMARY KEY,thread_id TEXT NOT NULL,role TEXT NOT NULL,content_ref TEXT NOT NULL,transcript_seq INTEGER NOT NULL,UNIQUE(thread_id,transcript_seq))")
        .execute(&mut *transaction).await.unwrap();
    let values = [
        PersistedPayload::Message(BaseMessage::human("输入 中文 🦀")),
        PersistedPayload::Message(BaseMessage::human("second input")),
    ];
    for value in &values {
        insert_payload(&mut transaction, "fixture", value)
            .await
            .unwrap();
    }
    let rows: Vec<(String, String, String)> = sqlx::query_as("SELECT message_id,role,content_ref FROM messages WHERE thread_id='fixture' ORDER BY transcript_seq")
        .fetch_all(&mut *transaction).await.unwrap();
    let decoded = decode_rows(&mut transaction, rows.clone()).await.unwrap();
    let encoded = |payloads: &[PersistedPayload]| {
        payloads
            .iter()
            .map(|value| peri_acp_types::store::serialize_persisted_payload(value).unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(encoded(&decoded), encoded(&values));
    assert!(
        decode_payload(&mut transaction, "wrong-id", &rows[0].1, &rows[0].2)
            .await
            .is_err()
    );
    assert!(
        decode_payload(&mut transaction, &rows[0].0, "assistant", &rows[0].2)
            .await
            .is_err()
    );
    let references: Vec<i64> =
        sqlx::query_scalar("SELECT transcript_seq FROM messages ORDER BY transcript_seq")
            .fetch_all(&mut *transaction)
            .await
            .unwrap();
    assert_eq!(references, [0, 1]);
    transaction.rollback().await.unwrap();
}
