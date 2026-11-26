use super::FOLD_OVERRIDES;
use super::FoldKey;
use super::FoldState;
use super::TuiRenderUnit;
use super::TuiSystemReminder;
use super::TuiToolCard;
use super::TuiToolPresentation;
use super::TuiUserBubble;
use crate::kit::message_area::entry_nav;
use crate::kit::tui_render_unit::TuiCollapsedGroup;
use ratatui_kit::prelude::*;

// ── Slice 2：entry 焦点导航纯函数 ─────────────────────────────────────────

#[test]
fn test_move_entry_focus_from_none_alt_up_targets_last_entry() {
    // Alt+Up 从无焦点 → 最新 entry（末项）
    assert_eq!(entry_nav::move_entry_focus(5, None, -1), Some(4));
    assert_eq!(entry_nav::move_entry_focus(1, None, -1), Some(0));
    assert_eq!(entry_nav::move_entry_focus(0, None, -1), None);
}

#[test]
fn test_move_entry_focus_from_none_alt_down_targets_first_entry() {
    assert_eq!(entry_nav::move_entry_focus(5, None, 1), Some(0));
    assert_eq!(entry_nav::move_entry_focus(0, None, 1), None);
}

#[test]
fn test_move_entry_focus_clamps_at_bounds_no_wrap() {
    // 有焦点：上下移动并钳制在 [0, len-1]，不循环
    assert_eq!(entry_nav::move_entry_focus(5, Some(3), -1), Some(2));
    assert_eq!(entry_nav::move_entry_focus(5, Some(0), -1), Some(0));
    assert_eq!(entry_nav::move_entry_focus(5, Some(4), 1), Some(4));
    assert_eq!(entry_nav::move_entry_focus(5, Some(2), 1), Some(3));
}

#[test]
fn test_fold_key_of_maps_vm_identities() {
    use crate::kit::tui_render_unit::{EntryStatus, TuiAssistantBubble, TuiReasoningBlock};

    // assistant + reasoning + message_id → Reasoning key
    let vm = TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
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
        }
        .into(),
    );
    assert_eq!(FoldKey::of(&vm), Some(FoldKey::Reasoning("msg_9".into())));
    assert_eq!(
        crate::kit::tui_render_unit::fold_state_of(&vm),
        Some(FoldState::Preview)
    );

    // 无 message_id 的 reasoning bubble → 无折叠键：不能写覆盖，正文在渲染侧
    // 保持可见（流式 Preview / 完成 Expanded），不做假身份键控。
    let vm_noid = TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
            // [Slice 1] 正文时长（§6.2 `12.4s`）：测试构造默认无起点/冻结值。
            started_at: None,
            duration_ms: None,
            text: "t".into(),
            reasoning: Some(TuiReasoningBlock {
                text: "r".into(),
                fold: FoldState::Expanded,
                status: EntryStatus::Completed,
                is_running: false,
                started_at: None,
                duration_ms: None,
            }),
            message_id: None,
            content_hash: 0,
        }
        .into(),
    );
    assert_eq!(FoldKey::of(&vm_noid), None);
    assert_eq!(
        crate::kit::tui_render_unit::fold_state_of(&vm_noid),
        None,
        "不可键控的 entry 没有可读 fold 语义"
    );

    // tool / subagent 按 tool_id / agent_id 键控
    let tool = TuiRenderUnit::TuiToolCard(TuiToolCard {
        images: Vec::new(),
        images_omitted: 0,
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
    assert_eq!(FoldKey::of(&tool), Some(FoldKey::Tool("tool-1".into())));
    assert_eq!(
        crate::kit::tui_render_unit::fold_state_of(&tool),
        Some(FoldState::Collapsed)
    );

    let reminder = TuiSystemReminder::legacy("maintenance notice".into());
    let reminder_id = reminder.reminder_id;
    assert_eq!(
        FoldKey::of(&TuiRenderUnit::TuiSystemReminder(reminder)),
        Some(FoldKey::SystemReminder(reminder_id))
    );

    // user bubble 无折叠能力
    let user = TuiRenderUnit::TuiUserBubble(TuiUserBubble::new("hi".into()));
    assert_eq!(FoldKey::of(&user), None);

    // §6.7 subagent：FoldKey::of 返回 SubAgent key——Enter 分派据此刻断打开
    // 详情 pane（折叠切换仍走同一 key 的覆盖表；分派改判在 mod.rs Enter 分支）。
    let sub = TuiRenderUnit::TuiSubAgentGroup(crate::kit::tui_render_unit::TuiSubAgentGroup {
        instance_id: "instance-agent-7".into(),
        agent_id: "agent-7".into(),
        agent_name: "explorer".into(),
        view_models: im::Vector::new(),
        collapsed: false,
        preview_omitted: false,
        is_running: false,
        is_error: false,
        error_reason: None,
        fold: FoldState::Collapsed,
        user_modified: false,
        content_hash: 0,
    });
    assert_eq!(
        FoldKey::of(&sub),
        Some(FoldKey::SubAgent("instance-agent-7".into())),
        "subagent 折叠恒 Collapsed（§7 表），Enter 分派以此为锚"
    );
    assert_eq!(
        crate::kit::tui_render_unit::fold_state_of(&sub),
        Some(FoldState::Collapsed)
    );
}

/// 无 message_id 的 reasoning 不能写覆盖键：完成后正文保持完整可见，
/// Enter 消费但不切换（不伪造身份键，也不把无法展开的正文藏起来）。
#[test]
#[serial_test::serial]
fn test_fold_toggle_keeps_unkeyed_reasoning_visible() {
    use crate::kit::tui_render_unit::{EntryStatus, TuiAssistantBubble, TuiReasoningBlock};

    crate::kit::atoms::init_atoms();
    FOLD_OVERRIDES.state().write().clear();
    let unit = TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
            started_at: None,
            duration_ms: None,
            text: "回复".into(),
            reasoning: Some(TuiReasoningBlock {
                text: "历史思考".into(),
                fold: FoldState::Expanded,
                status: EntryStatus::Completed,
                is_running: false,
                started_at: None,
                duration_ms: None,
            }),
            message_id: None,
            content_hash: 0,
        }
        .into(),
    );
    let mut snapshot = crate::kit::atoms::ViewModelsSnapshot {
        items: im::Vector::from(vec![unit]),
        generation: 1,
    };

    assert_eq!(
        entry_nav::apply_fold_toggle(&mut snapshot, 0, false),
        EventResult::Consumed
    );
    let TuiRenderUnit::TuiAssistantBubble(updated) = &snapshot.items[0] else {
        panic!("expected TuiAssistantBubble");
    };
    assert_eq!(
        updated.reasoning.as_ref().map(|r| r.fold),
        Some(FoldState::Expanded),
        "无身份时保持正文可见"
    );
    assert!(
        FOLD_OVERRIDES.state().read().is_empty(),
        "不得为无身份块写覆盖"
    );
}

