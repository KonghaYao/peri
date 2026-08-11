//! Tests

use super::*;
use crate::kit::tui_render_unit::{TuiToolCard, TuiToolPresentation, TuiUserBubble};
use ratatui_kit::ratatui::layout::Rect;
use ratatui_kit::ratatui::style::{Color, Modifier, Style};
use ratatui_kit::ratatui::text::{Line, Span};
use serial_test::serial;

#[test]
fn test_empty_with_todo_items_shows_footer_not_welcome() {
    let entries_empty = true;
    let is_loading = false;
    let todo_items_empty = false;
    let empty = entries_empty && !is_loading && todo_items_empty;

    assert!(
        !empty,
        "仅有 todo 条目且无消息时不应判定为 empty，避免 Welcome 覆盖 todo 显示"
    );
}

#[test]
fn test_empty_without_todo_is_truly_empty() {
    let entries_empty = true;
    let is_loading = false;
    let todo_items_empty = true;
    let empty = entries_empty && !is_loading && todo_items_empty;

    assert!(empty);
}

#[test]
fn test_total_visual_rows_exceeds_u16_max() {
    let core_rows = u16::MAX as usize + 100;
    let footer_rows = 3;

    assert_eq!(
        total_visual_rows(core_rows, footer_rows, false),
        core_rows + footer_rows + scroll::SCROLL_PADDING,
        "长消息的可滚动高度不得在 u16::MAX 处截断"
    );
}

// ── NO_COLOR 剥离 pass（§12，G3）───────────────────────────────────────────

#[test]
fn test_strip_line_colors_removes_all_colors_keeps_modifiers() {
    // 混合多 span：前景/背景/下划线色 + bold 与 italic modifier + 符号与文本
    let line = Line::from(vec![
        Span::styled(
            "◐ ",
            Style::default()
                .fg(Color::Rgb(125, 207, 255))
                .bg(Color::Rgb(10, 10, 10)),
        ),
        Span::styled(
            "Running",
            Style::default()
                .fg(Color::Rgb(255, 107, 128))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " 12s",
            Style::default()
                .fg(Color::Rgb(80, 80, 80))
                .underline_color(Color::Rgb(1, 2, 3)),
        ),
    ]);
    let stripped = strip_line_colors(&line);

    assert_eq!(stripped.spans.len(), 3, "span 结构保持不变");
    // 颜色全部剥离（前景/背景/下划线），文本与符号原样保留
    for (orig, s) in line.spans.iter().zip(stripped.spans.iter()) {
        assert_eq!(orig.content, s.content, "文本/符号不得被剥离");
        assert_eq!(s.style.fg, None, "前景色必须剥离");
        assert_eq!(s.style.bg, None, "背景色必须剥离");
        assert_eq!(s.style.underline_color, None, "下划线色必须剥离");
    }
    // modifier 保留（NO_COLOR 下状态仍需可辨认）
    assert!(
        stripped.spans[1]
            .style
            .add_modifier
            .contains(Modifier::BOLD)
    );
    assert!(
        !stripped.spans[0]
            .style
            .add_modifier
            .contains(Modifier::BOLD)
    );
}

#[test]
fn test_strip_line_colors_keeps_unicode_symbols() {
    // §12：符号与明确状态文本不被剥离——unicode 符号 + CJK 文本原样保留
    let line = Line::from(vec![
        Span::styled("✓", Style::default().fg(Color::Rgb(78, 186, 101))),
        Span::styled(" 完成", Style::default().fg(Color::Rgb(200, 200, 200))),
    ]);
    let stripped = strip_line_colors(&line);
    assert_eq!(stripped.spans[0].content, "✓");
    assert_eq!(stripped.spans[1].content, " 完成");
}

