//! MCP 终态交付的接收、去重与历史回灌契约。
use super::*;
use crate::agent::events_v2::EventHandles;
use crate::agent::stages::StageContext;
use crate::messages::BaseMessage;
use crate::session::queue::MessageSource;
use crate::session::store::FrozenContext;
use crate::session::{QueuedMessage, Session};
use std::sync::Arc;

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
                MessageSource::McpNotification,
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
            MessageSource::McpNotification,
            conflicting,
            delivery_id,
        ));
    assert!(run_receive(ReceiveInput {
        context: context.clone()
    })
    .await
    .is_err());
    assert_eq!(context.session.transcript.read().active_payloads().len(), 1);
    // 冲突消息进入 suppressed 诊断，不得重新入队形成死循环。
    assert_eq!(context.session.queue.suppressed_messages().len(), 1);
    run_receive(ReceiveInput {
        context: context.clone(),
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn mailbox_identity_conflict_is_suppressed_without_blocking_later_inputs() {
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
    // 提醒正文相同、来源不同：transcript 去重通过，但 mailbox 指纹冲突。
    for source in [MessageSource::McpNotification, MessageSource::ShellComplete] {
        context
            .session
            .queue
            .push(QueuedMessage::system_reminder_with_delivery_id(
                MessageKind::Defer,
                source,
                reminder.clone(),
                delivery_id,
            ));
    }
    assert!(run_receive(ReceiveInput {
        context: context.clone()
    })
    .await
    .is_err());
    assert_eq!(context.session.transcript.read().active_payloads().len(), 1);
    assert_eq!(context.session.queue.suppressed_messages().len(), 1);
    // 同一 ID 同内容重放仍幂等；冲突消息已被隔离，后续 Receive 不再失败。
    context
        .session
        .queue
        .push(QueuedMessage::system_reminder_with_delivery_id(
            MessageKind::Defer,
            MessageSource::McpNotification,
            reminder.clone(),
            delivery_id,
        ));
    run_receive(ReceiveInput {
        context: context.clone(),
    })
    .await
    .unwrap();
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
            MessageSource::McpNotification,
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
            MessageSource::McpNotification,
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
            MessageSource::McpNotification,
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
            MessageSource::McpNotification,
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
            MessageSource::McpNotification,
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
            MessageSource::McpNotification,
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
