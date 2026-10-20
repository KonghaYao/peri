//! 从 receive.rs 分离的测试模块
use super::*;
use crate::agent::events_v2::EventHandles;
use crate::agent::stages::StageContext;
use crate::messages::{BaseMessage, MessageContent};
use crate::session::queue::MessageSource;
use crate::session::store::FrozenContext;
use crate::session::{QueuedMessage, Session};
use std::sync::Arc;

#[test]
fn canonical_reminder_has_no_synthetic_event_but_plain_defer_keeps_legacy_event() {
    use peri_acp_types::system_reminder::{
        ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
        ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
    };
    let reminder = TrustedSystemReminderFactory::for_producer()
        .construct(SystemReminder {
            version: SYSTEM_REMINDER_VERSION,
            category: ReminderCategory::Task,
            source: ReminderSource("receive_test".into()),
            kind: "done".into(),
            severity: ReminderSeverity::Info,
            delivery: ReminderDelivery::Configurable,
            audiences: ReminderAudiences(vec![ReminderAudience::Model]),
            body: "canonical".into(),
            summary: None,
            metadata: serde_json::json!({}),
        })
        .unwrap();
    let canonical =
        QueuedMessage::system_reminder(MessageKind::Defer, MessageSource::SystemInjected, reminder);
    let plain = QueuedMessage::defer(
        MessageSource::SubAgentComplete,
        BaseMessage::human("legacy"),
    );

    assert_eq!(synthetic_defer_text(&canonical), None);
    assert_eq!(synthetic_defer_text(&plain).as_deref(), Some("legacy"));
}

fn make_context() -> StageContext {
    let cwd: Arc<str> = Arc::from("/tmp/test");
    let frozen = FrozenContext::builder().build();
    let session = Session::new(cwd, frozen, None);
    let turn = session.start_turn();
    StageContext::new(turn, session.transcript(), session.queue().clone())
}

/// 排空本轮发射的 `SystemReminder` 事件并计数（交付去重的可观察出口）。
fn drain_system_reminder_events(handles: &mut EventHandles) -> usize {
    let mut count = 0;
    while let Some(event) = handles.try_state() {
        if let StateEvent::ProtocolEvent { event, .. } = event {
            if matches!(event.as_ref(), ExecutorEvent::SystemReminder { .. }) {
                count += 1;
            }
        }
    }
    count
}

#[tokio::test]
async fn exhausted_receive_preserves_only_fresh_ensure_processing_inputs() {
    use peri_acp_types::session::MessagePolicy;
    let context = make_context();
    let execution = context.session.turn.execution_binding();
    let initial = BaseMessage::human("initial input");
    let initial_id = initial.id();
    context
        .session
        .queue
        .push(QueuedMessage::prompt(MessageSource::UserInput, initial));
    let watermark = context.session.queue.admission_watermark();
    let passive = BaseMessage::human("passive input");
    let passive_id = passive.id();
    context
        .session
        .queue
        .push(QueuedMessage::info(MessageSource::SystemInjected, passive));
    let continuation = BaseMessage::human("current run only");
    let continuation_id = continuation.id();
    context.session.queue.push(
        QueuedMessage::defer(MessageSource::SystemInjected, continuation)
            .with_policy(MessagePolicy::continue_current_run(execution)),
    );
    let stale_execution = make_context().session.turn.execution_binding();
    context.session.queue.push(
        QueuedMessage::defer(
            MessageSource::SystemInjected,
            BaseMessage::human("stale run input"),
        )
        .with_policy(MessagePolicy::continue_current_run(stale_execution)),
    );
    for body in ["first fresh result", "second fresh result"] {
        context.session.queue.push(QueuedMessage::defer(
            MessageSource::SubAgentComplete,
            BaseMessage::human(body),
        ));
    }
    let output = run_receive_with_budget(
        ReceiveInput {
            context: context.clone(),
        },
        Some(watermark),
    )
    .await
    .unwrap();
    assert_eq!(output.budget_deferred_count, 2);
    assert_eq!(output.consumed_count, 4);
    assert_eq!(output.wake_up_count, 2);
    let transcript = context.session.transcript.read();
    assert!(transcript.get(initial_id).is_some());
    assert!(transcript.get(passive_id).is_some());
    assert!(transcript.get(continuation_id).is_some());
    assert_eq!(transcript.len(), 3);
    assert_eq!(context.session.queue.suppressed_messages().len(), 1);
    let pending = context.session.queue.drain_all();
    assert_eq!(
        pending
            .iter()
            .map(|message| message.admission_sequence.unwrap())
            .collect::<Vec<_>>(),
        vec![5, 6]
    );
    assert_eq!(
        pending
            .iter()
            .map(|message| message.message().unwrap().content().to_string())
            .collect::<Vec<_>>(),
        ["first fresh result", "second fresh result"]
    );
    assert_eq!(context.session.queue.admission_watermark(), 6);
}

