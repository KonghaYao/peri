use super::*;
use crate::host::execution_admission::ReverseExecutionAdmission;
use crate::host::execution_admission_jsonl::{JsonlSdkDispatcher, SdkDispatcherLaunch};
use peri_acp_types::execution_admission::{
    AdmissionOutcome, AdmissionRequest, EntryOutcome, EntryRequest, ExecutionAdmissionPort,
};
use peri_acp_types::interaction::{ApprovalDecision, InteractionContext, InteractionResponse};
use peri_acp_types::permission::{PermissionMode, SharedPermissionMode};
use peri_acp_types::session_resources::{FrozenSnapshotBytes, NewSession, NewSessionMeta};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct StartedLeaseBrokerFixture {
    started: Arc<AtomicBool>,
    requests: AtomicUsize,
    approve: bool,
}

#[async_trait::async_trait]
impl UserInteractionBroker for StartedLeaseBrokerFixture {
    async fn request(&self, ctx: InteractionContext) -> InteractionResponse {
        assert!(
            self.started.load(Ordering::SeqCst),
            "scheduled HITL cannot precede Root SDK RunStarted ACK"
        );
        let InteractionContext::Approval { items } = ctx else {
            panic!("scheduled approval expected")
        };
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].tool_name, "cron_trigger");
        assert_eq!(items[0].tool_call_id, "scheduled-task-fixture");
        assert_eq!(items[0].tool_input["prompt"], "frozen original cron prompt");
        self.requests.fetch_add(1, Ordering::SeqCst);
        InteractionResponse::Decisions(vec![if self.approve {
            ApprovalDecision::Approve { source: None }
        } else {
            ApprovalDecision::Reject {
                reason: "fixture rejected".into(),
                source: None,
            }
        }])
    }
}

async fn registered_cron_fixture(
    mixed_user: bool,
) -> (
    tempfile::TempDir,
    Arc<dyn SessionResources>,
    Arc<JsonlSdkDispatcher>,
    WorkAdmission,
) {
    let directory = tempfile::tempdir().unwrap();
    let resources = peri_agent::resources::open_session_resources_with(Some(
        directory.path().join("threads.db"),
    ))
    .await
    .unwrap();
    let workspace = resources.resolve_workspace(directory.path()).await.unwrap();
    let session_id = uuid::Uuid::now_v7().to_string();
    resources
        .create_session(&NewSession {
            thread_id: session_id.clone(),
            created_at: chrono::Utc::now().to_rfc3339(),
            meta: NewSessionMeta {
                title: None,
                cwd: workspace.cwd.to_string_lossy().into_owned(),
                parent_thread_id: None,
                hidden: false,
                cancel_policy: Default::default(),
                snapshot_at_message_id: None,
            },
            binding: peri_acp_types::workspace::SessionBinding::from_workspace(&workspace),
            frozen: FrozenSnapshotBytes::new("{}"),
        })
        .await
        .unwrap();
    let queue = peri_acp_types::session::MessageQueue::default();
    if mixed_user {
        queue.push(peri_acp_types::session::QueuedMessage::prompt(
            peri_acp_types::session::MessageSource::UserInput,
            peri_acp_types::messages::BaseMessage::human("unrelated durable user request"),
        ));
    }
    super::super::continuation::enqueue_cron_trigger(
        &queue,
        &peri_acp_types::cron::CronTrigger {
            task_id: "scheduled-task-fixture".into(),
            prompt: "frozen original cron prompt".into(),
        },
    );
    peri_agent::agent::stages::publish_session_inbox(resources.clone(), &session_id, 1, &queue)
        .await
        .unwrap();
    let executable = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .filter(|path| path.is_absolute())
        .map(|path| path.join("bun"))
        .find(|path| path.is_file())
        .expect("real scheduled SDK gate requires Bun");
    let module = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("npm-packages/@peri-sdk/src/execution/sidecar.ts")
        .canonicalize()
        .unwrap();
    let dispatcher = Arc::new(
        JsonlSdkDispatcher::launch(SdkDispatcherLaunch {
            executable,
            module,
            database: directory.path().join("registry.db"),
            instance_id: "scheduled-fixture-instance".into(),
            generation_id: "scheduled-fixture-generation".into(),
        })
        .await
        .unwrap(),
    );
    let port = ReverseExecutionAdmission::new(dispatcher.clone());
    let snapshot = resources
        .load_session_work(&WorkQuery {
            session_id: session_id.clone(),
            limit: 1,
        })
        .await
        .unwrap();
    let AdmissionOutcome::Admitted { admission } = port
        .admit(AdmissionRequest {
            request_id: "scheduled-fixture-admit".into(),
            snapshot: (&snapshot).into(),
            existing_admission: None,
        })
        .await
        .unwrap()
    else {
        panic!("real SDK must admit the durable cron candidate")
    };
    let entering = commit_original(
        resources.as_ref(),
        &WorkCommand {
            session_id,
            recipient_lifecycle: admission.lifecycle,
            mutation_id: format!("register-admission:{}", admission.admission_id),
            action: WorkAction::RegisterAdmission {
                admission: admission.clone(),
            },
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        port.entered(EntryRequest {
            admission: admission.clone(),
            entry_evidence_id: entering.mutation_id
        })
        .await
        .unwrap(),
        EntryOutcome::Applied { .. }
    ));
    (directory, resources, dispatcher, admission)
}