/// 运行中 reasoning（ACP message_id 缺失，使用本地身份）可展开；渲染结果
/// 超过 Preview 的 4 行 tail，证明早期正文可被逐项展开阅读。
#[test]
#[serial_test::serial]
fn test_running_reasoning_local_identity_expands_beyond_preview() {
    use crate::kit::message_area::grid::GridSpec;
    use crate::kit::message_area::render::vm_to_lines;
    use crate::kit::tui_render_unit::{EntryStatus, TuiAssistantBubble, TuiReasoningBlock};

    crate::kit::atoms::init_atoms();
    crate::i18n::init(Some("en"));
    FOLD_OVERRIDES.state().write().clear();
    let reasoning_text = (1..=8)
        .map(|i| format!("思考行{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let unit = TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
            started_at: None,
            duration_ms: None,
            text: String::new(),
            reasoning: Some(TuiReasoningBlock {
                text: reasoning_text,
                fold: FoldState::Collapsed,
                status: EntryStatus::Running,
                is_running: true,
                started_at: Some(std::time::Instant::now()),
                duration_ms: None,
            }),
            message_id: Some(crate::kit::tui_render_unit::next_local_message_id()),
            content_hash: 0,
        }
        .into(),
    );
    let mut snapshot = crate::kit::atoms::ViewModelsSnapshot {
        items: im::Vector::from(vec![unit]),
        generation: 1,
    };

    assert_eq!(
        entry_nav::apply_fold_toggle(&mut snapshot, 0, false),
        EventResult::Consumed
    );
    let lines = vm_to_lines(&snapshot.items[0], &GridSpec::grid_for(80));
    let rendered = lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    for i in 1..=8 {
        assert!(
            rendered.contains(&format!("思考行{i}")),
            "展开后第 {i} 行应可见（Preview 只有 4 行）:\n{rendered}"
        );
    }
}