#[tokio::test]
async fn stable_terminal_delivery_id_is_recorded_once_across_receive_runs() {
    use peri_acp_types::system_reminder::{
        ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
        ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
    };
    let reminder = TrustedSystemReminderFactory::for_producer()
        .construct(SystemReminder {
            version: SYSTEM_REMINDER_VERSION,
            category: ReminderCategory::Task,
            source: ReminderSource("mcp".into()),
            kind: "completed".into(),
            severity: ReminderSeverity::Info,
            delivery: ReminderDelivery::Configurable,
            audiences: ReminderAudiences(vec![ReminderAudience::Model]),
            body: "completed".into(),
            summary: None,
            metadata: serde_json::json!({}),
        })
        .unwrap();
    let context = make_context();
    let delivery_id = crate::messages::MessageId::new();
    for _ in 0..2 {
        context
            .session
            .queue
            .push(QueuedMessage::system_reminder_with_delivery_id(
                MessageKind::Defer,
                MessageSource::DynamicMcpNotification,
                reminder.clone(),
                delivery_id,
            ));
        run_receive(ReceiveInput {
            context: context.clone(),
        })
        .await
        .unwrap();
    }
    {
        let transcript = context.session.transcript.read();
        assert_eq!(transcript.active_payloads().len(), 1);
        assert!(transcript.get(delivery_id).is_some());
    }
    let mut conflicting = reminder.into_inner();
    conflicting.body = "different terminal".into();
    let conflicting = TrustedSystemReminderFactory::for_producer()
        .construct(conflicting)
        .unwrap();
    context
        .session
        .queue
        .push(QueuedMessage::system_reminder_with_delivery_id(
            MessageKind::Defer,
            MessageSource::DynamicMcpNotification,
            conflicting,
            delivery_id,
        ));
    assert!(run_receive(ReceiveInput {
        context: context.clone()
    })
    .await
    .is_err());
    assert_eq!(context.session.transcript.read().active_payloads().len(), 1);
}

#[tokio::test]
async fn durable_terminal_delivery_preserves_order_and_dedups_after_exclusion() {
    use crate::session::test_resources::TestSession;
    use crate::session::transcript::MessageTranscript;
    use peri_acp_types::system_reminder::{
        ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
        ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
    };
    let bound = TestSession::open().await;
    let context = make_context();
    *context.session.transcript.write() =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id());
    let reminder = TrustedSystemReminderFactory::for_producer()
        .construct(SystemReminder {
            version: SYSTEM_REMINDER_VERSION,
            category: ReminderCategory::Task,
            source: ReminderSource("mcp".into()),
            kind: "completed".into(),
            severity: ReminderSeverity::Info,
            delivery: ReminderDelivery::Configurable,
            audiences: ReminderAudiences(vec![ReminderAudience::Model]),
            body: "owner result".into(),
            summary: None,
            metadata: serde_json::json!({}),
        })
        .unwrap();
    let first = BaseMessage::human("prior input");
    let first_id = first.id();
    let delivery_id = crate::messages::MessageId::new();
    context
        .session
        .queue
        .push(QueuedMessage::prompt(MessageSource::UserInput, first));
    context
        .session
        .queue
        .push(QueuedMessage::system_reminder_with_delivery_id(
            MessageKind::Defer,
            MessageSource::DynamicMcpNotification,
            reminder.clone(),
            delivery_id,
        ));
    run_receive(ReceiveInput {
        context: context.clone(),
    })
    .await
    .unwrap();
    let history = bound
        .resources()
        .load_session_history(&bound.thread_id())
        .await
        .unwrap();
    assert_eq!(
        history.iter().map(|item| item.id()).collect::<Vec<_>>(),
        vec![first_id, delivery_id]
    );

    context
        .session
        .transcript
        .write()
        .set_excluded(delivery_id, true);
    context
        .session
        .queue
        .push(QueuedMessage::system_reminder_with_delivery_id(
            MessageKind::Defer,
            MessageSource::DynamicMcpNotification,
            reminder,
            delivery_id,
        ));
    let output = run_receive(ReceiveInput {
        context: context.clone(),
    })
    .await
    .unwrap();
    assert_eq!(output.wake_up_count, 1);
    // 断言「没有追加第三条条目」：这里数的是热态全部条目（含被排除的 delivery），
    // 不是 `active_payloads` 的有效集合——已排除条目仍然留在 transcript 里用于查重。
    assert_eq!(context.session.transcript.read().len(), 2);
    let history = bound
        .resources()
        .load_session_history(&bound.thread_id())
        .await
        .unwrap();
    assert_eq!(history.len(), 2);
}

