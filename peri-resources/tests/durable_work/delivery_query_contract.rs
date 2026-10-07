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
        .apply_work_mutation(&prepare_command(&command(
            "delivery-query-publication",
            WorkAction::PublishDelivery {
                delivery: publication(DELIVERY_ID, MessagePolicy::ensure_processing()),
            },
        )))
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
    for session in ["work-session", "control-only", "work-only"] {
        let narrow = resources
            .load_work_availability(&session.into())
            .await
            .unwrap();
        assert!(!narrow.is_available(1, None));
        assert!(!narrow.is_available(1, Some(0)));
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
            .load_work_availability(&"missing".into())
            .await
            .unwrap_err()
            .kind(),
        SessionResourceErrorKind::NotFound
    ));
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

#[tokio::test]
async fn availability_sqlite_matches_notification_branches() {
    use peri_acp_types::session::MessageRequirement;
    let (_directory, resources, pool, record) = setup().await;
    let original = snapshot(resources.as_ref()).await;
    for variant in 0..17 {
        let mut state = original.state.clone();
        let floor = record.admission_sequence;
        match variant {
            0 => {}
            1 => {
                state.deliveries.clear();
                state.obligations.clear();
            }
            2 => {
                state
                    .deliveries
                    .get_mut(DELIVERY_ID)
                    .unwrap()
                    .recipient_lifecycle += 1
            }
            3 => state.deliveries.get_mut(DELIVERY_ID).unwrap().batch_id = Some("claimed".into()),
            4 => {
                state.deliveries.get_mut(DELIVERY_ID).unwrap().disposition = Some("disposed".into())
            }
            5 => {
                state.obligations.get_mut(DELIVERY_ID).unwrap().status =
                    ObligationStatus::InProgress
            }
            6 => {
                state.obligations.get_mut(DELIVERY_ID).unwrap().status = ObligationStatus::Satisfied
            }
            7 => state.obligations.get_mut(DELIVERY_ID).unwrap().status = ObligationStatus::Blocked,
            8 => {
                state.obligations.clear();
            }
            9 => {
                state
                    .deliveries
                    .get_mut(DELIVERY_ID)
                    .unwrap()
                    .publication
                    .policy
                    .requirement = MessageRequirement::Optional
            }
            10 => state.limits.max_batch_size = 0,
            11 | 12 => {
                state.limits.max_batch_size = 1;
                let mut earlier = record.clone();
                earlier.admission_sequence = floor - 1;
                earlier.publication.policy.requirement = if variant == 11 {
                    MessageRequirement::Optional
                } else {
                    MessageRequirement::Required
                };
                state.deliveries.insert("earlier".into(), earlier);
            }
            13 => {
                state.deliveries.clear();
            }
            14 => {
                state.deliveries.clear();
                state.obligations.clear();
                state
                    .legacy_unknown
                    .insert("legacy".into(), "unknown".into());
            }
            15 => {
                state.obligations.get_mut(DELIVERY_ID).unwrap().status =
                    ObligationStatus::Suppressed
            }
            16 => {
                state.obligations.get_mut(DELIVERY_ID).unwrap().status = ObligationStatus::Abandoned
            }
            _ => unreachable!(),
        }
        set_state(
            &pool,
            "work-session",
            &serde_json::to_value(&state).unwrap(),
        )
        .await;
        let narrow = resources
            .load_work_availability(&"work-session".into())
            .await
            .unwrap();
        let full = snapshot(resources.as_ref()).await;
        assert_eq!(narrow.control, full.control);
        assert_eq!(narrow.state.revision, full.state.revision);
        for lifecycle in [1, 2] {
            for observer_floor in [None, Some(0), Some(floor), Some(floor + 1)] {
                let expected = lifecycle == full.control.lifecycle
                    && match observer_floor {
                        None => full.has_pending_current_work(),
                        Some(floor) => full
                            .state
                            .claimable_deliveries(full.control.lifecycle)
                            .iter()
                            .any(|id| {
                                let delivery = &full.state.deliveries[id];
                                delivery.admission_sequence >= floor
                                    && delivery.publication.policy.requirement
                                        == MessageRequirement::Required
                            }),
                    };
                assert_eq!(
                    narrow.is_available(lifecycle, observer_floor),
                    expected,
                    "variant {variant}, lifecycle {lifecycle}, floor {observer_floor:?}"
                );
            }
        }
        let expected_some = matches!(variant, 0 | 7 | 8 | 11);
        assert_eq!(
            narrow.is_available(1, Some(floor)),
            expected_some,
            "variant {variant}"
        );
        let expected_none = !matches!(variant, 1 | 2 | 6 | 8 | 15 | 16);
        assert_eq!(
            narrow.is_available(1, None),
            expected_none,
            "variant {variant}"
        );
    }
}