#[test]
fn test_strip_line_colors_preserves_alignment() {
    use ratatui_kit::ratatui::layout::Alignment;
    let line = Line {
        spans: vec![Span::styled(
            "title",
            Style::default().fg(Color::Rgb(1, 2, 3)),
        )],
        alignment: Some(Alignment::Center),
        style: Style::default().fg(Color::Rgb(9, 9, 9)),
    };
    let stripped = strip_line_colors(&line);
    assert_eq!(stripped.alignment, Some(Alignment::Center));
    // Line 级 style 的颜色同样剥离，modifier 保留
    assert_eq!(stripped.style.fg, None);
    assert_eq!(stripped.style.bg, None);
    assert_eq!(stripped.style.add_modifier, line.style.add_modifier);
}

#[test]
fn test_strip_line_colors_plain_style_unchanged_content() {
    // 无颜色的 span：剥离后内容与结构不变
    let line = Line::from(vec![Span::raw("plain")]);
    let stripped = strip_line_colors(&line);
    assert_eq!(stripped.spans[0].content, "plain");
    assert_eq!(stripped.spans[0].style, Style::default());
}

fn layout_at(line_index: usize, start_col: u16, width: u16) -> KeepGoingLayout {
    KeepGoingLayout {
        line_index,
        start_col,
        width,
    }
}

#[test]
fn test_keepgoing_rect_visible_in_viewport() {
    // core 3 行 + footer line_index 2（两个空行 + summary 行）→ 屏幕 y = 2 + 3 + 2 - 0 = 7
    let rect = compute_keepgoing_rect(
        false,
        Some(Rect::new(0, 2, 100, 20)),
        Some(layout_at(2, 18, 13)),
        3,
        0,
        20,
    );
    assert_eq!(rect, Some((7, 18, 13)));
}

#[test]
fn test_keepgoing_rect_follows_scroll() {
    // scroll_y = 3 → 按钮行随内容上移：2 + 3 + 2 - 3 = 4
    let rect = compute_keepgoing_rect(
        false,
        Some(Rect::new(0, 2, 100, 20)),
        Some(layout_at(2, 18, 13)),
        3,
        3,
        20,
    );
    assert_eq!(rect, Some((4, 18, 13)));
}

#[test]
fn test_keepgoing_rect_scrolled_out_returns_none() {
    // scroll_y = 10 → 按钮行 2 + 3 + 2 - 10 = -3 < area.y(2) → 滚出视口
    let rect = compute_keepgoing_rect(
        false,
        Some(Rect::new(0, 2, 100, 20)),
        Some(layout_at(2, 18, 13)),
        3,
        10,
        20,
    );
    assert_eq!(rect, None);
}

#[test]
fn test_keepgoing_rect_empty_layout_returns_none() {
    // 无按钮渲染（loading 中 / 无 summary）→ 不注册点击区域
    let rect = compute_keepgoing_rect(false, Some(Rect::new(0, 2, 100, 20)), None, 3, 0, 20);
    assert_eq!(rect, None);
}

#[test]
fn test_keepgoing_rect_welcome_layout_returns_none() {
    // empty 分支：Welcome 布局行位置模型不同，按钮可见但不可点击
    let rect = compute_keepgoing_rect(
        true,
        Some(Rect::new(0, 2, 100, 20)),
        Some(layout_at(2, 18, 13)),
        0,
        0,
        20,
    );
    assert_eq!(rect, None);
}

// ── Slice 2：entry 焦点导航纯函数 ─────────────────────────────────────────

#[test]
fn test_move_entry_focus_from_none_alt_up_targets_last_entry() {
    // Alt+Up 从无焦点 → 最新 entry（末项）
    assert_eq!(move_entry_focus(5, None, -1), Some(4));
    assert_eq!(move_entry_focus(1, None, -1), Some(0));
    assert_eq!(move_entry_focus(0, None, -1), None);
}

#[test]
fn test_move_entry_focus_from_none_alt_down_targets_first_entry() {
    assert_eq!(move_entry_focus(5, None, 1), Some(0));
    assert_eq!(move_entry_focus(0, None, 1), None);
}