// REQ-T02 (b)：持久化事实已提交、热态因重建/收缩丢失该条目而条目仍有效时，
// Receive 只把提醒补装回热态，不把它当作新交付重复发射、也不重复入账。
#[tokio::test]
async fn committed_delivery_missing_from_hot_state_is_restored_without_reemitting() {
    use crate::agent::events_v2::EventBus;
    use crate::session::test_resources::TestSession;
    use crate::session::transcript::MessageTranscript;
    use peri_acp_types::system_reminder::{
        ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
        ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
    };

    let bound = TestSession::open().await;
    let mut context = make_context();
    *context.session.transcript.write() =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id());
    let reminder = TrustedSystemReminderFactory::for_producer()
        .construct(SystemReminder {
            version: SYSTEM_REMINDER_VERSION,
            category: ReminderCategory::Task,
            source: ReminderSource("mcp".into()),
            kind: "completed".into(),
            severity: ReminderSeverity::Info,
            delivery: ReminderDelivery::Configurable,
            audiences: ReminderAudiences(vec![ReminderAudience::Model]),
            body: "owner result".into(),
            summary: None,
            metadata: serde_json::json!({}),
        })
        .unwrap();
    let delivery_id = crate::messages::MessageId::new();
    let (bus, mut handles) = EventBus::new(Default::default());
    context.runtime.event_bus = Arc::new(bus);
    context
        .session
        .queue
        .push(QueuedMessage::system_reminder_with_delivery_id(
            MessageKind::Defer,
            MessageSource::DynamicMcpNotification,
            reminder.clone(),
            delivery_id,
        ));
    run_receive(ReceiveInput {
        context: context.clone(),
    })
    .await
    .unwrap();
    assert_eq!(
        drain_system_reminder_events(&mut handles),
        1,
        "首次交付是新交付，必须发射"
    );

    // 模拟热态重建（如收缩释放后重新装载）：同一存储上换一份空热态，条目仍在且未排除。
    *context.session.transcript.write() =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id());
    context
        .session
        .queue
        .push(QueuedMessage::system_reminder_with_delivery_id(
            MessageKind::Defer,
            MessageSource::DynamicMcpNotification,
            reminder,
            delivery_id,
        ));
    let output = run_receive(ReceiveInput {
        context: context.clone(),
    })
    .await
    .unwrap();
    assert_eq!(output.wake_up_count, 1, "已提交提醒仍驱动一次模型调用");
    assert!(
        context.session.transcript.read().get(delivery_id).is_some(),
        "仍有效的已提交提醒必须补装回热态"
    );
    assert_eq!(
        context.session.transcript.read().len(),
        1,
        "补装是恢复既有条目，不是重复追加"
    );
    assert_eq!(
        drain_system_reminder_events(&mut handles),
        0,
        "补装不得作为新交付重复发射"
    );
    let history = bound
        .resources()
        .load_session_history(&bound.thread_id())
        .await
        .unwrap();
    assert_eq!(history.len(), 1, "存储不重复入账");
}

