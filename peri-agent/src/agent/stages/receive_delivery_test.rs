//! MCP 终态交付的接收、去重与历史回灌契约。
use super::*;
use crate::agent::events_v2::EventHandles;
use crate::agent::stages::StageContext;
use crate::messages::BaseMessage;
use crate::session::queue::MessageSource;
use crate::session::store::FrozenContext;
use crate::session::test_resources::TestSession;
use crate::session::transcript::MessageTranscript;
use crate::session::{QueuedMessage, Session};
use peri_acp_types::session::MqEnqueueDisposition;
use std::sync::Arc;

fn make_context() -> StageContext {
    let cwd: Arc<str> = Arc::from("/tmp/test");
    let frozen = FrozenContext::builder().build();
    let session = Session::new(cwd, frozen, None);
    let turn = session.start_turn();
    StageContext::new(turn, session.transcript(), session.queue().clone())
}

/// 与生产同形：MQ 绑定真实资源与 thread，transcript 也绑定同一份持久化。
fn make_bound_context(bound: &TestSession) -> StageContext {
    let cwd: Arc<str> = Arc::from("/tmp/test");
    let frozen = FrozenContext::builder().build();
    let queue = bound.queue();
    let session = Session::new_with_cancel_and_queue(
        cwd,
        frozen,
        Some(bound.thread_id()),
        Arc::new(tokio_util::sync::CancellationToken::new()),
        queue.clone(),
    );
    *session.transcript().write() =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id());
    let turn = session.start_turn();
    StageContext::new(turn, session.transcript(), queue)
}

fn rebind_empty_transcript(context: &StageContext, bound: &TestSession) {
    *context.session.transcript.write() =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id());
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
    let queue = context.session.queue.clone();
    for expected in [
        MqEnqueueDisposition::Inserted,
        // 第二次入队时历史已有同 ID：返回已移交事实，不再创建 MQ 行。
        MqEnqueueDisposition::AlreadyHandedOff,
    ] {
        let outcome = queue
            .enqueue(QueuedMessage::system_reminder_with_delivery_id(
                MessageKind::Defer,
                MessageSource::McpNotification,
                reminder.clone(),
                delivery_id,
            ))
            .await
            .unwrap();
        assert_eq!(outcome.disposition, expected);
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
    let outcome = queue
        .enqueue(QueuedMessage::system_reminder_with_delivery_id(
            MessageKind::Defer,
            MessageSource::McpNotification,
            conflicting,
            delivery_id,
        ))
        .await
        .unwrap();
    assert_eq!(
        outcome.disposition,
        MqEnqueueDisposition::AlreadyHandedOff,
        "已移交的稳定 ID 不接纳替换正文，也不制造新的处理轮次"
    );
    run_receive(ReceiveInput {
        context: context.clone(),
    })
    .await
    .unwrap();
    assert_eq!(context.session.transcript.read().active_payloads().len(), 1);
}

#[tokio::test]
async fn identity_conflict_is_rejected_at_enqueue_without_blocking_later_inputs() {
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
    let queue = context.session.queue.clone();
    let delivery_id = crate::messages::MessageId::new();
    // 同一 ID 已在 MQ 但内容不同（来源不同也是不同行形态）：入队即拒绝，
    // 不能覆盖旧行，也不能留下两行等 Receive 去猜（thread-mq §6）。
    queue
        .enqueue(QueuedMessage::system_reminder_with_delivery_id(
            MessageKind::Defer,
            MessageSource::McpNotification,
            reminder.clone(),
            delivery_id,
        ))
        .await
        .unwrap();
    let conflict = queue
        .enqueue(QueuedMessage::system_reminder_with_delivery_id(
            MessageKind::Defer,
            MessageSource::ShellComplete,
            reminder.clone(),
            delivery_id,
        ))
        .await;
    assert!(conflict.is_err(), "身份冲突必须显式报错");
    // 冲突没有阻塞后续输入：原始那一行仍可正常移交。
    run_receive(ReceiveInput {
        context: context.clone(),
    })
    .await
    .unwrap();
    assert_eq!(context.session.transcript.read().active_payloads().len(), 1);
    // 同一 ID 同内容重放复用接纳结果；已移交后不再制造新行。
    let replay = queue
        .enqueue(QueuedMessage::system_reminder_with_delivery_id(
            MessageKind::Defer,
            MessageSource::McpNotification,
            reminder.clone(),
            delivery_id,
        ))
        .await
        .unwrap();
    assert_eq!(replay.disposition, MqEnqueueDisposition::AlreadyHandedOff);
    run_receive(ReceiveInput {
        context: context.clone(),
    })
    .await
    .unwrap();
    assert_eq!(context.session.transcript.read().active_payloads().len(), 1);
}