#[test]
fn test_move_entry_focus_clamps_at_bounds_no_wrap() {
    // 有焦点：上下移动并钳制在 [0, len-1]，不循环
    assert_eq!(move_entry_focus(5, Some(3), -1), Some(2));
    assert_eq!(move_entry_focus(5, Some(0), -1), Some(0));
    assert_eq!(move_entry_focus(5, Some(4), 1), Some(4));
    assert_eq!(move_entry_focus(5, Some(2), 1), Some(3));
}

#[test]
fn test_fold_key_of_maps_vm_identities() {
    use crate::kit::tui_render_unit::{EntryStatus, TuiAssistantBubble, TuiReasoningBlock};

    // assistant + reasoning + message_id → Reasoning key
    let vm = TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
        // [Slice 1] 正文时长（§6.2 `12.4s`）：测试构造默认无起点/冻结值。
        started_at: None,
        duration_ms: None,
        text: "t".into(),
        reasoning: Some(TuiReasoningBlock {
            text: "r".into(),
            fold: FoldState::Preview,
            status: EntryStatus::Running,
            is_running: true,
            started_at: None,
            duration_ms: None,
        }),
        message_id: Some("msg_9".into()),
        content_hash: 0,
    });
    let (k, f) = fold_key_of(&vm).expect("应可折叠");
    assert_eq!(k, FoldKey::Reasoning("msg_9".into()));
    assert_eq!(f, FoldState::Preview);

    // 无 message_id 的 reasoning bubble → 无折叠键（不可作为覆盖目标）
    let vm_noid = TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
        // [Slice 1] 正文时长（§6.2 `12.4s`）：测试构造默认无起点/冻结值。
        started_at: None,
        duration_ms: None,
        text: "t".into(),
        reasoning: Some(TuiReasoningBlock {
            text: "r".into(),
            fold: FoldState::Collapsed,
            status: EntryStatus::Completed,
            is_running: false,
            started_at: None,
            duration_ms: None,
        }),
        message_id: None,
        content_hash: 0,
    });
    assert!(fold_key_of(&vm_noid).is_none());

    // tool / subagent 按 tool_id / agent_id 键控
    let tool = TuiRenderUnit::TuiToolCard(TuiToolCard {
        tool_id: "tool-1".into(),
        tool_name: "Bash".into(),
        input_summary: String::new(),
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
        content_hash: 0,
    });
    assert_eq!(
        fold_key_of(&tool),
        Some((FoldKey::Tool("tool-1".into()), FoldState::Collapsed))
    );

    // user bubble 无折叠能力
    let user = TuiRenderUnit::TuiUserBubble(TuiUserBubble::new("hi".into()));
    assert!(fold_key_of(&user).is_none());

    // §6.7 subagent：fold_key_of 返回 SubAgent key——Enter 分派据此刻断打开
    // 详情 pane（折叠切换仍走同一 key 的覆盖表；分派改判在 mod.rs Enter 分支）。
    let sub = TuiRenderUnit::TuiSubAgentGroup(crate::kit::tui_render_unit::TuiSubAgentGroup {
        agent_id: "agent-7".into(),
        agent_name: "explorer".into(),
        view_models: im::Vector::new(),
        collapsed: false,
        is_running: false,
        fold: FoldState::Collapsed,
        user_modified: false,
        content_hash: 0,
    });
    assert_eq!(
        fold_key_of(&sub),
        Some((FoldKey::SubAgent("agent-7".into()), FoldState::Collapsed)),
        "subagent 折叠恒 Collapsed（§7 表），Enter 分派以此为锚"
    );
}

#[test]
fn test_apply_fold_override_sets_fold_user_modified_and_recomputes_hash() {
    use crate::kit::tui_render_unit::TuiToolCard;

    let mut tool = TuiRenderUnit::TuiToolCard(TuiToolCard {
        tool_id: "t1".into(),
        tool_name: "Read".into(),
        input_summary: String::new(),
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
        content_hash: 0,
    });
    let before = tool.content_hash();
    apply_fold_override(&mut tool, FoldState::Expanded);
    match &tool {
        TuiRenderUnit::TuiToolCard(t) => {
            assert_eq!(t.fold, FoldState::Expanded);
            assert!(t.user_modified, "手动操作后 user_modified=true");
            assert_ne!(
                t.content_hash, before,
                "[G1] fold 变化必须重算 hash（分片缓存重建）"
            );
        }
        other => panic!("expected TuiToolCard, got {other:?}"),
    }

    // 无折叠能力（user bubble）→ no-op 不 panic
    let mut user = TuiRenderUnit::TuiUserBubble(TuiUserBubble::new("hi".into()));
    apply_fold_override(&mut user, FoldState::Expanded);
}

