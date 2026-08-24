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
        source: None,
        content_hash: 11,
    });
    assert_eq!(user.content_hash(), 11);
    let assistant = TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
        // [Slice 1] 正文时长（§6.2 `12.4s`）：测试构造默认无起点/冻结值。
        started_at: None,
        duration_ms: None,
        text: "a".into(),
        reasoning: None,
        message_id: None,
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
        completed_duration_ms: None,
        diff: None,
        presentation: TuiToolPresentation::Generic,
        fold: FoldState::Collapsed,
        user_modified: false,
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
    let h1 = TuiAssistantBubble::compute_hash("hello", None, 0, false);
    let h2 = TuiAssistantBubble::compute_hash("hello", None, 0, false);
    let h3 = TuiAssistantBubble::compute_hash("world", None, 0, false);
    assert_eq!(h1, h2, "相同 text 应有相同 hash");
    assert_ne!(h1, h3, "不同 text 应有不同 hash");
}

#[test]
fn test_compute_hash_includes_fold_state() {
    // [回归测试] Bug 2 修复的 Slice 2 演进：reasoning.fold 必须纳入 hash，
    // 否则按 hash 分片的渲染缓存命中旧值、折叠/展开后 UI 不刷新。
    let reasoning_open = TuiReasoningBlock {
        text: "thinking".into(),
        fold: FoldState::Expanded,
        status: EntryStatus::Completed,
        is_running: false,
        started_at: None,
        duration_ms: None,
    };
    let reasoning_collapsed = TuiReasoningBlock {
        text: "thinking".into(),
        fold: FoldState::Collapsed,
        status: EntryStatus::Completed,
        is_running: false,
        started_at: None,
        duration_ms: None,
    };
    let h_open = TuiAssistantBubble::compute_hash("reply", Some(&reasoning_open), 0, false);
    let h_collapsed =
        TuiAssistantBubble::compute_hash("reply", Some(&reasoning_collapsed), 0, false);
    assert_ne!(h_open, h_collapsed, "fold 状态变化时 content_hash 必须变化");
}

#[test]
fn test_compute_hash_includes_status_and_is_running() {
    // [G1] status / is_running 纳入 hash：生命周期翻转（Running→Completed）
    // 必须触发分片渲染缓存重建。
    let running = TuiReasoningBlock {
        text: "thinking".into(),
        fold: FoldState::Preview,
        status: EntryStatus::Running,
        is_running: true,
        started_at: None,
        duration_ms: None,
    };
    let completed = TuiReasoningBlock {
        text: "thinking".into(),
        fold: FoldState::Preview,
        status: EntryStatus::Completed,
        is_running: false,
        started_at: None,
        duration_ms: None,
    };
    let h_running = TuiAssistantBubble::compute_hash("reply", Some(&running), 0, false);
    let h_completed = TuiAssistantBubble::compute_hash("reply", Some(&completed), 0, false);
    assert_ne!(
        h_running, h_completed,
        "status 变化时 content_hash 必须变化"
    );
}

#[test]
fn test_compute_hash_includes_reasoning_text() {
    let r1 = TuiReasoningBlock {
        text: "thought A".into(),
        fold: FoldState::Collapsed,
        status: EntryStatus::Completed,
        is_running: false,
        started_at: None,
        duration_ms: None,
    };
    let r2 = TuiReasoningBlock {
        text: "thought B".into(),
        fold: FoldState::Collapsed,
        status: EntryStatus::Completed,
        is_running: false,
        started_at: None,
        duration_ms: None,
    };
    let h1 = TuiAssistantBubble::compute_hash("reply", Some(&r1), 0, false);
    let h2 = TuiAssistantBubble::compute_hash("reply", Some(&r2), 0, false);
    assert_ne!(h1, h2, "reasoning.text 变化时 content_hash 必须变化");
}

