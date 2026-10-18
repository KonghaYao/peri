//! 事件泵的交付日志回归：Delivered 必须先于后续 assistant 输出，且按实际交付顺序。

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use peri_acp_types::{
    command::PromptStopReason,
    event::{
        EventMessage, EventSink, EventSinkError, EventSubscriber, ExecutorEvent, SubscriptionError,
    },
    identity::{EventDeliveryClass, EventEnvelope, SessionEpoch, SessionSeq},
    messages::{BaseMessage, MessageContent, MessageId},
    session::{EnqueueUserInputRequest, MessageQueue, SessionInbox, TurnTelemetryOutcome},
};
use tokio::sync::{mpsc, oneshot, Notify};
use tokio_util::sync::CancellationToken;

use super::{spawn_event_pump, SpawnPumpRequest};
use crate::session::user_input_mailbox::UserInputMailbox;

struct PendingSubscriber;

#[async_trait]
impl EventSubscriber for PendingSubscriber {
    async fn recv(&mut self) -> Result<EventMessage, SubscriptionError> {
        std::future::pending().await
    }

    fn try_recv(&mut self) -> Result<Option<EventMessage>, SubscriptionError> {
        Ok(None)
    }
}

#[derive(Default)]
struct RecordingSink {
    records: parking_lot::Mutex<Vec<String>>,
}

impl RecordingSink {
    fn records(&self) -> Vec<String> {
        self.records.lock().clone()
    }
}

#[async_trait]
impl EventSink for RecordingSink {
    async fn push_event(
        &self,
        _session_id: &str,
        event: &ExecutorEvent,
        _context_window: u32,
    ) -> Result<(), EventSinkError> {
        let record = match event {
            ExecutorEvent::UserInputDelivered { input_id, .. } => {
                format!("delivered:{input_id}")
            }
            ExecutorEvent::TextChunk { chunk, .. } => format!("text:{chunk}"),
            other => format!("other:{other:?}"),
        };
        self.records.lock().push(record);
        Ok(())
    }

    async fn push_unstable_event(&self, _session_id: &str, event: String, data: serde_json::Value) {
        self.records.lock().push(format!(
            "unstable:{event}:{}",
            data["text"].as_str().unwrap_or_default()
        ));
    }

    async fn push_done(&self, _session_id: &str, _stop_reason: &str, _request_id: Option<&str>) {
        self.records.lock().push("done".into());
    }
}

/// 可控订阅者：测试决定事件进入泵的时机，用于复现“事件已发布但仍在缓冲中”。
struct ScriptedSubscriber {
    rx: mpsc::UnboundedReceiver<EventMessage>,
}

#[async_trait]
impl EventSubscriber for ScriptedSubscriber {
    async fn recv(&mut self) -> Result<EventMessage, SubscriptionError> {
        self.rx.recv().await.ok_or(SubscriptionError::Closed)
    }

    fn try_recv(&mut self) -> Result<Option<EventMessage>, SubscriptionError> {
        match self.rx.try_recv() {
            Ok(message) => Ok(Some(message)),
            Err(mpsc::error::TryRecvError::Empty) => Ok(None),
            Err(mpsc::error::TryRecvError::Disconnected) => Err(SubscriptionError::Closed),
        }
    }
}

fn sequenced_message(session_seq: SessionSeq, event: ExecutorEvent) -> EventMessage {
    EventMessage::new(
        EventEnvelope::new(
            "s",
            SessionEpoch::initial(),
            "turn",
            "agent",
            session_seq,
            EventDeliveryClass::Critical,
        ),
        Some(event),
    )
}

fn input(mailbox: &UserInputMailbox, input_id: &str, text: &str) -> EnqueueUserInputRequest {
    EnqueueUserInputRequest {
        session_id: "s".into(),
        generation: mailbox.generation().into(),
        command_id: uuid::Uuid::now_v7().to_string(),
        input_id: input_id.into(),
        content: MessageContent::text(text),
        original_draft: text.into(),
    }
}

