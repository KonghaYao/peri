use super::*;
use peri_acp_types::identity::AttemptId;
use peri_acp_types::messages::{BaseMessage, ToolCallRequest};
use peri_acp_types::session::{MessagePolicy, TurnId};
use peri_acp_types::session_resources::work::*;
use peri_acp_types::session_resources::{
    ControlAttempt, FrozenSnapshotBytes, NewSession, NewSessionMeta,
};
use peri_acp_types::store::PersistedPayload;
use peri_acp_types::workspace::SessionBinding;
use peri_resources::sessions::SessionResourcesImpl;

pub(crate) struct Fixture {
    pub directory: tempfile::TempDir,
    pub resources: Arc<dyn SessionResources>,
    pub intent: Arc<InvocationIntent>,
    pub input: Value,
    pub target: WorkTarget,
}

impl Fixture {
    pub(crate) async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let resources: Arc<dyn SessionResources> = Arc::new(
            SessionResourcesImpl::open(directory.path().join("invocations.db"))
                .await
                .unwrap(),
        );
        let workspace = resources.resolve_workspace(directory.path()).await.unwrap();
        resources
            .create_session(&NewSession {
                thread_id: "mcp-session".into(),
                created_at: peri_time::now_utc_rfc3339(),
                meta: NewSessionMeta {
                    title: None,
                    cwd: workspace.cwd.to_string_lossy().into_owned(),
                    parent_thread_id: None,
                    hidden: false,
                    cancel_policy: Default::default(),
                    snapshot_at_message_id: None,
                },
                binding: SessionBinding::from_workspace(&workspace),
                frozen: FrozenSnapshotBytes::new("{\"version\":1}"),
            })
            .await
            .unwrap();
        let input = serde_json::json!({"command":"sleep 30"});
        let arguments_json = serde_json::to_string(&input).unwrap();
        let intent = Arc::new(InvocationIntent {
            invocation_id: "invocation-1".into(),
            tool_call_id: "call-1".into(),
            tool_name: "mcp__workspace__Bash".into(),
            arguments_digest: format!("{:x}", Sha256::digest(arguments_json.as_bytes())),
            effective_tool_name: "mcp__workspace__Bash".into(),
            effective_arguments_digest: format!("{:x}", Sha256::digest(arguments_json.as_bytes())),
            effective_arguments: resources
                .prepare_evidence(&EvidenceWrite {
                    session_id: "mcp-session".into(),
                    storage_scope: "mcp-session".into(),
                    payload_id: "invocation-arguments".into(),
                    encoding: 1,
                    bytes: arguments_json.as_bytes().to_vec(),
                })
                .await
                .unwrap(),
            arguments: resources
                .prepare_evidence(&EvidenceWrite {
                    session_id: "mcp-session".into(),
                    storage_scope: "mcp-session".into(),
                    payload_id: "invocation-arguments".into(),
                    encoding: 1,
                    bytes: arguments_json.as_bytes().to_vec(),
                })
                .await
                .unwrap(),
            owner_identity: "owner-1".into(),
            scope_id: "mcp-session".into(),
            scope_epoch: Some(7),
            authorization_ref: "policy-1".into(),
            recovery_locator: "trusted-connection-1".into(),
        });
        let mut fixture = Self {
            directory,
            resources,
            intent,
            input,
            target: WorkTarget {
                work_id: "act-1".into(),
                expected_work_revision: 0,
            },
        };
        fixture
            .apply(
                "publish",
                WorkAction::PublishDelivery {
                    delivery: PublishDelivery {
                        delivery_id: "delivery-1".into(),
                        event: WorkEvent {
                            producer_namespace: "trusted-fixture".into(),
                            event_id: "event-1".into(),
                            event_kind: "request".into(),
                            causation_id: None,
                            content: fixture
                                .payload(&PersistedPayload::Message(BaseMessage::human("run")))
                                .await,
                        },
                        purpose: DeliveryPurpose::UserInput,
                        policy: MessagePolicy::ensure_processing(),
                    },
                },
            )
            .await;
        let snapshot = fixture
            .resources
            .inspect_work(&WorkQuery::new("mcp-session", WorkSelector::Availability))
            .await
            .unwrap();
        let WorkPage::Availability(availability) = &snapshot.page else {
            panic!("availability expected")
        };
        let candidate = &availability.candidates[0];
        let delivery_ids = candidate.delivery_ids.clone();
        let admission = WorkAdmission {
            session_id: "mcp-session".into(),
            admission_id: "fixture-admission".into(),
            instance_id: "fixture-sdk".into(),
            generation_id: "fixture-generation".into(),
            lifecycle: 1,
            control_generation: snapshot.control.control_generation,
            work_id: candidate.work_id.clone(),
            work_revision: candidate.work_revision,
            execution: ControlAttempt {
                turn_id: TurnId::new(),
                attempt_id: AttemptId::new(),
            },
        };
        fixture
            .apply(
                "enter",
                WorkAction::RegisterAdmission {
                    admission: admission.clone(),
                },
            )
            .await;
        let snapshot = fixture.snapshot().await;
        fixture
            .apply(
                "claim",
                WorkAction::ClaimBatch {
                    guard: Self::guard(&snapshot),
                    batch_id: admission.work_id.clone(),
                    delivery_ids,
                },
            )
            .await;
        let snapshot = fixture.snapshot().await;
        fixture
            .apply(
                "reason",
                WorkAction::BeginReason {
                    guard: Self::guard(&snapshot),
                    target: Self::target(&snapshot, &admission.work_id),
                    request_id: "request-1".into(),
                    request: ReasonRequest {
                        payload: fixture
                            .resources
                            .prepare_evidence(&EvidenceWrite {
                                session_id: "mcp-session".into(),
                                storage_scope: "mcp-session".into(),
                                payload_id: "reason-request".into(),
                                encoding: 1,
                                bytes: b"{}".to_vec(),
                            })
                            .await
                            .unwrap(),
                        request_digest: format!("{:x}", Sha256::digest(b"{}")),
                        model_ref: "fixture-model".into(),
                        authorization_ref: "policy-1".into(),
                    },
                },
            )
            .await;
        let snapshot = fixture.snapshot().await;
        fixture
            .apply(
                "response",
                WorkAction::CommitReasonResponseAndDispatchIntent {
                    guard: Self::guard(&snapshot),
                    target: Self::target(&snapshot, &admission.work_id),
                    request_id: "request-1".into(),
                    response: fixture
                        .payload(&PersistedPayload::Message(BaseMessage::ai_with_tool_calls(
                            "run",
                            vec![ToolCallRequest::new(
                                "call-1",
                                "mcp__workspace__Bash",
                                fixture.input.clone(),
                            )],
                        )))
                        .await,
                    dispatch_intents: vec![(*fixture.intent).clone()],
                    next_work_id: Some(admission.work_id.clone()),
                },
            )
            .await;
        let snapshot = fixture.snapshot().await;
        fixture
            .apply(
                "dispatch",
                WorkAction::BeginDispatch {
                    guard: Self::guard(&snapshot),
                    target: Self::target(&snapshot, &admission.work_id),
                    invocation_id: fixture.intent.invocation_id.clone(),
                    expected_effect_revision: 0,
                },
            )
            .await;
        fixture.target = Self::target(&fixture.snapshot().await, &admission.work_id);
        fixture
    }

    pub(crate) fn context(&self) -> ToolContext<'_> {
        let mut context = ToolContext::new(&[], "/tmp")
            .with_session_identity("mcp-session", "fixture-turn")
            .with_work_invocation(
                1,
                self.intent.clone(),
                self.target.clone(),
                self.resources.clone(),
            );
        context.invocation_id = Some(self.intent.invocation_id.clone());
        context
    }

    pub(crate) async fn snapshot(&self) -> WorkInspection {
        self.resources
            .inspect_work(&WorkQuery {
                session_id: "mcp-session".into(),
                selector: WorkSelector::CurrentProcessing,
                limit: 1,
                cursor: None,
            })
            .await
            .unwrap()
    }

    async fn payload(&self, payload: &PersistedPayload) -> WorkPayload {
        let role = if matches!(
            payload,
            PersistedPayload::Message(BaseMessage::Human { .. })
        ) {
            "user"
        } else {
            "assistant"
        };
        let content = self
            .resources
            .prepare_evidence(&EvidenceWrite {
                session_id: "mcp-session".into(),
                storage_scope: "mcp-session".into(),
                payload_id: payload.id().as_uuid().to_string(),
                encoding: 1,
                bytes: peri_acp_types::store::serialize_persisted_payload(payload)
                    .unwrap()
                    .into_bytes(),
            })
            .await
            .unwrap();
        WorkPayload {
            message_id: payload.id(),
            role: role.into(),
            content,
            tool_call_id: None,
        }
    }

    async fn apply(&self, mutation_id: &str, action: WorkAction) {
        let receipt = self
            .resources
            .apply_work_mutation(&WorkCommand {
                session_id: "mcp-session".into(),
                recipient_lifecycle: 1,
                mutation_id: mutation_id.into(),
                action,
            })
            .await
            .unwrap();
        assert_eq!(receipt.decision, WorkDecision::Accepted);
    }

    fn guard(snapshot: &WorkInspection) -> WorkGuard {
        WorkGuard {
            expected_revision: snapshot.head.change_seq,
            expected_control_generation: snapshot.control.control_generation,
            execution: snapshot.control.attempt.clone().unwrap(),
        }
    }

    fn target(snapshot: &WorkInspection, work_id: &str) -> WorkTarget {
        WorkTarget {
            work_id: work_id.into(),
            expected_work_revision: match &snapshot.page {
                WorkPage::Processings(records) => {
                    records
                        .iter()
                        .find(|record| record.processing_id == work_id)
                        .unwrap()
                        .revision
                }
                _ => panic!("processing page expected"),
            },
        }
    }
}

