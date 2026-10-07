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

#[test]
fn observer_floor_requires_new_pending_required_delivery() {
    use peri_acp_types::session_resources::work::*;
    let content = EvidenceWrite {
        session_id: "observer".into(),
        storage_scope: "observer".into(),
        payload_id: "body".into(),
        encoding: 1,
        bytes: b"{}".to_vec(),
    }
    .reference()
    .unwrap();
    let mut delivery = Delivery {
        delivery_id: "new".into(),
        recipient_lifecycle: 1,
        revision: 0,
        admission_sequence: 9,
        delegation: None,
        publication: PublishDelivery {
            delivery_id: "new".into(),
            purpose: DeliveryPurpose::UserInput,
            policy: peri_acp_types::session::MessagePolicy::ensure_processing(),
            event: WorkEvent {
                producer_namespace: "fixture".into(),
                event_id: "new".into(),
                event_kind: "input".into(),
                causation_id: None,
                content: WorkPayload {
                    message_id: peri_acp_types::messages::MessageId::new(),
                    role: "user".into(),
                    content,
                    tool_call_id: None,
                },
            },
        },
        projection: None,
        projection_version: 0,
        processing_id: None,
        batch_ordinal: None,
        participates_in_reason: false,
        obligation: ObligationStatus::Pending,
        disposition: None,
    };
    assert!(super::observer_delivery_available(&delivery, 1, 9));
    assert!(!super::observer_delivery_available(&delivery, 1, 10));
    assert!(!super::observer_delivery_available(&delivery, 2, 9));
    delivery.obligation = ObligationStatus::Satisfied;
    assert!(!super::observer_delivery_available(&delivery, 1, 9));
    delivery.obligation = ObligationStatus::Pending;
    delivery.publication.policy = peri_acp_types::session::MessagePolicy::passive();
    assert!(!super::observer_delivery_available(&delivery, 1, 9));
}