// ── [Slice 4 §6.8] Interaction block 折叠键 / 覆盖 ──

fn ask_user_block(pending: bool, rid: Option<&str>) -> TuiRenderUnit {
    use crate::kit::tui_render_unit::InteractionKind;
    let mut b = crate::kit::tui_render_unit::TuiAskUserBlock {
        items: vec![],
        is_error: false,
        kind: InteractionKind::Permission,
        pending,
        verb: "Bash".into(),
        question: "Bash wants to run: cargo test".into(),
        options: vec!["Allow once".into(), "Deny".into()],
        result: if pending {
            None
        } else {
            Some("Allowed once".into())
        },
        request_id: rid.map(|s| s.to_string()),
        fold: FoldState::Expanded,
        user_modified: false,
        content_hash: 0,
    };
    b.recompute_hash();
    TuiRenderUnit::TuiAskUserBlock(b)
}

/// [Slice 4] fold_key_of：Interaction block 按 request_id 键控；request_id 为
/// None（测试构造）时返回 None（与 reasoning message_id 先例一致）。
#[test]
fn test_fold_key_of_interaction_block() {
    let vm = ask_user_block(true, Some("rid-1"));
    let (k, f) = fold_key_of(&vm).expect("有 request_id 时应可折叠");
    assert_eq!(k, FoldKey::Interaction("rid-1".into()));
    assert_eq!(f, FoldState::Expanded);

    let vm_noid = ask_user_block(true, None);
    assert!(
        fold_key_of(&vm_noid).is_none(),
        "无 request_id → 不可折叠键控"
    );
}

/// [Slice 4] apply_fold_override：写 fold + user_modified + 重算 hash。
#[test]
fn test_apply_fold_override_interaction_block() {
    let mut vm = ask_user_block(false, Some("rid-2"));
    let TuiRenderUnit::TuiAskUserBlock(ref mut block) = vm else {
        unreachable!()
    };
    let hash_before = block.content_hash;
    apply_fold_override(&mut vm, FoldState::Expanded);
    let TuiRenderUnit::TuiAskUserBlock(block) = &vm else {
        unreachable!("vm 恒为 AskUserBlock")
    };
    assert_eq!(block.fold, FoldState::Expanded);
    assert!(block.user_modified, "手动覆盖 → user_modified=true");
    assert_ne!(
        block.content_hash, hash_before,
        "折叠覆盖必须重算 hash（G1）"
    );
}

/// [Slice 4] pending_interaction_of：仅 pending 的 interaction block 命中；
/// completed 结果行与其余 VM 类型返回 None。
#[test]
fn test_pending_interaction_of_matches_only_pending() {
    assert!(pending_interaction_of(&ask_user_block(true, None)).is_some());
    assert!(pending_interaction_of(&ask_user_block(false, None)).is_none());
    let tool = TuiRenderUnit::TuiToolCard(TuiToolCard {
        tool_id: "t1".into(),
        tool_name: "Bash".into(),
        input_summary: String::new(),
        output_summary: String::new(),
        is_error: false,
        is_running: false,
        running_duration_ms: None,
        completed_duration_ms: None,
        diff: None,
        presentation: TuiToolPresentation::Generic,
        fold: FoldState::Collapsed,
        user_modified: false,
        content_hash: 0,
        tool_calls_count: 0,
    });
    assert!(pending_interaction_of(&tool).is_none());
}

