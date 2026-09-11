//! Mailbox 用户输入沿用上一轮 recall，但不改写可取回的原始输入内容。

use peri_acp_types::{
    session::{MessageKind, MessageQueue, MessageSource, QueuedMessage},
    system_reminder::{
        ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
        ReminderSource, SystemReminder, TrustedSystemReminderFactory, MAX_REMINDER_BODY_BYTES,
        SYSTEM_REMINDER_VERSION,
    },
};

pub(super) fn push_input_recalls(queue: &MessageQueue, recalls: &[String]) {
    let body = recalls.join("\n");
    let mut remaining = body.as_str();
    while !remaining.is_empty() {
        let mut end = remaining.len().min(MAX_REMINDER_BODY_BYTES);
        while !remaining.is_char_boundary(end) {
            end -= 1;
        }
        let reminder = TrustedSystemReminderFactory::for_producer()
            .construct(SystemReminder {
                version: SYSTEM_REMINDER_VERSION,
                category: ReminderCategory::Guidance,
                source: ReminderSource("user_input_recall".into()),
                kind: "previous_turn_recall".into(),
                severity: ReminderSeverity::Info,
                delivery: ReminderDelivery::Required,
                audiences: ReminderAudiences(vec![ReminderAudience::Model]),
                body: remaining[..end].to_owned(),
                summary: None,
                metadata: serde_json::json!({}),
            })
            .expect("recall 按 UTF-8 字节上限分段，受控元数据满足 Reminder 契约");
        queue.push(QueuedMessage::system_reminder(
            MessageKind::Info,
            MessageSource::SystemInjected,
            reminder,
        ));
        remaining = &remaining[end..];
    }
}

#[cfg(test)]
#[path = "input_recalls_test.rs"]
mod tests;
