use super::*;
use sqlx::sqlite::SqlitePoolOptions;

#[tokio::test]
async fn failed_guard_rolls_back_response_intent_and_receipt() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query("CREATE TABLE evidence (kind TEXT PRIMARY KEY, revision INTEGER NOT NULL)")
        .execute(&pool)
        .await
        .unwrap();
    let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    for kind in ["response", "intent", "receipt"] {
        execute_statement(
            &mut transaction,
            "INSERT INTO evidence VALUES (?1, 1)",
            &[SqlParam::Text(kind.into())],
            Some(1),
        )
        .await
        .unwrap();
    }
    let failure = execute_statement(
        &mut transaction,
        "UPDATE evidence SET revision=2 WHERE kind=?1 AND revision=?2",
        &[SqlParam::Text("response".into()), SqlParam::Integer(7)],
        Some(1),
    )
    .await
    .unwrap_err();
    let failure = rollback(transaction, failure, "fixture").await;
    assert!(!failure.is_persistence_uncertain());
    let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM evidence")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(remaining, 0);
}

#[tokio::test]
async fn bounded_read_preserves_order_and_decodes_parameter_types() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let mut connection = pool.acquire().await.unwrap();
    let records = read_rows(
        &mut connection,
        "SELECT json_object('value', ?1, 'optional', ?2) LIMIT ?3",
        &[
            SqlParam::Text("payload-reference".into()),
            SqlParam::Null,
            SqlParam::Integer(1),
        ],
    )
    .await
    .unwrap();
    assert_eq!(records.len(), 1);
    let SqlParam::Text(json) = &records[0][0] else {
        panic!("expected JSON text");
    };
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(json).unwrap(),
        serde_json::json!({ "value": "payload-reference", "optional": null })
    );
}