#[test]
fn test_recompute_hash_after_fold_change() {
    // [回归测试] 折叠 pass 修改 reasoning.fold 后必须调用 recompute_hash，
    // 否则缓存命中旧 hash 渲染不更新。
    let mut bubble = TuiAssistantBubble {
        // [Slice 1] 正文时长（§6.2 `12.4s`）：测试构造默认无起点/冻结值。
        started_at: None,
        duration_ms: None,
        text: "reply".into(),
        reasoning: Some(TuiReasoningBlock {
            text: "thinking".into(),
            fold: FoldState::Expanded,
            status: EntryStatus::Completed,
            is_running: false,
            started_at: None,
            duration_ms: None,
        }),
        message_id: Some("msg_1".into()),
        content_hash: 0,
    };
    bubble.content_hash =
        TuiAssistantBubble::compute_hash(&bubble.text, bubble.reasoning.as_ref(), 0, true);
    let initial_hash = bubble.content_hash;
    // 修改 fold 状态
    bubble.reasoning.as_mut().unwrap().fold = FoldState::Collapsed;
    // 不调用 recompute_hash → content_hash 仍是旧值（错误状态）
    assert_eq!(bubble.content_hash, initial_hash);
    // 调用 recompute_hash → content_hash 更新
    bubble.recompute_hash();
    assert_ne!(
        bubble.content_hash, initial_hash,
        "recompute_hash 后 content_hash 必须反映新 fold"
    );
    // 验证 recompute_hash 的结果与 compute_hash 一致
    let expected =
        TuiAssistantBubble::compute_hash(&bubble.text, bubble.reasoning.as_ref(), 0, true);
    assert_eq!(bubble.content_hash, expected);
}

#[test]
fn test_recompute_hash_no_reasoning_hashes_text_only() {
    let mut bubble = TuiAssistantBubble {
        // [Slice 1] 正文时长（§6.2 `12.4s`）：测试构造默认无起点/冻结值。
        started_at: None,
        duration_ms: None,
        text: "plain reply".into(),
        reasoning: None,
        message_id: None,
        content_hash: 0,
    };
    bubble.recompute_hash();
    let expected = TuiAssistantBubble::compute_hash(&bubble.text, None, 0, true);
    assert_eq!(bubble.content_hash, expected);
}

// ── 折叠状态机（spec §7 表）───────────────────────────────────────────────

#[test]
fn test_fold_for_status_matches_spec_table() {
    // spec §7 表原表逐项对照——折叠策略的唯一单点。
    use EntryStatus::*;
    use FoldState::*;
    use FoldTarget::*;
    // user / assistant 永远展开
    assert_eq!(fold_for_status(User, Running), Expanded);
    assert_eq!(fold_for_status(User, Completed), Expanded);
    assert_eq!(fold_for_status(Assistant, Running), Expanded);
    assert_eq!(fold_for_status(Assistant, Completed), Expanded);
    assert_eq!(fold_for_status(Assistant, Error), Expanded);
    // reasoning：Running → Preview / Completed → Collapsed / Error → Preview
    assert_eq!(fold_for_status(Reasoning, Running), Preview);
    assert_eq!(fold_for_status(Reasoning, Completed), Collapsed);
    assert_eq!(fold_for_status(Reasoning, Error), Preview);
    // tool：Preview → Collapsed → Expanded summary
    assert_eq!(fold_for_status(Tool, Running), Preview);
    assert_eq!(fold_for_status(Tool, Completed), Collapsed);
    assert_eq!(fold_for_status(Tool, Error), Expanded);
    // subagent：Collapsed + live summary → Collapsed → Expanded summary
    assert_eq!(fold_for_status(SubAgent, Running), Collapsed);
    assert_eq!(fold_for_status(SubAgent, Completed), Collapsed);
    assert_eq!(fold_for_status(SubAgent, Error), Expanded);
    // system：Collapsed → Collapsed → Expanded summary
    assert_eq!(fold_for_status(System, Running), Collapsed);
    assert_eq!(fold_for_status(System, Completed), Collapsed);
    assert_eq!(fold_for_status(System, Error), Expanded);
    // interaction：Expanded → Expanded（答毕完整展示，用户需求）→ Expanded
    assert_eq!(fold_for_status(Interaction, Running), Expanded);
    assert_eq!(fold_for_status(Interaction, Completed), Expanded);
    assert_eq!(fold_for_status(Interaction, Error), Expanded);
}

