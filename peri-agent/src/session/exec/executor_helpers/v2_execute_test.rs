//! forwarder 交付序号绑定的单元测试（无 ACP 装配面）。

use std::sync::Arc;

use parking_lot::Mutex;
use peri_acp_types::{
    event::{EventPublisher, ExecutorEvent},
    identity::{EventDeliveryClass, SessionSeq},
    messages::{MessageContent, MessageId},
    runtime::UnstampedEvent,
    session::{EnqueueUserInputRequest, MessageQueue, SessionInbox},
};
use tokio_util::sync::CancellationToken;

use crate::session::user_input_mailbox::UserInputMailbox;

use super::publish_forwarded_event;

struct RecordingPublisher {
    next: Mutex<Option<SessionSeq>>,
    published: Mutex<Vec<String>>,
}

impl RecordingPublisher {
    fn new() -> Self {
        Self {
            next: Mutex::new(Some(SessionSeq::initial())),
            published: Mutex::new(Vec::new()),
        }
    }

    fn unavailable() -> Self {
        Self {
            next: Mutex::new(None),
            published: Mutex::new(Vec::new()),
        }
    }

    fn take_seq(&self) -> Option<SessionSeq> {
        let mut next = self.next.lock();
        let seq = (*next)?;
        *next = Some(seq.next());
        Some(seq)
    }
}

impl EventPublisher for RecordingPublisher {
    fn publish_event(
        &self,
        session_id: &str,
        _source: &UnstampedEvent,
        _event: ExecutorEvent,
    ) -> Option<SessionSeq> {
        self.published.lock().push(session_id.to_string());
        self.take_seq()
    }
}

fn source() -> UnstampedEvent {
    UnstampedEvent::new("turn", "agent", None, EventDeliveryClass::Critical)
}

fn delivery_event(input_id: &str) -> ExecutorEvent {
    ExecutorEvent::UserInputDelivered {
        generation: "generation".into(),
        input_id: input_id.to_string(),
        content: MessageContent::text("问题"),
    }
}

/// 构造一条已写入交付日志的输入，返回 mailbox 与 input_id。
fn mailbox_with_delivery() -> (Arc<UserInputMailbox>, String) {
    let inbox = Arc::new(SessionInbox::new(Arc::new(MessageQueue::new())));
    let mailbox = UserInputMailbox::new("session".into(), inbox.clone(), Arc::new(|_| {}));
    mailbox
        .attach_external_attempt(CancellationToken::new(), false)
        .unwrap();
    let request = EnqueueUserInputRequest {
        session_id: "session".into(),
        generation: mailbox.generation().into(),
        command_id: "command".into(),
        input_id: uuid::Uuid::now_v7().to_string(),
        content: MessageContent::text("问题"),
        original_draft: "问题".into(),
    };
    mailbox.enqueue(&request).unwrap();
    mailbox.enter_idle();
    let messages = inbox.queue().drain_all();
    let id = messages[0].message().unwrap().id();
    mailbox.mark_claimed(&[id]);
    mailbox.mark_delivered(&[id]);
    (mailbox, request.input_id)
}

fn bound_sequence(mailbox: &UserInputMailbox, input_id: &str) -> Option<SessionSeq> {
    mailbox
        .pending_delivery_notifications()
        .into_iter()
        .find(|entry| entry.input_id == input_id)
        .and_then(|entry| entry.session_seq)
}

#[test]
fn forwarder_binds_the_published_sequence_to_the_delivery_journal() {
    let (mailbox, input_id) = mailbox_with_delivery();
    let publisher = RecordingPublisher::new();

    let published = publish_forwarded_event(
        &publisher,
        "session",
        &source(),
        delivery_event(&input_id),
        Some(mailbox.as_ref()),
    );

    let allocated = SessionSeq::initial();
    assert_eq!(published, Some(allocated));
    assert_eq!(*publisher.published.lock(), vec!["session".to_string()]);
    assert_eq!(
        bound_sequence(&mailbox, &input_id),
        Some(allocated),
        "交付日志必须记录实际发布序号，供泵在水位前补发"
    );
}

#[test]
fn forwarder_does_not_bind_when_publish_returns_no_sequence() {
    let (mailbox, input_id) = mailbox_with_delivery();
    let publisher = RecordingPublisher::unavailable();

    let published = publish_forwarded_event(
        &publisher,
        "session",
        &source(),
        delivery_event(&input_id),
        Some(mailbox.as_ref()),
    );

    assert_eq!(published, None);
    assert_eq!(bound_sequence(&mailbox, &input_id), None);
}

#[test]
fn forwarder_does_not_touch_mailbox_for_non_delivery_events() {
    let (mailbox, _input_id) = mailbox_with_delivery();
    let publisher = RecordingPublisher::new();

    publish_forwarded_event(
        &publisher,
        "session",
        &source(),
        ExecutorEvent::TextChunk {
            message_id: MessageId::new(),
            chunk: "回答".into(),
            source_agent_id: None,
        },
        Some(mailbox.as_ref()),
    );

    assert_eq!(*publisher.published.lock(), vec!["session".to_string()]);
    assert!(
        mailbox
            .pending_delivery_notifications()
            .iter()
            .all(|entry| entry.session_seq.is_none()),
        "非交付事件不得改动交付日志"
    );
}
