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
            effective_arguments_json: arguments_json.clone(),
            arguments_json,
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
                            content: WorkPayload::from_payload(&PersistedPayload::Message(
                                BaseMessage::human("run"),
                            ))
                            .unwrap(),
                        },
                        purpose: DeliveryPurpose::UserInput,
                        policy: MessagePolicy::ensure_processing(),
                    },
                },
            )
            .await;
        let snapshot = fixture.snapshot().await;
        let candidate = &snapshot.candidates[0];
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
                    delivery_ids: snapshot.candidates[0].delivery_ids.clone(),
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
                        serialized_request: "{}".into(),
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
                    response: WorkPayload::from_payload(&PersistedPayload::Message(
                        BaseMessage::ai_with_tool_calls(
                            "run",
                            vec![ToolCallRequest::new(
                                "call-1",
                                "mcp__workspace__Bash",
                                fixture.input.clone(),
                            )],
                        ),
                    ))
                    .unwrap(),
                    dispatch_intents: vec![(*fixture.intent).clone()],
                    next_work_id: Some("act-1".into()),
                },
            )
            .await;
        let snapshot = fixture.snapshot().await;
        fixture
            .apply(
                "dispatch",
                WorkAction::BeginDispatch {
                    guard: Self::guard(&snapshot),
                    target: Self::target(&snapshot, "act-1"),
                    invocation_id: fixture.intent.invocation_id.clone(),
                },
            )
            .await;
        fixture.target = Self::target(&fixture.snapshot().await, "act-1");
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

    pub(crate) async fn snapshot(&self) -> WorkSnapshot {
        self.resources
            .load_session_work(&WorkQuery {
                session_id: "mcp-session".into(),
                limit: 64,
            })
            .await
            .unwrap()
    }

    async fn apply(&self, mutation_id: &str, action: WorkAction) {
        let receipt = self
            .resources
            .apply_work_mutation(
                &PreparedWorkCommand::try_new(WorkCommand {
                    session_id: "mcp-session".into(),
                    recipient_lifecycle: 1,
                    mutation_id: mutation_id.into(),
                    action,
                })
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(receipt.decision, WorkDecision::Accepted);
    }

    fn guard(snapshot: &WorkSnapshot) -> WorkGuard {
        WorkGuard {
            expected_revision: snapshot.state.revision,
            expected_control_generation: snapshot.control.control_generation,
            execution: snapshot.control.attempt.clone().unwrap(),
        }
    }

    fn target(snapshot: &WorkSnapshot, work_id: &str) -> WorkTarget {
        WorkTarget {
            work_id: work_id.into(),
            expected_work_revision: snapshot.state.works[work_id].revision,
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
    .unwrap();
    let before = fixture.snapshot().await;
    invocation.prepare().await.unwrap();
    let snapshot = fixture.snapshot().await;
    assert_eq!(snapshot.state, before.state);
    assert_eq!(
        snapshot.state.invocations["invocation-1"]
            .work_id
            .as_deref(),
        Some("act-1")
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
    assert_eq!(fixture.snapshot().await.state, before.state);
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
    .unwrap();
    invocation.prepare().await.unwrap();
    let binding = invocation.bind_task("owner-task-1").await.unwrap();
    assert_eq!(invocation.bind_task("owner-task-1").await.unwrap(), binding);
    assert!(invocation.bind_task("different-task").await.is_err());
    let cold = SessionResourcesImpl::open(fixture.directory.path().join("invocations.db"))
        .await
        .unwrap();
    let snapshot = cold
        .load_session_work(&WorkQuery {
            session_id: "mcp-session".into(),
            limit: 0,
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
    .unwrap();
    invocation.prepare().await.unwrap();
    invocation.record_outcome_unknown().await.unwrap();
    assert!(matches!(
        invocation.prepare().await,
        Err(InvocationError::OutcomeUnknown)
    ));
    assert!(fixture.snapshot().await.blocked);
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
    .is_err());
    assert!(
        McpInvocation::from_context(&context, &fixture.input, "other-tool", "owner-1").is_err()
    );
    assert!(McpInvocation::from_context(
        &context,
        &fixture.input,
        "mcp__workspace__Bash",
        "other-owner"
    )
    .is_err());
    let mut context = fixture.context();
    context.invocation_id = Some("another-invocation".into());
    assert!(McpInvocation::from_context(
        &context,
        &fixture.input,
        "mcp__workspace__Bash",
        "owner-1"
    )
    .is_err());
}
