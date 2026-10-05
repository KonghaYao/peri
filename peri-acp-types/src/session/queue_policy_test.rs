use super::*;
use crate::identity::AttemptId;
use crate::messages::BaseMessage;

fn execution() -> ExecutionBinding {
    ExecutionBinding {
        turn_id: TurnId::new(),
        attempt_id: AttemptId::new(),
    }
}

fn message(policy: MessagePolicy) -> QueuedMessage {
    QueuedMessage::info(MessageSource::SystemInjected, BaseMessage::human("fact"))
        .with_policy(policy)
}

#[test]
fn properties_not_kind_or_severity_decide_activation() {
    let queue = MessageQueue::new();
    let current = execution();
    queue.push(message(MessagePolicy::passive()));
    assert!(!queue.has_required_for_run(&current));
    assert!(!queue.has_ensure_processing());
    queue.push(message(MessagePolicy::continue_current_run(
        current.clone(),
    )));
    assert!(queue.has_required_for_run(&current));
    assert!(!queue.has_ensure_processing());
    assert!(!queue.has_required_for_run(&execution()));
    queue.push(message(MessagePolicy::ensure_processing()));
    assert!(queue.has_ensure_processing());
}

#[test]
fn expired_guidance_is_explicitly_suppressed() {
    let queue = MessageQueue::new();
    let guidance = message(MessagePolicy::continue_current_run(execution()));
    assert_eq!(
        guidance.policy.disposition(&execution()),
        MessageDisposition::Suppressed
    );
    queue.suppress(guidance);
    assert_eq!(queue.suppressed_messages().len(), 1);
    assert!(!queue.has_ensure_processing());
}

#[test]
fn receive_batch_is_bounded_and_requeue_preserves_admission_order() {
    let queue = MessageQueue::new();
    for _ in 0..70 {
        queue.push(message(MessagePolicy::ensure_processing()));
    }
    let claimed = queue.drain_batch(64);
    assert_eq!(claimed.len(), 64);
    assert_eq!(queue.len(), 6);
    queue.push(message(MessagePolicy::ensure_processing()));
    queue.push_batch(claimed);
    let all = queue.drain_all();
    let sequences = all
        .iter()
        .map(|item| item.admission_sequence.unwrap())
        .collect::<Vec<_>>();
    assert_eq!(sequences, (1..=71).collect::<Vec<_>>());
}

#[tokio::test]
async fn run_bound_guidance_wakes_only_its_current_execution() {
    let queue = MessageQueue::new();
    let current = execution();
    queue.push(message(MessagePolicy::continue_current_run(
        current.clone(),
    )));
    tokio::time::timeout(
        std::time::Duration::from_millis(20),
        queue.await_wake_for_run(&current),
    )
    .await
    .unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), queue.await_wake())
            .await
            .is_err()
    );
}
