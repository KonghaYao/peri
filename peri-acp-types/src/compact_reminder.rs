//! Legacy Compact context projection; storage stays unchanged and provenance stays legacy.

use crate::compact::CONTINUATION_HINT;
use crate::messages::{BaseMessage, MessageContent};
use crate::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource, SystemReminder, SYSTEM_REMINDER_VERSION,
};

/// Recognizes only whole plain-text Human messages in the historical Compact format.
/// This is a compatibility heuristic, never evidence of trusted producer provenance.
pub fn legacy_compact_reminders(message: &BaseMessage) -> Vec<SystemReminder> {
    let BaseMessage::Human {
        content: MessageContent::Text(text),
        ..
    } = message
    else {
        return Vec::new();
    };
    let Some((header, _)) = text.split_once('\n') else {
        return Vec::new();
    };
    let recognized = ["[最近读取的文件: ", "[激活的 Skill 指令: "]
        .iter()
        .any(|prefix| {
            header
                .strip_prefix(prefix)
                .and_then(|value| value.strip_suffix(']'))
                .is_some_and(|value| !value.trim().is_empty())
        })
        || header == CONTINUATION_HINT;
    if !recognized {
        return Vec::new();
    }
    // XML escaping can expand one byte to six; bounded chunks preserve arbitrary legacy bodies.
    let mut remainder = text.as_str();
    let mut reminders = Vec::new();
    while !remainder.is_empty() {
        let mut end = remainder.len().min(8 * 1024);
        while !remainder.is_char_boundary(end) {
            end -= 1;
        }
        let (body, rest) = remainder.split_at(end);
        reminders.push(SystemReminder {
            version: SYSTEM_REMINDER_VERSION,
            category: ReminderCategory::Legacy,
            source: ReminderSource("legacy".into()),
            kind: "compact_context".into(),
            severity: ReminderSeverity::Info,
            delivery: ReminderDelivery::Configurable,
            audiences: ReminderAudiences(vec![ReminderAudience::Model, ReminderAudience::Tui]),
            body: body.to_owned(),
            summary: None,
            metadata: serde_json::json!({}),
        });
        remainder = rest;
    }
    reminders
}

#[cfg(test)]
#[path = "compact_reminder_test.rs"]
mod tests;