#[tokio::test]
async fn durable_terminal_delivery_preserves_order_and_dedups_after_exclusion() {
    use peri_acp_types::system_reminder::{
        ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
        ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
    };
    let bound = TestSession::open().await;
    let context = make_bound_context(&bound);
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
        .push_async(QueuedMessage::prompt(MessageSource::UserInput, first))
        .await;
    context
        .session
        .queue
        .push_async(QueuedMessage::system_reminder_with_delivery_id(
            MessageKind::Defer,
            MessageSource::McpNotification,
            reminder.clone(),
            delivery_id,
        ))
        .await;
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
    // 已移交的交付重试：存储返回「已移交」，不创建新行，也不制造新的模型调用
    // （`docs/design/thread-mq.md` §7「确认前退出」）。
    let replay = context
        .session
        .queue
        .enqueue(QueuedMessage::system_reminder_with_delivery_id(
            MessageKind::Defer,
            MessageSource::McpNotification,
            reminder,
            delivery_id,
        ))
        .await
        .unwrap();
    assert_eq!(replay.disposition, MqEnqueueDisposition::AlreadyHandedOff);
    let output = run_receive(ReceiveInput {
        context: context.clone(),
    })
    .await
    .unwrap();
    assert_eq!(output.wake_up_count, 0, "已移交的重试不再驱动模型调用");
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
//
// 「历史已有、MQ 行仍在」只可能来自旧写入路径或崩溃窗口（新写入在同一事务里
// 删除 MQ 行），这里用 `append_reminder_if_absent` 显式构造该状态。
#[tokio::test]
async fn committed_delivery_missing_from_hot_state_is_restored_without_reemitting() {
    use crate::agent::events_v2::EventBus;
    use peri_acp_types::system_reminder::{
        ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
        ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
    };

    let bound = TestSession::open().await;
    let mut context = make_bound_context(&bound);
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
        .enqueue(QueuedMessage::system_reminder_with_delivery_id(
            MessageKind::Defer,
            MessageSource::McpNotification,
            reminder.clone(),
            delivery_id,
        ))
        .await
        .unwrap();
    assert!(
        bound
            .resources()
            .append_reminder_if_absent(&bound.thread_id(), delivery_id, &reminder)
            .await
            .unwrap(),
        "先提交历史事实，模拟旧写入路径留下的 MQ 行"
    );

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
    use peri_acp_types::system_reminder::{
        ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
        ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
    };

    let bound = TestSession::open().await;
    let mut context = make_bound_context(&bound);
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
        .enqueue(QueuedMessage::system_reminder_with_delivery_id(
            MessageKind::Defer,
            MessageSource::McpNotification,
            reminder.clone(),
            delivery_id,
        ))
        .await
        .unwrap();
    assert!(
        bound
            .resources()
            .append_reminder_if_absent(&bound.thread_id(), delivery_id, &reminder)
            .await
            .unwrap(),
        "先提交历史事实，模拟旧写入路径留下的 MQ 行"
    );

    // 归档：条目被排除（Full Compact 提交后的持久化事实），随后由收缩从热态释放。
    let persist_tx = {
        let mut hot = context.session.transcript.write();
        hot.mirror_committed_reminder(delivery_id, reminder.clone());
        hot.set_excluded(delivery_id, true);
        hot.persist_tx_handle()
    };
    if let Some(tx) = persist_tx {
        MessageTranscript::flush_via_tx(&tx).await.unwrap();
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
    rebind_empty_transcript(&context, &bound);
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