#[test]
fn test_fold_and_status_codes_deterministic_and_distinct() {
    // [G1] hash 代码必须确定性且互不相同——状态混淆会破坏缓存重建。
    assert_eq!(fold_state_code(FoldState::Collapsed), 1);
    assert_eq!(fold_state_code(FoldState::Preview), 2);
    assert_eq!(fold_state_code(FoldState::Expanded), 3);
    assert_eq!(entry_status_code(EntryStatus::Running), 1);
    assert_eq!(entry_status_code(EntryStatus::Completed), 2);
    assert_eq!(entry_status_code(EntryStatus::Error), 3);
}

#[test]
fn test_reasoning_collapsed_accessor_maps_fold() {
    // 渲染层经 collapsed() 访问器保持二元语义（Slice 3 前零视觉变化）。
    let collapsed = TuiReasoningBlock {
        text: "t".into(),
        fold: FoldState::Collapsed,
        status: EntryStatus::Completed,
        is_running: false,
        started_at: None,
        duration_ms: None,
    };
    let preview = TuiReasoningBlock {
        text: "t".into(),
        fold: FoldState::Preview,
        status: EntryStatus::Running,
        is_running: true,
        started_at: None,
        duration_ms: None,
    };
    let expanded = TuiReasoningBlock {
        text: "t".into(),
        fold: FoldState::Expanded,
        status: EntryStatus::Completed,
        is_running: false,
        started_at: None,
        duration_ms: None,
    };
    assert!(collapsed.collapsed());
    assert!(!preview.collapsed());
    assert!(!expanded.collapsed());
}

// ── tui_impl_partial_eq! (content_hash excluded) ────────────────────

#[test]
fn test_user_bubble_partial_eq_ignores_content_hash() {
    let a = TuiUserBubble {
        text: "hi".into(),
        reminder: None,
        source: None,
        content_hash: 1,
    };
    let b = TuiUserBubble {
        text: "hi".into(),
        reminder: None,
        source: None,
        content_hash: 2,
    };
    assert_eq!(a, b, "content_hash 不同但其他字段相同 → 应相等");
}

#[test]
fn test_user_bubble_partial_eq_respects_text() {
    let a = TuiUserBubble {
        text: "hi".into(),
        reminder: None,
        source: None,
        content_hash: 0,
    };
    let b = TuiUserBubble {
        text: "ho".into(),
        reminder: None,
        source: None,
        content_hash: 0,
    };
    assert_ne!(a, b, "text 不同 → 应不等");
}

#[test]
fn test_assistant_bubble_partial_eq_ignores_content_hash_but_keeps_message_id() {
    let a = TuiAssistantBubble {
        // [Slice 1] 正文时长（§6.2 `12.4s`）：测试构造默认无起点/冻结值。
        started_at: None,
        duration_ms: None,
        text: "hello".into(),
        reasoning: None,
        message_id: Some("msg_1".into()),
        content_hash: 42,
    };
    let b = TuiAssistantBubble {
        // [Slice 1] 正文时长（§6.2 `12.4s`）：测试构造默认无起点/冻结值。
        started_at: None,
        duration_ms: None,
        text: "hello".into(),
        reasoning: None,
        message_id: Some("msg_1".into()),
        content_hash: 99,
    };
    assert_eq!(a, b, "content_hash 不同但其他字段相同 → 应相等");
    // message_id 是身份字段——进 partial_eq（折叠覆盖键按它匹配）。
    let c = TuiAssistantBubble {
        // [Slice 1] 正文时长（§6.2 `12.4s`）：测试构造默认无起点/冻结值。
        started_at: None,
        duration_ms: None,
        text: "hello".into(),
        reasoning: None,
        message_id: Some("msg_2".into()),
        content_hash: 42,
    };
    assert_ne!(a, c, "message_id 不同 → 应不等");
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
        completed_duration_ms: None,
        diff: None,
        presentation: TuiToolPresentation::Generic,
        fold: FoldState::Collapsed,
        user_modified: false,
        tool_calls_count: 0,
        content_hash: 1,
    };
    let b = TuiToolCard {
        content_hash: 2,
        ..a.clone()
    };
    assert_eq!(a, b);
    // fold / user_modified 参与相等比较（渲染缓存之外的语义相等）。
    let c = TuiToolCard {
        fold: FoldState::Expanded,
        ..a.clone()
    };
    assert_ne!(a, c);
}

