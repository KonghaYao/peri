//! Tests for tui_render_unit

use super::*;

// ── tui_hash_str ─────────────────────────────────────────────────────

#[test]
fn test_tui_hash_str_same_input_same_output() {
    assert_eq!(tui_hash_str("hello"), tui_hash_str("hello"));
}

#[test]
fn test_tui_hash_str_different_input_different_output() {
    assert_ne!(tui_hash_str("hello"), tui_hash_str("world"));
}

#[test]
fn test_tui_hash_str_empty_string() {
    // 空字符串不 panic
    let _h = tui_hash_str("");
}

// ── TuiRenderUnit::content_hash() dispatch ──────────────────────────

#[test]
fn test_content_hash_returns_inner_field_for_each_variant() {
    // 验证 content_hash() 方法正确派发到各变体的内部字段
    let user = TuiRenderUnit::TuiUserBubble(TuiUserBubble {
        text: "u".into(),
        reminder: None,
        content_hash: 11,
    });
    assert_eq!(user.content_hash(), 11);
    let assistant = TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
        text: "a".into(),
        reasoning: None,
        content_hash: 22,
    });
    assert_eq!(assistant.content_hash(), 22);
    let tool = TuiRenderUnit::TuiToolCard(TuiToolCard {
        tool_id: "t1".into(),
        tool_name: "Bash".into(),
        input_summary: "ls".into(),
        output_summary: String::new(),
        is_error: false,
        is_running: false,
        running_duration_ms: None,
        diff: None,
        tool_calls_count: 0,
        content_hash: 33,
    });
    assert_eq!(tool.content_hash(), 33);
    let note = TuiRenderUnit::TuiSystemNote(TuiSystemNote {
        text: "n".into(),
        level: TuiNoteLevel::Info,
        content_hash: 44,
    });
    assert_eq!(note.content_hash(), 44);
}

// ── TuiAssistantBubble::compute_hash / recompute_hash ──────────────

#[test]
fn test_compute_hash_no_reasoning_only_hashes_text() {
    // 无 reasoning：hash 只基于 text
    let h1 = TuiAssistantBubble::compute_hash("hello", None);
    let h2 = TuiAssistantBubble::compute_hash("hello", None);
    let h3 = TuiAssistantBubble::compute_hash("world", None);
    assert_eq!(h1, h2, "相同 text 应有相同 hash");
    assert_ne!(h1, h3, "不同 text 应有不同 hash");
}

#[test]
fn test_compute_hash_includes_collapsed_state() {
    // [回归测试] Bug 2 修复：reasoning.collapsed 必须纳入 hash，
    // 否则按 hash 分片的渲染缓存命中旧值、折叠/展开后 UI 不刷新。
    let reasoning_open = TuiReasoningBlock {
        text: "thinking".into(),
        collapsed: false,
    };
    let reasoning_collapsed = TuiReasoningBlock {
        text: "thinking".into(),
        collapsed: true,
    };
    let h_open = TuiAssistantBubble::compute_hash("reply", Some(&reasoning_open));
    let h_collapsed = TuiAssistantBubble::compute_hash("reply", Some(&reasoning_collapsed));
    assert_ne!(
        h_open, h_collapsed,
        "collapsed 状态变化时 content_hash 必须变化"
    );
}

#[test]
fn test_compute_hash_includes_reasoning_text() {
    let r1 = TuiReasoningBlock {
        text: "thought A".into(),
        collapsed: false,
    };
    let r2 = TuiReasoningBlock {
        text: "thought B".into(),
        collapsed: false,
    };
    let h1 = TuiAssistantBubble::compute_hash("reply", Some(&r1));
    let h2 = TuiAssistantBubble::compute_hash("reply", Some(&r2));
    assert_ne!(h1, h2, "reasoning.text 变化时 content_hash 必须变化");
}