// REQ-T02 (c)：持久化事实已提交且**已被排除**（原文归档）、热态又因收缩/重建没有该条目时，
// Receive 既不补装（归档原文不得回灌热态），也不把它当作新交付重复发射。
#[tokio::test]
async fn excluded_committed_delivery_is_neither_restored_nor_reemitted() {
    use crate::agent::events_v2::EventBus;
    use crate::session::test_resources::TestSession;
    use crate::session::transcript::MessageTranscript;
    use peri_acp_types::system_reminder::{
        ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
        ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
    };

    let bound = TestSession::open().await;
    let mut context = make_context();
    *context.session.transcript.write() =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id());
    let reminder = TrustedSystemReminderFactory::for_producer()
        .construct(SystemReminder {
            version: SYSTEM_REMINDER_VERSION,
            category: ReminderCategory::Task,
            source: ReminderSource("mcp".into()),
            kind: "completed".into(),
            severity: ReminderSeverity::Info,
            delivery: ReminderDelivery::Configurable,
            audiences: ReminderAudiences(vec![ReminderAudience::Model]),
            body: "owner result".into(),
            summary: None,
            metadata: serde_json::json!({}),
        })
        .unwrap();
    let delivery_id = crate::messages::MessageId::new();
    let (bus, mut handles) = EventBus::new(Default::default());
    context.runtime.event_bus = Arc::new(bus);
    context
        .session
        .queue
        .push(QueuedMessage::system_reminder_with_delivery_id(
            MessageKind::Defer,
            MessageSource::DynamicMcpNotification,
            reminder.clone(),
            delivery_id,
        ));
    run_receive(ReceiveInput {
        context: context.clone(),
    })
    .await
    .unwrap();
    assert_eq!(
        drain_system_reminder_events(&mut handles),
        1,
        "首次交付是新交付，必须发射"
    );

    // 归档：条目被排除（Full Compact 提交后的持久化事实），随后由收缩从热态释放。
    {
        let mut released = std::mem::take(&mut *context.session.transcript.write());
        released.set_excluded(delivery_id, true);
        released.flush_persistence().await.unwrap();
    }
    let flags = bound
        .resources()
        .load_message_flags(&bound.thread_id(), &[delivery_id])
        .await
        .unwrap();
    assert!(
        flags.get(&delivery_id).is_some_and(|flags| flags.excluded),
        "排除事实必须已落库"
    );
    let active = bound
        .resources()
        .load_active_snapshot(&bound.thread_id())
        .await
        .unwrap();
    assert!(
        active
            .payloads
            .iter()
            .all(|payload| payload.id() != delivery_id),
        "已排除条目必须离开有效视图"
    );
    // 模拟收缩释放热态条目：同一存储上换一份空热态。
    *context.session.transcript.write() =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id());
    context
        .session
        .queue
        .push(QueuedMessage::system_reminder_with_delivery_id(
            MessageKind::Defer,
            MessageSource::DynamicMcpNotification,
            reminder,
            delivery_id,
        ));
    let output = run_receive(ReceiveInput {
        context: context.clone(),
    })
    .await
    .unwrap();
    assert_eq!(output.wake_up_count, 1, "已提交交付仍被消费一次");
    assert!(
        context.session.transcript.read().get(delivery_id).is_none(),
        "已排除的已提交交付不得回灌热态"
    );
    assert_eq!(
        drain_system_reminder_events(&mut handles),
        0,
        "已排除的已提交交付不得作为新交付重复发射"
    );
    let history = bound
        .resources()
        .load_session_history(&bound.thread_id())
        .await
        .unwrap();
    assert_eq!(history.len(), 1, "存储不重复入账");
}

