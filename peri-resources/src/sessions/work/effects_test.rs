use super::*;
use peri_acp_types::{
    messages::BaseMessage,
    session::MessagePolicy,
    session_resources::work::{
        reduce_work, DeliveryPurpose, PublishDelivery, WorkEvent, WorkPayload,
    },
    store::PersistedPayload,
};
use sqlx::{sqlite::SqlitePoolOptions, SqlitePool};

fn command() -> PreparedWorkCommand {
    PreparedWorkCommand::try_new(WorkCommand {
        session_id: "raw-state-session".into(),
        recipient_lifecycle: 1,
        mutation_id: "raw-state-mutation".into(),
        action: WorkAction::PublishDelivery {
            delivery: PublishDelivery {
                delivery_id: "raw-state-delivery".into(),
                event: WorkEvent {
                    producer_namespace: "raw-state-test".into(),
                    event_id: "raw-state-event".into(),
                    event_kind: "request".into(),
                    causation_id: None,
                    content: WorkPayload::from_payload(&PersistedPayload::Message(
                        BaseMessage::human("process this"),
                    ))
                    .unwrap(),
                },
                purpose: DeliveryPurpose::UserInput,
                policy: MessagePolicy::ensure_processing(),
            },
        },
    })
    .unwrap()
}

fn noncanonical_state() -> String {
    format!(
        " \n{}\n ",
        serde_json::to_string_pretty(&WorkState::default()).unwrap()
    )
}

fn effects(json: Option<String>, has_history: bool) -> Vec<WorkEffect> {
    let state = state(json.as_deref(), has_history).unwrap();
    let control = ControlState::default();
    let command = command();
    let initial_json = pre_state_json(json, &state).unwrap();
    let parent_command = terminal_parent_command(&command, &state).unwrap();
    let reduction = reduce_work(&command, &control, state).unwrap();
    mutation_effects(
        &command,
        initial_json,
        parent_command.as_ref(),
        &control,
        &reduction,
    )
    .unwrap()
}

#[test]
fn noncanonical_json_is_reused_byte_for_byte_for_insert_and_guard() {
    let raw = noncanonical_state();
    assert_ne!(raw, encode(&state(Some(&raw), false).unwrap()).unwrap());
    let command = command();
    let current = state(Some(&raw), false).unwrap();
    let control = ControlState::default();
    let reduction = reduce_work(&command, &control, current).unwrap();
    let effects = mutation_effects(&command, raw.clone(), None, &control, &reduction).unwrap();
    assert_eq!(effects[2].sql, INSERT_STATE);
    assert_eq!(effects[2].params[1].as_bytes(), raw.as_bytes());
    assert_eq!(effects[3].sql, GUARD_STATE);
    assert_eq!(effects[3].params[1].as_bytes(), raw.as_bytes());
    let next = effects
        .iter()
        .find(|effect| effect.sql == UPDATE_STATE)
        .unwrap();
    assert_eq!(
        next.params[1].as_ref(),
        encode(&reduction.state.unwrap()).unwrap()
    );
}

fn assert_missing_state_initialization(has_history: bool) {
    let effects = effects(None, has_history);
    let initial = state(None, has_history).unwrap();
    assert_eq!(effects[2].sql, INSERT_STATE);
    assert_eq!(effects[2].params[1].as_ref(), encode(&initial).unwrap());
    assert_eq!(effects[3].params[1], effects[2].params[1]);
    assert_eq!(initial.legacy_unknown.is_empty(), !has_history);
}

#[test]
fn missing_state_without_history_encodes_default_semantics() {
    assert_missing_state_initialization(false);
}

#[test]
fn missing_state_with_history_encodes_legacy_unknown_semantics() {
    assert_missing_state_initialization(true);
}

async fn database(raw: &str) -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    for sql in [
        CREATE_STATE,
        CREATE_COMMANDS,
        CREATE_RECEIPTS,
        CREATE_EVENTS,
        crate::sessions::control::CREATE_STATE,
    ] {
        sqlx::query(sql).execute(&pool).await.unwrap();
    }
    sqlx::query(INSERT_STATE)
        .bind(&command().session_id)
        .bind(raw)
        .execute(&pool)
        .await
        .unwrap();
    pool
}

async fn apply(pool: &SqlitePool, effects: Vec<WorkEffect>) -> Result<(), sqlx::Error> {
    let mut transaction = pool.begin().await?;
    for effect in effects {
        let mut query = sqlx::query(effect.sql);
        for value in &effect.params {
            query = query.bind(value.as_ref());
        }
        if let Err(error) = query.execute(&mut *transaction).await {
            transaction.rollback().await?;
            return Err(error);
        }
    }
    transaction.commit().await
}

