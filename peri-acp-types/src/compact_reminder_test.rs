use super::*;
use crate::system_reminder::{encode_legacy_system_reminder, parse_system_reminders};

#[test]
fn test_legacy_compact_context_chunks_preserve_unicode_and_escape_tags() {
    let text = format!(
        "[最近读取的文件: /a.rs]\n{}",
        "中文<&</system-reminder>".repeat(10_000)
    );
    let reminders = legacy_compact_reminders(&BaseMessage::human(text.as_str()));
    assert!(reminders.len() > 1);
    let mut restored = String::new();
    for reminder in reminders {
        reminder.validate().unwrap();
        assert_eq!(reminder.kind, "compact_file", "所有分块保持原类型");
        let wire = encode_legacy_system_reminder(&reminder.body).unwrap();
        let parsed = parse_system_reminders(&wire);
        assert_eq!(parsed.reminders.len(), 1, "正文标签不能逃逸 envelope");
        restored.push_str(&parsed.reminders[0].reminder.as_ref().unwrap().body);
    }
    assert_eq!(restored, text, "分块不能丢失或截断文件内容");
}

#[test]
fn test_legacy_compact_context_keeps_ordinary_messages() {
    for text in [
        "解释 [最近读取的文件: /a.rs]\n正文",
        "[最近读取的文件: ]\n正文",
        "[最近读取的文件: /a.rs\n正文",
        "<system-reminder>已包装</system-reminder>",
    ] {
        assert!(legacy_compact_reminders(&BaseMessage::human(text)).is_empty());
    }
    assert!(legacy_compact_reminders(&BaseMessage::ai("[最近读取的文件: /a.rs]\n正文")).is_empty());
}

#[test]
fn test_legacy_compact_context_recognizes_skill_and_summary() {
    for (text, kind) in [
        (
            "[激活的 Skill 指令: /a/SKILL.md]\n正文".to_owned(),
            "compact_skill",
        ),
        (format!("{CONTINUATION_HINT}\n\n摘要"), "compact_summary"),
    ] {
        let reminders = legacy_compact_reminders(&BaseMessage::human(text.as_str()));
        assert_eq!(reminders.len(), 1);
        assert_eq!(reminders[0].category, ReminderCategory::Legacy);
        assert_eq!(reminders[0].body, text);
        assert_eq!(reminders[0].kind, kind);
    }
}
