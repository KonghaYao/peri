use super::*;
use crate::session::SessionInbox;
use std::future::{poll_fn, Future};
use std::task::Poll;
use std::time::Duration;

#[tokio::test]
async fn retained_work_hints_observe_shared_queue_without_consuming_messages() {
    let queue = MessageQueue::new();
    let shared = queue.clone();
    let mut changes = queue.subscribe_wake();
    shared.push(message(MessageKind::Defer));
    shared.push_batch(vec![message(MessageKind::Prompt)]);
    assert!(changes.has_changed().unwrap());
    assert_eq!(*changes.borrow_and_update(), 2);
    assert_eq!(*shared.subscribe_wake().borrow(), 2);
    assert!(!changes.has_changed().unwrap());
    assert_eq!(queue.len(), 2);
    assert_eq!(queue.drain_all().len(), 2);
    assert_eq!(*changes.borrow(), 2);
}

#[tokio::test]
async fn passive_messages_do_not_publish_retained_work_hints() {
    let queue = MessageQueue::new();
    let changes = queue.subscribe_wake();
    queue.push(message(MessageKind::Info));
    queue.push_batch(vec![message(MessageKind::Info)]);
    queue.push_batch(vec![]);
    assert!(!changes.has_changed().unwrap());
    assert_eq!(*changes.borrow(), 0);
    assert_eq!(queue.len(), 2);
}

fn message(kind: MessageKind) -> QueuedMessage {
    QueuedMessage::new(
        kind,
        MessageSource::SystemInjected,
        BaseMessage::human("work"),
    )
}

/// [回归测试] 先挂起再直接发布 Defer，不能只入队而漏唤醒。
#[tokio::test]
async fn direct_queue_publication_wakes_registered_inbox() {
    for kind in [MessageKind::Prompt, MessageKind::Defer] {
        let queue = Arc::new(MessageQueue::new());
        let inbox = SessionInbox::new(Arc::clone(&queue));
        let waiting = inbox.await_wake();
        tokio::pin!(waiting);
        poll_fn(|context| {
            assert!(waiting.as_mut().poll(context).is_pending());
            Poll::Ready(())
        })
        .await;
        queue.push(message(kind));
        tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .expect("queue publication must wake the registered inbox");
        assert_eq!(queue.len(), 1);
    }
}

/// [回归测试] 相同 queue 的包装器不能创建第二套唤醒身份。
#[tokio::test]
async fn shared_queue_inboxes_wake_all_registered_observers() {
    let queue = Arc::new(MessageQueue::new());
    let first = SessionInbox::new(Arc::clone(&queue));
    let second = SessionInbox::new(Arc::new(queue.as_ref().clone()));
    let first_waiting = first.await_wake();
    let second_waiting = second.await_wake();
    tokio::pin!(first_waiting, second_waiting);
    poll_fn(|context| {
        assert!(first_waiting.as_mut().poll(context).is_pending());
        assert!(second_waiting.as_mut().poll(context).is_pending());
        Poll::Ready(())
    })
    .await;
    second.handle().push(message(MessageKind::Defer));
    tokio::time::timeout(Duration::from_secs(1), async {
        first_waiting.await;
        second_waiting.await;
    })
    .await
    .expect("all wrappers must observe the same mailbox publication");
    assert_eq!(queue.len(), 1);
}

#[tokio::test]
async fn batch_publication_wakes_registered_inbox() {
    let queue = Arc::new(MessageQueue::new());
    let inbox = SessionInbox::new(Arc::clone(&queue));
    let waiting = inbox.await_wake();
    tokio::pin!(waiting);
    poll_fn(|context| {
        assert!(waiting.as_mut().poll(context).is_pending());
        Poll::Ready(())
    })
    .await;
    queue.push_batch(vec![
        message(MessageKind::Info),
        message(MessageKind::Defer),
    ]);
    tokio::time::timeout(Duration::from_secs(1), waiting)
        .await
        .expect("a batch with executable work must wake the inbox");
    assert_eq!(queue.len(), 2);
}

#[tokio::test]
async fn passive_publication_and_empty_batch_do_not_wake() {
    let queue = Arc::new(MessageQueue::new());
    let inbox = SessionInbox::new(Arc::clone(&queue));
    let waiting = inbox.await_wake();
    tokio::pin!(waiting);
    poll_fn(|context| {
        assert!(waiting.as_mut().poll(context).is_pending());
        Poll::Ready(())
    })
    .await;
    queue.push(message(MessageKind::Info));
    queue.push_batch(vec![message(MessageKind::Info)]);
    queue.push_batch(vec![]);
    poll_fn(|context| {
        assert!(waiting.as_mut().poll(context).is_pending());
        Poll::Ready(())
    })
    .await;
    assert!(!queue.has_wake_up());
    assert_eq!(queue.len(), 2);
}

#[tokio::test]
async fn consumed_work_does_not_leave_a_ready_waiter() {
    let queue = Arc::new(MessageQueue::new());
    let inbox = SessionInbox::new(Arc::clone(&queue));
    queue.push(message(MessageKind::Defer));
    assert_eq!(queue.drain_all().len(), 1);
    let waiting = inbox.await_wake();
    tokio::pin!(waiting);
    poll_fn(|context| {
        assert!(waiting.as_mut().poll(context).is_pending());
        Poll::Ready(())
    })
    .await;
    queue.push(message(MessageKind::Prompt));
    tokio::time::timeout(Duration::from_secs(1), waiting)
        .await
        .expect("new work must wake after a prior drain");
}
