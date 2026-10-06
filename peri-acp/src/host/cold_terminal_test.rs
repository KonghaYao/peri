use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use async_trait::async_trait;
use peri_acp_types::identity::AttemptId;
use peri_acp_types::messages::BaseMessage;
use peri_acp_types::messages::MessageId;
use peri_acp_types::session::{MessagePolicy, TurnId};
use peri_acp_types::session_resources::work::*;
use peri_acp_types::session_resources::*;
use peri_acp_types::store::PersistedPayload;
use peri_acp_types::store::{CompactionChange, MessageFlags};
use peri_acp_types::system_reminder::TrustedSystemReminder;
use peri_acp_types::thread::{ThreadId, ThreadMeta};
use peri_acp_types::workspace::{
    ResolvedWorkspace, ScopedThreadPage, ScopedThreadQuery, SessionBinding,
};
use peri_resources::sessions::SessionResourcesImpl;
use sha2::{Digest, Sha256};

use super::*;

struct Fixture {
    directory: tempfile::TempDir,
    resources: Arc<SessionResourcesImpl>,
    admission: WorkAdmission,
    terminal: WorkCommand,
}

impl Fixture {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let resources = Arc::new(
            SessionResourcesImpl::open(directory.path().join("terminal.db"))
                .await
                .unwrap(),
        );
        let workspace = resources.resolve_workspace(directory.path()).await.unwrap();
        for (id, parent) in [
            ("original-parent", None),
            ("child", Some("original-parent")),
        ] {
            resources
                .create_session(&NewSession {
                    thread_id: id.into(),
                    created_at: peri_time::now_utc_rfc3339(),
                    meta: NewSessionMeta {
                        title: None,
                        cwd: workspace.cwd.to_string_lossy().into_owned(),
                        parent_thread_id: parent.map(String::from),
                        hidden: false,
                        cancel_policy: Default::default(),
                        snapshot_at_message_id: None,
                    },
                    binding: SessionBinding::from_workspace(&workspace),
                    frozen: FrozenSnapshotBytes::new("{\"version\":1}"),
                })
                .await
                .unwrap();
        }
        let digest = format!("{:x}", Sha256::digest(b"{}"));
        let intent = InvocationIntent {
            invocation_id: "delegation-invocation".into(),
            tool_call_id: "delegation-call".into(),
            tool_name: "Task".into(),
            arguments: resources
                .prepare_evidence(&EvidenceWrite {
                    session_id: "original-parent".into(),
                    storage_scope: "original-parent".into(),
                    payload_id: "arguments".into(),
                    encoding: 1,
                    bytes: b"{}".to_vec(),
                })
                .await
                .unwrap(),
            arguments_digest: digest.clone(),
            effective_tool_name: "Task".into(),
            effective_arguments: resources
                .prepare_evidence(&EvidenceWrite {
                    session_id: "original-parent".into(),
                    storage_scope: "original-parent".into(),
                    payload_id: "arguments".into(),
                    encoding: 1,
                    bytes: b"{}".to_vec(),
                })
                .await
                .unwrap(),
            effective_arguments_digest: digest,
            owner_identity: "owned-child-factory".into(),
            scope_id: "original-parent".into(),
            scope_epoch: None,
            authorization_ref: "saved-child-authorization".into(),
            recovery_locator: "child".into(),
        };
        let binding = TaskBinding {
            invocation_id: intent.invocation_id.clone(),
            owner_identity: intent.owner_identity.clone(),
            owner_task_id: "original-task".into(),
            initiator_session_id: "original-parent".into(),
            recipient_lifecycle: 1,
            recovery_locator: intent.recovery_locator.clone(),
            authorization_ref: intent.authorization_ref.clone(),
        };
        apply(
            resources.as_ref(),
            "original-parent",
            "prepare",
            WorkAction::PrepareInvocation {
                expected_revision: 0,
                intent,
            },
        )
        .await;
        let parent_binding_receipt = apply(
            resources.as_ref(),
            "original-parent",
            "bind",
            WorkAction::ReconcileTaskBinding {
                expected_revision: 1,
                binding: binding.clone(),
            },
        )
        .await;
        let delivery = publication(
            resources.as_ref(),
            "child",
            "child-input",
            "execute saved child",
            DeliveryPurpose::UserInput,
        )
        .await;
        apply(
            resources.as_ref(),
            "child",
            "input",
            WorkAction::PublishDelivery { delivery },
        )
        .await;
        let work = snapshot(resources.as_ref(), "child").await;
        let candidate = &crate::host::work_query::availability(&work)
            .unwrap()
            .candidates[0];
        let admission = WorkAdmission {
            session_id: "child".into(),
            admission_id: "owned-admission".into(),
            instance_id: "sdk-instance".into(),
            generation_id: "sdk-generation".into(),
            lifecycle: 1,
            control_generation: work.control.control_generation,
            work_id: candidate.work_id.clone(),
            work_revision: candidate.work_revision,
            execution: ControlAttempt {
                turn_id: TurnId::new(),
                attempt_id: AttemptId::new(),
            },
        };
        apply(
            resources.as_ref(),
            "child",
            "enter",
            WorkAction::RegisterAdmission {
                admission: admission.clone(),
            },
        )
        .await;
        let work = snapshot(resources.as_ref(), "child").await;
        apply(
            resources.as_ref(),
            "child",
            "claim",
            WorkAction::ClaimBatch {
                guard: WorkGuard {
                    expected_revision: work.head.change_seq,
                    expected_control_generation: work.control.control_generation,
                    execution: admission.execution.clone(),
                },
                batch_id: admission.work_id.clone(),
                delivery_ids: crate::host::work_query::availability(&work)
                    .unwrap()
                    .candidates[0]
                    .delivery_ids
                    .clone(),
            },
        )
        .await;
        let work = snapshot(resources.as_ref(), "child").await;
        apply(
            resources.as_ref(),
            "child",
            "delegation",
            WorkAction::BindWorkDelegation {
                expected_revision: work.head.change_seq,
                work_id: admission.work_id.clone(),
                binding: binding.clone(),
                parent_binding_receipt,
            },
        )
        .await;