#[test]
fn test_recompute_hash_after_collapse_change() {
    // [回归测试] push_view_models 修改 collapsed 后必须调用 recompute_hash，
    // 否则缓存命中旧 hash 渲染不更新。
    let mut bubble = TuiAssistantBubble {
        text: "reply".into(),
        reasoning: Some(TuiReasoningBlock {
            text: "thinking".into(),
            collapsed: false,
        }),
        content_hash: 0,
    };
    bubble.content_hash =
        TuiAssistantBubble::compute_hash(&bubble.text, bubble.reasoning.as_ref());
    let initial_hash = bubble.content_hash;
    // 修改 collapsed 状态
    bubble.reasoning.as_mut().unwrap().collapsed = true;
    // 不调用 recompute_hash → content_hash 仍是旧值（错误状态）
    assert_eq!(bubble.content_hash, initial_hash);
    // 调用 recompute_hash → content_hash 更新
    bubble.recompute_hash();
    assert_ne!(
        bubble.content_hash, initial_hash,
        "recompute_hash 后 content_hash 必须反映新 collapsed"
    );
    // 验证 recompute_hash 的结果与 compute_hash 一致
    let expected = TuiAssistantBubble::compute_hash(&bubble.text, bubble.reasoning.as_ref());
    assert_eq!(bubble.content_hash, expected);
}

#[test]
fn test_recompute_hash_no_reasoning_hashes_text_only() {
    let mut bubble = TuiAssistantBubble {
        text: "plain reply".into(),
        reasoning: None,
        content_hash: 0,
    };
    bubble.recompute_hash();
    let expected = TuiAssistantBubble::compute_hash(&bubble.text, None);
    assert_eq!(bubble.content_hash, expected);
}

// ── tui_impl_partial_eq! (content_hash excluded) ────────────────────

#[test]
fn test_user_bubble_partial_eq_ignores_content_hash() {
    let a = TuiUserBubble {
        text: "hi".into(),
        reminder: None,
        content_hash: 1,
    };
    let b = TuiUserBubble {
        text: "hi".into(),
        reminder: None,
        content_hash: 2,
    };
    assert_eq!(a, b, "content_hash 不同但其他字段相同 → 应相等");
}

#[test]
fn test_user_bubble_partial_eq_respects_text() {
    let a = TuiUserBubble {
        text: "hi".into(),
        reminder: None,
        content_hash: 0,
    };
    let b = TuiUserBubble {
        text: "ho".into(),
        reminder: None,
        content_hash: 0,
    };
    assert_ne!(a, b, "text 不同 → 应不等");
}

#[test]
fn test_assistant_bubble_partial_eq_ignores_content_hash() {
    let a = TuiAssistantBubble {
        text: "hello".into(),
        reasoning: None,
        content_hash: 42,
    };
    let b = TuiAssistantBubble {
        text: "hello".into(),
        reasoning: None,
        content_hash: 99,
    };
    assert_eq!(a, b);
}

#[test]
fn test_tool_card_partial_eq_ignores_content_hash() {
    let a = TuiToolCard {
        tool_id: "tc-1".into(),
        tool_name: "Edit".into(),
        input_summary: "path: foo".into(),
        output_summary: "done".into(),
        is_error: false,
        is_running: false,
        running_duration_ms: None,
        diff: None,
        tool_calls_count: 0,
        content_hash: 1,
    };
    let b = TuiToolCard {
        content_hash: 2,
        ..a.clone()
    };
    assert_eq!(a, b);
}

#[test]
fn test_tui_render_unit_subagent_group_construction() {
    let inner = TuiRenderUnit::TuiDivider(TuiDivider {
        label: Some("inner".into()),
        content_hash: tui_hash_str("inner"),
    });
    let vm = TuiRenderUnit::TuiSubAgentGroup(TuiSubAgentGroup {
        agent_id: "sa-1".into(),
        agent_name: "explorer".into(),
        view_models: im::Vector::from(vec![inner]),
        collapsed: true,
        is_running: false,
        content_hash: 0,
    });
    match &vm {
        TuiRenderUnit::TuiSubAgentGroup(data) => {
            assert_eq!(data.agent_name, "explorer");
            assert_eq!(data.view_models.len(), 1);
            assert!(data.collapsed);
        }
        _ => panic!("expected TuiSubAgentGroup"),
    }
}

#[test]
fn test_tui_render_unit_divider_no_label() {
    let vm = TuiRenderUnit::TuiDivider(TuiDivider {
        label: None,
        content_hash: 0,
    });
    match &vm {
        TuiRenderUnit::TuiDivider(data) => assert!(data.label.is_none()),
        _ => panic!("expected TuiDivider"),
    }
}

// ── reminder 检测 ────────────────────────────────────────────────────

mod reminder_tests {
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
        let info =
            detect_reminder(&format!("<system-reminder>{}</system-reminder>", long_line))
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
            content_hash: 0,
        };
        let b = TuiUserBubble {
            text: "hi".into(),
            reminder: None,
            content_hash: 0,
        };
        assert_ne!(a, b, "reminder 不同 → 应不等");
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
