use peri_acp_types::session::{MessageKind, MessageSource, QueuedMessage};

use crate::agent::session::{InboxHandle, SessionInbox};
use crate::messages::{BaseMessage, MessageContent};
use crate::middleware::state::MiddlewareState;
use crate::session::MessageQueue;

struct TestState {
    queue: MessageQueue,
    inbox: Option<InboxHandle>,
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
    fn inbox_handle(&self) -> Option<&InboxHandle> {
        self.inbox.as_ref()
    }
}

#[test]
fn enqueue_v2_message_uses_inbox_when_present() {
    let queue = MessageQueue::new();
    let inbox = SessionInbox::new(std::sync::Arc::new(queue.clone()));
    let handle = inbox.handle();
    let state = TestState {
        queue: queue.clone(),
        inbox: Some(handle.clone()),
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

#[test]
fn enqueue_v2_message_falls_back_to_raw_queue() {
    let queue = MessageQueue::new();
    let state = TestState {
        queue: queue.clone(),
        inbox: None,
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
