use super::*;
use peri_acp_types::session_resources::work::{WorkAction, WorkDecision, WorkPage, WorkSelector};
use sqlx::sqlite::SqlitePoolOptions;

async fn fixture() -> (SqliteSessionData, WorkCommand) {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query("CREATE TABLE threads(id TEXT PRIMARY KEY)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE session_control_state(session_id TEXT PRIMARY KEY,state_json TEXT NOT NULL)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO threads VALUES ('fixture')")
        .execute(&pool)
        .await
        .unwrap();
    for statement in work_store::schema::initialization_sql() {
        sqlx::query(statement).execute(&pool).await.unwrap();
    }
    let data = SqliteSessionData::new(Arc::new(SqliteSessionDatabase::new(
        pool,
        false,
        std::path::PathBuf::from("memory-fixture"),
    )));
    let evidence = EvidenceWrite {
        session_id: "fixture".into(),
        storage_scope: "fixture".into(),
        payload_id: "original-input".into(),
        encoding: 1,
        bytes: b"original input".to_vec(),
    };
    let content = data.store_evidence(&evidence).await.unwrap();
    let command = WorkCommand {
        session_id: "fixture".into(),
        recipient_lifecycle: 1,
        mutation_id: "stage-original".into(),
        action: WorkAction::StageUserInput {
            input_id: "input".into(),
            content,
            command_id: "user-command".into(),
            fingerprint: 9,
        },
    };
    (data, command)
}

#[tokio::test]
async fn new_database_opens_schema17_without_aggregate_work_table() {
    let directory = tempfile::tempdir().unwrap();
    let database = SqliteSessionDatabase::open(directory.path().join("schema17.db"))
        .await
        .unwrap();
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&database.pool)
        .await
        .unwrap();
    assert_eq!(version, 17);
    let old_table: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='session_work_state')",
    )
    .fetch_one(&database.pool)
    .await
    .unwrap();
    assert!(!old_table);
    let columns: Vec<String> = sqlx::query_scalar("SELECT name FROM pragma_table_info('messages')")
        .fetch_all(&database.pool)
        .await
        .unwrap();
    assert!(columns.iter().any(|column| column == "content_ref"));
    assert!(columns.iter().any(|column| column == "transcript_seq"));
    assert!(!columns.iter().any(|column| column == "content"));
    database.close().await;
}

#[tokio::test]
async fn original_mutation_replays_one_draft_and_receipt() {
    let (data, command) = fixture().await;
    let first = data.write_work(&command).await.unwrap();
    assert_eq!(first.decision, WorkDecision::Accepted);
    assert_eq!(data.write_work(&command).await.unwrap(), first);
    let inspection = data
        .read_work(&WorkQuery::new("fixture", WorkSelector::Drafts))
        .await
        .unwrap();
    let WorkPage::Drafts(drafts) = inspection.page else {
        panic!("expected draft page");
    };
    assert_eq!(drafts.len(), 1);
    let pending: bool = sqlx::query_scalar(journal::HAS_PENDING)
        .bind("fixture")
        .fetch_one(&data.database.pool)
        .await
        .unwrap();
    assert!(!pending);
}

#[tokio::test]
async fn sealed_original_cannot_later_apply_and_identity_cannot_change() {
    let (data, command) = fixture().await;
    assert_eq!(
        data.resolve_work(&command).await.unwrap(),
        WorkResolution::NotApplied
    );
    assert!(data.write_work(&command).await.is_err());
    let mut changed = command.clone();
    changed.action = WorkAction::WithdrawStagedUserInput {
        input_id: "input".into(),
        command_id: "user-command".into(),
        fingerprint: 9,
    };
    assert!(data.resolve_work(&changed).await.is_err());
    let inspection = data
        .read_work(&WorkQuery::new("fixture", WorkSelector::Drafts))
        .await
        .unwrap();
    let WorkPage::Drafts(drafts) = inspection.page else {
        panic!("expected draft page");
    };
    assert!(drafts.is_empty());
}

#[tokio::test]
async fn unknown_original_blocks_replacement_and_history_until_sealed() {
    let (data, command) = fixture().await;
    let mut transaction = data
        .database
        .pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .unwrap();
    assert_eq!(
        record_original(&mut transaction, &command).await.unwrap(),
        None
    );
    transaction.commit().await.unwrap();
    assert!(data
        .write_work(&command)
        .await
        .unwrap_err()
        .is_persistence_uncertain());
    let mut replacement = command.clone();
    replacement.mutation_id = "replacement-must-not-exist".into();
    assert!(data.write_work(&replacement).await.is_err());
    let inspection = data
        .read_work(&WorkQuery::new("fixture", WorkSelector::Availability))
        .await
        .unwrap();
    let WorkPage::Availability(availability) = inspection.page else {
        panic!("expected availability");
    };
    assert!(availability.blocked);
    assert!(availability.candidates.is_empty());
    let mut transaction = data
        .database
        .pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .unwrap();
    assert!(
        messages::guard_history_mutation(&mut transaction, "fixture")
            .await
            .is_err()
    );
    transaction.rollback().await.unwrap();
    assert_eq!(
        data.resolve_work(&command).await.unwrap(),
        WorkResolution::NotApplied
    );
    let mut transaction = data
        .database
        .pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .unwrap();
    messages::guard_history_mutation(&mut transaction, "fixture")
        .await
        .unwrap();
    transaction.rollback().await.unwrap();
}
