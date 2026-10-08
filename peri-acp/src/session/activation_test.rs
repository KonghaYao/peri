use super::*;
use peri_acp_types::messages::BaseMessage;
use peri_acp_types::session::{MessageKind, MessageSource, QueuedMessage};

fn enqueue(queue: &MessageQueue, kind: MessageKind) {
    queue.push(QueuedMessage::new(
        kind,
        MessageSource::ShellComplete,
        BaseMessage::human("task result"),
    ));
}

#[test]
fn failed_inputs_and_passive_arrivals_do_not_restart_execution() {
    let activation = SessionActivation::default();
    let queue = MessageQueue::new();
    activation.allow();
    enqueue(&queue, MessageKind::Defer);
    activation.record_failed_attempt(queue.admission_watermark());
    assert!(!activation.can_activate(&queue));
    enqueue(&queue, MessageKind::Info);
    assert!(!activation.can_activate(&queue));
    enqueue(&queue, MessageKind::Defer);
    assert!(activation.can_activate(&queue));
}

#[test]
fn required_arrival_during_failed_attempt_remains_eligible() {
    let activation = SessionActivation::default();
    let queue = MessageQueue::new();
    activation.allow();
    enqueue(&queue, MessageKind::Defer);
    let watermark = queue.admission_watermark();
    enqueue(&queue, MessageKind::Defer);
    activation.record_failed_attempt(watermark);
    assert!(activation.can_activate(&queue));
}

#[test]
fn explicit_stop_survives_failure_and_fresh_required_work() {
    let activation = SessionActivation::default();
    let queue = MessageQueue::new();
    activation.allow();
    enqueue(&queue, MessageKind::Defer);
    let watermark = queue.admission_watermark();
    activation.suppress();
    activation.record_failed_attempt(watermark);
    enqueue(&queue, MessageKind::Defer);
    assert!(!activation.can_activate(&queue));
    activation.allow();
    assert!(activation.can_activate(&queue));
}

#[test]
fn requeued_failed_input_preserves_its_admission_boundary() {
    let activation = SessionActivation::default();
    let queue = MessageQueue::new();
    activation.allow();
    enqueue(&queue, MessageKind::Defer);
    let watermark = queue.admission_watermark();
    let messages = queue.drain_all();
    activation.record_failed_attempt(watermark);
    queue.push_batch(messages);
    assert_eq!(queue.admission_watermark(), watermark);
    assert!(!activation.can_activate(&queue));
}
