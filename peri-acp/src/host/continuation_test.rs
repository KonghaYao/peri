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

// ── M13：cron 触发承载预算与稳定身份 ──────────────────────────────────────────

fn cron_queue() -> peri_acp_types::session::MessageQueue {
    peri_acp_types::session::MessageQueue::default()
}

fn drain_reminders(
    queue: &peri_acp_types::session::MessageQueue,
) -> Vec<peri_acp_types::system_reminder::SystemReminder> {
    queue
        .drain_all()
        .into_iter()
        .filter_map(|message| match message.payload {
            peri_acp_types::session::QueuedPayload::SystemReminder(reminder) => {
                Some(reminder.into_inner())
            }
            peri_acp_types::session::QueuedPayload::Message(_) => None,
        })
        .collect()
}

/// 可承载预算内的触发按 task + firing 身份投递，且 metadata 不重复保存完整 prompt。
#[tokio::test]
async fn cron_trigger_is_delivered_with_stable_identity_and_identity_only_metadata() {
    let queue = cron_queue();
    let trigger = peri_acp_types::cron::CronTrigger {
        task_id: "task-1".into(),
        firing_id: "2026-10-07T00:00:00+00:00".into(),
        prompt: "检查构建状态".into(),
    };

    let first = super::enqueue_cron_trigger(&queue, &trigger);
    assert_eq!(first, super::CronTriggerAdmission::Delivered);
    let delivered_again = super::enqueue_cron_trigger(&queue, &trigger);
    assert_eq!(delivered_again, super::CronTriggerAdmission::Delivered);

    let reminders = drain_reminders(&queue);
    assert_eq!(reminders.len(), 2, "两次投递都进入队列");
    for reminder in &reminders {
        assert_eq!(reminder.kind, "triggered");
        assert!(reminder.metadata.get("prompt").is_none());
        assert_eq!(reminder.metadata["task_id"], "task-1");
        assert_eq!(reminder.metadata["firing_id"], "2026-10-07T00:00:00+00:00");
        assert_eq!(reminder.metadata["prompt_bytes"], "检查构建状态".len());
        assert_eq!(
            peri_acp_types::cron::cron_trigger_prompt(reminder).as_deref(),
            Some("检查构建状态")
        );
    }
    assert_eq!(
        peri_acp_types::cron::cron_firing_delivery_id("task-1", "2026-10-07T00:00:00+00:00"),
        peri_acp_types::cron::cron_firing_delivery_id("task-1", "2026-10-07T00:00:00+00:00"),
        "同一 firing 的重试必须复用同一 delivery_id"
    );
}

/// 历史超长任务不得 panic、不得截断后照常执行；必须留下可观察的投递失败。
#[tokio::test]
async fn oversized_historical_cron_trigger_fails_observably_without_truncated_execution() {
    let queue = cron_queue();
    let prompt = "汉".repeat(peri_acp_types::cron::MAX_CRON_PROMPT_BYTES / 3 + 1);
    let trigger = peri_acp_types::cron::CronTrigger {
        task_id: "legacy-task".into(),
        firing_id: "2026-10-07T00:00:00+00:00".into(),
        prompt: prompt.clone(),
    };

    match super::enqueue_cron_trigger(&queue, &trigger) {
        super::CronTriggerAdmission::Rejected { reason } => {
            assert!(
                reason.contains(&peri_acp_types::cron::MAX_CRON_PROMPT_BYTES.to_string()),
                "拒绝必须给出承载限制: {reason}"
            );
        }
        super::CronTriggerAdmission::Delivered => panic!("超长任务不得被静默接纳"),
    }

    let reminders = drain_reminders(&queue);
    assert_eq!(reminders.len(), 1, "必须留下恰好一条失败证据");
    let notice = &reminders[0];
    assert_eq!(notice.kind, "trigger_undeliverable");
    assert!(!notice.body.contains(&prompt), "失败证据不得回放超长原文");
    assert!(!notice
        .audiences
        .contains(peri_acp_types::system_reminder::ReminderAudience::Model));
    assert!(peri_acp_types::system_reminder::reminder_egress_allowed(
        notice,
        peri_acp_types::system_reminder::ReminderAudience::Tui
    ));
    assert_eq!(notice.metadata["prompt_bytes"], prompt.len());
}
