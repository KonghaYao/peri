use super::*;
use peri_acp_types::messages::BaseMessage;
use peri_acp_types::session::{MessageKind, MessageSource, QueuedMessage};

async fn enqueue(queue: &MessageQueue, kind: MessageKind) {
    queue
        .push_async(QueuedMessage::new(
            kind,
            MessageSource::ShellComplete,
            BaseMessage::human("task result"),
        ))
        .await;
}

#[tokio::test]
async fn failed_inputs_and_passive_arrivals_do_not_restart_execution() {
    let activation = SessionActivation::default();
    let queue = MessageQueue::new();
    activation.allow();
    enqueue(&queue, MessageKind::Defer).await;
    activation.record_failed_attempt(
        queue
            .admission_watermark()
            .await
            .expect("test MQ watermark"),
    );
    assert!(!activation.can_activate(&queue).await);
    enqueue(&queue, MessageKind::Info).await;
    assert!(!activation.can_activate(&queue).await);
    enqueue(&queue, MessageKind::Defer).await;
    assert!(activation.can_activate(&queue).await);
}

#[tokio::test]
async fn required_arrival_during_failed_attempt_remains_eligible() {
    let activation = SessionActivation::default();
    let queue = MessageQueue::new();
    activation.allow();
    enqueue(&queue, MessageKind::Defer).await;
    let watermark = queue
        .admission_watermark()
        .await
        .expect("test MQ watermark");
    enqueue(&queue, MessageKind::Defer).await;
    activation.record_failed_attempt(watermark);
    assert!(activation.can_activate(&queue).await);
}

#[tokio::test]
async fn explicit_stop_survives_failure_and_fresh_required_work() {
    let activation = SessionActivation::default();
    let queue = MessageQueue::new();
    activation.allow();
    enqueue(&queue, MessageKind::Defer).await;
    let watermark = queue
        .admission_watermark()
        .await
        .expect("test MQ watermark");
    activation.suppress();
    activation.record_failed_attempt(watermark);
    enqueue(&queue, MessageKind::Defer).await;
    assert!(!activation.can_activate(&queue).await);
    activation.allow();
    assert!(activation.can_activate(&queue).await);
}

#[test]
fn recovery_allow_does_not_clear_an_explicit_stop() {
    let activation = SessionActivation::default();
    activation.suppress();

    assert!(!activation.try_allow());
    assert!(activation.user_stopped());
    assert!(!activation.is_allowed());

    // 显式继续仍是唯一能解除 Stop 的入口。
    activation.allow();
    assert!(activation.is_allowed());
}

#[test]
fn recovery_allow_arms_execution_without_a_stop() {
    let activation = SessionActivation::default();
    activation.suppress();
    activation.allow();

    assert!(activation.try_allow());
    assert!(activation.is_allowed());
    assert!(!activation.user_stopped());
}

#[tokio::test]
async fn requeued_failed_input_preserves_its_admission_boundary() {
    let activation = SessionActivation::default();
    let queue = MessageQueue::new();
    activation.allow();
    enqueue(&queue, MessageKind::Defer).await;
    let watermark = queue
        .admission_watermark()
        .await
        .expect("test MQ watermark");
    // MQ 失败重试不先提交：快照后按稳定 ID 重投是幂等的，接纳边界保持不变。
    let messages = queue.snapshot().await.expect("test MQ snapshot");
    activation.record_failed_attempt(watermark.clone());
    queue.push_batch_async(messages).await;
    assert_eq!(
        queue
            .admission_watermark()
            .await
            .expect("test MQ watermark"),
        watermark
    );
    assert!(!activation.can_activate(&queue).await);
}

// ─── 后台结果：失败水位、Stop 与显式继续（T05/T12）──────────────────────────────

use peri_acp_types::messages::MessageId;
use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource as ReminderSourceKind, SystemReminder, TrustedSystemReminderFactory,
    SYSTEM_REMINDER_VERSION,
};
use peri_agent::session::async_result_mailbox::AsyncResultMailbox;

fn terminal_reminder(task_id: &str) -> peri_acp_types::system_reminder::TrustedSystemReminder {
    TrustedSystemReminderFactory::for_producer()
        .construct(SystemReminder {
            version: SYSTEM_REMINDER_VERSION,
            category: ReminderCategory::Task,
            source: ReminderSourceKind("shell".into()),
            kind: "terminal".into(),
            severity: ReminderSeverity::Info,
            delivery: ReminderDelivery::Configurable,
            audiences: ReminderAudiences(vec![ReminderAudience::Model]),
            body: format!("task {task_id} finished"),
            summary: None,
            metadata: serde_json::json!({ "task_id": task_id }),
        })
        .unwrap()
}

fn ready_result(mailbox: &AsyncResultMailbox, task_id: &str) -> MessageId {
    let id = MessageId::new();
    let reminder = terminal_reminder(task_id);
    mailbox
        .accept(id, &reminder, MessageSource::ShellComplete)
        .unwrap();
    mailbox
        .mark_canonical_ready(id, &reminder, MessageSource::ShellComplete, None)
        .unwrap();
    id
}

/// [T05] 启动闸门失败后：失败尝试期间已有的 Ready 结果不能自我激活；
/// 只有失败水位之后新接纳的 Ready 结果才是合法的恢复触发。
#[test]
fn async_result_failed_attempt_cannot_self_activate() {
    let activation = SessionActivation::default();
    activation.allow();
    let mailbox = AsyncResultMailbox::new("session-t05", "thread-t05");

    ready_result(&mailbox, "shell-t05-a");
    let failed_at = mailbox.admission_watermark();
    activation.record_failed_result_watermark(failed_at);

    assert!(
        !activation.can_activate_result(&mailbox),
        "失败尝试开始时已存在的 Ready 结果不能反复触发同一段执行"
    );
    assert!(mailbox.has_ready(), "结果本身仍保持 Ready，等待显式继续");

    ready_result(&mailbox, "shell-t05-b");
    assert!(
        activation.can_activate_result(&mailbox),
        "失败水位之后新到达的真实结果可以启动下一次尝试"
    );
}

/// [T12] 用户 Stop 优先：挂起期间新完成通知不得绕过暂停；显式继续后恢复。
#[test]
fn async_result_stop_requires_explicit_resume() {
    let activation = SessionActivation::default();
    activation.allow();
    let mailbox = AsyncResultMailbox::new("session-t12", "thread-t12");

    ready_result(&mailbox, "shell-t12-a");
    assert!(activation.can_activate_result(&mailbox));

    activation.suppress();
    mailbox.suspend_scope("cancelled");
    assert!(
        !activation.can_activate_result(&mailbox),
        "Stop 后不得自动续跑"
    );
    assert!(!mailbox.has_ready(), "挂起结果不再构成 Ready 工作来源");

    ready_result(&mailbox, "shell-t12-b");
    assert!(
        !activation.can_activate_result(&mailbox),
        "Stop 之后的新完成通知不能绕过已暂停的自动执行权限"
    );

    // 用户明确继续：宿主 allow() 并 resume_scope()，两件事缺一不可。
    activation.allow();
    assert!(
        !activation.can_activate_result(&mailbox),
        "仅恢复激活权限不足以让 Suspended 结果自动运行"
    );
    mailbox.resume_scope();
    assert!(mailbox.has_ready());
    assert!(activation.can_activate_result(&mailbox));
}
