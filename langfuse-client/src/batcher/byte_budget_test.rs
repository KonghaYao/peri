use super::*;
use crate::types::TraceBody;
use std::future::Future;

fn event(name: &str, size: usize) -> IngestionEvent {
    IngestionEvent::TraceCreate {
        id: name.into(),
        timestamp: "2026-09-30T00:00:00Z".into(),
        body: TraceBody {
            id: Some(name.into()),
            input: Some(serde_json::json!("x".repeat(size))),
            ..Default::default()
        },
        metadata: None,
    }
}

#[tokio::test]
async fn byte_capacity_is_independent_of_command_slots_and_is_released_on_receive() {
    let bytes = event_bytes(&event("same", 20), usize::MAX).unwrap();
    let (admission, mut receiver, _) = Admission::with_limits(8, bytes, bytes * 2);
    admission
        .try_add(event("same", 20), BackpressurePolicy::DropNew)
        .unwrap();
    admission
        .try_add(event("same", 20), BackpressurePolicy::DropNew)
        .unwrap();
    assert!(matches!(
        admission.try_add(event("same", 20), BackpressurePolicy::DropNew),
        Err(LangfuseError::QueueFull)
    ));
    assert!(receiver.try_recv().is_some());
    admission
        .try_add(event("same", 20), BackpressurePolicy::DropNew)
        .unwrap();
    assert_eq!(admission.shared.state.lock().unwrap().bytes, bytes * 2);
}

#[tokio::test]
async fn oversized_event_is_rejected_without_publishing_payload() {
    let (admission, mut receiver, _) = Admission::with_limits(8, 256, 2048);
    let error = admission
        .try_add(event("private-marker", 4096), BackpressurePolicy::DropNew)
        .err()
        .unwrap();
    assert!(matches!(
        error,
        LangfuseError::PayloadTooLarge { limit_bytes: 256 }
    ));
    assert!(!error.to_string().contains("private-marker"));
    assert!(receiver.try_recv().is_none());
}

#[tokio::test]
async fn drop_oldest_can_reclaim_multiple_byte_slots_without_touching_protected_prefix() {
    let large_bytes = event_bytes(&event("same", 200), usize::MAX).unwrap();
    let (admission, mut receiver, _) = Admission::with_limits(8, large_bytes, large_bytes);
    admission
        .try_add(event("same", 0), BackpressurePolicy::DropOldest)
        .unwrap();
    admission
        .try_add(event("same", 0), BackpressurePolicy::DropOldest)
        .unwrap();
    assert!(matches!(
        admission
            .try_add(event("same", 200), BackpressurePolicy::DropOldest)
            .unwrap(),
        AdmissionOutcome::ReplacedOldest(2)
    ));
    assert!(receiver.try_recv().is_some());
    assert!(receiver.try_recv().is_none());

    admission
        .try_add(event("same", 0), BackpressurePolicy::DropOldest)
        .unwrap();
    let (ack, _) = tokio::sync::oneshot::channel();
    admission.send(BatcherCommand::Flush(ack)).await.unwrap();
    admission
        .try_add(event("same", 0), BackpressurePolicy::DropOldest)
        .unwrap();
    assert!(matches!(
        admission.try_add(event("same", 200), BackpressurePolicy::DropOldest),
        Err(LangfuseError::QueueFull)
    ));
    assert_eq!(admission.shared.state.lock().unwrap().commands.len(), 3);
}

#[tokio::test]
async fn blocked_byte_waiter_wakes_on_close_and_does_not_publish() {
    let bytes = event_bytes(&event("same", 20), usize::MAX).unwrap();
    let (admission, mut receiver, _) = Admission::with_limits(8, bytes, bytes);
    admission
        .try_add(event("same", 20), BackpressurePolicy::Block)
        .unwrap();
    let mut send = Box::pin(admission.send(BatcherCommand::Add(event("same", 20))));
    std::future::poll_fn(|context| {
        assert!(send.as_mut().poll(context).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    admission.close();
    assert!(matches!(send.await, Err(LangfuseError::ChannelClosed)));
    assert!(receiver.try_recv().is_some());
    assert!(receiver.try_recv().is_none());
}
