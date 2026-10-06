use peri_acp_types::{
    messages::BaseMessage,
    session::{MessagePolicy, UserInput},
    session_resources::{
        work::*, FrozenSnapshotBytes, NewSession, NewSessionMeta, SessionResources,
    },
    store::{serialize_persisted_payload, PersistedPayload},
    workspace::SessionBinding,
};
use peri_resources::sessions::SessionResourcesImpl;
use sqlx::{sqlite::SqliteConnectOptions, Connection, SqliteConnection};
use tempfile::TempDir;

const SESSION: &str = "quick-acceptance";
const WITHDRAWAL_INPUT_ID: &str = "00000000-0000-4000-8000-000000000002";

async fn fixture() -> (TempDir, SessionResourcesImpl) {
    let directory = tempfile::tempdir().unwrap();
    let resources = SessionResourcesImpl::open(directory.path().join("fresh.db"))
        .await
        .unwrap();
    let workspace = resources.resolve_workspace(directory.path()).await.unwrap();
    resources
        .create_session(&NewSession {
            thread_id: SESSION.into(),
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
        session_id: SESSION.into(),
        recipient_lifecycle: 1,
        mutation_id: identity.into(),
        action,
    }
}

async fn inspect(resources: &dyn SessionResources, selector: WorkSelector) -> WorkInspection {
    resources
        .inspect_work(&WorkQuery::new(SESSION, selector))
        .await
        .unwrap()
}

async fn store_bytes(
    resources: &dyn SessionResources,
    identity: &str,
    bytes: Vec<u8>,
) -> PayloadRef {
    resources
        .prepare_evidence(&EvidenceWrite {
            session_id: SESSION.into(),
            storage_scope: SESSION.into(),
            payload_id: identity.into(),
            encoding: 1,
            bytes,
        })
        .await
        .unwrap()
}

async fn payload(
    resources: &dyn SessionResources,
    identity: &str,
    message: BaseMessage,
    role: &str,
) -> WorkPayload {
    let persisted = PersistedPayload::Message(message);
    let bytes = serialize_persisted_payload(&persisted)
        .unwrap()
        .into_bytes();
    WorkPayload {
        message_id: persisted.id(),
        role: role.into(),
        content: store_bytes(resources, identity, bytes).await,
        tool_call_id: None,
    }
}

async fn accept(resources: &dyn SessionResources, identity: &str, action: WorkAction) {
    let receipt = resources
        .apply_work_mutation(&command(identity, action))
        .await
        .unwrap();
    assert_eq!(receipt.decision, WorkDecision::Accepted, "{identity}");
}

async fn guard_and_target(resources: &dyn SessionResources) -> (WorkGuard, WorkTarget) {
    let snapshot = inspect(resources, WorkSelector::CurrentProcessing).await;
    let WorkPage::Processings(processings) = snapshot.page else {
        panic!("expected processing page");
    };
    assert_eq!(processings.len(), 1);
    (
        WorkGuard {
            expected_revision: snapshot.head.change_seq,
            expected_control_generation: snapshot.control.control_generation,
            execution: snapshot.control.attempt.unwrap(),
        },
        WorkTarget {
            work_id: processings[0].processing_id.clone(),
            expected_work_revision: processings[0].revision,
        },
    )
}

#[tokio::test]
async fn fresh_database_publish_claim_and_synthetic_response_settle_once() {
    let (_directory, resources) = fixture().await;
    let input = payload(
        &resources,
        "input",
        BaseMessage::human("quick input"),
        "user",
    )
    .await;
    accept(
        &resources,
        "publish",
        WorkAction::PublishDelivery {
            delivery: PublishDelivery {
                delivery_id: "delivery".into(),
                event: WorkEvent {
                    producer_namespace: "quick-smoke".into(),
                    event_id: "input-event".into(),
                    event_kind: "input".into(),
                    causation_id: None,
                    content: input,
                },
                purpose: DeliveryPurpose::UserInput,
                policy: MessagePolicy::ensure_processing(),
            },
        },
    )
    .await;
    let before = inspect(&resources, WorkSelector::Availability).await;
    let WorkPage::Availability(availability) = before.page else {
        panic!("expected availability page");
    };
    assert!(!availability.blocked && !availability.pending);
    let candidate = availability.candidates.first().unwrap();
    let execution = serde_json::from_value(serde_json::json!({
        "turnId": "00000000-0000-4000-8000-000000000001",
        "attemptId": "quick-synthetic-attempt"
    }))
    .unwrap();
    let admission = WorkAdmission {
        session_id: SESSION.into(),
        admission_id: "synthetic-sdk-ticket".into(),
        instance_id: "quick-instance".into(),
        generation_id: "quick-generation".into(),
        lifecycle: 1,
        control_generation: before.control.control_generation,
        work_id: candidate.work_id.clone(),
        work_revision: candidate.work_revision,
        execution,
    };
    accept(
        &resources,
        "register",
        WorkAction::RegisterAdmission {
            admission: admission.clone(),
        },
    )
    .await;
    let registered = inspect(&resources, WorkSelector::Head).await;
    accept(
        &resources,
        "claim",
        WorkAction::ClaimBatch {
            guard: WorkGuard {
                expected_revision: registered.head.change_seq,
                expected_control_generation: registered.control.control_generation,
                execution: admission.execution.clone(),
            },
            batch_id: candidate.work_id.clone(),
            delivery_ids: candidate.delivery_ids.clone(),
        },
    )
    .await;
    let request = store_bytes(
        &resources,
        "request",
        b"synthetic provider request".to_vec(),
    )
    .await;
    let (guard, target) = guard_and_target(&resources).await;
    accept(
        &resources,
        "begin-reason",
        WorkAction::BeginReason {
            guard,
            target,
            request_id: "request".into(),
            request: ReasonRequest {
                request_digest: request.sha256.clone(),
                payload: request,
                model_ref: "synthetic-no-provider".into(),
                authorization_ref: "synthetic-grant".into(),
            },
        },
    )
    .await;
    let response = payload(
        &resources,
        "response",
        BaseMessage::ai("synthetic answer"),
        "assistant",
    )
    .await;
    let (guard, target) = guard_and_target(&resources).await;
    let response_command = command(
        "commit-response",
        WorkAction::CommitReasonResponseAndDispatchIntent {
            guard,
            target,
            request_id: "request".into(),
            response: response.clone(),
            dispatch_intents: vec![],
            next_work_id: None,
        },
    );
    let receipt = resources
        .apply_work_mutation(&response_command)
        .await
        .unwrap();
    assert_eq!(receipt.decision, WorkDecision::Accepted);
    assert_eq!(
        resources
            .apply_work_mutation(&response_command)
            .await
            .unwrap(),
        receipt
    );
    let final_state = inspect(&resources, WorkSelector::Head).await;
    assert_eq!(final_state.head.required_count, 0);
    assert_eq!(final_state.head.unresolved_effects, 0);
    assert!(final_state.head.current_processing_id.is_none());
    let WorkPage::Processings(processings) = inspect(
        &resources,
        WorkSelector::Processing {
            processing_id: candidate.work_id.clone(),
        },
    )
    .await
    .page
    else {
        panic!("expected processing page");
    };
    assert_eq!(processings[0].stage, WorkStage::Settled);
    assert_eq!(processings[0].response.as_ref(), Some(&response));
    let connection_options =
        SqliteConnectOptions::new().filename(_directory.path().join("fresh.db"));
    let mut connection = SqliteConnection::connect_with(&connection_options)
        .await
        .unwrap();
    let response_rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM messages WHERE message_id=?1")
            .bind(response.message_id.as_uuid().to_string())
            .fetch_one(&mut connection)
            .await
            .unwrap();
    assert_eq!(response_rows, 1);
}

#[tokio::test]
async fn duplicate_receipt_keeps_one_draft_and_exact_original_evidence() {
    let (_directory, resources) = fixture().await;
    let content = store_bytes(&resources, "original-draft", b"original draft".to_vec()).await;
    let original = command(
        "stage-draft",
        WorkAction::StageUserInput {
            input_id: "draft".into(),
            content: content.clone(),
            command_id: "explicit-user-command".into(),
            fingerprint: 7,
        },
    );
    let first = resources.apply_work_mutation(&original).await.unwrap();
    assert_eq!(first.decision, WorkDecision::Accepted);
    assert_eq!(
        resources.apply_work_mutation(&original).await.unwrap(),
        first
    );
    let WorkPage::Drafts(drafts) = inspect(&resources, WorkSelector::Drafts).await.page else {
        panic!("expected draft page");
    };
    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].content, content);
    assert_eq!(drafts[0].status, StagedUserInputStatus::Queued);
    assert_eq!(
        resources
            .read_evidence(&EvidenceQuery {
                session_id: SESSION.into(),
                reference: content
            })
            .await
            .unwrap()
            .bytes,
        b"original draft"
    );
    let WorkPage::Commands(commands) = inspect(&resources, WorkSelector::PendingCommands)
        .await
        .page
    else {
        panic!("expected command page");
    };
    assert!(commands.is_empty());
}

