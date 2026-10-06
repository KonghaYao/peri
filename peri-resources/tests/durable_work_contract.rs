use peri_acp_types::{
    messages::BaseMessage,
    session::MessagePolicy,
    session_resources::{
        work::*, FrozenSnapshotBytes, NewSession, NewSessionMeta, SessionResources,
    },
    store::{serialize_persisted_payload, PersistedPayload},
    workspace::SessionBinding,
};
use peri_resources::sessions::SessionResourcesImpl;
use tempfile::TempDir;

async fn fixture() -> (TempDir, SessionResourcesImpl) {
    let directory = tempfile::tempdir().unwrap();
    let resources = SessionResourcesImpl::open(directory.path().join("work.db"))
        .await
        .unwrap();
    let workspace = resources.resolve_workspace(directory.path()).await.unwrap();
    resources
        .create_session(&NewSession {
            thread_id: "work-session".into(),
            created_at: "2026-10-06T00:00:00Z".into(),
            meta: NewSessionMeta {
                title: None,
                cwd: workspace.cwd.to_string_lossy().into_owned(),
                parent_thread_id: None,
                hidden: false,
                cancel_policy: Default::default(),
                snapshot_at_message_id: None,
            },
            binding: SessionBinding::from_workspace(&workspace),
            frozen: FrozenSnapshotBytes::new(r#"{"v":1}"#),
        })
        .await
        .unwrap();
    (directory, resources)
}

fn command(identity: &str, action: WorkAction) -> WorkCommand {
    WorkCommand {
        session_id: "work-session".into(),
        recipient_lifecycle: 1,
        mutation_id: identity.into(),
        action,
    }
}

async fn evidence(resources: &dyn SessionResources, identity: &str) -> PayloadRef {
    resources
        .prepare_evidence(&EvidenceWrite {
            session_id: "work-session".into(),
            storage_scope: "work-session".into(),
            payload_id: identity.into(),
            encoding: 1,
            bytes: identity.as_bytes().to_vec(),
        })
        .await
        .unwrap()
}

async fn inspect(resources: &dyn SessionResources, selector: WorkSelector) -> WorkInspection {
    resources
        .inspect_work(&WorkQuery::new("work-session", selector))
        .await
        .unwrap()
}

#[tokio::test]
async fn immutable_evidence_and_draft_have_independent_bounded_reads() {
    let (_directory, resources) = fixture().await;
    let reference = evidence(&resources, "original-input").await;
    let stage = command(
        "stage",
        WorkAction::StageUserInput {
            input_id: "input".into(),
            content: reference.clone(),
            command_id: "draft-command".into(),
            fingerprint: 1,
        },
    );
    let receipt = resources.apply_work_mutation(&stage).await.unwrap();
    assert_eq!(receipt.decision, WorkDecision::Accepted);
    let inspection = inspect(
        &resources,
        WorkSelector::Draft {
            input_id: "input".into(),
        },
    )
    .await;
    let WorkPage::Drafts(drafts) = inspection.page else {
        panic!("draft page expected");
    };
    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].content, reference);
    let read = resources
        .read_evidence(&EvidenceQuery {
            session_id: "work-session".into(),
            reference: reference.clone(),
        })
        .await
        .unwrap();
    assert_eq!(read.bytes, b"original-input");
    assert_eq!(
        resources.apply_work_mutation(&stage).await.unwrap(),
        receipt
    );
    let stored = inspect(
        &resources,
        WorkSelector::Command {
            mutation_id: stage.mutation_id.clone(),
        },
    )
    .await;
    let WorkPage::Commands(commands) = stored.page else {
        panic!("command page expected");
    };
    assert_eq!(commands[0].command, stage);
    assert!(!commands[0].pending);
    assert!(matches!(
        commands[0].resolution,
        Some(WorkResolution::Applied { .. })
    ));
    let withdraw = command(
        "withdraw",
        WorkAction::WithdrawStagedUserInput {
            input_id: "input".into(),
            command_id: "withdraw-command".into(),
            fingerprint: 2,
        },
    );
    assert_eq!(
        resources
            .apply_work_mutation(&withdraw)
            .await
            .unwrap()
            .decision,
        WorkDecision::Accepted
    );
    let WorkPage::Drafts(drafts) = inspect(&resources, WorkSelector::Drafts).await.page else {
        panic!("draft page expected");
    };
    assert!(drafts.is_empty());
    assert!(resources
        .read_evidence(&EvidenceQuery {
            session_id: "work-session".into(),
            reference
        })
        .await
        .is_ok());
}

#[tokio::test]
async fn published_content_is_a_reference_and_inbox_is_keyset_paged() {
    let (_directory, resources) = fixture().await;
    for index in 0..3 {
        let persisted = PersistedPayload::Message(BaseMessage::human(format!("input {index}")));
        let serialized = serialize_persisted_payload(&persisted).unwrap();
        let content = resources
            .prepare_evidence(&EvidenceWrite {
                session_id: "work-session".into(),
                storage_scope: "work-session".into(),
                payload_id: format!("message-{index}"),
                encoding: 1,
                bytes: serialized.into_bytes(),
            })
            .await
            .unwrap();
        let delivery = PublishDelivery {
            delivery_id: format!("delivery-{index}"),
            event: WorkEvent {
                producer_namespace: "contract".into(),
                event_id: format!("event-{index}"),
                event_kind: "input".into(),
                causation_id: None,
                content: WorkPayload {
                    message_id: persisted.id(),
                    role: "user".into(),
                    content,
                    tool_call_id: None,
                },
            },
            purpose: DeliveryPurpose::UserInput,
            policy: MessagePolicy::ensure_processing(),
        };
        assert_eq!(
            resources
                .apply_work_mutation(&command(
                    &format!("publish-{index}"),
                    WorkAction::PublishDelivery { delivery }
                ))
                .await
                .unwrap()
                .decision,
            WorkDecision::Accepted
        );
    }
    let mut query = WorkQuery::new("work-session", WorkSelector::Inbox);
    query.limit = 2;
    let first = resources.inspect_work(&query).await.unwrap();
    let WorkPage::Deliveries(records) = first.page else {
        panic!("delivery page expected");
    };
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].delivery_id, "delivery-0");
    query.cursor = first.next_cursor;
    let next = resources.inspect_work(&query).await.unwrap();
    let WorkPage::Deliveries(records) = next.page else {
        panic!("delivery page expected");
    };
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].delivery_id, "delivery-2");
    assert!(next.next_cursor.is_none());
}

#[tokio::test]
async fn absent_mutation_requires_final_seal_and_identity_conflicts_stay_closed() {
    let (_directory, resources) = fixture().await;
    let reference = evidence(&resources, "seal-input").await;
    let original = command(
        "sealed",
        WorkAction::StageUserInput {
            input_id: "input".into(),
            content: reference,
            command_id: "draft".into(),
            fingerprint: 1,
        },
    );
    assert_eq!(
        resources.resolve_work_mutation(&original).await.unwrap(),
        WorkResolution::NotApplied
    );
    assert!(resources.apply_work_mutation(&original).await.is_err());
    let mut conflicting = original.clone();
    conflicting.action = WorkAction::WithdrawStagedUserInput {
        input_id: "input".into(),
        command_id: "withdraw".into(),
        fingerprint: 2,
    };
    assert!(resources.resolve_work_mutation(&conflicting).await.is_err());
    let WorkPage::Drafts(drafts) = inspect(&resources, WorkSelector::Drafts).await.page else {
        panic!("draft page expected");
    };
    assert!(drafts.is_empty());
}