/// 两条输入依次在 idle 交付；返回 mailbox（日志中 B、A 顺序）。
fn mailbox_with_two_deliveries() -> Arc<UserInputMailbox> {
    let inbox = Arc::new(SessionInbox::new(Arc::new(MessageQueue::new())));
    let mailbox = UserInputMailbox::new("s".into(), inbox.clone(), Arc::new(|_| {}));
    mailbox
        .attach_external_attempt(CancellationToken::new(), false)
        .unwrap();
    let b = input(&mailbox, "00000000-0000-0000-0000-00000000000b", "B");
    let a = input(&mailbox, "00000000-0000-0000-0000-00000000000a", "A");
    mailbox.enqueue(&b).unwrap();
    mailbox.enqueue(&a).unwrap();
    for _ in [&b, &a] {
        mailbox.enter_idle();
        let messages = inbox.queue().drain_all();
        assert_eq!(messages.len(), 1);
        let id = messages[0].message().unwrap().id();
        mailbox.mark_claimed(&[id]);
        assert_eq!(mailbox.mark_delivered(&[id]).len(), 1);
    }
    // 前两条日志按 Receive 顺序存在；这里不再产生新交付。
    assert_eq!(
        mailbox
            .pending_delivery_notifications()
            .iter()
            .map(|item| item.input_id.clone())
            .collect::<Vec<_>>(),
        vec![b.input_id.clone(), a.input_id.clone()]
    );
    mailbox
}

type PumpHarness = (
    mpsc::UnboundedSender<ExecutorEvent>,
    oneshot::Sender<(PromptStopReason, TurnTelemetryOutcome)>,
    Arc<RecordingSink>,
    super::PumpHandle,
);

fn start_pump(mailbox: Arc<UserInputMailbox>) -> PumpHarness {
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let (stop_tx, stop_rx) = oneshot::channel();
    let sink = Arc::new(RecordingSink::default());
    let handle = spawn_event_pump(SpawnPumpRequest {
        subscription: Box::new(PendingSubscriber),
        event_rx,
        stop_reason_rx: stop_rx,
        sink: sink.clone(),
        session_id: "s".into(),
        effective_context_window: 0,
        langfuse_on_turn_start: None,
        langfuse_on_turn_end: None,
        request_id: None,
        delivery_journal: Some(mailbox),
    });
    (event_tx, stop_tx, sink, handle)
}

async fn finish(
    event_tx: mpsc::UnboundedSender<ExecutorEvent>,
    stop_tx: oneshot::Sender<(PromptStopReason, TurnTelemetryOutcome)>,
    handle: super::PumpHandle,
) {
    drop(event_tx);
    stop_tx
        .send((PromptStopReason::EndTurn, TurnTelemetryOutcome::Completed))
        .unwrap();
    peri_time::timeout(Duration::from_secs(2), handle.pump_done_rx)
        .await
        .expect("pump must finish")
        .expect("pump done signal");
}

#[tokio::test]
async fn delivery_journal_is_emitted_before_assistant_output_in_delivery_order() {
    let mailbox = mailbox_with_two_deliveries();
    let first = SessionSeq::initial();
    let second = first.next();
    mailbox.bind_delivery_sequence("00000000-0000-0000-0000-00000000000b", first);
    mailbox.bind_delivery_sequence("00000000-0000-0000-0000-00000000000a", second);
    let (message_tx, message_rx) = mpsc::unbounded_channel();
    let sink = Arc::new(RecordingSink::default());
    let (event_tx, stop_tx, handle) = start_pump_with_subscription(
        mailbox.clone(),
        Box::new(ScriptedSubscriber { rx: message_rx }),
        sink.clone(),
    );
    message_tx
        .send(sequenced_message(
            second.next(),
            ExecutorEvent::TextChunk {
                message_id: MessageId::new(),
                chunk: "回答".into(),
                source_agent_id: None,
            },
        ))
        .unwrap();
    drop(message_tx);
    finish(event_tx, stop_tx, handle).await;
    assert_eq!(
        sink.records(),
        vec![
            "delivered:00000000-0000-0000-0000-00000000000b",
            "delivered:00000000-0000-0000-0000-00000000000a",
            "text:回答",
            "done",
        ]
    );
    assert!(mailbox.pending_delivery_notifications().is_empty());
}