#[tokio::test]
async fn sealed_original_cannot_be_reapplied_or_replaced() {
    let (_directory, resources) = fixture().await;
    let content = store_bytes(&resources, "sealed-input", b"sealed input".to_vec()).await;
    let original = command(
        "sealed",
        WorkAction::StageUserInput {
            input_id: "draft".into(),
            content,
            command_id: "user-command".into(),
            fingerprint: 9,
        },
    );
    assert_eq!(
        resources.resolve_work_mutation(&original).await.unwrap(),
        WorkResolution::NotApplied
    );
    assert!(resources.apply_work_mutation(&original).await.is_err());
    let replacement = command(
        "sealed",
        WorkAction::WithdrawStagedUserInput {
            input_id: "draft".into(),
            command_id: "different-command".into(),
            fingerprint: 10,
        },
    );
    assert!(resources.resolve_work_mutation(&replacement).await.is_err());
    let WorkPage::Drafts(drafts) = inspect(&resources, WorkSelector::Drafts).await.page else {
        panic!("expected draft page");
    };
    assert!(drafts.is_empty());
}

#[tokio::test]
async fn schema17_open_is_rejected_without_changing_original_blob() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("legacy.db");
    let options = SqliteConnectOptions::new()
        .filename(&database)
        .create_if_missing(true);
    let mut connection = SqliteConnection::connect_with(&options).await.unwrap();
    sqlx::query(
        "CREATE TABLE session_work_state(session_id TEXT PRIMARY KEY,state_json TEXT NOT NULL)",
    )
    .execute(&mut connection)
    .await
    .unwrap();
    let original = r#"{"syntheticEvidence":"original draft and unknown command"}"#;
    sqlx::query("INSERT INTO session_work_state VALUES ('legacy',?1)")
        .bind(original)
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query("PRAGMA user_version=17")
        .execute(&mut connection)
        .await
        .unwrap();
    connection.close().await.unwrap();
    let result = SessionResourcesImpl::open(&database).await;
    assert!(
        result.is_err(),
        "ordinary open must not auto-migrate schema 17"
    );
    let mut connection = SqliteConnection::connect_with(&options).await.unwrap();
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    let evidence: String =
        sqlx::query_scalar("SELECT state_json FROM session_work_state WHERE session_id='legacy'")
            .fetch_one(&mut connection)
            .await
            .unwrap();
    assert_eq!(version, 17);
    assert_eq!(evidence, original);
}

