use super::*;

async fn publication(
    adapter: &RemoteSessionData,
    mutation_id: &str,
    delivery_id: &str,
) -> WorkCommand {
    let payload = PersistedPayload::Message(BaseMessage::human("process terminal result"));
    let (content, prepared) =
        crate::sessions::work_store::payload::prepare(&payload, "work-session").unwrap();
    adapter
        .commit_effects(
            "prepare_test_payload",
            &[delivery_id.into()],
            super::super::specifications(prepared).unwrap(),
            &"work-session".into(),
        )
        .await
        .unwrap();
    WorkCommand {
        session_id: "work-session".into(),
        recipient_lifecycle: 1,
        mutation_id: mutation_id.into(),
        action: WorkAction::PublishDelivery {
            delivery: PublishDelivery {
                delivery_id: delivery_id.into(),
                event: WorkEvent {
                    producer_namespace: "trusted-test".into(),
                    event_id: format!("event-{delivery_id}"),
                    event_kind: "terminal".into(),
                    causation_id: None,
                    content: WorkPayload {
                        message_id: payload.id(),
                        role: "user".into(),
                        content,
                        tool_call_id: None,
                    },
                },
                purpose: DeliveryPurpose::TaskTerminal,
                policy: MessagePolicy::ensure_processing(),
            },
        },
    }
}

async fn deliveries(adapter: &RemoteSessionData) -> Vec<Delivery> {
    let inspection = adapter
        .inspect_work(&WorkQuery::new("work-session", WorkSelector::Inbox))
        .await
        .unwrap();
    let WorkPage::Deliveries(deliveries) = inspection.page else {
        panic!("missing delivery page")
    };
    deliveries
}

#[tokio::test]
async fn workrecords_remote_lost_ack_recovers_one_delivery_original_receipt() {
    let fixture = Fixture::new().await;
    let adapter = fixture.adapter().await;
    let command = publication(&adapter, "lost-ack", "terminal").await;
    adapter
        .inject_faults(FaultPlan {
            drop_reply: Some("session_work".into()),
            drop_before_send: None,
        })
        .await;
    assert!(adapter
        .apply_work_mutation(&command)
        .await
        .unwrap_err()
        .is_persistence_uncertain());
    let reopened = fixture.adapter().await;
    let WorkResolution::Applied { receipt } =
        reopened.resolve_work_mutation(&command).await.unwrap()
    else {
        panic!("missing original applied receipt")
    };
    assert_eq!(receipt.decision, WorkDecision::Accepted);
    assert_eq!(
        reopened.apply_work_mutation(&command).await.unwrap(),
        receipt
    );
    assert_eq!(deliveries(&reopened).await.len(), 1);
    assert_eq!(
        deliveries(&reopened).await[0].obligation,
        ObligationStatus::Pending
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM session_work_receipts")
            .fetch_one(&fixture.pool)
            .await
            .unwrap(),
        1
    );
    assert!(!reopened
        .has_pending_work_mutations(&command.session_id)
        .await
        .unwrap());
}

#[tokio::test]
async fn workrecords_remote_missing_receipt_stays_unknown_until_final_seal() {
    let fixture = Fixture::new().await;
    let adapter = fixture.adapter().await;
    let command = publication(&adapter, "before-send", "terminal").await;
    adapter
        .inject_faults(FaultPlan {
            drop_reply: None,
            drop_before_send: Some("session_work".into()),
        })
        .await;
    assert!(adapter
        .apply_work_mutation(&command)
        .await
        .unwrap_err()
        .is_persistence_uncertain());
    assert_eq!(adapter.work_resolution(&command).await.unwrap(), None);
    assert!(adapter
        .has_pending_work_mutations(&command.session_id)
        .await
        .unwrap());
    let fresh = fixture.adapter().await;
    assert!(fresh
        .apply_work_mutation(&command)
        .await
        .unwrap_err()
        .is_persistence_uncertain());
    assert_eq!(
        fresh.resolve_work_mutation(&command).await.unwrap(),
        WorkResolution::NotApplied
    );
    assert!(fresh
        .apply_work_mutation(&command)
        .await
        .unwrap_err()
        .to_string()
        .contains("sealed"));
    assert!(deliveries(&fresh).await.is_empty());
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT state FROM peri_op_ledger WHERE operation_id='session-work.before-send'"
        )
        .fetch_one(&fixture.pool)
        .await
        .unwrap(),
        "closed"
    );
    let late = fresh
        .store()
        .await
        .unwrap()
        .apply_qualified(&super::super::QualifiedMutation {
            identity: super::super::identity(&command).unwrap(),
            effects: vec![StatementSpec::new(
                "UPDATE threads SET title=?2 WHERE id=?1",
                vec![
                    Value::Text(command.session_id.clone()),
                    Value::Text("late apply".into()),
                ],
            )],
        })
        .await
        .unwrap();
    assert!(matches!(
        late,
        super::super::MutationOutcome::ClosedNeverApplied
    ));
    assert_eq!(
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT title FROM threads WHERE id='work-session'"
        )
        .fetch_one(&fixture.pool)
        .await
        .unwrap(),
        None
    );
}