#[tokio::test]
async fn live_delivery_arriving_after_journal_is_not_emitted_twice() {
    let mailbox = mailbox_with_two_deliveries();
    let (event_tx, stop_tx, sink, handle) = start_pump(mailbox);
    event_tx
        .send(ExecutorEvent::UserInputDelivered {
            generation: "g".into(),
            input_id: "00000000-0000-0000-0000-00000000000a".into(),
            content: MessageContent::text("A"),
        })
        .unwrap();
    finish(event_tx, stop_tx, handle).await;
    assert_eq!(
        sink.records(),
        vec![
            "delivered:00000000-0000-0000-0000-00000000000b",
            "delivered:00000000-0000-0000-0000-00000000000a",
            "done",
        ]
    );
}

#[tokio::test]
async fn trailing_delivery_is_flushed_before_done_without_later_events() {
    let mailbox = mailbox_with_two_deliveries();
    let (event_tx, stop_tx, sink, handle) = start_pump(mailbox);
    finish(event_tx, stop_tx, handle).await;
    assert_eq!(
        sink.records(),
        vec![
            "delivered:00000000-0000-0000-0000-00000000000b",
            "delivered:00000000-0000-0000-0000-00000000000a",
            "done",
        ]
    );
}

fn start_pump_with_sink(
    mailbox: Arc<UserInputMailbox>,
    sink: Arc<dyn EventSink>,
) -> (
    mpsc::UnboundedSender<ExecutorEvent>,
    oneshot::Sender<(PromptStopReason, TurnTelemetryOutcome)>,
    super::PumpHandle,
) {
    start_pump_with_subscription(mailbox, Box::new(PendingSubscriber), sink)
}

fn start_pump_with_subscription(
    mailbox: Arc<UserInputMailbox>,
    subscription: Box<dyn EventSubscriber>,
    sink: Arc<dyn EventSink>,
) -> (
    mpsc::UnboundedSender<ExecutorEvent>,
    oneshot::Sender<(PromptStopReason, TurnTelemetryOutcome)>,
    super::PumpHandle,
) {
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let (stop_tx, stop_rx) = oneshot::channel();
    let handle = spawn_event_pump(SpawnPumpRequest {
        subscription,
        event_rx,
        stop_reason_rx: stop_rx,
        sink,
        session_id: "s".into(),
        effective_context_window: 0,
        langfuse_on_turn_start: None,
        langfuse_on_turn_end: None,
        request_id: None,
        delivery_journal: Some(mailbox),
    });
    (event_tx, stop_tx, handle)
}

#[tokio::test]
async fn delivery_recorded_after_older_event_is_not_emitted_before_it() {
    let (mailbox, delivery_id) = mailbox_with_claimed_input();
    mailbox.mark_delivered(&[delivery_id]);
    let delivery_seq = SessionSeq::initial().next();
    mailbox.bind_delivery_sequence("00000000-0000-0000-0000-00000000000c", delivery_seq);
    let (message_tx, message_rx) = mpsc::unbounded_channel();
    let sink = Arc::new(RecordingSink::default());
    let (event_tx, stop_tx, handle) = start_pump_with_subscription(
        mailbox.clone(),
        Box::new(ScriptedSubscriber { rx: message_rx }),
        sink.clone(),
    );

    // 旧 assistant 事件已发布（序号更小）但仍在订阅缓冲中；交付事件序号更大。
    message_tx
        .send(sequenced_message(
            SessionSeq::initial(),
            ExecutorEvent::TextChunk {
                message_id: MessageId::new(),
                chunk: "旧回答".into(),
                source_agent_id: None,
            },
        ))
        .unwrap();
    message_tx
        .send(sequenced_message(
            delivery_seq,
            ExecutorEvent::UserInputDelivered {
                generation: "g".into(),
                input_id: "00000000-0000-0000-0000-00000000000c".into(),
                content: MessageContent::text("并发"),
            },
        ))
        .unwrap();
    drop(message_tx);
    finish(event_tx, stop_tx, handle).await;

    assert_eq!(
        sink.records(),
        vec![
            "text:旧回答".to_string(),
            "delivered:00000000-0000-0000-0000-00000000000c".to_string(),
            "done".to_string(),
        ],
        "序号更大的交付不得越过已发布的旧回答事件"
    );
    assert!(mailbox.pending_delivery_notifications().is_empty());
}