async fn publish_unclaimed_draft(resources: &dyn SessionResources) -> StagedUserInput {
    let source_input = UserInput {
        input_id: WITHDRAWAL_INPUT_ID.into(),
        content: "withdrawal original".into(),
        original_draft: "  withdrawal original  ".into(),
    };
    let mut source_envelope = serde_json::to_value(&source_input).unwrap();
    source_envelope
        .as_object_mut()
        .unwrap()
        .insert("enqueue_publication".into(), serde_json::Value::Null);
    let source_content = store_bytes(
        resources,
        "withdrawal-draft-source",
        serde_json::to_vec(&source_envelope).unwrap(),
    )
    .await;
    let input = payload(
        resources,
        "withdrawal-canonical-message",
        BaseMessage::Human {
            id: uuid::Uuid::parse_str(WITHDRAWAL_INPUT_ID).unwrap().into(),
            content: source_input.content,
        },
        "user",
    )
    .await;
    assert_ne!(source_content, input.content);
    accept(
        resources,
        "stage-withdrawal-input",
        WorkAction::StageUserInput {
            input_id: WITHDRAWAL_INPUT_ID.into(),
            content: source_content.clone(),
            command_id: "first-user-send".into(),
            fingerprint: 12,
        },
    )
    .await;
    let staged = withdrawal_draft(resources).await;
    let identity = UserInputPublicationIdentity {
        input_id: WITHDRAWAL_INPUT_ID.into(),
        publication_generation: "first-user-send".into(),
        fingerprint: 12,
        command_id: "first-user-send".into(),
        draft_binding: StagedUserInputPublicationBinding {
            draft_revision: staged.revision,
            draft_fingerprint: staged.fingerprint,
            canonical_content: input.content.clone(),
        },
    };
    let snapshot = inspect(resources, WorkSelector::Head).await;
    accept(
        resources,
        "publish-withdrawal-command",
        WorkAction::PublishStagedUserInputs {
            expected_revision: snapshot.head.change_seq,
            expected_control_generation: snapshot.control.control_generation,
            expected_attempt: snapshot.control.attempt,
            interrupt_current: false,
            deliveries: vec![PublishDelivery {
                delivery_id: "withdrawal-delivery".into(),
                event: WorkEvent {
                    producer_namespace: "quick-withdrawal".into(),
                    event_id: format!("user-input:{WITHDRAWAL_INPUT_ID}:first-user-send"),
                    event_kind: "userInput".into(),
                    causation_id: Some(serde_json::to_string(&identity).unwrap()),
                    content: input,
                },
                purpose: DeliveryPurpose::UserInput,
                policy: MessagePolicy::ensure_processing(),
            }],
        },
    )
    .await;
    let draft = withdrawal_draft(resources).await;
    assert_eq!(draft.status, StagedUserInputStatus::Published);
    assert_eq!(draft.publication_id.as_deref(), Some("withdrawal-delivery"));
    draft
}

