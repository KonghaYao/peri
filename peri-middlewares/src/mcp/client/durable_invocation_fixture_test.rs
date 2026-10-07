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

pub(crate) struct DurableInvocationFixture {
    _directory: tempfile::TempDir,
    pub(crate) resources: Arc<dyn SessionResources>,
    session_id: String,
    intents: Vec<Arc<InvocationIntent>>,
    target: WorkTarget,
}

impl DurableInvocationFixture {
    pub(crate) async fn new(session_id: &str, tool: &str, inputs: &[Value]) -> Self {
        Self::new_with_owner(session_id, tool, inputs, "fixture-owner", None).await
    }

    async fn new_with_owner(
        session_id: &str,
        tool: &str,
        inputs: &[Value],
        owner_identity: &str,
        scope_epoch: Option<u64>,
    ) -> Self {
        assert!(!inputs.is_empty());
        let directory = tempfile::tempdir().unwrap();
        let resources: Arc<dyn SessionResources> = Arc::new(
            SessionResourcesImpl::open(directory.path().join("invocations.db"))
                .await
                .unwrap(),
        );
        let workspace = resources.resolve_workspace(directory.path()).await.unwrap();
        resources
            .create_session(&NewSession {
                thread_id: session_id.into(),
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
        let intents = inputs
            .iter()
            .enumerate()
            .map(|(index, input)| {
                let arguments_json = serde_json::to_string(input).unwrap();
                let digest = format!("{:x}", Sha256::digest(arguments_json.as_bytes()));
                Arc::new(InvocationIntent {
                    invocation_id: format!("invocation-{index}-{}", uuid::Uuid::now_v7()),
                    tool_call_id: format!("call-{index}"),
                    tool_name: tool.into(),
                    arguments_digest: digest.clone(),
                    effective_tool_name: tool.into(),
                    effective_arguments_digest: digest,
                    effective_arguments_json: arguments_json.clone(),
                    arguments_json,
                    owner_identity: owner_identity.into(),
                    scope_id: session_id.into(),
                    scope_epoch,
                    authorization_ref: "fixture-policy".into(),
                    recovery_locator: "workspace".into(),
                })
            })
            .collect::<Vec<_>>();
        let mut fixture = Self {
            _directory: directory,
            resources,
            session_id: session_id.into(),
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
                        session_id: session_id.into(),
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
                                        tool,
                                        inputs[index].clone(),
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

    pub(crate) fn context<'a>(&self, index: usize, cwd: &'a str) -> ToolContext<'a> {
        let intent = &self.intents[index];
        let mut context = ToolContext::new(&[], cwd)
            .with_session_identity(&self.session_id, "fixture-turn")
            .with_work_invocation(
                1,
                intent.clone(),
                self.target.clone(),
                self.resources.clone(),
            );
        context.invocation_id = Some(intent.invocation_id.clone());
        context
    }

    pub(crate) fn owner_catalog(&self, index: usize, task_id: &str) -> Value {
        let intent = &self.intents[index];
        serde_json::json!({
            "ownerIdentity": intent.owner_identity,
            "scopeId": self.session_id,
            "invocations": [{
                "ownerTaskId": task_id,
                "metadata": {
                    "invocationId": intent.invocation_id,
                    "initiatorSessionId": self.session_id,
                    "recipientLifecycle": 1,
                    "scopeEpoch": intent.scope_epoch,
                    "argumentsDigest": intent.effective_arguments_digest,
                    "argumentsJson": intent.effective_arguments_json,
                    "toolName": intent.effective_tool_name,
                    "ownerIdentity": intent.owner_identity,
                    "authorizationRef": intent.authorization_ref,
                }
            }]
        })
    }

    async fn snapshot(&self) -> WorkSnapshot {
        self.resources
            .load_session_work(&WorkQuery {
                session_id: self.session_id.clone(),
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
                    session_id: self.session_id.clone(),
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