#[tokio::test]
async fn bound_delivery_missed_by_lagged_subscription_is_replayed_before_next_event() {
    let (mailbox, delivery_id) = mailbox_with_claimed_input();
    mailbox.mark_delivered(&[delivery_id]);
    let delivery_seq = SessionSeq::initial().next();
    mailbox.bind_delivery_sequence("00000000-0000-0000-0000-00000000000c", delivery_seq);
    let (message_tx, message_rx) = mpsc::unbounded_channel();
    let sink = Arc::new(RecordingSink::default());
    let (event_tx, stop_tx, handle) = start_pump_with_subscription(
        mailbox.clone(),
        Box::new(ScriptedSubscriber { rx: message_rx }),
        sink.clone(),
    );

    // 交付事件在广播中丢失（Lagged）：下一条事件序号更大时按水位补发。
    message_tx
        .send(sequenced_message(
            delivery_seq.next(),
            ExecutorEvent::TextChunk {
                message_id: MessageId::new(),
                chunk: "之后".into(),
                source_agent_id: None,
            },
        ))
        .unwrap();
    drop(message_tx);
    finish(event_tx, stop_tx, handle).await;

    assert_eq!(
        sink.records(),
        vec![
            "delivered:00000000-0000-0000-0000-00000000000c".to_string(),
            "text:之后".to_string(),
            "done".to_string(),
        ],
        "丢失的交付确认必须在下一条事件前补发且只发一次"
    );
    assert!(mailbox.pending_delivery_notifications().is_empty());
}

#[tokio::test]
async fn unbound_delivery_is_flushed_at_tail_not_before_older_event() {
    let (mailbox, delivery_id) = mailbox_with_claimed_input();
    mailbox.mark_delivered(&[delivery_id]);
    let (message_tx, message_rx) = mpsc::unbounded_channel();
    let sink = Arc::new(RecordingSink::default());
    let (event_tx, stop_tx, handle) = start_pump_with_subscription(
        mailbox.clone(),
        Box::new(ScriptedSubscriber { rx: message_rx }),
        sink.clone(),
    );

    message_tx
        .send(sequenced_message(
            SessionSeq::initial(),
            ExecutorEvent::TextChunk {
                message_id: MessageId::new(),
                chunk: "旧回答".into(),
                source_agent_id: None,
            },
        ))
        .unwrap();
    drop(message_tx);
    finish(event_tx, stop_tx, handle).await;

    assert_eq!(
        sink.records(),
        vec![
            "text:旧回答".to_string(),
            "delivered:00000000-0000-0000-0000-00000000000c".to_string(),
            "done".to_string(),
        ],
        "未绑定序号的交付只能尾部补发"
    );
}

