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
        .inspect_work(&WorkQuery {
            session_id: session_id.clone(),
            limit: 64,
            selector: WorkSelector::Availability,
            cursor: None,
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
    let original_deliveries = deliveries(resources.as_ref(), &admission.session_id, None).await;
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
    assert!(
        callback(admission.clone())
            .await
            .unwrap_err()
            .contains("explicitly abandoned")
    );
    let snapshot = resources
        .inspect_work(&WorkQuery {
            session_id: admission.session_id.clone(),
            limit: 64,
            selector: WorkSelector::Availability,
            cursor: None,
        })
        .await
        .unwrap();
    assert!(snapshot.head.current_processing_id.is_none());
    let deliveries = reread_deliveries(
        resources.as_ref(),
        &admission.session_id,
        &original_deliveries,
    )
    .await;
    assert!(deliveries.iter().all(|delivery| delivery.obligation
        == peri_acp_types::session_resources::work::ObligationStatus::Abandoned));
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
            .inspect_work(&WorkQuery {
                session_id: admission.session_id,
                limit: 1,
                selector: WorkSelector::Availability,
                cursor: None,
            })
            .await
            .unwrap();
        assert!(
            snapshot.head.current_processing_id.is_none(),
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
    assert!(
        after_run_started(
            ack,
            resources.clone(),
            SharedPermissionMode::new(PermissionMode::Default),
            broker.clone()
        )(admission.clone())
        .await
        .is_err()
    );
    assert_eq!(broker.requests.load(Ordering::SeqCst), 0);
    let snapshot = resources
        .inspect_work(&WorkQuery {
            session_id: admission.session_id,
            limit: 64,
            selector: WorkSelector::Availability,
            cursor: None,
        })
        .await
        .unwrap();
    assert!(snapshot.head.current_processing_id.is_none());
    assert!(snapshot.head.has_pending_work());
}

#[tokio::test]
async fn real_sdk_mixed_cron_rejection_preserves_earlier_user_delivery_with_broker_fixture() {
    use peri_acp_types::session_resources::work::ObligationStatus;
    let (_directory, resources, _dispatcher, admission) = registered_cron_fixture(true).await;
    let original_deliveries = deliveries(resources.as_ref(), &admission.session_id, None).await;
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
    assert!(
        after_run_started(
            ack,
            resources.clone(),
            SharedPermissionMode::new(PermissionMode::Default),
            broker
        )(admission.clone())
        .await
        .unwrap_err()
        .contains("explicitly abandoned")
    );
    let snapshot = resources
        .inspect_work(&WorkQuery {
            session_id: admission.session_id,
            limit: 64,
            selector: WorkSelector::Availability,
            cursor: None,
        })
        .await
        .unwrap();
    let rows = reread_deliveries(
        resources.as_ref(),
        &snapshot.session_id,
        &original_deliveries,
    )
    .await;
    let mut user = None;
    let mut cron = None;
    for delivery in &rows {
        let evidence = resources
            .read_evidence(&EvidenceQuery {
                session_id: snapshot.session_id.clone(),
                reference: delivery.publication.event.content.content.clone(),
            })
            .await
            .unwrap();
        evidence.validate().unwrap();
        let serialized = String::from_utf8(evidence.bytes).unwrap();
        if serialized.contains("unrelated durable user request") {
            user = Some(delivery);
        }
        if serialized.contains("scheduled-task-fixture") {
            cron = Some(delivery);
        }
    }
    let user = user.unwrap();
    let cron = cron.unwrap();
    assert!(user.admission_sequence < cron.admission_sequence);
    assert_eq!(user.obligation, ObligationStatus::Pending);
    assert!(user.processing_id.is_none());
    assert_eq!(cron.obligation, ObligationStatus::Abandoned);
    assert!(snapshot.head.current_processing_id.is_none());
    assert!(
        crate::host::work_query::availability(&snapshot)
            .unwrap()
            .candidates
            .iter()
            .any(|candidate| candidate.delivery_ids == vec![user.delivery_id.clone()])
    );
}

async fn claim_reason_ready_fixture(
    resources: &dyn SessionResources,
    admission: &WorkAdmission,
) -> WorkInspection {
    let snapshot = snapshot_for(resources, admission).await.unwrap();
    let candidate = crate::host::work_query::availability(&snapshot)
        .unwrap()
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
                    expected_revision: snapshot.head.change_seq,
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
    let original_deliveries = deliveries(
        resources.as_ref(),
        &admission.session_id,
        Some(&admission.work_id),
    )
    .await;
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
        restored.head, original.head,
        "reapproval cannot manufacture a new event, delivery or work generation"
    );
    assert_eq!(restored.control, original.control);
    assert_eq!(
        deliveries(
            resources.as_ref(),
            &admission.session_id,
            Some(&admission.work_id)
        )
        .await,
        original_deliveries
    );
    assert_eq!(broker.requests.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn real_store_claimed_mixed_cron_rejection_blocks_without_abandoning_user_with_broker_fixture()
 {
    let (_directory, resources, _dispatcher, admission) = registered_cron_fixture(true).await;
    claim_reason_ready_fixture(resources.as_ref(), &admission).await;
    let original_deliveries = deliveries(
        resources.as_ref(),
        &admission.session_id,
        Some(&admission.work_id),
    )
    .await;
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
    assert!(
        after_run_started(
            ack,
            resources.clone(),
            SharedPermissionMode::new(PermissionMode::Default),
            broker
        )(admission.clone())
        .await
        .unwrap_err()
        .contains("durably blocked")
    );
    let snapshot = resources
        .inspect_work(&WorkQuery {
            session_id: admission.session_id.clone(),
            limit: 64,
            selector: WorkSelector::Availability,
            cursor: None,
        })
        .await
        .unwrap();
    assert_eq!(
        crate::host::work_query::test_processing(
            resources.as_ref(),
            &admission.session_id,
            &admission.work_id
        )
        .await
        .stage,
        WorkStage::Blocked
    );
    assert_eq!(
        deliveries(
            resources.as_ref(),
            &admission.session_id,
            Some(&admission.work_id)
        )
        .await,
        original_deliveries
    );
    assert!(
        crate::host::work_query::availability(&snapshot)
            .unwrap()
            .blocked
    );
}

async fn deliveries(
    resources: &dyn SessionResources,
    session_id: &str,
    processing_id: Option<&str>,
) -> Vec<Delivery> {
    let selector = processing_id.map_or(WorkSelector::Inbox, |identity| {
        WorkSelector::ProcessingDeliveries {
            processing_id: identity.into(),
        }
    });
    let inspection = resources
        .inspect_work(&WorkQuery::new(session_id, selector))
        .await
        .unwrap();
    let WorkPage::Deliveries(rows) = inspection.page else {
        panic!("expected deliveries")
    };
    assert!(inspection.next_cursor.is_none());
    rows
}

async fn reread_deliveries(
    resources: &dyn SessionResources,
    session_id: &str,
    original: &[Delivery],
) -> Vec<Delivery> {
    let mut rows = Vec::new();
    for delivery in original {
        rows.push(
            crate::host::work_query::test_delivery(resources, session_id, &delivery.delivery_id)
                .await,
        );
    }
    rows
}
