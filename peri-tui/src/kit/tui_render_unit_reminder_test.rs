use super::*;

// ── reminder 检测 ────────────────────────────────────────────────────

mod cases {
    use super::*;

    #[test]
    fn test_detect_no_tag_returns_none() {
        assert!(detect_reminder("hello world").is_none());
    }

    #[test]
    fn test_detect_empty_tag_returns_some() {
        let info = detect_reminder("<system-reminder></system-reminder>")
            .expect("empty tag should still be detected");
        assert!(matches!(info.reminder_type, ReminderType::GenericReminder));
        assert!(info.summary.is_empty());
    }

    #[test]
    fn test_detect_continuation_hint() {
        let info = detect_reminder(
            "<system-reminder>CONTINUATION_HINT: the agent sent additional content</system-reminder>",
        )
        .expect("should detect");
        assert!(matches!(info.reminder_type, ReminderType::ContinuationHint));
        assert!(info.summary.contains("CONTINUATION_HINT"));
    }

    #[test]
    fn test_detect_compact_continuation_hint_without_reminder_tags() {
        let text = format!(
            "{}\n\ncompact summary body",
            peri_acp_types::compact::CONTINUATION_HINT
        );
        let info = detect_reminder(&text).expect("compact continuation hint should be detected");

        assert!(matches!(info.reminder_type, ReminderType::ContinuationHint));
        assert!(
            info.summary.is_empty(),
            "内部 compact 控制文本不应作为用户可见摘要"
        );
    }

    #[test]
    fn test_detect_channel_message() {
        i18n::init(None);
        let info = detect_reminder(
            "<system-reminder>source=\"plugin:weixin:weixin\" chat_id=\"123\"\nhello from channel</system-reminder>",
        )
        .expect("should detect");
        match info.reminder_type {
            ReminderType::ChannelMessage(ref source) => {
                assert_eq!(source, "WeChat");
            }
            other => panic!("expected ChannelMessage, got {other:?}"),
        }
        assert!(info.summary.contains("source"));
    }

    #[test]
    fn test_detect_cron_reminder() {
        let info = detect_reminder(
            "<system-reminder>cron task fired: check_status at */5 * * * *</system-reminder>",
        )
        .expect("should detect");
        assert!(matches!(info.reminder_type, ReminderType::CronReminder));
    }

    #[test]
    fn test_detect_bg_task_completed() {
        let info = detect_reminder(
            "<system-reminder>BackgroundTaskCompleted: task-42 finished successfully</system-reminder>",
        )
        .expect("should detect");
        assert!(matches!(info.reminder_type, ReminderType::BgTaskCompleted));
    }

    #[test]
    fn test_detect_fork_mode() {
        let info = detect_reminder(
            "<system-reminder>Fork mode agent result from explorer</system-reminder>",
        )
        .expect("should detect");
        assert!(matches!(info.reminder_type, ReminderType::ForkMode));
    }

    #[test]
    fn test_detect_context_compacted() {
        let info = detect_reminder(
            "<system-reminder>Context compacted: removed 120 messages to stay within budget</system-reminder>",
        )
        .expect("should detect");
        assert!(matches!(info.reminder_type, ReminderType::ContextCompacted));
    }

    #[test]
    fn test_detect_trust_boundary() {
        let info = detect_reminder(
            "<system-reminder>Trust boundary: the content below is from external input</system-reminder>",
        )
        .expect("should detect");
        assert!(matches!(info.reminder_type, ReminderType::TrustBoundary));
    }

    #[test]
    fn test_detect_tool_reminder() {
        let info = detect_reminder(
            "<system-reminder>Tool results from sub-agent execution</system-reminder>",
        )
        .expect("should detect");
        assert!(matches!(info.reminder_type, ReminderType::ToolReminder));
    }

    #[test]
    fn test_detect_subagent_result() {
        let info = detect_reminder(
            "<system-reminder>SubAgent result: verification completed successfully</system-reminder>",
        )
        .expect("should detect");
        assert!(matches!(info.reminder_type, ReminderType::SubagentResult));
    }