#[tokio::test]
async fn duplicate_delivery_event_and_journal_entry_emit_once() {
    let (mailbox, delivery_id) = mailbox_with_claimed_input();
    mailbox.mark_delivered(&[delivery_id]);
    let delivery_seq = SessionSeq::initial();
    mailbox.bind_delivery_sequence("00000000-0000-0000-0000-00000000000c", delivery_seq);
    let (message_tx, message_rx) = mpsc::unbounded_channel();
    let sink = Arc::new(RecordingSink::default());
    let (event_tx, stop_tx, handle) = start_pump_with_subscription(
        mailbox.clone(),
        Box::new(ScriptedSubscriber { rx: message_rx }),
        sink.clone(),
    );

    message_tx
        .send(sequenced_message(
            delivery_seq,
            ExecutorEvent::UserInputDelivered {
                generation: "g".into(),
                input_id: "00000000-0000-0000-0000-00000000000c".into(),
                content: MessageContent::text("并发"),
            },
        ))
        .unwrap();
    message_tx
        .send(sequenced_message(
            delivery_seq.next(),
            ExecutorEvent::TextChunk {
                message_id: MessageId::new(),
                chunk: "回答".into(),
                source_agent_id: None,
            },
        ))
        .unwrap();
    drop(message_tx);
    finish(event_tx, stop_tx, handle).await;

    assert_eq!(
        sink.records(),
        vec![
            "delivered:00000000-0000-0000-0000-00000000000c".to_string(),
            "text:回答".to_string(),
            "done".to_string(),
        ],
        "同一交付的事件与 journal 只能推送一次"
    );
    assert!(mailbox.pending_delivery_notifications().is_empty());
}

#[tokio::test]
async fn background_tail_event_precedes_later_delivery_bubble() {
    let (mailbox, delivery_id) = mailbox_with_claimed_input();
    mailbox.mark_delivered(&[delivery_id]);
    let delivery_seq = SessionSeq::initial().next();
    mailbox.bind_delivery_sequence("00000000-0000-0000-0000-00000000000c", delivery_seq);
    let (message_tx, message_rx) = mpsc::unbounded_channel();
    let sink = Arc::new(RecordingSink::default());
    let (event_tx, stop_tx, handle) = start_pump_with_subscription(
        mailbox.clone(),
        Box::new(ScriptedSubscriber { rx: message_rx }),
        sink.clone(),
    );

    message_tx
        .send(sequenced_message(
            SessionSeq::initial(),
            ExecutorEvent::MessageAdded(BaseMessage::human("后台回调")),
        ))
        .unwrap();
    message_tx
        .send(sequenced_message(
            delivery_seq,
            ExecutorEvent::UserInputDelivered {
                generation: "g".into(),
                input_id: "00000000-0000-0000-0000-00000000000c".into(),
                content: MessageContent::text("并发"),
            },
        ))
        .unwrap();
    drop(message_tx);
    finish(event_tx, stop_tx, handle).await;

    let records = sink.records();
    let unstable = records
        .iter()
        .position(|record| record.starts_with("unstable:"))
        .expect("后台回调事件必须写出");
    let delivered = records
        .iter()
        .position(|record| record.starts_with("delivered:"))
        .expect("交付确认必须写出");
    assert!(
        unstable < delivered,
        "后台任务尾事件必须先于序号更大的交付气泡：{records:?}"
    );
}

fn mailbox_with_claimed_input() -> (Arc<UserInputMailbox>, MessageId) {
    let inbox = Arc::new(SessionInbox::new(Arc::new(MessageQueue::new())));
    let mailbox = UserInputMailbox::new("s".into(), inbox.clone(), Arc::new(|_| {}));
    mailbox
        .attach_external_attempt(CancellationToken::new(), false)
        .unwrap();
    let request = input(&mailbox, "00000000-0000-0000-0000-00000000000c", "并发");
    mailbox.enqueue(&request).unwrap();
    mailbox.enter_idle();
    let messages = inbox.queue().drain_all();
    let id = messages[0].message().unwrap().id();
    mailbox.mark_claimed(&[id]);
    (mailbox, id)
}

struct BlockingSink {
    records: parking_lot::Mutex<Vec<String>>,
    entered: Notify,
    release: Notify,
}

impl BlockingSink {
    fn new() -> Self {
        Self {
            records: parking_lot::Mutex::new(Vec::new()),
            entered: Notify::new(),
            release: Notify::new(),
        }
    }

    fn records(&self) -> Vec<String> {
        self.records.lock().clone()
    }
}