async fn withdrawal_draft(resources: &dyn SessionResources) -> StagedUserInput {
    let WorkPage::Drafts(mut drafts) = inspect(
        resources,
        WorkSelector::Draft {
            input_id: WITHDRAWAL_INPUT_ID.into(),
        },
    )
    .await
    .page
    else {
        panic!("expected exact draft page");
    };
    assert_eq!(drafts.len(), 1);
    drafts.remove(0)
}

async fn assert_withdrawal_delivery_abandoned(resources: &dyn SessionResources) {
    let WorkPage::Deliveries(deliveries) = inspect(
        resources,
        WorkSelector::Delivery {
            delivery_id: "withdrawal-delivery".into(),
        },
    )
    .await
    .page
    else {
        panic!("expected exact delivery page");
    };
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].obligation, ObligationStatus::Abandoned);
    assert!(deliveries[0].disposition.is_some());
    assert!(deliveries[0].processing_id.is_none());
    assert_eq!(
        inspect(resources, WorkSelector::Head)
            .await
            .head
            .required_count,
        0
    );
}

#[tokio::test]
async fn ordinary_withdrawal_disposes_draft_and_resend_allocates_new_fifo_sequence() {
    let (directory, resources) = fixture().await;
    let original = publish_unclaimed_draft(&resources).await;
    let snapshot = inspect(&resources, WorkSelector::Head).await;
    accept(
        &resources,
        "ordinary-withdrawal",
        WorkAction::WithdrawDelivery {
            expected_revision: snapshot.head.change_seq,
            expected_control_generation: None,
            delivery_id: "withdrawal-delivery".into(),
            authorization_ref: "synthetic-user-withdrawal".into(),
        },
    )
    .await;
    let withdrawn = withdrawal_draft(&resources).await;
    assert_eq!(withdrawn.status, StagedUserInputStatus::Withdrawn);
    assert_eq!(withdrawn.content, original.content);
    assert!(withdrawn.publication_id.is_none());
    assert_withdrawal_delivery_abandoned(&resources).await;
    accept(
        &resources,
        "restage-new-mutation",
        WorkAction::StageUserInput {
            input_id: WITHDRAWAL_INPUT_ID.into(),
            content: original.content.clone(),
            command_id: "second-user-send".into(),
            fingerprint: 13,
        },
    )
    .await;
    let restaged = withdrawal_draft(&resources).await;
    assert_eq!(restaged.status, StagedUserInputStatus::Queued);
    assert_eq!(restaged.content, original.content);
    assert!(restaged.sequence > original.sequence);
    assert!(restaged.publication_generation > original.publication_generation);
    let options = SqliteConnectOptions::new().filename(directory.path().join("fresh.db"));
    let mut connection = SqliteConnection::connect_with(&options).await.unwrap();
    let stored_sequence: i64 = sqlx::query_scalar(
        "SELECT fifo_seq FROM session_inputs WHERE session_id=?1 AND input_id=?2",
    )
    .bind(SESSION)
    .bind(WITHDRAWAL_INPUT_ID)
    .fetch_one(&mut connection)
    .await
    .unwrap();
    assert_eq!(u64::try_from(stored_sequence).unwrap(), restaged.sequence);
}

#[tokio::test]
async fn stop_withdrawal_requeues_draft_and_abandons_original_unclaimed_delivery() {
    let (_directory, resources) = fixture().await;
    let original = publish_unclaimed_draft(&resources).await;
    let original_bytes = resources
        .read_evidence(&EvidenceQuery {
            session_id: SESSION.into(),
            reference: original.content.clone(),
        })
        .await
        .unwrap()
        .bytes;
    let snapshot = inspect(&resources, WorkSelector::Head).await;
    accept(
        &resources,
        "stop-withdrawal",
        WorkAction::WithdrawDelivery {
            expected_revision: snapshot.head.change_seq,
            expected_control_generation: Some(snapshot.control.control_generation),
            delivery_id: "withdrawal-delivery".into(),
            authorization_ref: "synthetic-stop".into(),
        },
    )
    .await;
    let requeued = withdrawal_draft(&resources).await;
    assert_eq!(requeued.status, StagedUserInputStatus::Queued);
    assert_eq!(requeued.content, original.content);
    assert!(requeued.publication_id.is_none());
    assert_withdrawal_delivery_abandoned(&resources).await;
    assert_eq!(
        resources
            .read_evidence(&EvidenceQuery {
                session_id: SESSION.into(),
                reference: original.content,
            })
            .await
            .unwrap()
            .bytes,
        original_bytes
    );
}
