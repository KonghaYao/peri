use crate::agent::token::TokenTracker;
use peri_acp_types::session::{MessageKind, MessageSource, QueuedMessage};

use crate::agent::session::{InboxHandle, SessionInbox};
use crate::messages::{BaseMessage, MessageContent};
use crate::middleware::state::MiddlewareState;
use crate::session::MessageQueue;

struct TestState {
    queue: MessageQueue,
    inbox: Option<InboxHandle>,
    token_tracker: TokenTracker,
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
    fn prepend_message(&mut self, _: BaseMessage) {}
    fn messages_mut(&mut self) -> &mut Vec<BaseMessage> {
        &mut self.messages
    }
    fn current_step(&self) -> usize {
        0
    }
    fn get_context(&self, _: &str) -> Option<&str> {
        None
    }
    fn set_context(&mut self, _: String, _: String) {}
    fn token_tracker(&self) -> &TokenTracker {
        &self.token_tracker
    }
    fn token_tracker_mut(&mut self) -> &mut TokenTracker {
        &mut self.token_tracker
    }
    fn push_recall(&mut self, _: String) {}
    fn drain_recall(&mut self) -> Vec<String> {
        vec![]
    }
    fn ancestor_len(&self) -> usize {
        0
    }
    fn store(&self) -> Option<&std::sync::Arc<dyn crate::thread::ThreadStore>> {
        None
    }
    #[allow(deprecated)]
    fn set_cwd(&mut self, _: String) {}
    #[allow(deprecated)]
    fn set_current_step(&mut self, _: usize) {}
    #[allow(deprecated)]
    fn own_thread_id(&self) -> Option<&crate::thread::ThreadId> {
        None
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
        token_tracker: TokenTracker::default(),
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
        token_tracker: TokenTracker::default(),
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