#[tokio::test]
async fn delivery_journal_admission_precedes_receive_and_recovers_after_ack() {
    use crate::session::user_input_mailbox::{UserInputMailbox, UserInputQueueError};
    use peri_acp_types::session::{EnqueueUserInputRequest, SessionInbox};
    use tokio_util::sync::CancellationToken;

    let context = make_context();
    let inbox = Arc::new(SessionInbox::new(Arc::new(context.session.queue.clone())));
    let mailbox = UserInputMailbox::new("session".into(), inbox.clone(), Arc::new(|_| {}));
    mailbox
        .attach_external_attempt(CancellationToken::new(), false)
        .unwrap();
    let enqueue = |text: &str| EnqueueUserInputRequest {
        session_id: "session".into(),
        generation: mailbox.generation().into(),
        command_id: uuid::Uuid::now_v7().to_string(),
        input_id: uuid::Uuid::now_v7().to_string(),
        content: MessageContent::text(text),
        original_draft: text.into(),
    };

    for _ in 0..128 {
        let request = enqueue("填充");
        mailbox.enqueue(&request).unwrap();
        mailbox.enter_idle();
        let messages = inbox.queue().drain_all();
        let id = messages[0].message().unwrap().id();
        mailbox.mark_claimed(&[id]);
        mailbox.mark_delivered(&[id]);
    }

    // 容量准入在 enqueue/claim 之前：日志满时拒绝，不产生 Claimed，也不写入 MQ。
    let rejected = enqueue("先拒绝后恢复");
    assert_eq!(
        mailbox.enqueue(&rejected).unwrap_err(),
        UserInputQueueError::Capacity
    );
    assert!(context.session.queue.is_empty(), "拒绝输入不得进入消费队列");

    // 泵确认旧日志后释放槽位；同一输入重新准入后走真实 Receive 并交付。
    for item in mailbox.pending_delivery_notifications() {
        mailbox.ack_delivery_notification(&item.input_id);
    }
    mailbox.enqueue(&rejected).unwrap();
    mailbox.enter_idle();
    let mut context = context;
    context.session.user_input_mailbox = Some(mailbox.clone());
    run_receive(ReceiveInput { context }).await.unwrap();
    assert!(mailbox.snapshot().items.is_empty());
    assert_eq!(mailbox.pending_delivery_notifications().len(), 1);
}

#[tokio::test]
async fn test_receive_user_input_delivery_keeps_identity_order_and_render_fifo() {
    use crate::agent::events_v2::{EventBus, RenderEvent};
    use crate::session::user_input_mailbox::{UserInputAttemptOutcome, UserInputMailbox};
    use peri_acp_types::session::{
        DispatchUserInputsRequest, EnqueueUserInputRequest, SessionInbox,
    };
    use tokio_util::sync::CancellationToken;
    let mut context = make_context();
    let inbox = Arc::new(SessionInbox::new(Arc::new(context.session.queue.clone())));
    let mailbox = UserInputMailbox::new("session".into(), inbox, Arc::new(|_| {}));
    let current = mailbox
        .attach_external_attempt(CancellationToken::new(), false)
        .unwrap();
    let a = EnqueueUserInputRequest {
        session_id: "session".into(),
        generation: mailbox.generation().into(),
        command_id: "enqueue-a".into(),
        input_id: uuid::Uuid::now_v7().to_string(),
        content: MessageContent::text("A"),
        original_draft: "A".into(),
    };
    let b = EnqueueUserInputRequest {
        command_id: "enqueue-b".into(),
        input_id: uuid::Uuid::now_v7().to_string(),
        content: MessageContent::text("B"),
        original_draft: "B".into(),
        ..a.clone()
    };
    mailbox.enqueue(&a).unwrap();
    mailbox.enqueue(&b).unwrap();
    for (command_id, input) in [("dispatch-b", &b), ("dispatch-a", &a)] {
        mailbox
            .dispatch(&DispatchUserInputsRequest {
                session_id: "session".into(),
                generation: mailbox.generation().into(),
                command_id: command_id.into(),
                input_ids: vec![input.input_id.clone()],
            })
            .unwrap();
    }
    mailbox.finish_attempt(&current, UserInputAttemptOutcome::Interrupted);
    let ticket = mailbox.reserve_run().unwrap();
    mailbox.attach_attempt(&ticket, CancellationToken::new());
    context.session.user_input_mailbox = Some(mailbox.clone());
    let (bus, mut handles) = EventBus::new(Default::default());
    context.runtime.event_bus = Arc::new(bus);
    run_receive(ReceiveInput {
        context: context.clone(),
    })
    .await
    .unwrap();
    context
        .runtime
        .event_bus
        .emit_render(RenderEvent::TextChunk {
            turn_id: context.turn_id(),
            agent_id: context.session.agent_id,
            message_id: crate::messages::MessageId::new(),
            chunk: "assistant".into(),
        });
    for input in [&b, &a] {
        let RenderEvent::UserInputDelivered {
            input_id,
            generation,
            content,
            ..
        } = handles.render_rx.recv().await.unwrap()
        else {
            panic!("真实用户气泡必须先于 assistant token");
        };
        assert_eq!(input_id, input.input_id, "逐条接受顺序必须保留");
        assert_eq!(generation, mailbox.generation(), "投递事件绑定同一会话实例");
        assert_eq!(
            content.text_content(),
            input.content.text_content(),
            "内容保留"
        );
    }
    assert!(
        matches!(
            handles.render_rx.recv().await.unwrap(),
            RenderEvent::TextChunk { .. }
        ),
        "assistant 输出在全部用户接收事件之后"
    );
    let transcript = context.session.transcript.read();
    assert_eq!(
        transcript.entries()[0].message().id().as_uuid().to_string(),
        b.input_id,
        "canonical transcript 与气泡使用同一稳定 ID"
    );
    assert_eq!(
        transcript.entries()[1].message().id().as_uuid().to_string(),
        a.input_id,
        "顺序不按历史队列位置重排"
    );
    assert!(mailbox.snapshot().items.is_empty(), "写入后从待发送区移除");
}