/// [G-Diff] TuiToolCard::recompute_hash 纳入 diff 稳定摘要：diff 变更
/// （path / change 数 / 截断）触发按 hash 分片的渲染缓存重建；diff=None 时
/// hash 与无 diff 卡片一致（diff_code()==0 不改变 hash）。
#[test]
fn test_tool_card_hash_includes_diff() {
    use crate::kit::tui_render_unit::{TuiDiffBlock, TuiHunk, TuiHunkLine, TuiHunkLineKind};

    let mk = |diff: Option<TuiDiffBlock>| {
        let mut card = TuiToolCard {
            tool_id: "tc-1".into(),
            tool_name: "Edit".into(),
            input_summary: "src/main.rs".into(),
            output_summary: "diff".into(),
            is_error: false,
            is_running: false,
            running_duration_ms: None,
            completed_duration_ms: None,
            diff,
            presentation: TuiToolPresentation::Generic,
            fold: FoldState::Collapsed,
            user_modified: false,
            tool_calls_count: 0,
            content_hash: 0,
        };
        card.recompute_hash();
        card.content_hash
    };
    let base = mk(None);
    let diff = TuiDiffBlock {
        path: "src/main.rs".into(),
        hunks: vec![TuiHunk {
            old_range: "-1,2".into(),
            new_range: "+1,3".into(),
            lines: vec![
                TuiHunkLine {
                    kind: TuiHunkLineKind::Del,
                    text: "a".into(),
                    old_no: Some(1),
                    new_no: None,
                },
                TuiHunkLine {
                    kind: TuiHunkLineKind::Add,
                    text: "b".into(),
                    old_no: None,
                    new_no: Some(1),
                },
            ],
            truncated_lines: 0,
        }],
        is_binary: false,
        is_too_large: false,
        is_new_file: false,
        more_change_lines: 0,
        adds: 1,
        dels: 1,
    };
    let with_diff = mk(Some(diff.clone()));
    assert_ne!(base, with_diff, "diff 摘要必须纳入 hash");

    // 跨 rebuild 稳定（同 diff 同 hash）
    assert_eq!(with_diff, mk(Some(diff.clone())), "diff 定型后 hash 稳定");

    // change 数变化 → hash 变化（顶层 adds 字段是计数事实源，同步更新）
    let mut diff2 = diff.clone();
    diff2.hunks[0].lines.push(TuiHunkLine {
        kind: TuiHunkLineKind::Add,
        text: "c".into(),
        old_no: None,
        new_no: Some(2),
    });
    diff2.adds = 2;
    assert_ne!(with_diff, mk(Some(diff2)), "change 数变化必须改 hash");

    // path 变化 → hash 变化
    let mut diff3 = diff.clone();
    diff3.path = "other.rs".into();
    assert_ne!(with_diff, mk(Some(diff3)), "path 变化必须改 hash");

    // 截断信息变化 → hash 变化
    let mut diff4 = diff.clone();
    diff4.more_change_lines = 3;
    assert_ne!(
        with_diff,
        mk(Some(diff4)),
        "more_change_lines 变化必须改 hash"
    );
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
        is_error: false,
        error_reason: None,
        fold: FoldState::Collapsed,
        user_modified: false,
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

/// [G1/cache] parent `is_error` 与可见 `error_reason` 必须参与 recompute_hash
/// 与 PartialEq——仅终态或错误文本变化也必须使按 hash 分片的渲染缓存失效。
#[test]
fn test_subagent_group_hash_and_eq_include_is_error_and_error_reason() {
    fn mk(is_error: bool, error_reason: Option<&str>) -> TuiSubAgentGroup {
        let mut g = TuiSubAgentGroup {
            agent_id: "sa-1".into(),
            agent_name: "explorer".into(),
            view_models: im::Vector::new(),
            collapsed: false,
            is_running: false,
            is_error,
            error_reason: error_reason.map(String::from),
            fold: FoldState::Collapsed,
            user_modified: false,
            content_hash: 0,
        };
        g.recompute_hash();
        g
    }

    let ok = mk(false, None);
    let err = mk(true, None);
    let err_reasoned = mk(true, Some("loop failed"));
    let err_reasoned2 = mk(true, Some("other reason"));

    // is_error 参与 hash/eq
    assert_ne!(ok.content_hash, err.content_hash, "is_error 必须改 hash");
    assert_ne!(ok, err, "is_error 必须参与 PartialEq");
    // error_reason 参与 hash/eq
    assert_ne!(
        err.content_hash, err_reasoned.content_hash,
        "error_reason 必须改 hash"
    );
    assert_ne!(err, err_reasoned, "error_reason 必须参与 PartialEq");
    assert_ne!(
        err_reasoned.content_hash, err_reasoned2.content_hash,
        "错误文本变化必须改 hash"
    );
    assert_ne!(
        err_reasoned, err_reasoned2,
        "错误文本变化必须参与 PartialEq"
    );
    // 同构 group 相等（hash 公式确定性）
    assert_eq!(ok, mk(false, None));
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

// ── Slice 1：空 reasoning 占位块 + 正文时长 hash（§6.2/§6.3）──────────────

/// [R6] 空 reasoning 占位块（§6.3）：running 空块 hash ≠ 无块（None）分支，
/// 且空块 hash 跨 rebuild 确定性稳定（无起点 → duration_code=0）。
#[test]
fn test_compute_hash_empty_reasoning_block_differs_from_none_and_stable() {
    let empty_running = TuiReasoningBlock {
        text: String::new(),
        fold: FoldState::Preview,
        status: EntryStatus::Running,
        is_running: true,
        started_at: None,
        duration_ms: None,
    };
    let h_with = TuiAssistantBubble::compute_hash("reply", Some(&empty_running), 0, false);
    let h_with_again = TuiAssistantBubble::compute_hash("reply", Some(&empty_running), 0, false);
    let h_none = TuiAssistantBubble::compute_hash("reply", None, 0, false);
    assert_eq!(h_with, h_with_again, "空块 hash 确定性稳定");
    assert_ne!(h_with, h_none, "空块与 None 分支 hash 各异");

    // 折叠 pass 翻转（Running→Completed/Collapsed）必须改变空块 hash
    let folded = TuiReasoningBlock {
        text: String::new(),
        fold: FoldState::Collapsed,
        status: EntryStatus::Completed,
        is_running: false,
        started_at: None,
        duration_ms: Some(0),
    };
    let h_folded = TuiAssistantBubble::compute_hash("reply", Some(&folded), 0, false);
    assert_ne!(h_with, h_folded, "空块状态翻转 hash 必须变化");
}

/// [G1] 正文时长秒数（None→0，秒取整）纳入 hash 三单点。
#[test]
fn test_compute_hash_includes_duration_secs() {
    let h0 = TuiAssistantBubble::compute_hash("reply", None, 0, false);
    let h5 = TuiAssistantBubble::compute_hash("reply", None, 5, false);
    assert_ne!(h0, h5, "duration_secs 变化时 hash 必须变化");

    // duration_secs() 口径：冻结值秒取整（12400ms → 12s）
    let mut bubble = TuiAssistantBubble {
        text: "reply".into(),
        reasoning: None,
        message_id: None,
        started_at: None,
        duration_ms: Some(12_400),
        content_hash: 0,
    };
    assert_eq!(bubble.duration_secs(), 12, "冻结时长秒取整");
    bubble.recompute_hash();
    assert_eq!(
        bubble.content_hash,
        TuiAssistantBubble::compute_hash("reply", None, 12, true),
        "recompute_hash 与 compute_hash 公式一致（G1，冻结形态 frozen=true）"
    );

    // running（started_at 有值）取已耗时；冻结后（started_at=None）取冻结值
    let mut running = TuiAssistantBubble {
        text: "reply".into(),
        reasoning: None,
        message_id: None,
        started_at: Some(std::time::Instant::now()),
        duration_ms: None,
        content_hash: 0,
    };
    let running_secs = running.duration_secs();
    running.started_at = None;
    running.duration_ms = Some(running_secs * 1000);
    assert_eq!(
        running.duration_secs(),
        running_secs,
        "冻结后秒数与 running 一致"
    );
}

/// [G1 回归] 冻结判别位：running→frozen 翻转在同一秒内落地时 `duration_secs`
/// 数值不变（渲染内容却不同——§6.2 `12.4s` meta 出现），hash 必须区分——
/// 否则按 hash 分片的渲染缓存持续供应运行中（无 meta）的旧帧。
#[test]
fn test_compute_hash_frozen_discriminator() {
    // 同一秒数值、不同形态：hash 必须不同
    let running_form = TuiAssistantBubble::compute_hash("reply", None, 12, false);
    let frozen_form = TuiAssistantBubble::compute_hash("reply", None, 12, true);
    assert_ne!(
        running_form, frozen_form,
        "同秒冻结翻转（duration 数值不变）hash 必须变化"
    );

    // recompute_hash 与公式同口径：started_at.is_none() → frozen=true
    let mut running = TuiAssistantBubble {
        text: "reply".into(),
        reasoning: None,
        message_id: None,
        started_at: Some(std::time::Instant::now()),
        duration_ms: None,
        content_hash: 0,
    };
    running.recompute_hash();
    let running_hash = running.content_hash;
    // 折叠 pass 冻结：started_at → duration_ms（同一秒内模拟）
    running.started_at = None;
    running.duration_ms = Some(12_000);
    running.recompute_hash();
    assert_ne!(
        running_hash, running.content_hash,
        "running→frozen 翻转（即使同秒）hash 必须变化（缓存失效依据）"
    );
}

/// [D2][G1] TuiCollapsedGroup hash 含 failed_count：变化必须触发分片缓存重建；
/// 相同内容 recompute 稳定（跨 rebuild hash 不变）。
#[test]
fn test_collapsed_group_hash_includes_failed_count() {
    let base = || TuiCollapsedGroup {
        title: "Read 2".into(),
        count: 2,
        failed_count: 0,
        view_models: vec![],
        content_hash: 0,
    };
    let mut a = base();
    a.recompute_hash();
    let mut b = base();
    b.failed_count = 1;
    b.recompute_hash();
    assert_ne!(
        a.content_hash, b.content_hash,
        "failed_count 必须纳入组 hash（G1）"
    );
    // 稳定性：同内容重复 recompute 结果一致
    let mut c = base();
    c.recompute_hash();
    assert_eq!(a.content_hash, c.content_hash, "组 hash 跨 rebuild 稳定");
    // 与 count 区别：count 变化也改变 hash（既有语义保留）
    let mut d = base();
    d.count = 3;
    d.recompute_hash();
    assert_ne!(a.content_hash, d.content_hash);
}

// ── [Slice 4] TuiAskUserBlock：InteractionKind + pending/options/result hash ──

fn ask_user_block_base() -> TuiAskUserBlock {
    TuiAskUserBlock {
        items: vec![],
        is_error: false,
        kind: InteractionKind::Permission,
        pending: true,
        verb: "Bash".into(),
        question: "Bash wants to run: cargo test".into(),
        options: vec!["Allow once".into(), "Deny".into()],
        result: None,
        request_id: Some("rid-1".into()),
        question_ids: vec![],
        fold: FoldState::Expanded,
        user_modified: false,
        content_hash: 0,
    }
}

/// [G1][Slice 4] interaction block hash：pending 翻转 / options / result 变化
/// 必须触发分片缓存重建；同内容 recompute 稳定；kind 区分。
#[test]
fn test_ask_user_block_hash_includes_pending_options_result() {
    let mut a = ask_user_block_base();
    a.recompute_hash();

    // pending → completed（结果回写）必须改变 hash
    let mut b = ask_user_block_base();
    b.pending = false;
    b.result = Some("Allowed once".into());
    b.fold = FoldState::Collapsed;
    b.recompute_hash();
    assert_ne!(
        a.content_hash, b.content_hash,
        "pending/result 变化必须进 hash"
    );

    // options 变化（不同选项集）改变 hash
    let mut c = ask_user_block_base();
    c.options = vec!["Deny".into()];
    c.recompute_hash();
    assert_ne!(a.content_hash, c.content_hash, "options 必须进 hash");

    // kind 区分（AskUser vs Permission）
    let mut d = ask_user_block_base();
    d.kind = InteractionKind::AskUser;
    d.recompute_hash();
    assert_ne!(a.content_hash, d.content_hash, "kind 必须进 hash");

    // 稳定性：同内容重复 recompute 结果一致
    let mut e = ask_user_block_base();
    e.recompute_hash();
    assert_eq!(a.content_hash, e.content_hash, "block hash 跨 rebuild 稳定");

    // request_id 是身份字段，不进 hash（同 message_id 先例）
    let mut f = ask_user_block_base();
    f.request_id = Some("rid-other".into());
    f.recompute_hash();
    assert_eq!(
        a.content_hash, f.content_hash,
        "request_id 不进 content_hash"
    );

    let mut g = ask_user_block_base();
    g.question_ids = vec!["q1".into()];
    g.recompute_hash();
    assert_eq!(
        a.content_hash, g.content_hash,
        "question_ids 不进可见内容 hash"
    );
    assert_ne!(a, g, "question_ids 参与结构相等比较");
}

/// [Slice 4] partial_eq：身份字段（request_id）参与相等比较、content_hash 忽略。
#[test]
fn test_ask_user_block_partial_eq_ignores_hash_keeps_request_id() {
    let mut a = ask_user_block_base();
    a.recompute_hash();
    let mut b = ask_user_block_base();
    b.recompute_hash();
    assert_eq!(a, b, "相同内容（含 request_id）相等");

    // content_hash 不同但内容相同 → 相等（partial_eq 忽略 hash）
    let mut c = ask_user_block_base();
    c.content_hash = 999;
    assert_eq!(a, c, "content_hash 不参与相等比较");

    // request_id 不同 → 不相等（身份字段）
    let mut d = ask_user_block_base();
    d.request_id = Some("other".into());
    assert_ne!(a, d, "request_id 参与相等比较");

    // pending/result 不同 → 不相等
    let mut e = ask_user_block_base();
    e.pending = false;
    assert_ne!(a, e, "pending 参与相等比较");
}

/// [Slice 4] FoldKey::Interaction 键控与 Hash/PartialEq。
#[test]
fn test_fold_key_interaction_roundtrip() {
    let k = FoldKey::Interaction("rid-9".into());
    assert_eq!(k, FoldKey::Interaction("rid-9".into()));
    assert_ne!(k, FoldKey::Interaction("other".into()));
    assert_ne!(k, FoldKey::Tool("rid-9".into()), "不同变体不相等");
}
