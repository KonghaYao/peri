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

fn command() -> WorkCommand {
    WorkCommand {
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
    }
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
    let reduction = reduce_work(&command, &control, &state).unwrap();
    mutation_effects(&command, &state, json, &control, &reduction).unwrap()
}

#[test]
fn noncanonical_json_is_reused_byte_for_byte_for_insert_and_guard() {
    let raw = noncanonical_state();
    assert_ne!(raw, encode(&state(Some(&raw), false).unwrap()).unwrap());
    let command = command();
    let current = state(Some(&raw), false).unwrap();
    let control = ControlState::default();
    let reduction = reduce_work(&command, &control, &current).unwrap();
    let effects =
        mutation_effects(&command, &current, Some(raw.clone()), &control, &reduction).unwrap();
    assert_eq!(effects[2].sql, INSERT_STATE);
    assert_eq!(effects[2].params[1].as_bytes(), raw.as_bytes());
    assert_eq!(effects[3].sql, GUARD_STATE);
    assert_eq!(effects[3].params[1].as_bytes(), raw.as_bytes());
    let next = effects
        .iter()
        .find(|effect| effect.sql == UPDATE_STATE)
        .unwrap();
    assert_eq!(next.params[1], encode(&reduction.state).unwrap());
}

fn assert_missing_state_initialization(has_history: bool) {
    let effects = effects(None, has_history);
    let initial = state(None, has_history).unwrap();
    assert_eq!(effects[2].params[1], encode(&initial).unwrap());
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
        for value in effect.params {
            query = query.bind(value);
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
