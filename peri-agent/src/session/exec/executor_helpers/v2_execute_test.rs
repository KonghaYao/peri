//! forwarder 交付序号复用的单元测试（无 ACP 装配面）。

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
    next: Mutex<SessionSeq>,
    published: Mutex<Vec<String>>,
    reserved: Mutex<Vec<SessionSeq>>,
}

impl RecordingPublisher {
    fn new() -> Self {
        Self {
            next: Mutex::new(SessionSeq::initial()),
            published: Mutex::new(Vec::new()),
            reserved: Mutex::new(Vec::new()),
        }
    }

    fn next_seq(&self) -> SessionSeq {
        let mut next = self.next.lock();
        let seq = *next;
        *next = next.next();
        seq
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
        Some(self.next_seq())
    }

    fn publish_event_with_sequence(
        &self,
        _session_id: &str,
        _source: &UnstampedEvent,
        _event: ExecutorEvent,
        session_seq: SessionSeq,
    ) -> Option<SessionSeq> {
        self.reserved.lock().push(session_seq);
        Some(session_seq)
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

#[test]
fn forwarder_reuses_sequence_reserved_before_the_lossy_channel() {
    let (mailbox, input_id) = mailbox_with_delivery();
    let reserved = SessionSeq::initial();
    mailbox.bind_delivery_sequence(&input_id, reserved);
    let publisher = RecordingPublisher::new();

    let published = publish_forwarded_event(
        &publisher,
        "session",
        &source(),
        delivery_event(&input_id),
        Some(mailbox.as_ref()),
    );

    assert_eq!(published, Some(reserved));
    assert_eq!(
        *publisher.reserved.lock(),
        vec![reserved],
        "交付事件必须复用进入有损通道前预留的序号"
    );
    assert!(
        publisher.published.lock().is_empty(),
        "不得为已预留的交付再分配新序号"
    );
    assert_eq!(
        mailbox.delivery_sequence(&input_id),
        Some(reserved),
        "重复绑定保留首个序号"
    );
}

#[test]
fn forwarder_falls_back_when_delivery_was_not_reserved() {
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
        mailbox.delivery_sequence(&input_id),
        Some(allocated),
        "无预留能力时保持发布时补打的旧语义"
    );
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
    assert!(publisher.reserved.lock().is_empty());
}
