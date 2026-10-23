use super::*;

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
    let (k, f) = fold_key_of(&vm).expect("应可折叠");
    assert_eq!(k, FoldKey::Reasoning("msg_9".into()));
    assert_eq!(f, FoldState::Preview);

    // 无 message_id 的 reasoning bubble → 无折叠键（不可作为覆盖目标）
    let vm_noid = TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
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
        }
        .into(),
    );
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

    let reminder = TuiSystemReminder::legacy("maintenance notice".into());
    let reminder_id = reminder.reminder_id;
    assert_eq!(
        fold_key_of(&TuiRenderUnit::TuiSystemReminder(reminder)),
        Some((FoldKey::SystemReminder(reminder_id), FoldState::Collapsed))
    );

    // user bubble 无折叠能力
    let user = TuiRenderUnit::TuiUserBubble(TuiUserBubble::new("hi".into()));
    assert!(fold_key_of(&user).is_none());

    // §6.7 subagent：fold_key_of 返回 SubAgent key——Enter 分派据此刻断打开
    // 详情 pane（折叠切换仍走同一 key 的覆盖表；分派改判在 mod.rs Enter 分支）。
    let sub = TuiRenderUnit::TuiSubAgentGroup(crate::kit::tui_render_unit::TuiSubAgentGroup {
        instance_id: "instance-agent-7".into(),
        agent_id: "agent-7".into(),
        agent_name: "explorer".into(),
        view_models: im::Vector::new(),
        collapsed: false,
        is_running: false,
        is_error: false,
        error_reason: None,
        fold: FoldState::Collapsed,
        user_modified: false,
        content_hash: 0,
    });
    assert_eq!(
        fold_key_of(&sub),
        Some((
            FoldKey::SubAgent("instance-agent-7".into()),
            FoldState::Collapsed
        )),
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

#[test]
#[serial]
fn test_collapsed_group_toggle_persists_override() {
    crate::kit::atoms::init_atoms();
    FOLD_OVERRIDES.state().write().clear();
    let child = TuiRenderUnit::TuiToolCard(TuiToolCard {
        tool_id: "group-tool-1".into(),
        tool_name: "Read".into(),
        input_summary: "a.rs".into(),
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
    });
    let mut group = TuiCollapsedGroup {
        title: "Read 1".into(),
        count: 1,
        failed_count: 0,
        view_models: vec![child],
        fold: FoldState::Collapsed,
        content_hash: 0,
    };
    group.recompute_hash();
    let mut snapshot = crate::kit::atoms::ViewModelsSnapshot {
        items: im::Vector::from(vec![TuiRenderUnit::TuiCollapsedGroup(group)]),
        generation: 7,
    };

    assert_eq!(
        apply_fold_toggle(&mut snapshot, 0, false),
        EventResult::Consumed
    );
    assert!(matches!(
        &snapshot.items[0],
        TuiRenderUnit::TuiCollapsedGroup(g) if g.fold == FoldState::Expanded
    ));
    assert_eq!(snapshot.generation, 8);
    assert_eq!(
        FOLD_OVERRIDES
            .state()
            .read()
            .get(&FoldKey::Group(vec!["group-tool-1".into()])),
        Some(&FoldState::Expanded)
    );
    FOLD_OVERRIDES.state().write().clear();
}