#[tokio::test]
async fn availability_sqlite_skips_large_invalid_typed_payloads() {
    let (_directory, resources, pool, _) = setup().await;
    let mut state = serde_json::to_value(snapshot(resources.as_ref()).await.state).unwrap();
    state["works"]["historical"] = json!({
        "workId": "historical", "batchId": "historical", "stage": "settled",
        "reasonRequest": {"serializedRequest": "history body".repeat(100_000)},
        "revision": "not a number"
    });
    state["deliveries"][DELIVERY_ID]["publication"]["event"]["content"] = json!(42);
    assert!(serde_json::from_value::<WorkState>(state.clone()).is_err());
    set_state(&pool, "work-session", &state).await;
    assert!(resources
        .load_session_work(&WorkQuery {
            session_id: "work-session".into(),
            limit: 1
        })
        .await
        .is_err());
    let narrow = resources
        .load_work_availability(&"work-session".into())
        .await
        .unwrap();
    assert!(narrow.is_available(1, None));
    assert!(narrow.is_available(1, Some(0)));
    assert!(format!("{narrow:?}").len() < 4096);
    let saved: String = sqlx::query_scalar(
        "SELECT state_json FROM session_work_state WHERE session_id='work-session'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(saved, state.to_string());
}

#[tokio::test]
async fn availability_sqlite_pending_work_and_terminal_metadata() {
    let (_directory, resources, pool, _) = setup().await;
    let original = snapshot(resources.as_ref()).await.state;
    let execution = peri_acp_types::session_resources::ControlAttempt {
        turn_id: TurnId::new(),
        attempt_id: AttemptId::new(),
    };
    for variant in 0..9 {
        let mut state = original.clone();
        state.deliveries.clear();
        state.obligations.clear();
        state.works.insert(
            "work".into(),
            WorkRecord {
                work_id: "work".into(),
                revision: 1,
                budget_id: "budget".into(),
                batch_id: "batch".into(),
                stage: WorkStage::ReasonReady,
                resume_stage: None,
                request_id: None,
                reason_request: None,
                response: None,
                invocation_ids: vec![],
                reason: None,
                recovery_condition: None,
            },
        );
        state.batches.insert(
            "batch".into(),
            ProcessingBatch {
                batch_id: "batch".into(),
                delivery_ids: vec![],
                processing_delivery_ids: vec!["delivery".into()],
                projection_versions: Default::default(),
                execution: execution.clone(),
                recipient_lifecycle: 1,
            },
        );
        match variant {
            0 => {}
            1 => state.batches.get_mut("batch").unwrap().recipient_lifecycle = 2,
            2 => {
                state.batches.clear();
            }
            3 => state.works.get_mut("work").unwrap().stage = WorkStage::Settled,
            4..=8 => {
                state.works.get_mut("work").unwrap().stage = WorkStage::Abandoned;
                let admission = WorkAdmission {
                    session_id: "work-session".into(),
                    admission_id: "admission".into(),
                    instance_id: "instance".into(),
                    generation_id: "generation".into(),
                    lifecycle: 1,
                    control_generation: 0,
                    work_id: if variant == 8 {
                        "delivery".into()
                    } else {
                        "work".into()
                    },
                    work_revision: 1,
                    execution: execution.clone(),
                };
                if variant != 4 {
                    state.admissions.insert(
                        "admission".into(),
                        AdmissionRecord {
                            admission,
                            entering_receipt: None,
                            settled_receipt: None,
                            evidence_id: None,
                        },
                    );
                }
                state.terminal_obligations.insert(
                    "admission".into(),
                    command(
                        "terminal",
                        WorkAction::PublishDelivery {
                            delivery: publication("terminal", MessagePolicy::ensure_processing()),
                        },
                    ),
                );
                if variant == 6 {
                    state.works.get_mut("work").unwrap().stage = WorkStage::Settled;
                }
                if variant == 7 {
                    state
                        .admissions
                        .get_mut("admission")
                        .unwrap()
                        .admission
                        .lifecycle = 2;
                }
                if variant == 8 {
                    let mut duplicate = state.batches["batch"].clone();
                    duplicate.batch_id = "duplicate".into();
                    state.batches.insert("duplicate".into(), duplicate);
                }
            }
            _ => unreachable!(),
        }
        state.revision = u64::MAX;
        set_state(
            &pool,
            "work-session",
            &serde_json::to_value(&state).unwrap(),
        )
        .await;
        let narrow = resources
            .load_work_availability(&"work-session".into())
            .await
            .unwrap();
        assert_eq!(narrow.state.revision, u64::MAX);
        assert_eq!(
            narrow.is_available(1, None),
            matches!(variant, 0 | 2 | 4 | 6 | 8),
            "variant {variant}"
        );
        assert!(!narrow.is_available(1, Some(0)));
        assert_eq!(
            narrow.is_available(1, None),
            snapshot(resources.as_ref())
                .await
                .has_pending_current_work()
        );
    }
}