#[tokio::test]
async fn test_receive_enters_reliable_render_fifo_without_reserving_sequence() {
    use crate::agent::events_v2::{EventBus, EventBusConfig, RenderEvent};
    use crate::session::user_input_mailbox::UserInputMailbox;
    use peri_acp_types::session::{EnqueueUserInputRequest, SessionInbox};
    use tokio_util::sync::CancellationToken;

    let mut context = make_context();
    // 生产 stage builder 会写入 canonical session 身份；测试构造器为空，显式补齐。
    context
        .session
        .session_context
        .write()
        .insert("session_id".into(), "session".into());
    let inbox = Arc::new(SessionInbox::new(Arc::new(context.session.queue.clone())));
    let mailbox = UserInputMailbox::new("session".into(), inbox, Arc::new(|_| {}));
    mailbox
        .attach_external_attempt(CancellationToken::new(), false)
        .unwrap();
    let request = EnqueueUserInputRequest {
        session_id: "session".into(),
        generation: mailbox.generation().into(),
        command_id: "enqueue-first".into(),
        input_id: uuid::Uuid::now_v7().to_string(),
        content: MessageContent::text("问题"),
        original_draft: "问题".into(),
    };
    mailbox.enqueue(&request).unwrap();
    mailbox.enter_idle();
    context.session.user_input_mailbox = Some(mailbox.clone());

    // 容量 1：先占满 render 通道，证明交付事件等待容量而不是降级丢弃。
    let (bus, mut handles) = EventBus::new(EventBusConfig {
        render_capacity: 1,
        ..Default::default()
    });
    context.runtime.event_bus = Arc::new(bus);
    context
        .runtime
        .event_bus
        .emit_render(RenderEvent::TextChunk {
            turn_id: context.turn_id(),
            agent_id: context.session.agent_id,
            message_id: crate::messages::MessageId::new(),
            chunk: "occupied".into(),
        });

    let receive = run_receive(ReceiveInput {
        context: context.clone(),
    });
    tokio::pin!(receive);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut receive)
            .await
            .is_err(),
        "render 通道满时 Receive 必须等待容量，不得丢弃交付事件"
    );
    assert!(
        matches!(
            handles.render_rx.recv().await.unwrap(),
            RenderEvent::TextChunk { .. }
        ),
        "先释放占位事件"
    );
    receive.await.unwrap();

    let RenderEvent::UserInputDelivered { input_id, .. } = handles.render_rx.recv().await.unwrap()
    else {
        panic!("交付事件必须进入同一条 render FIFO");
    };
    assert_eq!(input_id, request.input_id, "交付事件与输入身份一致");
    assert!(
        mailbox
            .pending_delivery_notifications()
            .iter()
            .all(|entry| entry.session_seq.is_none()),
        "发布前不得由 Receive 预留序号，序号由 forwarder 发布成功后绑定"
    );
}

#[tokio::test]
async fn test_receive_empty_queue() {
    let ctx = make_context();
    let input = ReceiveInput {
        context: ctx.clone(),
    };
    let output = run_receive(input).await.unwrap();
    assert_eq!(output.consumed_count, 0);
    assert!(ctx.session.transcript.read().is_empty());
}

