use super::*;
use crate::types::TraceBody;
use tokio::sync::oneshot;

fn event(id: &str) -> IngestionEvent {
    IngestionEvent::TraceCreate {
        id: id.into(),
        timestamp: "2026-01-01T00:00:00Z".into(),
        body: TraceBody {
            id: Some(id.into()),
            ..Default::default()
        },
        metadata: None,
    }
}

async fn poll_pending<T>(future: std::pin::Pin<&mut impl std::future::Future<Output = T>>) {
    let mut future = future;
    std::future::poll_fn(|cx| {
        assert!(future.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
}

#[tokio::test]
async fn test_close_drains_committed_work_without_waiting_for_reserved_permit() {
    let (admission, mut receiver, _) = Admission::new(2);
    let reserved = Arc::clone(&admission.shared.slots)
        .acquire_owned()
        .await
        .unwrap();
    admission
        .try_add(event("committed"), BackpressurePolicy::Block)
        .unwrap();
    admission.close();
    assert!(matches!(receiver.try_recv(), Some(BatcherCommand::Add(_))));
    assert!(
        receiver.try_recv().is_none(),
        "尚未提交的 permit 不进入 drain 集合"
    );
    assert!(matches!(
        admission.commit(BatcherCommand::Add(event("late")), reserved),
        Err(LangfuseError::ChannelClosed)
    ));
    assert!(receiver.recv().await.is_none());
}

#[tokio::test]
async fn test_cancelled_capacity_waiter_cannot_publish_and_returns_its_slot() {
    let (admission, mut receiver, _) = Admission::new(1);
    admission
        .try_add(event("first"), BackpressurePolicy::Block)
        .unwrap();
    let mut pending = Box::pin(admission.send(BatcherCommand::Add(event("cancelled"))));
    poll_pending(pending.as_mut()).await;
    drop(pending);
    assert!(matches!(
        receiver.recv().await,
        Some(BatcherCommand::Add(_))
    ));
    admission
        .try_add(event("next"), BackpressurePolicy::Block)
        .unwrap();
    let Some(BatcherCommand::Add(IngestionEvent::TraceCreate { id, .. })) = receiver.try_recv()
    else {
        panic!("必须保留后续成功准入事件")
    };
    assert_eq!(id, "next");
    assert!(receiver.try_recv().is_none(), "已取消的等待者不得迟到提交");
}

#[tokio::test]
async fn test_receiver_drop_releases_flush_ack_and_blocked_producer() {
    let (admission, receiver, _) = Admission::new(1);
    let (ack, response) = oneshot::channel();
    admission.send(BatcherCommand::Flush(ack)).await.unwrap();
    let mut pending = Box::pin(admission.send(BatcherCommand::Add(event("blocked"))));
    poll_pending(pending.as_mut()).await;
    drop(receiver);
    assert!(
        response.await.is_err(),
        "worker 消失必须释放已入队的 flush ack"
    );
    assert!(matches!(pending.await, Err(LangfuseError::ChannelClosed)));
    assert!(matches!(
        admission.try_add(event("late"), BackpressurePolicy::DropOldest),
        Err(LangfuseError::ChannelClosed)
    ));
}

#[tokio::test]
async fn test_cancelled_receive_does_not_lose_future_notification() {
    let (admission, mut receiver, _) = Admission::new(1);
    let mut pending = Box::pin(receiver.recv());
    poll_pending(pending.as_mut()).await;
    drop(pending);
    admission
        .try_add(event("later"), BackpressurePolicy::DropNew)
        .unwrap();
    let command = tokio::time::timeout(std::time::Duration::from_secs(1), receiver.recv())
        .await
        .expect("取消 recv 后下一次等待仍应被唤醒");
    assert!(matches!(command, Some(BatcherCommand::Add(_))));
}