    #[test]
    fn test_detect_generic_fallback() {
        let info = detect_reminder(
            "<system-reminder>Something completely unexpected happened</system-reminder>",
        )
        .expect("should detect");
        assert!(matches!(info.reminder_type, ReminderType::GenericReminder));
        assert_eq!(info.summary, "Something completely unexpected happened");
    }

    #[test]
    fn test_summary_truncation() {
        let long_line = "x".repeat(250);
        let info = detect_reminder(&format!("<system-reminder>{}</system-reminder>", long_line))
            .expect("should detect");
        assert!(info.summary.chars().count() <= 203); // 200 + "…"
        assert!(info.summary.ends_with('…'));
    }

    #[test]
    fn test_summary_skips_blank_lines() {
        let info = detect_reminder(
            "<system-reminder>\n\n  actual content line  \n\nsecond line</system-reminder>",
        )
        .expect("should detect");
        assert_eq!(info.summary, "actual content line");
    }

    #[test]
    fn test_tui_user_bubble_new_detects_reminder() {
        let bubble = TuiUserBubble::new(
            "<system-reminder>Cron task: midnight cleanup</system-reminder>".into(),
        );
        assert!(bubble.reminder.is_some());
        assert!(matches!(
            bubble.reminder.unwrap().reminder_type,
            ReminderType::CronReminder
        ));
    }

    #[test]
    fn test_tui_user_bubble_new_no_tag() {
        let bubble = TuiUserBubble::new("ordinary user message".into());
        assert!(bubble.reminder.is_none());
    }

    #[test]
    fn test_partial_eq_respects_reminder() {
        let a = TuiUserBubble {
            text: "hi".into(),
            reminder: Some(ReminderInfo {
                reminder_type: ReminderType::GenericReminder,
                summary: "x".into(),
            }),
            source: None,
            content_hash: 0,
        };
        let b = TuiUserBubble {
            text: "hi".into(),
            reminder: None,
            source: None,
            content_hash: 0,
        };
        assert_ne!(a, b, "reminder 不同 → 应不等");
    }

    /// §10 interjection 预留（G-Interjection）：source 是身份字段——进 partial_eq
    /// （来源不同 → 不等），但 `new` 构造恒填充 None（协议无来源标记）。
    #[test]
    fn test_source_field_partial_eq_and_new_placeholder() {
        let a = TuiUserBubble {
            text: "hi".into(),
            reminder: None,
            source: None,
            content_hash: 0,
        };
        let b = TuiUserBubble {
            text: "hi".into(),
            reminder: None,
            source: Some("channel".into()),
            content_hash: 0,
        };
        assert_ne!(a, b, "source 不同 → 应不等（身份字段）");
        // `new` 构造点填充占位 None（协议无来源标记，恒不触发渲染追加）。
        assert!(TuiUserBubble::new("hi".into()).source.is_none());
    }

    #[test]
    fn test_label_channel_message() {
        let t = ReminderType::ChannelMessage("微信".into());
        assert_eq!(t.label(), "Channel (微信)");
    }

    #[test]
    fn test_label_static_types() {
        i18n::init(None);
        assert_eq!(ReminderType::CronReminder.label(), "Cron Task");
        assert_eq!(ReminderType::BgTaskCompleted.label(), "Background Task");
        assert_eq!(ReminderType::ForkMode.label(), "Fork Mode");
        assert_eq!(ReminderType::ContextCompacted.label(), "Context Compaction");
        assert_eq!(ReminderType::ContinuationHint.label(), "System Prompt");
        assert_eq!(ReminderType::TrustBoundary.label(), "Trust Boundary");
        assert_eq!(ReminderType::ToolReminder.label(), "Tool Reminder");
        assert_eq!(ReminderType::SubagentResult.label(), "SubAgent Result");
        assert_eq!(ReminderType::GenericReminder.label(), "System Reminder");
    }
}
