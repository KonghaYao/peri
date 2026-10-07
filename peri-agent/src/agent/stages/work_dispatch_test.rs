use super::super::work_test_support::fixture_with_input;
use super::*;
use crate::agent::react::Reasoning;
use crate::tools::{BaseTool, ToolContext};
use peri_acp_types::session_resources::*;
use peri_acp_types::store::{CompactionChange, MessageFlags};
use peri_acp_types::system_reminder::TrustedSystemReminder;
use peri_acp_types::thread::{ThreadId, ThreadMeta};
use peri_acp_types::workspace::{ResolvedWorkspace, ScopedThreadPage, ScopedThreadQuery};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Mutex;

use super::super::work_budget_test_support as budget_support;

struct BudgetProbe(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl BaseTool for BudgetProbe {
    fn name(&self) -> &str {
        "budget_probe"
    }
    fn description(&self) -> &str {
        "bounded dispatch probe"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type":"object", "properties":{}})
    }
    async fn invoke(
        &self,
        _: serde_json::Value,
        _: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok("accepted effect outcome".into())
    }
}

#[tokio::test]
async fn mid_batch_budget_denial_persists_accepted_outcome_before_returning_budget_error() {
    let effects = Arc::new(AtomicUsize::new(0));
    let tool = Arc::new(BudgetProbe(Arc::clone(&effects)));
    let model = peri_model::OpenAiModel::new(peri_model::OpenAiConfig::new(
        "http://127.0.0.1:1".parse().unwrap(),
        "unused-key",
        "offline-model",
    ));
    let fixture = fixture_with_input(
        Arc::new(crate::agent::model_bridge::AgentModelBridge::new(Arc::new(
            model,
        ))),
        vec![tool.clone()],
        "budget settlement probe",
    )
    .await;
    super::super::receive::run_receive(super::super::ReceiveInput {
        context: fixture.context.clone(),
    })
    .await
    .unwrap();
    budget_support::install(&fixture.context, 2, 1).await;
    super::super::work_reason::prepare(
        &fixture.context,
        &[BaseMessage::human("budget probe")],
        &[tool.as_ref()],
    )
    .await
    .unwrap();
    let calls = (0..3)
        .map(|sequence| {
            ToolCall::new(
                format!("call-{sequence}"),
                "budget_probe",
                serde_json::json!({}),
            )
        })
        .collect();
    let mut reasoning = Reasoning::with_tools("budget probe", calls);
    let catalog = fixture.context.runtime.tool_catalog.snapshot();
    super::super::work_reason::commit_response(&fixture.context, &mut reasoning, &catalog)
        .await
        .unwrap();
    let error = super::super::tool_dispatch::dispatch_tools(
        &fixture.context,
        &reasoning,
        &catalog,
        &tokio_util::sync::CancellationToken::new(),
    )
    .await
    .err()
    .unwrap();
    assert!(matches!(
        error,
        crate::error::AgentError::WorkBudgetExhausted {
            budget: peri_acp_types::error::WorkBudgetKind::Dispatches,
            used: 1,
            limit: 1,
        }
    ));
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    let session = fixture
        .context
        .work
        .state
        .lock()
        .await
        .session
        .clone()
        .unwrap();
    let snapshot = session.snapshot().await.unwrap();
    assert_eq!(
        snapshot
            .state
            .invocations
            .values()
            .filter(|invocation| matches!(
                invocation.outcome,
                Some(InvocationOutcome::Completed { .. })
            ))
            .count(),
        1
    );
    assert_eq!(
        snapshot
            .state
            .invocations
            .values()
            .filter(|invocation| matches!(&invocation.outcome, Some(InvocationOutcome::Cancelled { evidence }) if evidence.starts_with("before-effect:processing-stopped-before-dispatch:")))
            .count(),
        2
    );
    let work_id = fixture
        .context
        .work
        .state
        .lock()
        .await
        .work_id
        .clone()
        .unwrap();
    assert_eq!(snapshot.state.works[&work_id].stage, WorkStage::Blocked);
    assert_eq!(
        snapshot
            .state
            .works
            .values()
            .filter(|work| work.stage == WorkStage::ReasonReady)
            .count(),
        0
    );
    assert!(fixture
        .context
        .session
        .transcript
        .read()
        .entries()
        .iter()
        .any(|entry| matches!(entry.as_message(), Some(BaseMessage::Tool { .. }))));
    assert!(begin(&fixture.context, &reasoning.tool_calls[1])
        .await
        .is_err());
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    let transcript = fixture.context.session.transcript.read();
    for call in &reasoning.tool_calls {
        assert!(transcript.entries().iter().any(|entry| {
            matches!(entry.as_message(), Some(BaseMessage::Tool { tool_call_id, .. }) if tool_call_id == &call.id)
        }));
    }
}

struct AbandonRaceResources {
    inner: Arc<dyn SessionResources>,
    work_revision_only: bool,
    harmless_revision_only: bool,
    attempts: Mutex<Vec<peri_acp_types::session_resources::work::PreparedWorkCommand>>,
}

macro_rules! race_resources {
    ($(fn $method:ident($($argument:ident: $argument_type:ty),*) -> $result:ty;)*) => {
        #[async_trait::async_trait]
        impl SessionResources for AbandonRaceResources {
            async fn load_session_work(&self, query: &WorkQuery) -> SessionResourceResult<WorkSnapshot> {
                self.inner.load_session_work(query).await
            }
            async fn load_session_control(&self, id: &ThreadId) -> SessionResourceResult<ControlState> {
                self.inner.load_session_control(id).await
            }
            async fn apply_work_mutation(&self, command: &peri_acp_types::session_resources::work::PreparedWorkCommand) -> SessionResourceResult<WorkReceipt> {
                let WorkAction::CommitAct { target, .. } = &command.action else {
                    return self.inner.apply_work_mutation(command).await;
                };
                let mut attempts = self.attempts.lock().await;
                attempts.push(command.clone());
                if attempts.len() == 1 {
                    if self.harmless_revision_only {
                        let published = self.inner.apply_work_mutation(&peri_acp_types::session_resources::work::PreparedWorkCommand::try_new(WorkCommand {
                            session_id: command.session_id.clone(),
                            recipient_lifecycle: command.recipient_lifecycle,
                            mutation_id: uuid::Uuid::now_v7().to_string(),
                            action: WorkAction::PublishDelivery { delivery: PublishDelivery {
                                delivery_id: "unrelated-delivery".into(),
                                event: WorkEvent {
                                    producer_namespace: "race-test".into(), event_id: "unrelated-event".into(),
                                    event_kind: "input".into(), causation_id: None,
                                    content: WorkPayload::from_payload(&PersistedPayload::Message(
                                        BaseMessage::human("unrelated concurrent publication"),
                                    ))?,
                                },
                                purpose: DeliveryPurpose::UserInput,
                                policy: peri_acp_types::session::MessagePolicy::ensure_processing(),
                            } },
                        }).unwrap()).await?;
                        assert_eq!(published.decision, WorkDecision::Accepted);
                        return self.inner.apply_work_mutation(command).await;
                    }
                    let before = self.inner.load_session_work(&WorkQuery {
                        session_id: command.session_id.clone(), limit: 1,
                    }).await?;
                    let abandoned = self.inner.apply_work_mutation(&peri_acp_types::session_resources::work::PreparedWorkCommand::try_new(WorkCommand {
                        session_id: command.session_id.clone(),
                        recipient_lifecycle: command.recipient_lifecycle,
                        mutation_id: uuid::Uuid::now_v7().to_string(),
                        action: WorkAction::AbandonWork {
                            expected_revision: before.state.revision,
                            expected_control_generation: before.control.control_generation,
                            target: target.clone(), reason: "racing user abandonment".into(),
                            authorization_ref: "explicit-user-authorization".into(),
                        },
                    }).unwrap()).await?;
                    assert_eq!(abandoned.decision, WorkDecision::Accepted);
                    replace_current_execution(&self.inner, &command.session_id).await;
                    if self.work_revision_only {
                        let after = self.inner.load_session_work(&WorkQuery {
                            session_id: command.session_id.clone(), limit: 1,
                        }).await?;
                        let mut stale_target = command.clone().into_command();
                        let WorkAction::CommitAct { guard, .. } = &mut stale_target.action else {
                            unreachable!();
                        };
                        guard.expected_revision = after.state.revision;
                        guard.expected_control_generation = after.control.control_generation;
                        guard.execution = after.control.attempt.clone().unwrap();
                        let reduction = reduce_work(&peri_acp_types::session_resources::work::PreparedWorkCommand::try_new(stale_target.clone()).unwrap(), &after.control, after.state)?;
                        assert_eq!(reduction.receipt.decision,
                            WorkDecision::Rejected { reason: WorkRejection::StaleWorkRevision });
                        return Ok(reduction.receipt);
                    }
                }
                self.inner.apply_work_mutation(command).await
            }
            $(async fn $method(&self, $($argument: $argument_type),*) -> $result {
                self.inner.$method($($argument),*).await
            })*
        }
    };
}

race_resources! {
    fn inspect_availability(session: Option<&ThreadId>) -> SessionResourceResult<SessionAvailability>;
    fn resolve_workspace(cwd: &std::path::Path) -> SessionResourceResult<ResolvedWorkspace>;
    fn validate_session(id: &ThreadId, workspace: &ResolvedWorkspace) -> SessionResourceResult<()>;
    fn finish_close(id: &ThreadId) -> SessionResourceResult<()>;
    fn close_settlement(id: &ThreadId) -> SessionResourceResult<CloseSettlement>;
    fn create_session(input: &NewSession) -> SessionResourceResult<()>;
    fn abandon_initialization(id: &ThreadId) -> SessionResourceResult<()>;
    fn begin_initialization(draft: &NewSessionDraft) -> SessionResourceResult<Arc<dyn SessionInitialization>>;
    fn discard_incomplete_initialization(id: &ThreadId) -> SessionResourceResult<()>;
    fn adopt_legacy_session(id: &ThreadId, cwd: &str, workspace: &ResolvedWorkspace, frozen: &FrozenSnapshotBytes) -> SessionResourceResult<()>;
    fn load_session_snapshot(id: &ThreadId) -> SessionResourceResult<SessionSnapshot>;
    fn load_session_binding(id: &ThreadId) -> SessionResourceResult<BindingState>;
    fn validate_bound_workspace(id: &ThreadId, check: BindingRecheck) -> SessionResourceResult<ResolvedWorkspace>;
    fn load_session_history(id: &ThreadId) -> SessionResourceResult<Vec<PersistedPayload>>;
    fn load_session_meta(id: &ThreadId) -> SessionResourceResult<ThreadMeta>;
    fn list_sessions(query: &ScopedThreadQuery) -> SessionResourceResult<ScopedThreadPage>;
    fn list_children(parent: &ThreadId) -> SessionResourceResult<Vec<ThreadMeta>>;
    fn list_session_tree(root: &ThreadId) -> SessionResourceResult<Vec<ThreadMeta>>;
    fn append_history(id: &ThreadId, payloads: &[PersistedPayload]) -> SessionResourceResult<()>;
    fn append_reminder_if_absent(id: &ThreadId, message_id: peri_acp_types::messages::MessageId, reminder: &TrustedSystemReminder) -> SessionResourceResult<bool>;
    fn mark_session_closing(id: &ThreadId) -> SessionResourceResult<()>;
    fn is_session_closing(id: &ThreadId) -> SessionResourceResult<bool>;
    fn save_fork(fork: &ForkSnapshot) -> SessionResourceResult<()>;
    fn save_child(child: &ChildSnapshot) -> SessionResourceResult<()>;
    fn claim_child_resume(child: &ThreadId, root: &ThreadId) -> SessionResourceResult<Box<dyn ChildResumeClaim>>;
    fn apply_compaction(id: &ThreadId, change: &CompactionChange) -> SessionResourceResult<()>;
    fn apply_message_projections(id: &ThreadId, updates: &[(peri_acp_types::messages::MessageId, MessageFlags)]) -> SessionResourceResult<()>;
    fn rewind_history(id: &ThreadId, boundary: RewindBoundary) -> SessionResourceResult<()>;
    fn remove_history_entries(id: &ThreadId, ids: &[peri_acp_types::messages::MessageId]) -> SessionResourceResult<()>;
    fn update_session_meta(id: &ThreadId, patch: &SessionMetaPatch) -> SessionResourceResult<()>;
    fn delete_session_tree(id: &ThreadId) -> SessionResourceResult<()>;
    fn recover_session_persistence(id: &ThreadId) -> SessionResourceResult<PersistenceRecovery>;
    fn drain_persistence(id: &ThreadId) -> SessionResourceResult<()>;
}

async fn replace_current_execution(resources: &Arc<dyn SessionResources>, session_id: &str) {
    let control = resources
        .load_session_control(&session_id.to_owned())
        .await
        .unwrap();
    let stopped = resources
        .apply_session_control(&ControlCommand {
            session_id: session_id.into(),
            command_id: uuid::Uuid::now_v7().to_string(),
            expected_lifecycle: control.lifecycle,
            expected_revision: control.revision,
            expected_control_generation: control.control_generation,
            action: ControlAction::Stop {
                target: control.attempt.clone().unwrap(),
            },
        })
        .await
        .unwrap();
    assert_eq!(stopped.decision, ControlDecision::Accepted);
    let cleared = resources
        .apply_session_control(&ControlCommand {
            session_id: session_id.into(),
            command_id: uuid::Uuid::now_v7().to_string(),
            expected_lifecycle: stopped.state.lifecycle,
            expected_revision: stopped.state.revision,
            expected_control_generation: stopped.state.control_generation,
            action: ControlAction::ObserveAttempt { target: None },
        })
        .await
        .unwrap();
    assert_eq!(cleared.decision, ControlDecision::Accepted);
    assert!(cleared.state.attempt.is_none());
    let resumed = resources
        .apply_session_control(&ControlCommand {
            session_id: session_id.into(),
            command_id: uuid::Uuid::now_v7().to_string(),
            expected_lifecycle: cleared.state.lifecycle,
            expected_revision: cleared.state.revision,
            expected_control_generation: cleared.state.control_generation,
            action: ControlAction::Resume,
        })
        .await
        .unwrap();
    assert_eq!(resumed.decision, ControlDecision::Accepted);
    let observed = resources
        .apply_session_control(&ControlCommand {
            session_id: session_id.into(),
            command_id: uuid::Uuid::now_v7().to_string(),
            expected_lifecycle: resumed.state.lifecycle,
            expected_revision: resumed.state.revision,
            expected_control_generation: resumed.state.control_generation,
            action: ControlAction::ObserveAttempt {
                target: Some(ControlAttempt {
                    turn_id: peri_acp_types::session::TurnId::new(),
                    attempt_id: peri_acp_types::identity::AttemptId::new(),
                }),
            },
        })
        .await
        .unwrap();
    assert_eq!(observed.decision, ControlDecision::Accepted);
}

async fn race_fixture(
    effects: Arc<AtomicUsize>,
) -> (
    super::super::work_test_support::ProductionFixture,
    Reasoning,
) {
    let tool = Arc::new(BudgetProbe(effects));
    let model = peri_model::OpenAiModel::new(peri_model::OpenAiConfig::new(
        "http://127.0.0.1:1".parse().unwrap(),
        "unused-key",
        "offline-model",
    ));
    let fixture = fixture_with_input(
        Arc::new(crate::agent::model_bridge::AgentModelBridge::new(Arc::new(
            model,
        ))),
        vec![tool.clone()],
        "race settlement probe",
    )
    .await;
    super::super::receive::run_receive(super::super::ReceiveInput {
        context: fixture.context.clone(),
    })
    .await
    .unwrap();
    super::super::work_reason::prepare(
        &fixture.context,
        &[BaseMessage::human("race probe")],
        &[tool.as_ref()],
    )
    .await
    .unwrap();
    let mut reasoning = Reasoning::with_tools(
        "race probe",
        vec![ToolCall::new(
            "race-call",
            "budget_probe",
            serde_json::json!({}),
        )],
    );
    super::super::work_reason::commit_response(
        &fixture.context,
        &mut reasoning,
        &fixture.context.runtime.tool_catalog.snapshot(),
    )
    .await
    .unwrap();
    (fixture, reasoning)
}

#[tokio::test]
async fn result_commit_abandon_race_rebuilds_only_settlement_with_original_outcome_and_identity() {
    for work_revision_only in [false, true] {
        let effects = Arc::new(AtomicUsize::new(0));
        let (fixture, reasoning) = race_fixture(Arc::clone(&effects)).await;
        let mut state = fixture.context.work.state.lock().await;
        let session = state.session.clone().unwrap();
        let work_id = state.work_id.clone().unwrap();
        let resources = Arc::new(AbandonRaceResources {
            inner: session.ledger.resources(),
            work_revision_only,
            harmless_revision_only: false,
            attempts: Mutex::new(Vec::new()),
        });
        state.session = Some(Arc::new(WorkSession {
            admission: session.admission.clone(),
            ledger: super::super::work_ledger::WorkMutationBarrier::new(resources.clone()),
        }));
        drop(state);
        let error = super::super::tool_dispatch::dispatch_tools(
            &fixture.context,
            &reasoning,
            &fixture.context.runtime.tool_catalog.snapshot(),
            &tokio_util::sync::CancellationToken::new(),
        )
        .await
        .err();
        assert!(error.is_some());
        assert_eq!(effects.load(Ordering::SeqCst), 1);
        let snapshot = session.snapshot().await.unwrap();
        assert_eq!(snapshot.state.works[&work_id].stage, WorkStage::Abandoned);
        assert_eq!(
            snapshot.state.invocations.values().next().unwrap().status,
            InvocationStatus::Settled
        );
        assert!(!snapshot
            .state
            .works
            .values()
            .any(|work| work.stage == WorkStage::ReasonReady));
        let attempts = resources.attempts.lock().await;
        assert_eq!(attempts.len(), 2);
        let WorkAction::CommitAct {
            guard: first_guard,
            target: first_target,
            results: first_results,
            next_work_id: Some(_),
            ..
        } = &attempts[0].action
        else {
            panic!("first commit must try advancing");
        };
        let WorkAction::CommitAct {
            guard: final_guard,
            target: final_target,
            results: final_results,
            next_work_id: None,
            ..
        } = &attempts[1].action
        else {
            panic!("retry must only settle");
        };
        assert_eq!(first_results, final_results);
        assert_eq!(final_guard.execution, fixture.admission.execution);
        assert_eq!(
            final_guard.expected_control_generation,
            first_guard.expected_control_generation
        );
        assert_ne!(
            snapshot.control.attempt.as_ref(),
            Some(&final_guard.execution)
        );
        assert!(final_target.expected_work_revision > first_target.expected_work_revision);
        assert_ne!(attempts[0].mutation_id, attempts[1].mutation_id);
        assert!(fixture.context.work.state.lock().await.frozen);
        assert!(fixture
            .context
            .session
            .transcript
            .read()
            .entries()
            .iter()
            .any(|entry| matches!(entry.as_message(), Some(BaseMessage::Tool { .. }))));
    }
}

#[tokio::test]
async fn settlement_ledger_stale_revision_retry_preserves_original_execution_across_control_change()
{
    let (fixture, reasoning) = race_fixture(Arc::new(AtomicUsize::new(0))).await;
    let binding = begin(&fixture.context, &reasoning.tool_calls[0])
        .await
        .unwrap()
        .unwrap();
    let session = fixture
        .context
        .work
        .state
        .lock()
        .await
        .session
        .clone()
        .unwrap();
    let snapshot = session.snapshot().await.unwrap();
    let resources = session.ledger.resources();
    replace_current_execution(&resources, &session.admission.session_id).await;
    let mut guard = session.guard(&snapshot).unwrap();
    guard.expected_revision -= 1;
    let command = session
        .command(WorkAction::CommitAct {
            guard,
            target: binding.target,
            results: vec![InvocationResult {
                invocation_id: binding.intent.invocation_id.clone(),
                outcome: InvocationOutcome::Completed {
                    result: WorkPayload::from_payload(&PersistedPayload::Message(
                        BaseMessage::tool_result("race-call", "original owner result"),
                    ))
                    .unwrap(),
                },
            }],
            next_work_id: None,
        })
        .unwrap();
    let receipt = session
        .ledger
        .commit_execution_transition(&command)
        .await
        .unwrap();
    assert_eq!(receipt.decision, WorkDecision::Accepted);
    assert_eq!(receipt.stage, Some(WorkStage::Settled));
    assert_ne!(receipt.mutation_id, command.mutation_id);
    let after = session.snapshot().await.unwrap();
    assert_eq!(
        after.state.invocations[&binding.intent.invocation_id].status,
        InvocationStatus::Settled
    );
    assert_ne!(
        after.control.attempt.as_ref(),
        Some(&fixture.admission.execution)
    );
    assert!(session.ledger.pending_command().await.is_none());
}

#[tokio::test]
async fn harmless_global_revision_race_preserves_successor_without_freezing_or_reexecuting_tool() {
    let effects = Arc::new(AtomicUsize::new(0));
    let (fixture, reasoning) = race_fixture(Arc::clone(&effects)).await;
    let mut state = fixture.context.work.state.lock().await;
    let session = state.session.clone().unwrap();
    let resources = Arc::new(AbandonRaceResources {
        inner: session.ledger.resources(),
        work_revision_only: false,
        harmless_revision_only: true,
        attempts: Mutex::new(Vec::new()),
    });
    state.session = Some(Arc::new(WorkSession {
        admission: session.admission.clone(),
        ledger: super::super::work_ledger::WorkMutationBarrier::new(resources.clone()),
    }));
    drop(state);
    super::super::tool_dispatch::dispatch_tools(
        &fixture.context,
        &reasoning,
        &fixture.context.runtime.tool_catalog.snapshot(),
        &tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    let attempts = resources.attempts.lock().await;
    assert_eq!(attempts.len(), 2);
    let WorkAction::CommitAct {
        guard: first_guard,
        target: first_target,
        results: first_results,
        next_work_id: first_next,
        ..
    } = &attempts[0].action
    else {
        unreachable!();
    };
    let WorkAction::CommitAct {
        guard: final_guard,
        target: final_target,
        results: final_results,
        next_work_id: final_next,
        ..
    } = &attempts[1].action
    else {
        unreachable!();
    };
    assert!(first_next.is_some());
    assert_eq!(first_next, final_next);
    assert_eq!(first_target, final_target);
    assert_eq!(first_results, final_results);
    let mut refreshed_guard = first_guard.clone();
    refreshed_guard.expected_revision = final_guard.expected_revision;
    assert_eq!(&refreshed_guard, final_guard);
    assert!(final_guard.expected_revision > first_guard.expected_revision);
    let state = fixture.context.work.state.lock().await;
    assert!(!state.frozen);
    assert_eq!(state.work_id.as_deref(), final_next.as_deref());
    let snapshot = session.snapshot().await.unwrap();
    assert_eq!(
        snapshot.state.works[final_next.as_ref().unwrap()].stage,
        WorkStage::ReasonReady
    );
}
