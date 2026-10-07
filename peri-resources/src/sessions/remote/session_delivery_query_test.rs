use super::*;
use peri_acp_types::session_resources::SessionResourceErrorKind;
use serde_json::{json, Value as JsonValue};

const DELIVERY_ID: &str = "delivery.\"[']雪?2 OR 1=1 --";

fn delivery_query(session_id: &str) -> WorkDeliveryQuery {
    WorkDeliveryQuery {
        session_id: session_id.into(),
        delivery_id: DELIVERY_ID.into(),
    }
}

async fn setup() -> (Fixture, RemoteSessionData, DeliveryRecord) {
    let fixture = Fixture::new().await;
    let adapter = fixture.adapter().await;
    adapter
        .apply_work_mutation(&publication("delivery-query-publication", DELIVERY_ID))
        .await
        .unwrap();
    let expected = adapter
        .load_session_work(&query())
        .await
        .unwrap()
        .state
        .deliveries[DELIVERY_ID]
        .clone();
    (fixture, adapter, expected)
}

async fn set_state(fixture: &Fixture, session: &str, state: &JsonValue) {
    sqlx::query("INSERT INTO session_work_state(session_id,state_json) VALUES (?1,?2) ON CONFLICT(session_id) DO UPDATE SET state_json=excluded.state_json")
        .bind(session)
        .bind(state.to_string())
        .execute(&fixture.pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn work_revision_remote_reads_only_the_schema17_revision() {
    let (fixture, adapter, _) = setup().await;
    let session = "work-session".to_owned();
    assert!(StatementSpec::new(crate::sessions::work::READ_REVISION, vec![]).is_read_only());
    set_state(&fixture, &session, &json!({"revision": 23})).await;
    assert_eq!(adapter.load_work_revision(&session).await.unwrap(), 23);
    assert!(matches!(
        adapter
            .load_work_revision(&"missing".into())
            .await
            .unwrap_err()
            .kind(),
        SessionResourceErrorKind::NotFound
    ));
}

#[tokio::test]
async fn delivery_query_remote_selects_bound_id_and_session_without_writes() {
    let (fixture, adapter, expected) = setup().await;
    let query = delivery_query("work-session");
    assert!(StatementSpec::new(crate::sessions::work::READ_DELIVERY, vec![]).is_read_only());
    assert_eq!(
        adapter.load_work_delivery(&query).await.unwrap(),
        Some(expected.clone())
    );
    assert!(adapter
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
    set_state(&fixture, "other-session", &state).await;
    assert_eq!(
        adapter
            .load_work_delivery(&delivery_query("other-session"))
            .await
            .unwrap(),
        Some(other)
    );
    assert_eq!(
        adapter.load_work_delivery(&query).await.unwrap(),
        Some(expected)
    );
    let saved: String = sqlx::query_scalar(
        "SELECT state_json FROM session_work_state WHERE session_id='other-session'",
    )
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(saved, state.to_string());
    assert!(matches!(
        adapter
            .load_work_delivery(&delivery_query("absent-session"))
            .await
            .unwrap_err()
            .kind(),
        SessionResourceErrorKind::NotFound
    ));
}

#[tokio::test]
async fn delivery_query_remote_rejects_malformed_target_and_identity() {
    let (fixture, adapter, expected) = setup().await;
    let query = delivery_query("work-session");
    let mut mismatch = serde_json::to_value(&expected).unwrap();
    mismatch["publication"]["deliveryId"] = json!("different-delivery");
    let mut invalid_type = serde_json::to_value(&expected).unwrap();
    invalid_type["admissionSequence"] = json!("not-a-number");
    for target in [
        JsonValue::Null,
        json!(42),
        json!({}),
        invalid_type,
        mismatch,
    ] {
        set_state(
            &fixture,
            "work-session",
            &json!({"deliveries": {DELIVERY_ID: target}}),
        )
        .await;
        assert!(adapter.load_work_delivery(&query).await.is_err());
    }
    sqlx::query(
        "UPDATE session_work_state SET state_json='not-json' WHERE session_id='work-session'",
    )
    .execute(&fixture.pool)
    .await
    .unwrap();
    assert!(adapter.load_work_delivery(&query).await.is_err());
}

#[tokio::test]
async fn delivery_query_remote_skips_invalid_typed_history_with_large_reason_body() {
    let (fixture, adapter, expected) = setup().await;
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
    set_state(&fixture, "work-session", &state).await;
    assert!(adapter.load_session_work(&query()).await.is_err());
    assert_eq!(
        adapter
            .load_work_delivery(&delivery_query("work-session"))
            .await
            .unwrap(),
        Some(expected)
    );
    assert!(adapter
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
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(saved, state.to_string());
}

#[tokio::test]
async fn delivery_query_remote_existence_matches_snapshot_sources() {
    let fixture = Fixture::new().await;
    let adapter = fixture.adapter().await;
    assert!(adapter
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
    .execute(&fixture.pool)
    .await
    .unwrap();
    set_state(
        &fixture,
        "work-only",
        &serde_json::to_value(WorkState::default()).unwrap(),
    )
    .await;
    for session in ["work-session", "control-only", "work-only"] {
        let narrow = adapter
            .load_work_availability(&session.into())
            .await
            .unwrap();
        assert!(!narrow.is_available(1, None));
        assert!(!narrow.is_available(1, Some(0)));
        adapter
            .load_session_work(&WorkQuery {
                session_id: session.into(),
                limit: 1,
            })
            .await
            .unwrap();
        assert!(adapter
            .load_work_delivery(&delivery_query(session))
            .await
            .unwrap()
            .is_none());
    }
    assert!(matches!(
        adapter
            .load_work_availability(&"missing".into())
            .await
            .unwrap_err()
            .kind(),
        SessionResourceErrorKind::NotFound
    ));
    assert!(matches!(
        adapter
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
        adapter
            .load_work_delivery(&delivery_query("missing"))
            .await
            .unwrap_err()
            .kind(),
        SessionResourceErrorKind::NotFound
    ));
}

#[tokio::test]
async fn availability_remote_matches_notification_branches() {
    use peri_acp_types::session::MessageRequirement;
    let (fixture, adapter, record) = setup().await;
    let original = adapter.load_session_work(&query()).await.unwrap();
    assert!(StatementSpec::new(crate::sessions::work::READ_AVAILABILITY, vec![]).is_read_only());
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
            &fixture,
            "work-session",
            &serde_json::to_value(&state).unwrap(),
        )
        .await;
        let narrow = adapter
            .load_work_availability(&"work-session".into())
            .await
            .unwrap();
        let full = adapter.load_session_work(&query()).await.unwrap();
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
        assert_eq!(
            narrow.is_available(1, Some(floor)),
            matches!(variant, 0 | 7 | 8 | 11),
            "variant {variant}"
        );
        assert_eq!(
            narrow.is_available(1, None),
            !matches!(variant, 1 | 2 | 6 | 8 | 15 | 16),
            "variant {variant}"
        );
    }
}

#[tokio::test]
async fn availability_remote_skips_large_invalid_typed_payloads() {
    let (fixture, adapter, _) = setup().await;
    let mut state =
        serde_json::to_value(adapter.load_session_work(&query()).await.unwrap().state).unwrap();
    state["works"]["historical"] = json!({
        "workId": "historical", "batchId": "historical", "stage": "settled",
        "reasonRequest": {"serializedRequest": "history body".repeat(100_000)},
        "revision": "not a number"
    });
    state["deliveries"][DELIVERY_ID]["publication"]["event"]["content"] = json!(42);
    assert!(serde_json::from_value::<WorkState>(state.clone()).is_err());
    set_state(&fixture, "work-session", &state).await;
    assert!(adapter.load_session_work(&query()).await.is_err());
    let narrow = adapter
        .load_work_availability(&"work-session".into())
        .await
        .unwrap();
    assert!(narrow.is_available(1, None));
    assert!(narrow.is_available(1, Some(0)));
    assert!(format!("{narrow:?}").len() < 4096);
    let rows = adapter
        .store()
        .await
        .unwrap()
        .read_batch(vec![StatementSpec::new(
            crate::sessions::work::READ_AVAILABILITY,
            vec![Value::Text("work-session".into())],
        )])
        .await
        .unwrap();
    assert!(
        format!("{rows:?}").len() < 4096,
        "远端结果集不得携带历史正文"
    );
    let saved: String = sqlx::query_scalar(
        "SELECT state_json FROM session_work_state WHERE session_id='work-session'",
    )
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(saved, state.to_string());
}

#[tokio::test]
async fn availability_remote_root_barrier_allows_pending_work_but_rejects_pending_control() {
    use crate::sessions::{
        resources::{SessionDataHome, SessionResourcesImpl},
        sqlite_store::LocalExecution,
    };
    use peri_acp_types::session_resources::{ControlAction, ControlCommand, SessionResources};
    for work_pending in [true, false] {
        let fixture = Fixture::new().await;
        sqlx::query("INSERT INTO threads(id,created_at,updated_at,workspace_id,parent_thread_id) VALUES ('child','now','now','workspace','work-session')")
            .execute(&fixture.pool).await.unwrap();
        let adapter = Arc::new(fixture.adapter().await);
        let local = LocalExecution::open(fixture.directory.path().join("local.db"))
            .await
            .unwrap();
        let resources = SessionResourcesImpl::from_ports(
            adapter.clone(),
            Arc::new(local),
            SessionDataHome::RemoteStore,
        );
        resources
            .apply_work_mutation(&publication("initial", "initial"))
            .await
            .unwrap();
        if work_pending {
            adapter
                .inject_faults(FaultPlan {
                    drop_reply: Some("session_work".into()),
                    drop_before_send: None,
                })
                .await;
            assert!(resources
                .apply_work_mutation(&publication("pending", "pending"))
                .await
                .unwrap_err()
                .is_persistence_uncertain());
        } else {
            adapter
                .inject_faults(FaultPlan {
                    drop_reply: Some("session_control".into()),
                    drop_before_send: None,
                })
                .await;
            let command = ControlCommand {
                session_id: "work-session".into(),
                command_id: "pause".into(),
                expected_lifecycle: 1,
                expected_revision: 0,
                expected_control_generation: 0,
                action: ControlAction::Pause,
            };
            assert!(resources
                .apply_session_control(&command)
                .await
                .unwrap_err()
                .is_persistence_uncertain());
        }
        for id in ["work-session", "child"] {
            let narrow = resources.load_work_availability(&id.into()).await;
            let full = resources
                .load_session_work(&WorkQuery {
                    session_id: id.into(),
                    limit: 1,
                })
                .await;
            if work_pending {
                let narrow = narrow.unwrap();
                let full = full.unwrap();
                assert_eq!(
                    narrow.is_available(1, None),
                    full.has_pending_current_work()
                );
                assert_eq!(narrow.control, full.control);
            } else {
                assert!(narrow.unwrap_err().is_persistence_uncertain());
                assert!(full.unwrap_err().is_persistence_uncertain());
            }
        }
    }
}

#[tokio::test]
async fn availability_remote_pending_work_and_terminal_metadata() {
    let (fixture, adapter, _) = setup().await;
    let original = adapter.load_session_work(&query()).await.unwrap().state;
    let execution = peri_acp_types::session_resources::ControlAttempt {
        turn_id: peri_acp_types::session::TurnId::new(),
        attempt_id: peri_acp_types::identity::AttemptId::new(),
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
                state
                    .terminal_obligations
                    .insert("admission".into(), publication("terminal", "terminal"));
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
            &fixture,
            "work-session",
            &serde_json::to_value(&state).unwrap(),
        )
        .await;
        let narrow = adapter
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
            adapter
                .load_session_work(&query())
                .await
                .unwrap()
                .has_pending_current_work()
        );
    }
}

#[tokio::test]
async fn availability_remote_legacy_history_without_ledger_keeps_pending_hint() {
    let fixture = Fixture::new().await;
    let adapter = fixture.adapter().await;
    let id = "work-session".to_owned();
    adapter
        .append_history(
            &id,
            &[PersistedPayload::Message(BaseMessage::human("legacy"))],
        )
        .await
        .unwrap();
    sqlx::query("DELETE FROM session_work_state WHERE session_id=?1")
        .bind(&id)
        .execute(&fixture.pool)
        .await
        .unwrap();
    let narrow = adapter.load_work_availability(&id).await.unwrap();
    assert!(narrow.is_available(1, None));
    assert!(!narrow.is_available(1, Some(0)));
    assert!(adapter
        .load_session_work(&query())
        .await
        .unwrap()
        .has_pending_current_work());
}