#[tokio::test]
async fn test_receive_consumes_prompt() {
    let ctx = make_context();
    ctx.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human(MessageContent::text("hello")),
    ));

    let input = ReceiveInput {
        context: ctx.clone(),
    };
    let output = run_receive(input).await.unwrap();
    assert_eq!(output.consumed_count, 1);
    assert_eq!(ctx.session.transcript.read().len(), 1);
}

#[tokio::test]
async fn test_receive_consumes_info_wrapped_in_reminder() {
    let ctx = make_context();
    ctx.session.queue.push(QueuedMessage::info(
        MessageSource::SystemInjected,
        BaseMessage::human(MessageContent::text("system info")),
    ));

    let input = ReceiveInput {
        context: ctx.clone(),
    };
    let output = run_receive(input).await.unwrap();
    assert_eq!(output.consumed_count, 1);
    assert_eq!(output.wake_up_count, 0);

    let transcript = ctx.session.transcript.read();
    assert_eq!(transcript.len(), 1);
    let content = transcript.entries()[0].message().content();
    assert_eq!(content, "system info");
}

#[tokio::test]
async fn test_receive_consumes_defer() {
    // RCRA：Receive 消费 Defer（不再保留）
    let ctx = make_context();
    ctx.session.queue.push(QueuedMessage::defer(
        MessageSource::SubAgentComplete,
        BaseMessage::human(MessageContent::text("deferred")),
    ));
    ctx.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human(MessageContent::text("prompt")),
    ));

    let input = ReceiveInput {
        context: ctx.clone(),
    };
    let output = run_receive(input).await.unwrap();
    // 消费全部（Prompt + Defer）
    assert_eq!(output.consumed_count, 2);
    assert_eq!(output.wake_up_count, 2);
    assert!(ctx.session.queue.is_empty(), "队列应完全排空");
    assert_eq!(ctx.session.transcript.read().len(), 2);
}

#[tokio::test]
async fn test_receive_consumes_prompt_defer_and_info_together() {
    // RCRA：混合队列应全部消费
    let ctx = make_context();
    ctx.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human(MessageContent::text("p")),
    ));
    ctx.session.queue.push(QueuedMessage::defer(
        MessageSource::SubAgentComplete,
        BaseMessage::human(MessageContent::text("d")),
    ));
    ctx.session.queue.push(QueuedMessage::info(
        MessageSource::SystemInjected,
        BaseMessage::human(MessageContent::text("i")),
    ));

    let input = ReceiveInput {
        context: ctx.clone(),
    };
    let output = run_receive(input).await.unwrap();
    assert_eq!(output.consumed_count, 3);
    assert!(ctx.session.queue.is_empty());
}

#[tokio::test]
async fn test_receive_exit_on_empty_queue() {
    // RCRA：空队列 → consumed=0 → 退出判断触发
    let ctx = make_context();
    let input = ReceiveInput {
        context: ctx.clone(),
    };
    let output = run_receive(input).await.unwrap();
    assert_eq!(output.consumed_count, 0);
}

/// M10：hook 的显式停止意图（`continue:false`）经 Receive 唯一出口停止：
/// 消费必须置位 `stop_requested`，且不得按可唤醒消息驱动额外模型请求。
#[tokio::test]
async fn hook_stop_intent_requests_stop_without_waking_the_run() {
    use peri_acp_types::system_reminder::{
        ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
        ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
    };
    let reminder = TrustedSystemReminderFactory::for_producer()
        .construct(SystemReminder {
            version: SYSTEM_REMINDER_VERSION,
            category: ReminderCategory::Lifecycle,
            source: ReminderSource("hook".into()),
            kind: "post_tool_batch_stop".into(),
            severity: ReminderSeverity::Info,
            delivery: ReminderDelivery::Configurable,
            audiences: ReminderAudiences(vec![ReminderAudience::Tui]),
            body: "hook requested stop".into(),
            summary: None,
            metadata: serde_json::json!({}),
        })
        .unwrap();
    let context = make_context();
    context.session.queue.push(
        QueuedMessage::system_reminder(MessageKind::Info, MessageSource::HookStopIntent, reminder)
            .with_policy(crate::session::MessagePolicy::passive()),
    );

    let output = run_receive(ReceiveInput {
        context: context.clone(),
    })
    .await
    .expect("receive must consume the stop intent");

    assert!(
        output.stop_requested,
        "消费到 HookStopIntent 必须置位停止请求（Receive 是唯一退出口）"
    );
    assert_eq!(
        output.wake_up_count, 0,
        "停止意图不得作为可唤醒消息驱动额外模型请求"
    );
}