#[tokio::test]
async fn workrecords_remote_row_sequence_and_conflicting_identity() {
    let fixture = Fixture::new().await;
    let adapter = fixture.adapter().await;
    let first = publication(&adapter, "first", "delivery-first").await;
    let second = publication(&adapter, "second", "delivery-second").await;
    assert_eq!(
        adapter.apply_work_mutation(&first).await.unwrap().decision,
        WorkDecision::Accepted
    );
    assert_eq!(
        adapter.apply_work_mutation(&second).await.unwrap().decision,
        WorkDecision::Accepted
    );
    let records = deliveries(&adapter).await;
    assert_eq!(
        records
            .iter()
            .map(|record| record.admission_sequence)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    let conflict = WorkCommand {
        mutation_id: first.mutation_id.clone(),
        ..second
    };
    assert!(adapter
        .apply_work_mutation(&conflict)
        .await
        .unwrap_err()
        .to_string()
        .contains("identity conflicts"));
    assert_eq!(deliveries(&adapter).await.len(), 2);
}

#[tokio::test]
async fn workrecords_remote_sql_limit_and_payload_on_demand() {
    let fixture = Fixture::new().await;
    let adapter = fixture.adapter().await;
    for index in 0..4 {
        let identity = format!("delivery-{index}");
        let command = publication(&adapter, &identity, &identity).await;
        adapter.apply_work_mutation(&command).await.unwrap();
    }
    sqlx::query("UPDATE session_payloads SET bytes=zeroblob(byte_length) WHERE kind='message'")
        .execute(&fixture.pool)
        .await
        .unwrap();
    let mut query = WorkQuery::new("work-session", WorkSelector::Inbox);
    query.limit = 2;
    let inspected = adapter.inspect_work(&query).await.unwrap();
    let cursor = inspected
        .next_cursor
        .clone()
        .expect("bounded page continuation");
    let WorkPage::Deliveries(records) = inspected.page else {
        panic!("missing typed page")
    };
    assert_eq!(records.len(), 2);
    query.cursor = Some(cursor);
    let continued = adapter.inspect_work(&query).await.unwrap();
    let WorkPage::Deliveries(remaining) = continued.page else {
        panic!("missing delivery continuation")
    };
    assert_eq!(remaining.len(), 2);
    assert!(remaining.iter().all(|remaining| {
        records
            .iter()
            .all(|record| record.delivery_id != remaining.delivery_id)
    }));
    let evidence = EvidenceQuery {
        session_id: "work-session".into(),
        reference: records[0].publication.event.content.content.clone(),
    };
    assert!(adapter
        .read_evidence(&evidence)
        .await
        .unwrap_err()
        .to_string()
        .contains("digest mismatch"));
    let missing = adapter
        .inspect_work(&WorkQuery::new(
            "work-session",
            WorkSelector::Delivery {
                delivery_id: "absent".into(),
            },
        ))
        .await
        .unwrap();
    assert_eq!(missing.page, WorkPage::Deliveries(Vec::new()));
}

#[tokio::test]
async fn workrecords_remote_history_append_compact_roundtrips_payload_reference() {
    let fixture = Fixture::new().await;
    let adapter = fixture.adapter().await;
    let payload = PersistedPayload::Message(BaseMessage::human("history payload"));
    adapter
        .append_history(&"work-session".into(), std::slice::from_ref(&payload))
        .await
        .unwrap();
    let loaded = adapter
        .load_session_history(&"work-session".into())
        .await
        .unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(
        peri_acp_types::store::serialize_persisted_payload(&loaded[0]).unwrap(),
        peri_acp_types::store::serialize_persisted_payload(&payload).unwrap()
    );
    let columns = sqlx::query("PRAGMA table_info(messages)")
        .fetch_all(&fixture.pool)
        .await
        .unwrap();
    let names = columns
        .iter()
        .map(|column| column.get::<String, _>("name"))
        .collect::<Vec<_>>();
    assert!(names.contains(&"content_ref".into()));
    assert!(!names.contains(&"content".into()));
    let stored: String =
        sqlx::query_scalar("SELECT content_ref FROM messages WHERE thread_id='work-session'")
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    let reference: PayloadRef = serde_json::from_str(&stored).unwrap();
    let evidence = adapter
        .read_evidence(&EvidenceQuery {
            session_id: "work-session".into(),
            reference,
        })
        .await
        .unwrap();
    let decoded = peri_acp_types::store::deserialize_persisted_payload(
        std::str::from_utf8(&evidence.bytes).unwrap(),
    )
    .unwrap();
    assert_eq!(
        peri_acp_types::store::serialize_persisted_payload(&decoded).unwrap(),
        peri_acp_types::store::serialize_persisted_payload(&payload).unwrap()
    );
    let summary = BaseMessage::ai("summary");
    let change = peri_acp_types::store::CompactionChange {
        flag_updates: Vec::new(),
        appended_messages: vec![summary.clone()],
    };
    adapter
        .apply_compaction(&"work-session".into(), &change)
        .await
        .unwrap();
    let loaded = adapter
        .load_session_history(&"work-session".into())
        .await
        .unwrap();
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded[0].id(), payload.id());
    assert_eq!(loaded[1].id(), summary.id());
    assert_eq!(
        peri_acp_types::store::serialize_persisted_payload(&loaded[1]).unwrap(),
        peri_acp_types::store::serialize_persisted_payload(&PersistedPayload::Message(summary))
            .unwrap()
    );
}