#[tokio::test]
async fn noncanonical_snapshot_guard_allows_unchanged_state() {
    let raw = noncanonical_state();
    let pool = database(&raw).await;
    let saved: String = sqlx::query_scalar(READ_STATE)
        .bind(&command().session_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    apply(&pool, effects(Some(saved), false)).await.unwrap();
    let resolution: String = sqlx::query_scalar(
        "SELECT resolution_json FROM session_work_receipts WHERE mutation_id=?1",
    )
    .bind(&command().mutation_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    let WorkResolution::Applied { receipt } = decode(&resolution).unwrap() else {
        panic!("missing original receipt")
    };
    assert_eq!(receipt.decision, WorkDecision::Accepted);
}

#[tokio::test]
async fn concurrent_change_rejects_stale_raw_guard_and_rolls_back_effects() {
    let raw = noncanonical_state();
    let pool = database(&raw).await;
    let saved: String = sqlx::query_scalar(READ_STATE)
        .bind(&command().session_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let mut concurrent = state(Some(&saved), false).unwrap();
    let stale_effects = effects(Some(saved), false);
    concurrent.revision += 1;
    let changed = encode(&concurrent).unwrap();
    sqlx::query(UPDATE_STATE)
        .bind(&command().session_id)
        .bind(&changed)
        .execute(&pool)
        .await
        .unwrap();
    let error = apply(&pool, stale_effects).await.unwrap_err();
    assert!(
        matches!(error, sqlx::Error::Database(ref error) if error.kind() == sqlx::error::ErrorKind::NotNullViolation)
    );
    let stored: String = sqlx::query_scalar(READ_STATE)
        .bind(&command().session_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored, changed);
    for query in [
        "SELECT COUNT(*) FROM session_work_commands",
        "SELECT COUNT(*) FROM session_work_events",
        "SELECT COUNT(*) FROM session_work_receipts",
    ] {
        let count: i64 = sqlx::query_scalar(query).fetch_one(&pool).await.unwrap();
        assert_eq!(count, 0);
    }
}

#[tokio::test]
async fn pending_sql_preserves_subtree_order_and_uses_table_scan() {
    let pool = database(&noncanonical_state()).await;
    sqlx::query("CREATE TABLE threads(id TEXT PRIMARY KEY, parent_thread_id TEXT)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO threads VALUES ('root',NULL),('child','root'),('grandchild','child'),('other',NULL)")
        .execute(&pool)
        .await
        .unwrap();
    for (mutation, owner, reconciled) in [
        ("z-root", "root", 0),
        ("a-child", "child", 0),
        ("m-grandchild", "grandchild", 0),
        ("b-done", "root", 1),
        ("unrelated", "other", 0),
    ] {
        sqlx::query("INSERT INTO session_work_commands VALUES (?1,?2,'digest',?1,?3)")
            .bind(mutation)
            .bind(owner)
            .bind(reconciled)
            .execute(&pool)
            .await
            .unwrap();
    }
    for (owner, expected) in [
        ("root", vec!["a-child", "m-grandchild", "z-root"]),
        ("child", vec!["a-child", "m-grandchild"]),
        ("grandchild", vec!["m-grandchild"]),
        ("missing", vec![]),
    ] {
        let pending: Vec<String> = sqlx::query_scalar(READ_PENDING)
            .bind(owner)
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(pending, expected);
        let has_pending: bool = sqlx::query_scalar(HAS_PENDING)
            .bind(owner)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(has_pending, !expected.is_empty());
    }
    let plan: Vec<(i64, i64, i64, String)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "EXPLAIN QUERY PLAN {READ_PENDING}"
    )))
    .bind("root")
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(plan.iter().any(|row| row.3 == "SCAN session_work_commands"));
    assert!(plan.iter().any(|row| row.3.contains("TEMP B-TREE")));
    assert!(!plan
        .iter()
        .any(|row| row.3.contains("sqlite_autoindex_session_work_commands")));
}

#[tokio::test]
async fn command_guard_preserves_same_session_pending_and_identity_barriers() {
    let pool = database(&noncanonical_state()).await;
    sqlx::query(INSERT_COMMAND)
        .bind("current")
        .bind("root")
        .bind("digest")
        .bind("command")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(INSERT_COMMAND)
        .bind("other")
        .bind("child")
        .bind("digest")
        .bind("command")
        .execute(&pool)
        .await
        .unwrap();
    for (digest, allowed) in [("digest", true), ("conflicting", false)] {
        let result = sqlx::query(GUARD_COMMAND)
            .bind("current")
            .bind("root")
            .bind(digest)
            .bind("command")
            .execute(&pool)
            .await;
        assert_eq!(result.is_ok(), allowed);
    }
    sqlx::query("UPDATE session_work_commands SET session_id='root' WHERE mutation_id='other'")
        .execute(&pool)
        .await
        .unwrap();
    for (reconciled, allowed) in [(0, false), (1, true)] {
        sqlx::query("UPDATE session_work_commands SET reconciled=?1 WHERE mutation_id='other'")
            .bind(reconciled)
            .execute(&pool)
            .await
            .unwrap();
        let result = sqlx::query(GUARD_COMMAND)
            .bind("current")
            .bind("root")
            .bind("digest")
            .bind("command")
            .execute(&pool)
            .await;
        assert_eq!(result.is_ok(), allowed);
    }
}

#[tokio::test]
async fn snapshot_sql_preserves_missing_legacy_and_raw_json_facts() {
    let raw = noncanonical_state();
    let pool = database(&raw).await;
    for statement in [
        "CREATE TABLE threads(id TEXT PRIMARY KEY)",
        "CREATE TABLE messages(thread_id TEXT)",
        "INSERT INTO threads VALUES ('empty'),('legacy')",
        "INSERT INTO messages VALUES ('legacy'),('raw-state-session')",
    ] {
        sqlx::query(statement).execute(&pool).await.unwrap();
    }
    for (session, expected_exists, expected_history) in [
        ("empty", true, false),
        ("legacy", true, true),
        ("missing", false, false),
        ("raw-state-session", false, false),
    ] {
        let (exists, control, state_json, history): (bool, Option<String>, Option<String>, bool) =
            sqlx::query_as(READ_SNAPSHOT)
                .bind(session)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(exists, expected_exists);
        assert_eq!(history, expected_history);
        assert!(control.is_none());
        if session == "raw-state-session" {
            assert_eq!(state_json.as_deref(), Some(raw.as_str()));
        } else {
            assert!(state_json.is_none());
        }
        let current = state(state_json.as_deref(), history).unwrap();
        assert_eq!(!current.legacy_unknown.is_empty(), session == "legacy");
    }
}

#[path = "prepared_effects_test.rs"]
mod prepared_effects;
