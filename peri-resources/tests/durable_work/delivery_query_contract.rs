use super::*;
use peri_acp_types::session_resources::SessionResourceErrorKind;
use serde_json::{json, Value};
use sqlx::SqlitePool;

const DELIVERY_ID: &str = "delivery.\"[']雪?2 OR 1=1 --";

fn delivery_query(session_id: &str) -> WorkDeliveryQuery {
    WorkDeliveryQuery {
        session_id: session_id.into(),
        delivery_id: DELIVERY_ID.into(),
    }
}

async fn setup() -> (
    TempDir,
    Arc<dyn SessionResources>,
    SqlitePool,
    DeliveryRecord,
) {
    let (directory, resources) = fixture().await;
    resources
        .apply_work_mutation(&command(
            "delivery-query-publication",
            WorkAction::PublishDelivery {
                delivery: publication(DELIVERY_ID, MessagePolicy::ensure_processing()),
            },
        ))
        .await
        .unwrap();
    let record = snapshot(resources.as_ref()).await.state.deliveries[DELIVERY_ID].clone();
    let pool = SqlitePool::connect(&format!(
        "sqlite:{}",
        directory.path().join("work.db").display()
    ))
    .await
    .unwrap();
    (directory, resources, pool, record)
}

async fn set_state(pool: &SqlitePool, session: &str, state: &Value) {
    sqlx::query("INSERT INTO session_work_state(session_id,state_json) VALUES (?1,?2) ON CONFLICT(session_id) DO UPDATE SET state_json=excluded.state_json")
        .bind(session)
        .bind(state.to_string())
        .execute(pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn delivery_query_sqlite_selects_bound_id_and_session_without_writes() {
    let (_directory, resources, pool, expected) = setup().await;
    let query = delivery_query("work-session");
    assert_eq!(
        resources.load_work_delivery(&query).await.unwrap(),
        Some(expected.clone())
    );
    assert!(resources
        .load_work_delivery(&WorkDeliveryQuery {
            delivery_id: "absent".into(),
            ..query.clone()
        })
        .await
        .unwrap()
        .is_none());
    let mut other = expected.clone();
    other.disposition = Some("other-session".into());
    let state = json!({"deliveries": {DELIVERY_ID: other}});
    set_state(&pool, "other-session", &state).await;
    assert_eq!(
        resources
            .load_work_delivery(&delivery_query("other-session"))
            .await
            .unwrap(),
        Some(other)
    );
    assert_eq!(
        resources.load_work_delivery(&query).await.unwrap(),
        Some(expected)
    );
    let saved: String = sqlx::query_scalar(
        "SELECT state_json FROM session_work_state WHERE session_id='other-session'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(saved, state.to_string());
    assert!(matches!(
        resources
            .load_work_delivery(&delivery_query("absent-session"))
            .await
            .unwrap_err()
            .kind(),
        SessionResourceErrorKind::NotFound
    ));
}

#[tokio::test]
async fn delivery_query_sqlite_rejects_malformed_target_and_identity() {
    let (_directory, resources, pool, expected) = setup().await;
    let query = delivery_query("work-session");
    let mut mismatch = serde_json::to_value(&expected).unwrap();
    mismatch["publication"]["deliveryId"] = json!("different-delivery");
    let mut invalid_type = serde_json::to_value(&expected).unwrap();
    invalid_type["admissionSequence"] = json!("not-a-number");
    for target in [Value::Null, json!(42), json!({}), invalid_type, mismatch] {
        set_state(
            &pool,
            "work-session",
            &json!({"deliveries": {DELIVERY_ID: target}}),
        )
        .await;
        assert!(resources.load_work_delivery(&query).await.is_err());
    }
    sqlx::query(
        "UPDATE session_work_state SET state_json='not-json' WHERE session_id='work-session'",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert!(resources.load_work_delivery(&query).await.is_err());
}

#[tokio::test]
async fn delivery_query_sqlite_skips_invalid_typed_history_with_large_reason_body() {
    let (_directory, resources, pool, expected) = setup().await;
    let mut state = serde_json::to_value(WorkState::default()).unwrap();
    state["deliveries"][DELIVERY_ID] = serde_json::to_value(&expected).unwrap();
    state["works"]["historical"] = json!({
        "workId": "historical",
        "revision": "invalid typed revision",
        "budgetId": "historical",
        "batchId": "historical",
        "stage": "reasonInFlight",
        "resumeStage": null,
        "requestId": "historical-request",
        "reasonRequest": {
            "serializedRequest": "history body".repeat(100_000),
            "requestDigest": "historical-digest",
            "modelRef": "historical-model",
            "authorizationRef": "historical-authorization"
        },
        "response": null,
        "invocationIds": [],
        "reason": null,
        "recoveryCondition": null
    });
    assert!(serde_json::from_value::<WorkState>(state.clone()).is_err());
    set_state(&pool, "work-session", &state).await;
    assert!(resources
        .load_session_work(&WorkQuery {
            session_id: "work-session".into(),
            limit: 1
        })
        .await
        .is_err());
    assert_eq!(
        resources
            .load_work_delivery(&delivery_query("work-session"))
            .await
            .unwrap(),
        Some(expected)
    );
    assert!(resources
        .load_work_delivery(&WorkDeliveryQuery {
            session_id: "work-session".into(),
            delivery_id: "missing".into()
        })
        .await
        .unwrap()
        .is_none());
    let saved: String = sqlx::query_scalar(
        "SELECT state_json FROM session_work_state WHERE session_id='work-session'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(saved, state.to_string());
}

#[tokio::test]
async fn delivery_query_sqlite_existence_matches_snapshot_sources() {
    let (_directory, resources) = fixture().await;
    let pool = SqlitePool::connect(&format!(
        "sqlite:{}",
        _directory.path().join("work.db").display()
    ))
    .await
    .unwrap();
    assert!(resources
        .load_work_delivery(&delivery_query("work-session"))
        .await
        .unwrap()
        .is_none());
    sqlx::query(
        "INSERT INTO session_control_state(session_id,state_json) VALUES ('control-only',?1)",
    )
    .bind(
        serde_json::to_string(&peri_acp_types::session_resources::ControlState::default()).unwrap(),
    )
    .execute(&pool)
    .await
    .unwrap();
    set_state(
        &pool,
        "work-only",
        &serde_json::to_value(WorkState::default()).unwrap(),
    )
    .await;
    for session in ["control-only", "work-only"] {
        resources
            .load_session_work(&WorkQuery {
                session_id: session.into(),
                limit: 1,
            })
            .await
            .unwrap();
        assert!(resources
            .load_work_delivery(&delivery_query(session))
            .await
            .unwrap()
            .is_none());
    }
    assert!(matches!(
        resources
            .load_session_work(&WorkQuery {
                session_id: "missing".into(),
                limit: 1
            })
            .await
            .unwrap_err()
            .kind(),
        SessionResourceErrorKind::NotFound
    ));
    assert!(matches!(
        resources
            .load_work_delivery(&delivery_query("missing"))
            .await
            .unwrap_err()
            .kind(),
        SessionResourceErrorKind::NotFound
    ));
}