        let work = snapshot(resources.as_ref(), "child").await;
        apply(
            resources.as_ref(),
            "child",
            "reason",
            WorkAction::BeginReason {
                guard: WorkGuard {
                    expected_revision: work.head.change_seq,
                    expected_control_generation: work.control.control_generation,
                    execution: admission.execution.clone(),
                },
                target: WorkTarget {
                    work_id: admission.work_id.clone(),
                    expected_work_revision: crate::host::work_query::test_processing(
                        resources.as_ref(),
                        "child",
                        &admission.work_id,
                    )
                    .await
                    .revision,
                },
                request_id: "child-request".into(),
                request: ReasonRequest {
                    payload: resources
                        .prepare_evidence(&EvidenceWrite {
                            session_id: "child".into(),
                            storage_scope: "child".into(),
                            payload_id: "request".into(),
                            encoding: 1,
                            bytes: b"{}".to_vec(),
                        })
                        .await
                        .unwrap(),
                    request_digest: format!("{:x}", Sha256::digest(b"{}")),
                    model_ref: "saved-child-model".into(),
                    authorization_ref: "saved-child-authorization".into(),
                },
            },
        )
        .await;
        let work = snapshot(resources.as_ref(), "child").await;
        apply(
            resources.as_ref(),
            "child",
            "model-response",
            WorkAction::CommitReasonResponseAndDispatchIntent {
                guard: WorkGuard {
                    expected_revision: work.head.change_seq,
                    expected_control_generation: work.control.control_generation,
                    execution: admission.execution.clone(),
                },
                target: WorkTarget {
                    work_id: admission.work_id.clone(),
                    expected_work_revision: crate::host::work_query::test_processing(
                        resources.as_ref(),
                        "child",
                        &admission.work_id,
                    )
                    .await
                    .revision,
                },
                request_id: "child-request".into(),
                response: crate::host::work_query::test_payload(
                    resources.as_ref(),
                    "child",
                    &PersistedPayload::Message(BaseMessage::ai("saved child result")),
                )
                .await,
                dispatch_intents: Vec::new(),
                next_work_id: None,
            },
        )
        .await;
        let work = snapshot(resources.as_ref(), "child").await;
        apply(
            resources.as_ref(),
            "child",
            "metadata",
            WorkAction::BindChildResumeMetadata {
                expected_revision: work.head.change_seq,
                metadata_json:
                    serde_json::json!({"version":1,"childSessionId":"child","recipientLifecycle":1,
                "directInitiatorSessionId":"original-parent","directInitiatorLifecycle":1,
                "delegationInvocationId":"delegation-invocation","delegationTaskId":"original-task",
                "authorizationRef":"saved-child-authorization"})
                    .to_string(),
            },
        )
        .await;
        let control = resources
            .load_session_control(&"child".into())
            .await
            .unwrap();
        let receipt = resources
            .apply_session_control(&ControlCommand {
                session_id: "child".into(),
                command_id: "model-exited".into(),
                expected_lifecycle: control.lifecycle,
                expected_revision: control.revision,
                expected_control_generation: control.control_generation,
                action: ControlAction::ObserveAttempt { target: None },
            })
            .await
            .unwrap();
        assert_eq!(receipt.decision, ControlDecision::Accepted);
        let terminal = WorkCommand {
            session_id: "original-parent".into(),
            recipient_lifecycle: 1,
            mutation_id: "original-terminal-command".into(),
            action: WorkAction::PublishTaskSettlement {
                delivery: publication(
                    resources.as_ref(),
                    "original-parent",
                    "original-terminal",
                    "saved child result",
                    DeliveryPurpose::TaskTerminal,
                )
                .await,
                binding,
            },
        };
        Self {
            directory,
            resources,
            admission,
            terminal,
        }
    }

    async fn reopen(&self) -> SessionResourcesImpl {
        SessionResourcesImpl::open(self.directory.path().join("terminal.db"))
            .await
            .unwrap()
    }
}