#[tokio::test]
async fn real_sdk_durable_cron_rejection_abandons_exact_work_after_started_ack_with_broker_fixture()
{
    let (_directory, resources, _dispatcher, admission) = registered_cron_fixture(false).await;
    let started = Arc::new(AtomicBool::new(false));
    let broker = Arc::new(StartedLeaseBrokerFixture {
        started: started.clone(),
        requests: AtomicUsize::new(0),
        approve: false,
    });
    let ack: SdkRunStartedFn = Arc::new(move |_| {
        started.store(true, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    });
    let callback = after_run_started(
        ack,
        resources.clone(),
        SharedPermissionMode::new(PermissionMode::Default),
        broker.clone(),
    );
    assert!(callback(admission.clone())
        .await
        .unwrap_err()
        .contains("explicitly abandoned"));
    let snapshot = resources
        .load_session_work(&WorkQuery {
            session_id: admission.session_id.clone(),
            limit: 1,
        })
        .await
        .unwrap();
    assert!(snapshot.state.works.is_empty());
    assert!(snapshot.state.batches.is_empty());
    assert!(snapshot
        .state
        .obligations
        .values()
        .all(|obligation| obligation.status
            == peri_acp_types::session_resources::work::ObligationStatus::Abandoned));
    assert!(snapshot
        .state
        .budgets
        .values()
        .all(|budget| budget.reason_requests == 0 && budget.dispatches == 0));
    assert_eq!(broker.requests.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn real_sdk_durable_cron_preserves_existing_approval_policy_with_broker_fixture() {
    for mode in [PermissionMode::Default, PermissionMode::Bypass] {
        let (_directory, resources, _dispatcher, admission) = registered_cron_fixture(false).await;
        let started = Arc::new(AtomicBool::new(false));
        let broker = Arc::new(StartedLeaseBrokerFixture {
            started: started.clone(),
            requests: AtomicUsize::new(0),
            approve: true,
        });
        let ack: SdkRunStartedFn = Arc::new(move |_| {
            started.store(true, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        });
        after_run_started(
            ack,
            resources.clone(),
            SharedPermissionMode::new(mode),
            broker.clone(),
        )(admission.clone())
        .await
        .unwrap();
        let snapshot = resources
            .load_session_work(&WorkQuery {
                session_id: admission.session_id,
                limit: 1,
            })
            .await
            .unwrap();
        assert!(
            snapshot.state.works.is_empty(),
            "approval must not fake processing or Reason"
        );
        assert_eq!(
            broker.requests.load(Ordering::SeqCst),
            usize::from(mode != PermissionMode::Bypass)
        );
    }
}

#[tokio::test]
async fn real_sdk_durable_cron_cannot_prompt_without_started_ack_with_broker_fixture() {
    let (_directory, resources, _dispatcher, admission) = registered_cron_fixture(false).await;
    let broker = Arc::new(StartedLeaseBrokerFixture {
        started: Arc::new(AtomicBool::new(false)),
        requests: AtomicUsize::new(0),
        approve: false,
    });
    let ack: SdkRunStartedFn =
        Arc::new(|_| Box::pin(async { Err("RunStarted ACK unknown".into()) }));
    assert!(after_run_started(
        ack,
        resources.clone(),
        SharedPermissionMode::new(PermissionMode::Default),
        broker.clone()
    )(admission.clone())
    .await
    .is_err());
    assert_eq!(broker.requests.load(Ordering::SeqCst), 0);
    let snapshot = resources
        .load_session_work(&WorkQuery {
            session_id: admission.session_id,
            limit: 1,
        })
        .await
        .unwrap();
    assert!(snapshot.state.works.is_empty());
    assert!(snapshot.state.has_pending_work());
}

#[tokio::test]
async fn real_sdk_mixed_cron_rejection_preserves_earlier_user_delivery_with_broker_fixture() {
    use peri_acp_types::session_resources::work::ObligationStatus;
    let (_directory, resources, _dispatcher, admission) = registered_cron_fixture(true).await;
    let started = Arc::new(AtomicBool::new(false));
    let broker = Arc::new(StartedLeaseBrokerFixture {
        started: started.clone(),
        requests: AtomicUsize::new(0),
        approve: false,
    });
    let ack: SdkRunStartedFn = Arc::new(move |_| {
        started.store(true, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    });
    assert!(after_run_started(
        ack,
        resources.clone(),
        SharedPermissionMode::new(PermissionMode::Default),
        broker
    )(admission.clone())
    .await
    .unwrap_err()
    .contains("explicitly abandoned"));
    let snapshot = resources
        .load_session_work(&WorkQuery {
            session_id: admission.session_id,
            limit: 1,
        })
        .await
        .unwrap();
    let user = snapshot
        .state
        .deliveries
        .iter()
        .find(|(_, delivery)| {
            delivery
                .publication
                .event
                .content
                .serialized
                .contains("unrelated durable user request")
        })
        .unwrap();
    let cron = snapshot
        .state
        .deliveries
        .iter()
        .find(|(_, delivery)| {
            delivery
                .publication
                .event
                .content
                .serialized
                .contains("scheduled-task-fixture")
        })
        .unwrap();
    assert!(
        user.1.admission_sequence < cron.1.admission_sequence,
        "must cover selected non-prefix rejection"
    );
    assert_eq!(
        snapshot.state.obligations[user.0].status,
        ObligationStatus::Pending
    );
    assert!(user.1.batch_id.is_none());
    assert_eq!(
        snapshot.state.obligations[cron.0].status,
        ObligationStatus::Abandoned
    );
    assert!(snapshot.state.works.is_empty());
    assert!(snapshot.state.batches.is_empty());
    assert!(!cron.1.projected);
    assert!(snapshot
        .candidates
        .iter()
        .any(|candidate| candidate.delivery_ids == vec![user.0.clone()]));
}

async fn claim_reason_ready_fixture(
    resources: &dyn SessionResources,
    admission: &WorkAdmission,
) -> WorkSnapshot {
    let snapshot = snapshot_for(resources, admission).await.unwrap();
    let candidate = snapshot
        .candidates
        .iter()
        .find(|candidate| candidate.work_id == admission.work_id)
        .unwrap();
    commit_original(
        resources,
        &WorkCommand {
            session_id: admission.session_id.clone(),
            recipient_lifecycle: admission.lifecycle,
            mutation_id: format!("restored-claim-fixture:{}", admission.admission_id),
            action: WorkAction::ClaimBatch {
                guard: WorkGuard {
                    expected_revision: snapshot.state.revision,
                    expected_control_generation: admission.control_generation,
                    execution: admission.execution.clone(),
                },
                batch_id: admission.work_id.clone(),
                delivery_ids: candidate.delivery_ids.clone(),
            },
        },
    )
    .await
    .unwrap();
    snapshot_for(resources, admission).await.unwrap()
}

#[tokio::test]
async fn real_store_restored_reason_ready_reapproval_keeps_original_delivery_with_broker_fixture() {
    let (_directory, resources, _dispatcher, admission) = registered_cron_fixture(false).await;
    let original = claim_reason_ready_fixture(resources.as_ref(), &admission).await;
    let started = Arc::new(AtomicBool::new(false));
    let broker = Arc::new(StartedLeaseBrokerFixture {
        started: started.clone(),
        requests: AtomicUsize::new(0),
        approve: true,
    });
    let ack: SdkRunStartedFn = Arc::new(move |_| {
        started.store(true, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    });
    let callback = after_run_started(
        ack,
        resources.clone(),
        SharedPermissionMode::new(PermissionMode::Default),
        broker.clone(),
    );
    callback(admission.clone()).await.unwrap();
    callback(admission.clone()).await.unwrap();
    let restored = snapshot_for(resources.as_ref(), &admission).await.unwrap();
    assert_eq!(
        restored.state, original.state,
        "reapproval cannot manufacture a new event, delivery or work generation"
    );
    assert_eq!(restored.control, original.control);
    assert_eq!(broker.requests.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn real_store_claimed_mixed_cron_rejection_blocks_without_abandoning_user_with_broker_fixture(
) {
    use peri_acp_types::session_resources::work::ObligationStatus;
    let (_directory, resources, _dispatcher, admission) = registered_cron_fixture(true).await;
    let original = claim_reason_ready_fixture(resources.as_ref(), &admission).await;
    let started = Arc::new(AtomicBool::new(false));
    let broker = Arc::new(StartedLeaseBrokerFixture {
        started: started.clone(),
        requests: AtomicUsize::new(0),
        approve: false,
    });
    let ack: SdkRunStartedFn = Arc::new(move |_| {
        started.store(true, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    });
    assert!(after_run_started(
        ack,
        resources.clone(),
        SharedPermissionMode::new(PermissionMode::Default),
        broker
    )(admission.clone())
    .await
    .unwrap_err()
    .contains("durably blocked"));
    let snapshot = resources
        .load_session_work(&WorkQuery {
            session_id: admission.session_id.clone(),
            limit: 1,
        })
        .await
        .unwrap();
    assert_eq!(
        snapshot.state.works[&admission.work_id].stage,
        WorkStage::Blocked
    );
    assert_eq!(
        snapshot.state.obligations.keys().collect::<Vec<_>>(),
        original.state.obligations.keys().collect::<Vec<_>>()
    );
    for (delivery_id, obligation) in &snapshot.state.obligations {
        let original_obligation = &original.state.obligations[delivery_id];
        assert_eq!(original_obligation.status, ObligationStatus::InProgress);
        assert_eq!(obligation.status, ObligationStatus::Blocked);
        assert_eq!(obligation.work_id, original_obligation.work_id);
        assert_eq!(
            obligation.reason.as_deref(),
            Some("restored mixed scheduled batch approval rejected")
        );
    }
    assert_eq!(snapshot.state.deliveries, original.state.deliveries);
    assert_eq!(snapshot.state.batches, original.state.batches);
    assert!(snapshot.blocked);
}
