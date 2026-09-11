use super::*;
use peri_acp_types::session::QueuedPayload;

#[test]
fn test_user_input_recall_preserves_unicode_and_model_only_audience() {
    let queue = MessageQueue::new();
    let body = "回顾\n".repeat(MAX_REMINDER_BODY_BYTES / 3);
    push_input_recalls(&queue, std::slice::from_ref(&body));
    assert!(!queue.has_wake_up(), "recall 不单独唤醒新执行");
    let chunks: Vec<_> = queue
        .drain_all()
        .into_iter()
        .map(|message| {
            let QueuedPayload::SystemReminder(reminder) = message.payload else {
                panic!("recall 必须是受控 Reminder");
            };
            assert_eq!(
                reminder.as_reminder().audiences.0,
                [ReminderAudience::Model],
                "不能在聊天区额外展示 recall"
            );
            reminder.as_reminder().body.clone()
        })
        .collect();
    assert!(chunks.len() > 1, "超限内容应安全分段");
    assert_eq!(chunks.concat(), body, "分段不能破坏 Unicode 内容或丢字");
}

#[test]
fn test_user_input_recall_empty_is_noop() {
    let queue = MessageQueue::new();
    push_input_recalls(&queue, &[]);
    assert!(queue.is_empty(), "空 recall 不引入空消息");
}
