//! Durable notification bridge: approval, frozen recipient, and owned shutdown.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use peri_acp_types::interaction::{
    ApprovalDecision, InteractionContext, InteractionResponse, UserInteractionBroker,
};
use peri_acp_types::permission::{PermissionMode, SharedPermissionMode};
use tokio_util::sync::CancellationToken;

use super::{recv_until_shutdown, same_recipient};
use crate::session::executor::ContinuationRequest;

struct FixedBroker(InteractionResponse);

#[async_trait::async_trait]
impl UserInteractionBroker for FixedBroker {
    async fn request(&self, _ctx: InteractionContext) -> InteractionResponse {
        self.0.clone()
    }
}

#[tokio::test]
async fn test_scheduled_trigger_permission_contract() {
    let bypass = SharedPermissionMode::new(PermissionMode::Bypass);
    assert!(
        super::super::prompt::approve_scheduled_trigger(bypass.as_ref(), None, "task-1", "status")
            .await
    );

    let default = SharedPermissionMode::new(PermissionMode::Default);
    assert!(
        !super::super::prompt::approve_scheduled_trigger(
            default.as_ref(),
            None,
            "task-1",
            "status"
        )
        .await
    );

    let broker: Arc<dyn UserInteractionBroker> =
        Arc::new(FixedBroker(InteractionResponse::Decisions(vec![
            ApprovalDecision::Approve { source: None },
        ])));
    assert!(
        super::super::prompt::approve_scheduled_trigger(
            default.as_ref(),
            Some(&broker),
            "task-1",
            "status"
        )
        .await
    );
}

#[tokio::test]
async fn test_scheduler_exits_on_host_shutdown_even_if_continuation_ingress_still_exists() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ContinuationRequest>();
    let root = Arc::new(tx);
    let _bounded_callback_clone = root.clone();
    let shutdown = CancellationToken::new();
    shutdown.cancel();

    assert!(recv_until_shutdown(&mut rx, &shutdown).await.is_none());
    assert!(
        !root.is_closed(),
        "kept-alive ingress must not be the exit signal"
    );
}

#[tokio::test]
async fn test_scheduler_does_not_own_its_ingress_sender() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ContinuationRequest>();
    let root = Arc::new(tx);
    let scheduler_ingress = Arc::downgrade(&root);
    drop(root);

    assert!(scheduler_ingress.upgrade().is_none());
    assert!(recv_until_shutdown(&mut rx, &CancellationToken::new())
        .await
        .is_none());
}

#[tokio::test]
async fn test_continuation_child_is_rejected_after_admission_closes() {
    let (owner, spawner) = crate::host::task_scope::HostTaskOwner::new();
    owner.begin_shutdown();
    let started = Arc::new(AtomicBool::new(false));
    let started_task = started.clone();
    assert!(spawner
        .spawn(
            crate::host::task_scope::HostTaskOwnerKind::Session,
            crate::host::task_scope::HostTaskKind::ContinuationTurn,
            async move {
                started_task.store(true, Ordering::SeqCst);
            },
        )
        .is_err());
    tokio::task::yield_now().await;
    assert!(!started.load(Ordering::SeqCst));
}

#[test]
fn cron_frozen_recipient_never_retargets_reopened_lifecycle() {
    use peri_acp_types::session_resources::{ControlState, ControlStatus};
    let frozen = ControlState::default();
    let mut current = frozen.clone();
    assert!(same_recipient(&current, &frozen));
    current.lifecycle += 1;
    assert!(!same_recipient(&current, &frozen));
    current.lifecycle = frozen.lifecycle;
    current.control_generation += 1;
    assert!(same_recipient(&current, &frozen));
    current.control_generation = frozen.control_generation;
    current.status = ControlStatus::Paused;
    assert!(same_recipient(&current, &frozen));
    current.status = ControlStatus::Closing;
    assert!(
        same_recipient(&current, &frozen),
        "closing publication decisions belong to the common Store"
    );
}

fn inbox_work_available(
    snapshot: &peri_acp_types::session_resources::work::WorkSnapshot,
    floor: Option<u64>,
) -> bool {
    peri_acp_types::session_resources::work::WorkAvailability {
        control: snapshot.control.clone(),
        state: (&snapshot.state).into(),
    }
    .is_available(snapshot.control.lifecycle, floor)
}

#[test]
fn observer_floor_requires_unprocessed_new_required_delivery() {
    use peri_acp_types::session_resources::work::*;
    use peri_acp_types::session_resources::ControlState;

    let control = ControlState::default();
    let query = WorkQuery {
        session_id: "observer-floor".into(),
        limit: 1,
    };
    let mut state = WorkState::default();
    for delivery_id in ["old", "new"] {
        let content = WorkPayload::from_payload(&peri_acp_types::store::PersistedPayload::Message(
            peri_acp_types::messages::BaseMessage::human(delivery_id),
        ))
        .unwrap();
        let command = WorkCommand {
            session_id: query.session_id.clone(),
            recipient_lifecycle: control.lifecycle,
            mutation_id: delivery_id.into(),
            action: WorkAction::PublishDelivery {
                delivery: PublishDelivery {
                    delivery_id: delivery_id.into(),
                    event: WorkEvent {
                        producer_namespace: "observer-test".into(),
                        event_id: delivery_id.into(),
                        event_kind: "input".into(),
                        causation_id: None,
                        content,
                    },
                    purpose: DeliveryPurpose::UserInput,
                    policy: peri_acp_types::session::MessagePolicy::ensure_processing(),
                },
            },
        };
        let command = PreparedWorkCommand::try_new(command).unwrap();
        let reduction = reduce_work(&command, &control, state.clone()).unwrap();
        assert_eq!(reduction.receipt.decision, WorkDecision::Accepted);
        state = reduction.state.unwrap();
        if delivery_id == "old" {
            let snapshot = WorkSnapshot::from_state(&query, control.clone(), state.clone());
            assert!(!inbox_work_available(
                &snapshot,
                Some(state.next_admission_sequence)
            ));
            assert!(inbox_work_available(&snapshot, None));
        }
    }
    let floor = state.deliveries["new"].admission_sequence;
    let mut snapshot = WorkSnapshot::from_state(&query, control, state);
    assert!(inbox_work_available(&snapshot, Some(floor)));
    snapshot.state.obligations.get_mut("new").unwrap().status = ObligationStatus::Satisfied;
    assert!(!inbox_work_available(&snapshot, Some(floor)));
    assert!(inbox_work_available(&snapshot, None));
    assert_eq!(
        snapshot.state.obligations["old"].status,
        ObligationStatus::Pending
    );
}