#[async_trait]
impl EventSink for BlockingSink {
    async fn push_event(
        &self,
        _session_id: &str,
        event: &ExecutorEvent,
        _context_window: u32,
    ) -> Result<(), EventSinkError> {
        if let ExecutorEvent::TextChunk { chunk, .. } = event {
            if chunk == "answer1" {
                self.records.lock().push(format!("text:{chunk}"));
                self.entered.notify_one();
                self.release.notified().await;
                return Ok(());
            }
            self.records.lock().push(format!("text:{chunk}"));
            return Ok(());
        }
        if let ExecutorEvent::UserInputDelivered { input_id, .. } = event {
            self.records.lock().push(format!("delivered:{input_id}"));
        }
        Ok(())
    }

    async fn push_done(&self, _session_id: &str, _stop_reason: &str, _request_id: Option<&str>) {
        self.records.lock().push("done".into());
    }
}

/// 交付发生在 assistant 已送入 sink、等待写出的窗口内：它必须进入同一日志，
/// 并在下一次 assistant 事件之前发出；不能丢失，也不能越过已提交事件。
#[tokio::test]
async fn concurrent_delivery_during_assistant_send_is_ordered_after_it_and_not_lost() {
    let (mailbox, delivery_id) = mailbox_with_claimed_input();
    let sink = Arc::new(BlockingSink::new());
    let (event_tx, stop_tx, handle) = start_pump_with_sink(mailbox.clone(), sink.clone());

    event_tx
        .send(ExecutorEvent::TextChunk {
            message_id: MessageId::new(),
            chunk: "answer1".into(),
            source_agent_id: None,
        })
        .unwrap();
    // 事件已过第二次日志快照并进入 sink；此时 mark_delivered 是 mid-turn 并发交付。
    peri_time::timeout(Duration::from_secs(2), sink.entered.notified())
        .await
        .expect("assistant push must start");
    mailbox.mark_delivered(&[delivery_id]);
    sink.release.notify_one();

    event_tx
        .send(ExecutorEvent::TextChunk {
            message_id: MessageId::new(),
            chunk: "answer2".into(),
            source_agent_id: None,
        })
        .unwrap();
    finish(event_tx, stop_tx, handle).await;

    assert_eq!(
        sink.records(),
        vec![
            "text:answer1".to_string(),
            "delivered:00000000-0000-0000-0000-00000000000c".to_string(),
            "text:answer2".to_string(),
            "done".to_string(),
        ]
    );
    assert!(
        mailbox.pending_delivery_notifications().is_empty(),
        "成功推送后必须确认日志"
    );
}

struct FlakySink {
    records: parking_lot::Mutex<Vec<String>>,
    fail_next_delivery: AtomicBool,
    failed: Notify,
}

impl FlakySink {
    fn new() -> Self {
        Self {
            records: parking_lot::Mutex::new(Vec::new()),
            fail_next_delivery: AtomicBool::new(true),
            failed: Notify::new(),
        }
    }

    fn records(&self) -> Vec<String> {
        self.records.lock().clone()
    }
}

#[async_trait]
impl EventSink for FlakySink {
    async fn push_event(
        &self,
        _session_id: &str,
        event: &ExecutorEvent,
        _context_window: u32,
    ) -> Result<(), EventSinkError> {
        if let ExecutorEvent::UserInputDelivered { input_id, .. } = event {
            if self.fail_next_delivery.swap(false, Ordering::SeqCst) {
                self.failed.notify_one();
                return Err(EventSinkError::new("injected delivery send failure"));
            }
            self.records.lock().push(format!("delivered:{input_id}"));
            return Ok(());
        }
        if let ExecutorEvent::TextChunk { chunk, .. } = event {
            self.records.lock().push(format!("text:{chunk}"));
        }
        Ok(())
    }

    async fn push_done(&self, _session_id: &str, _stop_reason: &str, _request_id: Option<&str>) {
        self.records.lock().push("done".into());
    }
}

