use super::*;
use crate::types::TraceBody;
use crate::IngestionEvent;
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

/// 测试预算：只用于通过准入的预算身份校验，不参与字节容量断言。
fn budget() -> Arc<MemoryBudget> {
    MemoryBudget::new(usize::MAX).unwrap()
}

fn telemetry(name: &str, size: usize, budget: &Arc<MemoryBudget>) -> TelemetryEvent {
    TelemetryEvent::try_new(event(name, size), None, None, budget).unwrap()
}

#[tokio::test]
async fn byte_capacity_is_independent_of_command_slots_and_is_released_on_receive() {
    let budget = budget();
    let bytes = queued_bytes(&telemetry("same", 20, &budget), usize::MAX).unwrap();
    let (admission, mut receiver, _) = Admission::for_test(8, usize::MAX, bytes * 2);
    admission
        .try_add(telemetry("same", 20, &budget), BackpressurePolicy::DropNew)
        .unwrap();
    admission
        .try_add(telemetry("same", 20, &budget), BackpressurePolicy::DropNew)
        .unwrap();
    assert!(matches!(
        admission.try_add(telemetry("same", 20, &budget), BackpressurePolicy::DropNew),
        Err(LangfuseError::QueueFull)
    ));
    assert!(receiver.try_recv().is_some());
    admission
        .try_add(telemetry("same", 20, &budget), BackpressurePolicy::DropNew)
        .unwrap();
    assert_eq!(admission.shared.state.lock().unwrap().bytes, bytes * 2);
}

#[tokio::test]
async fn oversized_event_is_rejected_without_publishing_payload() {
    let budget = budget();
    let (admission, mut receiver, _) = Admission::for_test(8, 256, 2048);
    let error = admission
        .try_add(
            telemetry("private-marker", 4096, &budget),
            BackpressurePolicy::DropNew,
        )
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
    let budget = budget();
    let large_bytes = queued_bytes(&telemetry("same", 200, &budget), usize::MAX).unwrap();
    let (admission, mut receiver, _) = Admission::for_test(8, usize::MAX, large_bytes);
    admission
        .try_add(
            telemetry("same", 0, &budget),
            BackpressurePolicy::DropOldest,
        )
        .unwrap();
    admission
        .try_add(
            telemetry("same", 0, &budget),
            BackpressurePolicy::DropOldest,
        )
        .unwrap();
    assert!(matches!(
        admission
            .try_add(
                telemetry("same", 200, &budget),
                BackpressurePolicy::DropOldest
            )
            .unwrap(),
        AdmissionOutcome::ReplacedOldest(2)
    ));
    assert!(receiver.try_recv().is_some());
    assert!(receiver.try_recv().is_none());

    admission
        .try_add(
            telemetry("same", 0, &budget),
            BackpressurePolicy::DropOldest,
        )
        .unwrap();
    let (ack, _) = tokio::sync::oneshot::channel();
    admission.flush_command(ack).await.unwrap();
    admission
        .try_add(
            telemetry("same", 0, &budget),
            BackpressurePolicy::DropOldest,
        )
        .unwrap();
    assert!(matches!(
        admission.try_add(
            telemetry("same", 200, &budget),
            BackpressurePolicy::DropOldest
        ),
        Err(LangfuseError::QueueFull)
    ));
    assert_eq!(admission.shared.state.lock().unwrap().commands.len(), 3);
}

#[tokio::test]
async fn blocked_byte_waiter_wakes_on_close_and_does_not_publish() {
    let budget = budget();
    let bytes = queued_bytes(&telemetry("same", 20, &budget), usize::MAX).unwrap();
    let (admission, mut receiver, _) = Admission::for_test(8, usize::MAX, bytes);
    admission
        .try_add(telemetry("same", 20, &budget), BackpressurePolicy::Block)
        .unwrap();
    let mut send = Box::pin(admission.send(telemetry("same", 20, &budget)));
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
