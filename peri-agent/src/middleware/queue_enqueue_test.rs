use peri_acp_types::session::{MessageKind, MessageSource, QueuedMessage};

use crate::agent::session::SessionInbox;
use crate::messages::{BaseMessage, MessageContent};
use crate::middleware::state::MiddlewareState;
use crate::session::MessageQueue;

struct TestState {
    queue: MessageQueue,
    messages: Vec<BaseMessage>,
}

impl MiddlewareState for TestState {
    fn cwd(&self) -> &str {
        ""
    }
    fn messages(&self) -> &[BaseMessage] {
        &self.messages
    }
    fn add_message(&mut self, _: BaseMessage) {}
    fn replace_message(&mut self, message: BaseMessage) -> bool {
        let Some(existing) = self
            .messages
            .iter_mut()
            .find(|existing| existing.id() == message.id())
        else {
            return false;
        };
        *existing = message;
        true
    }
    fn current_step(&self) -> usize {
        0
    }
    fn push_recall(&mut self, _: String) {}
    fn drain_recall(&mut self) -> Vec<String> {
        vec![]
    }
    fn v2_queue(&self) -> &MessageQueue {
        &self.queue
    }
}

#[tokio::test]
async fn enqueue_v2_message_wakes_registered_consumer_without_extra_handle() {
    let queue = MessageQueue::new();
    let inbox = SessionInbox::new(std::sync::Arc::new(queue.clone()));
    let state = TestState {
        queue: queue.clone(),
        messages: Vec::new(),
    };
    let waiting = inbox.await_wake();
    tokio::pin!(waiting);
    futures::future::poll_fn(|context| {
        use std::future::Future;
        assert!(waiting.as_mut().poll(context).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    let msg = QueuedMessage::new(
        MessageKind::Defer,
        MessageSource::GoalSteering,
        BaseMessage::human(MessageContent::text("steer")),
    );
    state.enqueue_v2_message(msg);
    assert_eq!(queue.len(), 1);
    assert!(queue.has_wake_up());
    tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
        .await
        .expect("middleware publication must wake the shared mailbox");
}

#[test]
fn enqueue_v2_message_publishes_without_a_consumer() {
    let queue = MessageQueue::new();
    let state = TestState {
        queue: queue.clone(),
        messages: Vec::new(),
    };
    let msg = QueuedMessage::new(
        MessageKind::Defer,
        MessageSource::GoalSteering,
        BaseMessage::human(MessageContent::text("steer")),
    );
    state.enqueue_v2_message(msg);
    assert_eq!(queue.len(), 1);
    assert!(queue.has_wake_up());
}
