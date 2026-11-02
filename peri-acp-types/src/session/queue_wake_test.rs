use super::*;
use crate::identity::AttemptId;
use crate::session::{MqReceivePurpose, SessionInbox, TurnId};
use std::future::{poll_fn, Future};
use std::task::Poll;
use std::time::Duration;

fn execution() -> ExecutionBinding {
    ExecutionBinding {
        turn_id: TurnId::new(),
        attempt_id: AttemptId::new(),
    }
}

async fn receive_all(queue: &MessageQueue) {
    let permit = queue.begin_receive(MqReceivePurpose::ModelLoop, execution());
    let items = queue.select_for_receive(&permit).await.unwrap();
    queue.receive(permit, items).await.unwrap();
}

#[tokio::test]
async fn retained_work_hints_observe_shared_queue_without_consuming_messages() {
    let queue = MessageQueue::new();
    let shared = queue.clone();
    let mut changes = queue.subscribe_wake();
    shared.enqueue(message(MessageKind::Defer)).await.unwrap();
    shared
        .enqueue_batch(vec![message(MessageKind::Prompt)])
        .await
        .unwrap();
    assert!(changes.has_changed().unwrap());
    assert_eq!(*changes.borrow_and_update(), 2);
    assert_eq!(*shared.subscribe_wake().borrow(), 2);
    assert!(!changes.has_changed().unwrap());
    assert_eq!(queue.len().await.unwrap(), 2);
    assert_eq!(queue.snapshot().await.unwrap().len(), 2);
    assert_eq!(*changes.borrow(), 2);
}

#[tokio::test]
async fn passive_messages_do_not_publish_retained_work_hints() {
    let queue = MessageQueue::new();
    let changes = queue.subscribe_wake();
    queue.enqueue(message(MessageKind::Info)).await.unwrap();
    queue
        .enqueue_batch(vec![message(MessageKind::Info)])
        .await
        .unwrap();
    queue.enqueue_batch(vec![]).await.unwrap();
    assert!(!changes.has_changed().unwrap());
    assert_eq!(*changes.borrow(), 0);
    assert_eq!(queue.len().await.unwrap(), 2);
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
        queue.enqueue(message(kind)).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .expect("queue publication must wake the registered inbox");
        assert_eq!(queue.len().await.unwrap(), 1);
    }
}

/// [回归测试] 相同 queue 的包装器不能创建第二套唤醒身份。
#[tokio::test]
async fn shared_queue_inboxes_wake_all_registered_observers() {
    let queue = Arc::new(MessageQueue::new());
    let first = SessionInbox::new(Arc::clone(&queue));
    let second = SessionInbox::new(Arc::clone(&queue));
    let first_waiting = first.await_wake();
    let second_waiting = second.await_wake();
    tokio::pin!(first_waiting, second_waiting);
    poll_fn(|context| {
        assert!(first_waiting.as_mut().poll(context).is_pending());
        assert!(second_waiting.as_mut().poll(context).is_pending());
        Poll::Ready(())
    })
    .await;
    second
        .handle()
        .push(message(MessageKind::Defer))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        first_waiting.await;
        second_waiting.await;
    })
    .await
    .expect("all wrappers must observe the same mailbox publication");
    assert_eq!(queue.len().await.unwrap(), 1);
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
    queue
        .enqueue_batch(vec![
            message(MessageKind::Info),
            message(MessageKind::Defer),
        ])
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), waiting)
        .await
        .expect("a batch with executable work must wake the inbox");
    assert_eq!(queue.len().await.unwrap(), 2);
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
    queue.enqueue(message(MessageKind::Info)).await.unwrap();
    queue
        .enqueue_batch(vec![message(MessageKind::Info)])
        .await
        .unwrap();
    queue.enqueue_batch(vec![]).await.unwrap();
    poll_fn(|context| {
        assert!(waiting.as_mut().poll(context).is_pending());
        Poll::Ready(())
    })
    .await;
    assert!(!queue.has_wake_up().await.unwrap());
    assert_eq!(queue.len().await.unwrap(), 2);
}

#[tokio::test]
async fn consumed_work_does_not_leave_a_ready_waiter() {
    let queue = Arc::new(MessageQueue::new());
    let inbox = SessionInbox::new(Arc::clone(&queue));
    queue.enqueue(message(MessageKind::Defer)).await.unwrap();
    receive_all(&queue).await;
    let waiting = inbox.await_wake();
    tokio::pin!(waiting);
    poll_fn(|context| {
        assert!(waiting.as_mut().poll(context).is_pending());
        Poll::Ready(())
    })
    .await;
    queue.enqueue(message(MessageKind::Prompt)).await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), waiting)
        .await
        .expect("new work must wake after a prior receive");
}

/// [回归测试] 未发送的用户输入即使策略要求处理，也不算本轮可消费工作：
/// 判定与裁剪必须共用同一口径，否则「有工作但 Receive 取不出」会让循环空转。
#[tokio::test]
async fn unsent_user_input_waits_for_send_selection() {
    let queue = MessageQueue::new();
    let execution = execution();
    let prompt = QueuedMessage::prompt(MessageSource::UserInput, BaseMessage::human("pending"));
    let id = prompt.id();
    queue.enqueue(prompt).await.unwrap();

    assert!(!queue.has_ensure_processing().await.unwrap());
    assert!(!queue.has_required_for_run(&execution).await.unwrap());
    assert!(!queue.has_wake_up().await.unwrap());
    assert!(!queue.needs_mq_continuation().await.unwrap());
    let permit = queue.begin_receive(MqReceivePurpose::ModelLoop, execution.clone());
    assert!(queue.select_for_receive(&permit).await.unwrap().is_empty());
    drop(permit);

    queue.send_inputs(vec![id]).await.unwrap();
    assert!(queue.has_required_for_run(&execution).await.unwrap());
    assert!(queue.has_wake_up().await.unwrap());
    let permit = queue.begin_receive(MqReceivePurpose::ModelLoop, execution.clone());
    let items = queue.select_for_receive(&permit).await.unwrap();
    assert_eq!(items.len(), 1);
    queue.receive(permit, items).await.unwrap();
    assert!(!queue.has_required_for_run(&execution).await.unwrap());
    assert!(queue.is_empty().await.unwrap());
}