#[test]
fn test_apply_fold_override_sets_fold_user_modified_and_recomputes_hash() {
    use crate::kit::tui_render_unit::TuiToolCard;

    let mut tool = TuiRenderUnit::TuiToolCard(TuiToolCard {
        images: Vec::new(),
        images_omitted: 0,
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
    entry_nav::apply_fold_override(&mut tool, FoldState::Expanded);
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
    entry_nav::apply_fold_override(&mut user, FoldState::Expanded);
}

/// 展开工具卡片会认领超预算历史结果的补读定位（进入在途，避免重复 RPC）。
#[test]
#[serial_test::serial]
fn test_tool_expand_claims_deferred_history_result_refetch() {
    use crate::kit::acp_events::history_window::{
        ToolResultSource, insert_deferred_tool_result_for_tests,
    };
    use crate::kit::tui_render_unit::TuiToolCard;

    crate::kit::atoms::init_atoms();
    crate::kit::atoms::NOTIFICATION.set(None);
    *crate::kit::atoms::ACTIVE_SESSION_ID.state().write() = "s1".into();
    crate::kit::acp_events::history_window::clear_pending_tool_ends();
    insert_deferred_tool_result_for_tests(
        "t-big",
        ToolResultSource {
            session_id: "s1".into(),
            snapshot_token: "tok".into(),
            cursor: "cursor-result".into(),
            limit: 50,
            entry_id: "s1:result".into(),
            entry_position: 20,
            sub_item_position: 0,
            tool_id: "t-big".into(),
        },
    );
    let card = TuiToolCard {
        images: Vec::new(),
        images_omitted: 0,
        tool_id: "t-big".into(),
        tool_name: "Bash".into(),
        input_summary: String::new(),
        output_summary: "结果正文较大，展开后按需加载".into(),
        is_error: false,
        is_running: false,
        running_duration_ms: None,
        completed_duration_ms: None,
        diff: None,
        presentation: Default::default(),
        fold: FoldState::Collapsed,
        user_modified: false,
        tool_calls_count: 0,
        content_hash: 0,
    };
    let mut snapshot = crate::kit::atoms::ViewModelsSnapshot {
        items: im::Vector::from(vec![TuiRenderUnit::TuiToolCard(card)]),
        generation: 1,
    };
    let claimed = entry_nav::apply_fold_toggle(&mut snapshot, 0, false);
    assert_eq!(claimed, EventResult::Consumed);
    assert!(
        crate::kit::atoms::NOTIFICATION.state().read().is_some(),
        "expanding the card must reach the refetch entry and report a missing client/runtime"
    );
    FOLD_OVERRIDES.state().write().clear();
    crate::kit::acp_events::history_window::clear_pending_tool_ends();
}

/// 展开正文空档组时，组内工具成员的延迟历史结果同样要认领补读（组行是唯一入口）。
#[test]
#[serial_test::serial]
fn test_group_expand_claims_deferred_history_result_refetch() {
    use crate::kit::acp_events::history_window::{
        ToolResultSource, insert_deferred_tool_result_for_tests,
    };

    crate::kit::atoms::init_atoms();
    crate::kit::atoms::NOTIFICATION.set(None);
    *crate::kit::atoms::ACTIVE_SESSION_ID.state().write() = "s1".into();
    crate::kit::acp_events::history_window::clear_pending_tool_ends();
    insert_deferred_tool_result_for_tests(
        "t-big",
        ToolResultSource {
            session_id: "s1".into(),
            snapshot_token: "tok".into(),
            cursor: "cursor-result".into(),
            limit: 50,
            entry_id: "s1:result".into(),
            entry_position: 20,
            sub_item_position: 0,
            tool_id: "t-big".into(),
        },
    );
    let mut card = TuiToolCard {
        images: Vec::new(),
        images_omitted: 0,
        tool_id: "t-big".into(),
        tool_name: "Bash".into(),
        input_summary: String::new(),
        output_summary: "结果正文较大，展开后按需加载".into(),
        is_error: false,
        is_running: false,
        running_duration_ms: None,
        completed_duration_ms: None,
        diff: None,
        presentation: Default::default(),
        fold: FoldState::Expanded,
        user_modified: false,
        tool_calls_count: 0,
        content_hash: 0,
    };
    card.recompute_hash();
    let mut group = TuiCollapsedGroup {
        key: "tool:t-big".into(),
        title: "Bash".into(),
        count: 1,
        failed_count: 0,
        view_models: vec![TuiRenderUnit::TuiToolCard(card)],
        fold: FoldState::Collapsed,
        is_working: false,
        content_hash: 0,
    };
    group.recompute_hash();
    let mut snapshot = crate::kit::atoms::ViewModelsSnapshot {
        items: im::Vector::from(vec![TuiRenderUnit::TuiCollapsedGroup(group)]),
        generation: 1,
    };
    let claimed = entry_nav::apply_fold_toggle(&mut snapshot, 0, false);
    assert_eq!(claimed, EventResult::Consumed);
    assert!(
        crate::kit::atoms::NOTIFICATION.state().read().is_some(),
        "展开组行必须递归到组内工具，认领延迟结果补读"
    );
    FOLD_OVERRIDES.state().write().clear();
    crate::kit::acp_events::history_window::clear_pending_tool_ends();
}