#[tokio::test]
async fn workrecords_remote_affected_row_guard_rolls_back_entire_qualified_batch() {
    use crate::sessions::work_store::{SqlParam, SqlStatement};
    let fixture = Fixture::new().await;
    let adapter = fixture.adapter().await;
    let command = publication(&adapter, "checked-update", "checked-delivery").await;
    let effects = super::super::specifications(vec![
        SqlStatement::new(
            "UPDATE threads SET title=?2 WHERE id=?1",
            vec![
                SqlParam::Text(command.session_id.clone()),
                SqlParam::Text("tentative".into()),
            ],
        ),
        SqlStatement::checked(
            "UPDATE threads SET title=?2 WHERE id=?1",
            vec![
                SqlParam::Text("missing-session".into()),
                SqlParam::Text("not applied".into()),
            ],
            1,
        ),
    ])
    .unwrap();
    let outcome = adapter
        .store()
        .await
        .unwrap()
        .apply_qualified(&super::super::QualifiedMutation {
            identity: super::super::identity(&command).unwrap(),
            effects,
        })
        .await
        .unwrap();
    assert!(matches!(
        outcome,
        super::super::MutationOutcome::NotApplied {
            rejected_statement: Some(3),
            ..
        }
    ));
    assert_eq!(
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT title FROM threads WHERE id='work-session'"
        )
        .fetch_one(&fixture.pool)
        .await
        .unwrap(),
        None
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM peri_op_ledger WHERE operation_id='session-work.checked-update'"
        )
        .fetch_one(&fixture.pool)
        .await
        .unwrap(),
        0
    );
}

#[tokio::test]
async fn workrecords_remote_close_and_tree_delete_preserve_closed_authority() {
    use peri_acp_types::session_resources::{CloseSettlement, ControlStatus};
    let fixture = Fixture::new().await;
    let adapter = fixture.adapter().await;
    let session = "work-session".to_owned();
    adapter.mark_session_closing(&session).await.unwrap();
    assert_eq!(
        adapter.load_session_control(&session).await.unwrap().status,
        ControlStatus::Closing
    );
    assert_eq!(
        adapter.close_settlement(&session).await.unwrap(),
        CloseSettlement::Pending
    );
    sqlx::query("DELETE FROM session_close_intents WHERE thread_id='work-session'")
        .execute(&fixture.pool)
        .await
        .unwrap();
    assert!(adapter.is_session_closing(&session).await.unwrap());
    adapter.finish_close(&session).await.unwrap();
    assert_eq!(
        adapter.close_settlement(&session).await.unwrap(),
        CloseSettlement::Finished
    );
    adapter.delete_tree(&session).await.unwrap();
    assert!(!adapter.session_exists(&session).await.unwrap());
    assert_eq!(
        adapter.load_session_control(&session).await.unwrap().status,
        ControlStatus::Closed
    );
    assert_eq!(
        adapter.close_settlement(&session).await.unwrap(),
        CloseSettlement::Finished
    );
}

#[tokio::test]
async fn workrecords_remote_unknown_blocks_destructive_history_and_tree_delete() {
    use peri_acp_types::session_resources::RewindBoundary;
    let fixture = Fixture::new().await;
    let adapter = fixture.adapter().await;
    let payload = PersistedPayload::Message(BaseMessage::human("preserve evidence"));
    let session = "work-session".to_owned();
    adapter
        .append_history(&session, std::slice::from_ref(&payload))
        .await
        .unwrap();
    let command = publication(&adapter, "blocked-mutation", "blocked-delivery").await;
    adapter
        .inject_faults(FaultPlan {
            drop_reply: None,
            drop_before_send: Some("session_work".into()),
        })
        .await;
    assert!(adapter
        .apply_work_mutation(&command)
        .await
        .unwrap_err()
        .is_persistence_uncertain());
    assert!(adapter
        .remove_history_entries(&session, &[payload.id()])
        .await
        .is_err());
    assert!(adapter
        .rewind_history(&session, RewindBoundary::RemoveFrom(payload.id()))
        .await
        .is_err());
    assert!(adapter.delete_tree(&session).await.is_err());
    let history = adapter.load_session_history(&session).await.unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].id(), payload.id());
    assert!(adapter.has_pending_work_mutations(&session).await.unwrap());
}