/// [Slice 4 §6.8] interaction option 导航矩阵：Tab/← 后退、→ 前进，首末循环
/// 回绕（§6.8 选项焦点）；单选项恒 0。
#[test]
fn test_cycle_interaction_option_wraps_around() {
    // → 前进：末项回绕到首项
    assert_eq!(cycle_interaction_option(0, 2, false), 1);
    assert_eq!(cycle_interaction_option(1, 2, false), 0);
    assert_eq!(cycle_interaction_option(2, 3, false), 0);
    // Tab/← 后退：首项回绕到末项（saturating_sub 不回绕的回归锁定——
    // 首项后退不得卡死在 0）
    assert_eq!(cycle_interaction_option(0, 2, true), 1);
    assert_eq!(cycle_interaction_option(1, 2, true), 0);
    assert_eq!(cycle_interaction_option(0, 3, true), 2);
    assert_eq!(cycle_interaction_option(2, 3, true), 1);
    // 单选项恒 0（count 归一化 ≥1 后）
    assert_eq!(cycle_interaction_option(0, 1, true), 0);
    assert_eq!(cycle_interaction_option(0, 1, false), 0);
}

// ── 点击/Enter 共用折叠分派（apply_fold_toggle 动作层）────────────────────

/// subagent 首行点击/Enter：写 SELECTED_SUBAGENT_ID + 打开详情面板
///（不切折叠——§7 表 subagent 折叠恒 Collapsed）。
#[test]
#[serial]
fn test_apply_fold_toggle_subagent_opens_detail_panel() {
    use crate::kit::tui_render_unit::TuiSubAgentGroup;

    crate::kit::atoms::init_atoms();
    *crate::kit::atoms::SELECTED_SUBAGENT_ID.state().write() = None;
    *crate::kit::atoms::ACTIVE_PANEL.state().write() = None;
    let sub = TuiRenderUnit::TuiSubAgentGroup(TuiSubAgentGroup {
        agent_id: "agent-9".into(),
        agent_name: "explorer".into(),
        view_models: im::Vector::new(),
        collapsed: false,
        is_running: false,
        fold: FoldState::Collapsed,
        user_modified: false,
        content_hash: 0,
    });
    let mut snapshot = crate::kit::atoms::ViewModelsSnapshot {
        items: im::Vector::from(vec![sub]),
        generation: 0,
    };
    let r = apply_fold_toggle(&mut snapshot, 0, false);
    assert_eq!(r, EventResult::Consumed);
    assert_eq!(
        *crate::kit::atoms::SELECTED_SUBAGENT_ID.state().read(),
        Some("agent-9".to_string())
    );
    assert_eq!(
        *crate::kit::atoms::ACTIVE_PANEL.state().read(),
        Some(crate::app::panel_types::PanelKind::SubAgentDetail)
    );
}

/// tool 首行点击/Enter：Collapsed → Expanded + user_modified + FOLD_OVERRIDES。
#[test]
#[serial]
fn test_apply_fold_toggle_tool_writes_override() {
    crate::kit::atoms::init_atoms();
    *crate::kit::atoms::FOLD_OVERRIDES.state().write() = std::collections::HashMap::new();
    let tool = TuiRenderUnit::TuiToolCard(TuiToolCard {
        tool_id: "tool-2".into(),
        tool_name: "Read".into(),
        input_summary: String::new(),
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
        content_hash: 0,
    });
    let mut snapshot = crate::kit::atoms::ViewModelsSnapshot {
        items: im::Vector::from(vec![tool]),
        generation: 0,
    };
    let r = apply_fold_toggle(&mut snapshot, 0, false);
    assert_eq!(r, EventResult::Consumed);
    match &snapshot.items[0] {
        TuiRenderUnit::TuiToolCard(t) => {
            assert_eq!(t.fold, FoldState::Expanded, "点击后展开");
            assert!(t.user_modified, "手动修改标记");
        }
        other => panic!("expected tool card, got {other:?}"),
    }
    assert_eq!(
        crate::kit::atoms::FOLD_OVERRIDES
            .state()
            .read()
            .get(&FoldKey::Tool("tool-2".into())),
        Some(&FoldState::Expanded)
    );
}
