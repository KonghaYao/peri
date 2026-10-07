use std::sync::Arc;

use peri_acp_types::identity::AttemptId;
use peri_acp_types::messages::{BaseMessage, ToolCallRequest};
use peri_acp_types::session::{MessagePolicy, TurnId};
use peri_acp_types::session_resources::work::*;
use peri_acp_types::session_resources::{
    ControlAttempt, FrozenSnapshotBytes, NewSession, NewSessionMeta, SessionResources,
};
use peri_acp_types::store::PersistedPayload;
use peri_acp_types::tools::ToolContext;
use peri_acp_types::workspace::SessionBinding;
use peri_resources::sessions::SessionResourcesImpl;
use serde_json::Value;
use sha2::{Digest, Sha256};

pub(super) const FIXTURE_SESSION_ID: &str = "builtin-runtime-session";

pub(super) struct InvocationFixture {
    _directory: tempfile::TempDir,
    resources: Arc<dyn SessionResources>,
    intents: Vec<Arc<InvocationIntent>>,
    target: WorkTarget,
}

impl InvocationFixture {
    pub(super) async fn new(tool: &str, inputs: &[Value]) -> Self {
        let calls = inputs
            .iter()
            .cloned()
            .map(|input| (tool, input))
            .collect::<Vec<_>>();
        Self::new_calls(&calls).await
    }

    pub(super) async fn new_calls(calls: &[(&str, Value)]) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let resources: Arc<dyn SessionResources> = Arc::new(
            SessionResourcesImpl::open(directory.path().join("invocations.db"))
                .await
                .unwrap(),
        );
        let workspace = resources.resolve_workspace(directory.path()).await.unwrap();
        resources
            .create_session(&NewSession {
                thread_id: FIXTURE_SESSION_ID.into(),
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
        let intents = calls
            .iter()
            .enumerate()
            .map(|(index, (tool, input))| {
                let arguments_json = serde_json::to_string(input).unwrap();
                let digest = format!("{:x}", Sha256::digest(arguments_json.as_bytes()));
                Arc::new(InvocationIntent {
                    invocation_id: format!("invocation-{index}"),
                    tool_call_id: format!("call-{index}"),
                    tool_name: (*tool).into(),
                    arguments_digest: digest.clone(),
                    effective_tool_name: (*tool).into(),
                    effective_arguments_digest: digest,
                    effective_arguments_json: arguments_json.clone(),
                    arguments_json,
                    owner_identity: "fixture-owner".into(),
                    scope_id: FIXTURE_SESSION_ID.into(),
                    scope_epoch: None,
                    authorization_ref: "fixture-policy".into(),
                    recovery_locator: "fixture-link".into(),
                })
            })
            .collect::<Vec<_>>();
        let mut fixture = Self {
            _directory: directory,
            resources,
            intents,
            target: WorkTarget {
                work_id: "act-work".into(),
                expected_work_revision: 0,
            },
        };
        fixture
            .apply(
                "publish",
                WorkAction::PublishDelivery {
                    delivery: PublishDelivery {
                        delivery_id: "delivery".into(),
                        event: WorkEvent {
                            producer_namespace: "fixture".into(),
                            event_id: "event".into(),
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
        fixture
            .apply(
                "admit",
                WorkAction::RegisterAdmission {
                    admission: WorkAdmission {
                        session_id: FIXTURE_SESSION_ID.into(),
                        admission_id: "admission".into(),
                        instance_id: "fixture-sdk".into(),
                        generation_id: "fixture-generation".into(),
                        lifecycle: 1,
                        control_generation: snapshot.control.control_generation,
                        work_id: snapshot.candidates[0].work_id.clone(),
                        work_revision: snapshot.candidates[0].work_revision,
                        execution: ControlAttempt {
                            turn_id: TurnId::new(),
                            attempt_id: AttemptId::new(),
                        },
                    },
                },
            )
            .await;
        let snapshot = fixture.snapshot().await;
        fixture
            .apply(
                "claim",
                WorkAction::ClaimBatch {
                    guard: Self::guard(&snapshot),
                    batch_id: snapshot.candidates[0].work_id.clone(),
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
                    target: Self::target(&snapshot, &snapshot.candidates[0].work_id),
                    request_id: "request".into(),
                    request: ReasonRequest {
                        serialized_request: "{}".into(),
                        request_digest: format!("{:x}", Sha256::digest(b"{}")),
                        model_ref: "fixture-model".into(),
                        authorization_ref: "fixture-policy".into(),
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
                    target: Self::target(&snapshot, &snapshot.candidates[0].work_id),
                    request_id: "request".into(),
                    response: WorkPayload::from_payload(&PersistedPayload::Message(
                        BaseMessage::ai_with_tool_calls(
                            "run",
                            fixture
                                .intents
                                .iter()
                                .enumerate()
                                .map(|(index, intent)| {
                                    ToolCallRequest::new(
                                        &intent.tool_call_id,
                                        &intent.tool_name,
                                        calls[index].1.clone(),
                                    )
                                })
                                .collect(),
                        ),
                    ))
                    .unwrap(),
                    dispatch_intents: fixture
                        .intents
                        .iter()
                        .map(|intent| (**intent).clone())
                        .collect(),
                    next_work_id: Some("act-work".into()),
                },
            )
            .await;
        for (index, intent) in fixture.intents.iter().enumerate() {
            let snapshot = fixture.snapshot().await;
            fixture
                .apply(
                    &format!("dispatch-{index}"),
                    WorkAction::BeginDispatch {
                        guard: Self::guard(&snapshot),
                        target: Self::target(&snapshot, "act-work"),
                        invocation_id: intent.invocation_id.clone(),
                    },
                )
                .await;
        }
        fixture.target = Self::target(&fixture.snapshot().await, "act-work");
        fixture
    }

    pub(super) fn context(&self, index: usize) -> ToolContext<'_> {
        let intent = &self.intents[index];
        let mut context = ToolContext::new(&[], "/tmp")
            .with_session_identity(FIXTURE_SESSION_ID, "fixture-turn")
            .with_work_invocation(
                1,
                intent.clone(),
                self.target.clone(),
                self.resources.clone(),
            );
        context.invocation_id = Some(intent.invocation_id.clone());
        context
    }

    async fn snapshot(&self) -> WorkSnapshot {
        self.resources
            .load_session_work(&WorkQuery {
                session_id: FIXTURE_SESSION_ID.into(),
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
                    session_id: FIXTURE_SESSION_ID.into(),
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