#[tokio::test]
async fn real_sql_prepare_preserves_same_work_link_and_wire_identity() {
    let fixture = Fixture::new().await;
    let invocation = McpInvocation::from_context(
        &fixture.context(),
        &fixture.input,
        "mcp__workspace__Bash",
        "owner-1",
    )
    .await
    .unwrap();
    let before = fixture.snapshot().await;
    invocation.prepare().await.unwrap();
    let snapshot = fixture.snapshot().await;
    assert_eq!(snapshot.page, before.page);
    let effect_page = fixture
        .resources
        .inspect_work(&WorkQuery::new(
            "mcp-session",
            WorkSelector::Effect {
                invocation_id: "invocation-1".into(),
            },
        ))
        .await
        .unwrap();
    let WorkPage::Effects(effects) = effect_page.page else {
        panic!("effect page expected")
    };
    assert_eq!(
        effects[0].processing_id.as_deref(),
        Some(fixture.target.work_id.as_str())
    );
    let metadata = invocation.request_meta(None).unwrap();
    assert_eq!(
        metadata.0 .0[INVOCATION_META_KEY]["invocationId"],
        "invocation-1"
    );
    assert_eq!(
        metadata.0 .0[INVOCATION_META_KEY]["initiatorSessionId"],
        "mcp-session"
    );
}

