use super::*;
use crate::identity::AttemptId;
use crate::messages::BaseMessage;
use crate::session::{MqReceiveDisposition, MqReceivePurpose, TurnId};

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

#[tokio::test]
async fn properties_not_kind_or_severity_decide_activation() {
    let queue = MessageQueue::new();
    let current = execution();
    queue
        .enqueue(message(MessagePolicy::passive()))
        .await
        .unwrap();
    assert!(!queue.has_required_for_run(&current).await.unwrap());
    assert!(!queue.has_ensure_processing().await.unwrap());
    queue
        .enqueue(message(MessagePolicy::continue_current_run(
            current.clone(),
        )))
        .await
        .unwrap();
    assert!(queue.has_required_for_run(&current).await.unwrap());
    assert!(!queue.has_ensure_processing().await.unwrap());
    assert!(!queue.has_required_for_run(&execution()).await.unwrap());
    queue
        .enqueue(message(MessagePolicy::ensure_processing()))
        .await
        .unwrap();
    assert!(queue.has_ensure_processing().await.unwrap());
}

#[test]
fn stop_intent_is_bound_to_its_own_execution() {
    let current = execution();
    let stop = message(MessagePolicy::stop_current_run(current.clone()));
    assert_eq!(
        stop.policy.disposition(&current),
        MessageDisposition::ProjectOnly,
        "同一执行的 Stop 不驱动语义续跑，但仍必须可被 Receive 识别"
    );
    assert_eq!(
        stop.policy.disposition(&execution()),
        MessageDisposition::Suppressed,
        "旧执行的 Stop 不得影响新执行"
    );
    assert!(!stop.policy.ensures_processing());
    assert!(!stop.policy.notifies_execution());
}

/// 旧执行绑定的 guidance 在 Receive 中被显式标记为 `Suppress`：删行、不写历史。
#[tokio::test]
async fn expired_guidance_is_explicitly_suppressed() {
    let queue = MessageQueue::new();
    let guidance = message(MessagePolicy::continue_current_run(execution()));
    assert_eq!(
        guidance.policy.disposition(&execution()),
        MessageDisposition::Suppressed
    );
    queue.enqueue(guidance).await.unwrap();
    let permit = queue.begin_receive(MqReceivePurpose::ModelLoop, execution());
    let items = queue.select_for_receive(&permit).await.unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].disposition, MqReceiveDisposition::Suppress);
    assert!(!queue.has_ensure_processing().await.unwrap());
}

/// 单次 Receive 有界；剩余项按 `(enqueued_at, id)` 保留，不因批次被截断而丢顺序。
#[tokio::test]
async fn receive_selection_is_bounded_and_preserves_remaining_order() {
    let queue = MessageQueue::new();
    let mut ids = Vec::new();
    for _ in 0..70 {
        let outcome = queue
            .enqueue(message(MessagePolicy::ensure_processing()))
            .await
            .unwrap();
        ids.push(outcome.entry.id);
    }
    let permit = queue.begin_receive(MqReceivePurpose::ModelLoop, execution());
    let claimed = queue.select_for_receive(&permit).await.unwrap();
    assert_eq!(claimed.len(), 64);
    let claimed_ids = claimed.iter().map(|item| item.entry.id).collect::<Vec<_>>();
    assert_eq!(claimed_ids, ids[..64]);
    queue.receive(permit, claimed).await.unwrap();
    assert_eq!(queue.len().await.unwrap(), 6);

    let permit = queue.begin_receive(MqReceivePurpose::ModelLoop, execution());
    let remaining = queue.select_for_receive(&permit).await.unwrap();
    let remaining_ids = remaining
        .iter()
        .map(|item| item.entry.id)
        .collect::<Vec<_>>();
    assert_eq!(remaining_ids, ids[64..]);
}

#[tokio::test]
async fn run_bound_guidance_wakes_only_its_current_execution() {
    let queue = MessageQueue::new();
    let current = execution();
    queue
        .enqueue(message(MessagePolicy::continue_current_run(
            current.clone(),
        )))
        .await
        .unwrap();
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
