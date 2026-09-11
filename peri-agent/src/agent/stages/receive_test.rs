//! 从 receive.rs 分离的测试模块
use super::*;
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