/// sink 拒绝写出时不得 ack；日志保留并在下一次事件重试成功后才确认。
#[tokio::test]
async fn failed_delivery_send_keeps_journal_until_retry_succeeds() {
    let (mailbox, delivery_id) = mailbox_with_claimed_input();
    mailbox.mark_delivered(&[delivery_id]);
    assert_eq!(mailbox.pending_delivery_notifications().len(), 1);
    let sink = Arc::new(FlakySink::new());
    let (event_tx, stop_tx, handle) = start_pump_with_sink(mailbox.clone(), sink.clone());

    event_tx
        .send(ExecutorEvent::TextChunk {
            message_id: MessageId::new(),
            chunk: "dropped".into(),
            source_agent_id: None,
        })
        .unwrap();
    peri_time::timeout(Duration::from_secs(2), sink.failed.notified())
        .await
        .expect("sink must report failure");
    assert_eq!(
        mailbox
            .pending_delivery_notifications()
            .iter()
            .map(|item| item.input_id.clone())
            .collect::<Vec<_>>(),
        vec![delivery_id.as_uuid().to_string()],
        "失败后日志必须保留"
    );

    event_tx
        .send(ExecutorEvent::TextChunk {
            message_id: MessageId::new(),
            chunk: "retry".into(),
            source_agent_id: None,
        })
        .unwrap();
    finish(event_tx, stop_tx, handle).await;

    assert_eq!(
        sink.records(),
        vec![
            "delivered:00000000-0000-0000-0000-00000000000c".to_string(),
            "text:dropped".to_string(),
            "text:retry".to_string(),
            "done".to_string(),
        ],
        "交付确认成功后才发送被重试的 assistant 事件；文本不得丢失"
    );
    assert!(mailbox.pending_delivery_notifications().is_empty());
}

struct FlakyTextSink {
    records: parking_lot::Mutex<Vec<String>>,
    fail_next_text: AtomicBool,
}

impl FlakyTextSink {
    fn new() -> Self {
        Self {
            records: parking_lot::Mutex::new(Vec::new()),
            fail_next_text: AtomicBool::new(true),
        }
    }

    fn records(&self) -> Vec<String> {
        self.records.lock().clone()
    }
}

#[async_trait]
impl EventSink for FlakyTextSink {
    async fn push_event(
        &self,
        _session_id: &str,
        event: &ExecutorEvent,
        _context_window: u32,
    ) -> Result<(), EventSinkError> {
        if let ExecutorEvent::TextChunk { chunk, .. } = event {
            if self.fail_next_text.swap(false, Ordering::SeqCst) {
                return Err(EventSinkError::new("injected text send failure"));
            }
            self.records.lock().push(format!("text:{chunk}"));
        } else if let ExecutorEvent::UserInputDelivered { input_id, .. } = event {
            self.records.lock().push(format!("delivered:{input_id}"));
        } else if matches!(event, ExecutorEvent::AgentExecutionFailed { .. }) {
            self.records.lock().push("execution-failed".into());
        }
        Ok(())
    }

    async fn push_done(&self, _session_id: &str, _stop_reason: &str, _request_id: Option<&str>) {
        self.records.lock().push("done".into());
    }
}

/// assistant 文本本身发送失败时重试同一事件，文本完整且只出现一次。
#[tokio::test]
async fn assistant_text_send_failure_is_retried_without_loss_or_duplicate() {
    let inbox = Arc::new(SessionInbox::new(Arc::new(MessageQueue::new())));
    let mailbox = UserInputMailbox::new("s".into(), inbox, Arc::new(|_| {}));
    let sink = Arc::new(FlakyTextSink::new());
    let (event_tx, stop_tx, handle) = start_pump_with_sink(mailbox, sink.clone());

    event_tx
        .send(ExecutorEvent::TextChunk {
            message_id: MessageId::new(),
            chunk: "answer".into(),
            source_agent_id: None,
        })
        .unwrap();
    finish(event_tx, stop_tx, handle).await;

    assert_eq!(
        sink.records(),
        vec!["text:answer".to_string(), "done".to_string()],
        "重试成功后文本恰好出现一次"
    );
}

struct TailFailSink {
    records: parking_lot::Mutex<Vec<String>>,
}