async fn publication(
    resources: &dyn SessionResources,
    session_id: &str,
    id: &str,
    text: &str,
    purpose: DeliveryPurpose,
) -> PublishDelivery {
    PublishDelivery {
        delivery_id: id.into(),
        event: WorkEvent {
            producer_namespace: "cold-child-fixture".into(),
            event_id: id.into(),
            event_kind: "terminal-test".into(),
            causation_id: None,
            content: crate::host::work_query::test_payload(
                resources,
                session_id,
                &PersistedPayload::Message(BaseMessage::human(text)),
            )
            .await,
        },
        purpose,
        policy: MessagePolicy::ensure_processing(),
    }
}

async fn snapshot(resources: &dyn SessionResources, session_id: &str) -> WorkInspection {
    resources
        .inspect_work(&WorkQuery {
            session_id: session_id.into(),
            selector: WorkSelector::Availability,
            limit: 1,
            cursor: None,
        })
        .await
        .unwrap()
}

async fn apply(
    resources: &dyn SessionResources,
    session_id: &str,
    mutation_id: &str,
    action: WorkAction,
) -> WorkReceipt {
    let receipt = resources
        .apply_work_mutation(&WorkCommand {
            session_id: session_id.into(),
            recipient_lifecycle: 1,
            mutation_id: mutation_id.into(),
            action,
        })
        .await
        .unwrap();
    assert_eq!(receipt.decision, WorkDecision::Accepted);
    receipt
}