#[tokio::test]
async fn concurrent_dispatch_validation_does_not_mutate_session_revision() {
    let fixture = Fixture::new().await;
    let invocation = Arc::new(
        McpInvocation::from_context(
            &fixture.context(),
            &fixture.input,
            "mcp__workspace__Bash",
            "owner-1",
        )
        .await
        .unwrap(),
    );
    let before = fixture.snapshot().await;
    let mut validations = tokio::task::JoinSet::new();
    for _ in 0..16 {
        let invocation = Arc::clone(&invocation);
        validations.spawn(async move { invocation.prepare().await });
    }
    while let Some(result) = validations.join_next().await {
        result.unwrap().unwrap();
    }
    assert_eq!(fixture.snapshot().await.page, before.page);
}

#[tokio::test]
async fn real_sql_task_binding_ack_survives_cold_reload() {
    let fixture = Fixture::new().await;
    let invocation = McpInvocation::from_context(
        &fixture.context(),
        &fixture.input,
        "mcp__workspace__Bash",
        "owner-1",
    )
    .await
    .unwrap();
    invocation.prepare().await.unwrap();
    let binding = invocation.bind_task("owner-task-1").await.unwrap();
    assert_eq!(invocation.bind_task("owner-task-1").await.unwrap(), binding);
    assert!(invocation.bind_task("different-task").await.is_err());
    let cold = SessionResourcesImpl::open(fixture.directory.path().join("invocations.db"))
        .await
        .unwrap();
    let snapshot = cold
        .inspect_work(&WorkQuery {
            session_id: "mcp-session".into(),
            selector: WorkSelector::TaskBinding {
                owner_identity: "owner-1".into(),
                owner_task_id: "owner-task-1".into(),
            },
            limit: 1,
            cursor: None,
        })
        .await
        .unwrap();
    let recovered = super::super::invocation_recovery::recover_task_binding(
        &snapshot,
        "owner-1",
        "owner-task-1",
    )
    .unwrap();
    assert_eq!(recovered.binding, binding);
}

#[tokio::test]
async fn real_sql_unknown_response_blocks_replay_of_original_rpc() {
    let fixture = Fixture::new().await;
    let invocation = McpInvocation::from_context(
        &fixture.context(),
        &fixture.input,
        "mcp__workspace__Bash",
        "owner-1",
    )
    .await
    .unwrap();
    invocation.prepare().await.unwrap();
    invocation.record_outcome_unknown().await.unwrap();
    assert!(matches!(
        invocation.prepare().await,
        Err(InvocationError::OutcomeUnknown)
    ));
    let availability = fixture
        .resources
        .inspect_work(&WorkQuery::new("mcp-session", WorkSelector::Availability))
        .await
        .unwrap();
    let WorkPage::Availability(facts) = availability.page else {
        panic!("availability expected")
    };
    assert!(facts.blocked);
}

#[tokio::test]
async fn identity_or_arguments_cannot_be_reinterpreted_after_prepare() {
    let fixture = Fixture::new().await;
    let context = fixture.context();
    assert!(McpInvocation::from_context(
        &context,
        &serde_json::json!({"command":"other"}),
        "mcp__workspace__Bash",
        "owner-1"
    )
    .await
    .is_err());
    assert!(
        McpInvocation::from_context(&context, &fixture.input, "other-tool", "owner-1")
            .await
            .is_err()
    );
    assert!(McpInvocation::from_context(
        &context,
        &fixture.input,
        "mcp__workspace__Bash",
        "other-owner"
    )
    .await
    .is_err());
    let mut context = fixture.context();
    context.invocation_id = Some("another-invocation".into());
    assert!(McpInvocation::from_context(
        &context,
        &fixture.input,
        "mcp__workspace__Bash",
        "owner-1"
    )
    .await
    .is_err());
}