fn bound_stop(execution: peri_acp_types::session::ExecutionBinding) -> QueuedMessage {
    use peri_acp_types::system_reminder::{ReminderCategory, ReminderDelivery, ReminderSeverity};
    let reminder = crate::session::producer_reminders::trusted_reminder(
        ReminderCategory::Lifecycle,
        "hook",
        "post_tool_batch_stop",
        ReminderSeverity::Info,
        ReminderDelivery::Configurable,
        "hook requested stop".into(),
        None,
        serde_json::json!({}),
    );
    QueuedMessage::system_reminder(MessageKind::Info, MessageSource::HookStopIntent, reminder)
        .with_policy(peri_acp_types::session::MessagePolicy::stop_current_run(
            execution,
        ))
}

/// 同一执行的 Stop 仍经 Receive 唯一退出口生效。
#[tokio::test]
async fn stop_intent_bound_to_current_execution_stops() {
    let context = make_context();
    let execution = context.session.turn.execution_binding();
    context.session.queue.push(bound_stop(execution));

    let output = run_receive(ReceiveInput {
        context: context.clone(),
    })
    .await
    .unwrap();

    assert!(output.stop_requested);
    assert_eq!(output.consumed_count, 1);
    assert_eq!(output.wake_up_count, 0);
    assert!(
        context.session.queue.suppressed_messages().is_empty(),
        "当前执行的 Stop 不得被当作过期消息"
    );
}

/// resume 后新执行收到旧 Stop：抑制、不停新 run、不写 transcript。
#[tokio::test]
async fn stop_intent_from_previous_execution_is_suppressed() {
    let previous = make_context().session.turn.execution_binding();
    let context = make_context();
    assert_ne!(context.session.turn.execution_binding(), previous);
    context.session.queue.push(bound_stop(previous));

    let output = run_receive(ReceiveInput {
        context: context.clone(),
    })
    .await
    .unwrap();

    assert!(!output.stop_requested, "旧 Stop 不得终止新执行");
    assert_eq!(output.consumed_count, 1);
    assert_eq!(output.wake_up_count, 0);
    assert_eq!(context.session.queue.suppressed_messages().len(), 1);
    assert!(context.session.transcript.read().is_empty());
}

#[tokio::test]
async fn receive_protocol_event_waits_when_state_channel_is_full() {
    use crate::agent::events_v2::{EventBus, EventBusConfig};
    use peri_acp_types::system_reminder::{
        ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
        ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
    };
    let mut context = make_context();
    let (bus, mut handles) = EventBus::new(EventBusConfig {
        state_capacity: 1,
        ..Default::default()
    });
    context.runtime.event_bus = Arc::new(bus);
    context
        .runtime
        .event_bus
        .emit_state(StateEvent::SyntheticUserMessage {
            turn_id: context.turn_id(),
            agent_id: context.session.agent_id,
            text: "占用容量".into(),
        });
    let reminder = TrustedSystemReminderFactory::for_producer()
        .construct(SystemReminder {
            version: SYSTEM_REMINDER_VERSION,
            category: ReminderCategory::Task,
            source: ReminderSource("receive_test".into()),
            kind: "done".into(),
            severity: ReminderSeverity::Info,
            delivery: ReminderDelivery::Configurable,
            audiences: ReminderAudiences(vec![ReminderAudience::Model]),
            body: "任务完成".into(),
            summary: None,
            metadata: serde_json::json!({}),
        })
        .unwrap();
    context.session.queue.push(QueuedMessage::system_reminder(
        MessageKind::Defer,
        MessageSource::SystemInjected,
        reminder,
    ));
    let receive = run_receive(ReceiveInput {
        context: context.clone(),
    });
    tokio::pin!(receive);
    assert!(
        std::future::poll_fn(|cx| {
            std::task::Poll::Ready(std::future::Future::poll(receive.as_mut(), cx).is_pending())
        })
        .await
    );
    assert!(!context.runtime.event_bus.health().dirty);
    handles.state_rx.recv().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), receive)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(drain_system_reminder_events(&mut handles), 1);
    assert!(!context.runtime.event_bus.health().dirty);
}