#[tokio::test]
async fn crash_before_parent_rpc_recovers_original_obligation_without_model_reexecution() {
    let fixture = Fixture::new().await;
    persist(
        fixture.resources.as_ref(),
        &fixture.admission,
        fixture.terminal.clone(),
    )
    .await
    .unwrap();
    let cold = fixture.reopen().await;
    assert!(snapshot(&cold, "original-parent").await.head.required_count == 0);
    assert!(
        crate::host::work_query::test_admission(&cold, "child", "owned-admission")
            .await
            .leaving_evidence_id
            .is_none()
    );
    assert!(reconcile(&cold, &fixture.admission).await.unwrap());
    let owned = crate::host::work_query::command(
        &cold,
        &fixture.terminal.session_id,
        &fixture.terminal.mutation_id,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(owned.command, fixture.terminal);
    assert!(
        matches!(owned.resolution, Some(WorkResolution::Applied { receipt }) if receipt.decision == WorkDecision::Accepted)
    );
    super::super::execution::finish_admission(&cold, &fixture.admission)
        .await
        .unwrap();
    assert!(
        crate::host::work_query::test_admission(&cold, "child", "owned-admission")
            .await
            .leaving_evidence_id
            .is_some()
    );
    assert!(reconcile(&cold, &fixture.admission).await.unwrap());
    assert_eq!(
        snapshot(&cold, "original-parent").await.head.required_count,
        1
    );
}

#[tokio::test]
async fn conflicting_terminal_payload_cannot_replace_unfinished_obligation() {
    let fixture = Fixture::new().await;
    persist(
        fixture.resources.as_ref(),
        &fixture.admission,
        fixture.terminal.clone(),
    )
    .await
    .unwrap();
    let mut foreign = fixture.terminal.clone();
    foreign.recipient_lifecycle = 2;
    assert!(
        persist(fixture.resources.as_ref(), &fixture.admission, foreign)
            .await
            .is_err()
    );
    assert_eq!(
        *crate::host::work_query::test_terminal(
            fixture.resources.as_ref(),
            "child",
            "owned-admission"
        )
        .await
        .command,
        fixture.terminal
    );
    assert!(
        crate::host::work_query::test_admission(
            fixture.resources.as_ref(),
            "child",
            "owned-admission"
        )
        .await
        .leaving_evidence_id
        .is_none()
    );
}

struct LostTerminalAck {
    inner: Arc<SessionResourcesImpl>,
    unavailable: AtomicBool,
}

macro_rules! unsupported_facade_operations {
    ($( $method:ident ( $( $argument:ident : $kind:ty ),* ) -> $output:ty; )*) => {
        #[async_trait]
        impl SessionResources for LostTerminalAck {
            $(async fn $method(&self, $( $argument: $kind ),*) -> SessionResourceResult<$output> {
                let _ = ($( $argument ),*);
                Err(SessionResourceError::new(SessionResourceErrorKind::Unsupported))
            })*

            async fn inspect_work(&self, query: &WorkQuery) -> SessionResourceResult<WorkInspection> {
                let inspected = self.inner.inspect_work(query).await?;
                if query.session_id == "original-parent" && matches!(query.selector, WorkSelector::Command { .. })
                    && matches!(&inspected.page, WorkPage::Commands(commands) if !commands.is_empty())
                    && self.unavailable.load(Ordering::SeqCst) {
                    return Err(SessionResourceError::persistence_uncertain(Some(query.session_id.clone())));
                }
                Ok(inspected)
            }

            async fn prepare_evidence(&self, write: &EvidenceWrite) -> SessionResourceResult<PayloadRef> {
                self.inner.prepare_evidence(write).await
            }

            async fn read_evidence(&self, query: &EvidenceQuery) -> SessionResourceResult<EvidenceRecord> {
                self.inner.read_evidence(query).await
            }

            async fn apply_work_mutation(&self, command: &WorkCommand) -> SessionResourceResult<WorkReceipt> {
                let receipt = self.inner.apply_work_mutation(command).await?;
                if matches!(command.action, WorkAction::PublishTaskSettlement { .. }) && self.unavailable.load(Ordering::SeqCst) {
                    return Err(SessionResourceError::persistence_uncertain(Some(command.session_id.clone())));
                }
                Ok(receipt)
            }

            async fn resolve_work_mutation(&self, command: &WorkCommand) -> SessionResourceResult<WorkResolution> {
                if matches!(command.action, WorkAction::PublishTaskSettlement { .. }) && self.unavailable.load(Ordering::SeqCst) {
                    return Ok(WorkResolution::Unknown);
                }
                self.inner.resolve_work_mutation(command).await
            }
        }
    };
}

unsupported_facade_operations! {
    inspect_availability(session: Option<&ThreadId>) -> SessionAvailability;
    resolve_workspace(cwd: &std::path::Path) -> ResolvedWorkspace;
    validate_session(id: &ThreadId, workspace: &ResolvedWorkspace) -> ();
    finish_close(id: &ThreadId) -> ();
    close_settlement(id: &ThreadId) -> CloseSettlement;
    create_session(input: &NewSession) -> ();
    abandon_initialization(id: &ThreadId) -> ();
    begin_initialization(draft: &NewSessionDraft) -> Arc<dyn SessionInitialization>;
    discard_incomplete_initialization(id: &ThreadId) -> ();
    adopt_legacy_session(id: &ThreadId, saved_cwd: &str, workspace: &ResolvedWorkspace, frozen: &FrozenSnapshotBytes) -> ();
    load_session_snapshot(id: &ThreadId) -> SessionSnapshot;
    load_session_binding(id: &ThreadId) -> BindingState;
    validate_bound_workspace(id: &ThreadId, check: BindingRecheck) -> ResolvedWorkspace;
    load_session_history(id: &ThreadId) -> Vec<PersistedPayload>;
    load_session_meta(id: &ThreadId) -> ThreadMeta;
    list_sessions(query: &ScopedThreadQuery) -> ScopedThreadPage;
    list_children(parent: &ThreadId) -> Vec<ThreadMeta>;
    list_session_tree(root: &ThreadId) -> Vec<ThreadMeta>;
    append_history(id: &ThreadId, payloads: &[PersistedPayload]) -> ();
    append_reminder_if_absent(id: &ThreadId, message_id: MessageId, reminder: &TrustedSystemReminder) -> bool;
    mark_session_closing(id: &ThreadId) -> ();
    is_session_closing(id: &ThreadId) -> bool;
    save_fork(fork: &ForkSnapshot) -> ();
    save_child(child: &ChildSnapshot) -> ();
    claim_child_resume(child: &ThreadId, root: &ThreadId) -> Box<dyn ChildResumeClaim>;
    apply_compaction(id: &ThreadId, change: &CompactionChange) -> ();
    apply_message_projections(id: &ThreadId, updates: &[(MessageId, MessageFlags)]) -> ();
    rewind_history(id: &ThreadId, boundary: RewindBoundary) -> ();
    remove_history_entries(id: &ThreadId, ids: &[MessageId]) -> ();
    update_session_meta(id: &ThreadId, patch: &SessionMetaPatch) -> ();
    delete_session_tree(id: &ThreadId) -> ();
    recover_session_persistence(id: &ThreadId) -> PersistenceRecovery;
    drain_persistence(id: &ThreadId) -> ();
}

#[tokio::test]
async fn lost_parent_ack_keeps_child_admission_running_until_cold_original_command_ack() {
    let fixture = Fixture::new().await;
    persist(
        fixture.resources.as_ref(),
        &fixture.admission,
        fixture.terminal.clone(),
    )
    .await
    .unwrap();
    let partitioned = LostTerminalAck {
        inner: fixture.resources.clone(),
        unavailable: AtomicBool::new(true),
    };
    assert!(reconcile(&partitioned, &fixture.admission).await.is_err());
    assert_eq!(
        crate::host::work_query::test_processing(
            fixture.resources.as_ref(),
            "child",
            &fixture.admission.work_id
        )
        .await
        .stage,
        WorkStage::Settled
    );
    assert!(
        crate::host::work_query::test_admission(
            fixture.resources.as_ref(),
            "child",
            "owned-admission"
        )
        .await
        .leaving_evidence_id
        .is_none()
    );
    assert_eq!(
        *crate::host::work_query::test_terminal(
            fixture.resources.as_ref(),
            "child",
            "owned-admission"
        )
        .await
        .command,
        fixture.terminal
    );
    let cold = fixture.reopen().await;
    assert!(reconcile(&cold, &fixture.admission).await.unwrap());
    super::super::execution::finish_admission(&cold, &fixture.admission)
        .await
        .unwrap();
    assert!(
        crate::host::work_query::test_admission(&cold, "child", "owned-admission")
            .await
            .leaving_evidence_id
            .is_some()
    );
    assert_eq!(
        snapshot(&cold, "original-parent").await.head.required_count,
        1
    );
}