impl TailFailSink {
    fn new() -> Self {
        Self {
            records: parking_lot::Mutex::new(Vec::new()),
        }
    }

    fn records(&self) -> Vec<String> {
        self.records.lock().clone()
    }
}

#[async_trait]
impl EventSink for TailFailSink {
    async fn push_event(
        &self,
        _session_id: &str,
        event: &ExecutorEvent,
        _context_window: u32,
    ) -> Result<(), EventSinkError> {
        if let ExecutorEvent::UserInputDelivered { .. } = event {
            return Err(EventSinkError::new("injected trailing delivery failure"));
        }
        if matches!(event, ExecutorEvent::AgentExecutionFailed { .. }) {
            self.records.lock().push("execution-failed".into());
        }
        Ok(())
    }

    async fn push_done(&self, _session_id: &str, _stop_reason: &str, _request_id: Option<&str>) {
        self.records.lock().push("done".into());
    }
}

/// 尾部交付重试耗尽后必须显式失败，不能以正常 done 收尾。
#[tokio::test]
async fn trailing_delivery_failure_emits_error_and_skips_done() {
    let (mailbox, delivery_id) = mailbox_with_claimed_input();
    mailbox.mark_delivered(&[delivery_id]);
    let sink = Arc::new(TailFailSink::new());
    let (event_tx, stop_tx, handle) = start_pump_with_sink(mailbox.clone(), sink.clone());

    finish(event_tx, stop_tx, handle).await;

    assert_eq!(
        sink.records(),
        vec!["execution-failed".to_string()],
        "尾部失败必须产生可观察错误且不得发送正常 done"
    );
    assert_eq!(
        mailbox.pending_delivery_notifications().len(),
        1,
        "未确认的交付事实必须保留"
    );
}

struct DeafSink {
    delivery_attempts: AtomicUsize,
    failure_attempts: AtomicUsize,
    done: AtomicBool,
}

impl DeafSink {
    fn new() -> Self {
        Self {
            delivery_attempts: AtomicUsize::new(0),
            failure_attempts: AtomicUsize::new(0),
            done: AtomicBool::new(false),
        }
    }
}

#[async_trait]
impl EventSink for DeafSink {
    async fn push_event(
        &self,
        _session_id: &str,
        event: &ExecutorEvent,
        _context_window: u32,
    ) -> Result<(), EventSinkError> {
        match event {
            ExecutorEvent::UserInputDelivered { .. } => {
                self.delivery_attempts.fetch_add(1, Ordering::SeqCst);
                Err(EventSinkError::new("transport is dead"))
            }
            ExecutorEvent::AgentExecutionFailed { .. } => {
                self.failure_attempts.fetch_add(1, Ordering::SeqCst);
                Err(EventSinkError::new("transport is dead"))
            }
            _ => Ok(()),
        }
    }

    async fn push_done(&self, _session_id: &str, _stop_reason: &str, _request_id: Option<&str>) {
        self.done.store(true, Ordering::SeqCst);
    }
}

/// transport 完全不可用时，失败通知本身也走有界重试；耗尽后泵仍完成，
/// 客户端由通知通道关闭路径收敛，不会永久挂起。
#[tokio::test]
async fn unreachable_failure_event_does_not_hang_pump_and_is_bounded() {
    let (mailbox, delivery_id) = mailbox_with_claimed_input();
    mailbox.mark_delivered(&[delivery_id]);
    let sink = Arc::new(DeafSink::new());
    let (event_tx, stop_tx, handle) = start_pump_with_sink(mailbox, sink.clone());

    finish(event_tx, stop_tx, handle).await;

    assert_eq!(
        sink.delivery_attempts.load(Ordering::SeqCst),
        3,
        "Delivered 重试必须有界"
    );
    assert_eq!(
        sink.failure_attempts.load(Ordering::SeqCst),
        3,
        "AgentExecutionFailed 本身也必须复用同一有界重试"
    );
    assert!(
        !sink.done.load(Ordering::SeqCst),
        "失败通知不可达时不得作为正常 done 完成"
    );
}
