//! 正文空档分组回归——正文段落之间连续的过程单元（工具 / thinking / 通知等）
//! 合成一张折叠组，一组一行；点击展开整组，组内不再逐项折叠。
//!
//! 这里从 ACP 事件驱动 `push_view_models` 的完整装配路径，断言快照槽位、
//! 组身份、流式追加入组与冷重建一致性。

use super::AcpEventData;
use super::FoldKey;
use super::FoldState;
use super::TuiReasoningChunk;
use super::TuiRenderUnit;
use super::TuiTextChunk;
use super::TuiToolEnded;
use super::TuiToolStarted;
use super::serial;

use crate::kit::atoms::{VIEW_MODELS, ViewModelsSnapshot};
use crate::kit::tui_render_unit::{TuiCollapsedGroup, TuiNoteLevel};

fn text(t: &str, id: &str) -> AcpEventData {
    AcpEventData::TextChunk(TuiTextChunk {
        text: t.into(),
        message_id: Some(id.into()),
        agent_id: None,
    })
}

fn reasoning(t: &str, id: &str) -> AcpEventData {
    AcpEventData::ReasoningChunk(TuiReasoningChunk {
        text: t.into(),
        message_id: Some(id.into()),
        agent_id: None,
    })
}

fn tool_started(id: &str) -> AcpEventData {
    AcpEventData::ToolStarted(TuiToolStarted {
        tool_id: id.into(),
        tool_name: "Read".into(),
        input_summary: format!("{id}.rs"),
        raw_input: serde_json::json!({ "path": format!("{id}.rs") }),
        agent_id: None,
        origin_message_id: None,
        origin_readable: None,
    })
}

fn tool_ended(id: &str) -> AcpEventData {
    AcpEventData::ToolEnded(TuiToolEnded {
        tool_id: id.into(),
        output_summary: "ok".into(),
        is_error: false,
        agent_id: None,
    })
}

fn dispatch_all(state: &mut super::BridgeState, events: &[AcpEventData]) {
    for event in events {
        super::dispatch_and_notify(state, event);
    }
}

fn current_snapshot() -> ViewModelsSnapshot {
    VIEW_MODELS.state().read().clone()
}

fn body_text(vm: &TuiRenderUnit) -> Option<&str> {
    match vm {
        TuiRenderUnit::TuiAssistantBubble(bubble) => Some(&bubble.text),
        _ => None,
    }
}

fn group_at(snapshot: &ViewModelsSnapshot, idx: usize) -> &TuiCollapsedGroup {
    match &snapshot.items[idx] {
        TuiRenderUnit::TuiCollapsedGroup(group) => group,
        other => panic!("expected TuiCollapsedGroup at [{idx}], got {other:?}"),
    }
}

fn prompt_state() -> super::BridgeState {
    let mut state = super::make_fold_test_state();
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::PromptSubmitted {
            request_id: Some("r1".into()),
        },
    );
    state
}

/// 正文 A + 3 个过程项（工具 / thinking / 工具）+ 正文 B → 两个正文段 + 一张
/// 折叠组；组行只有一行，展开后组内 3 项原样可见。
#[test]
#[serial]
fn test_body_gap_groups_tools_and_thinking_between_paragraphs() {
    let mut state = prompt_state();
    dispatch_all(
        &mut state,
        &[
            text("正文 A", "m1"),
            tool_started("t1"),
            tool_ended("t1"),
            reasoning("先读一下再回答", "m2"),
            tool_started("t2"),
            tool_ended("t2"),
            text("正文 B", "m3"),
        ],
    );
    let snapshot = current_snapshot();
    assert_eq!(
        snapshot.items.len(),
        3,
        "正文 A + 过程区 + 正文 B 应合成 3 个槽位，实际 {:?}",
        snapshot.items
    );
    assert_eq!(body_text(&snapshot.items[0]), Some("正文 A"));
    assert_eq!(body_text(&snapshot.items[2]), Some("正文 B"));

    let group = group_at(&snapshot, 1);
    assert_eq!(group.count, 3, "组内 3 个过程单元");
    assert_eq!(group.fold, FoldState::Collapsed, "组默认折叠为一行");
    assert_eq!(group.key, "tool:t1", "组身份取首个成员的工具身份");
    assert!(
        group
            .view_models
            .iter()
            .any(|vm| matches!(vm, TuiRenderUnit::TuiAssistantBubble(b) if b.reasoning.is_some())),
        "thinking 必须进组"
    );

    // 组行 = 一行；展开后组内 3 项按原样显示。
    let grid = crate::kit::message_area::grid::GridSpec::grid_for(120);
    let collapsed_lines = crate::kit::message_area::render::vm_to_lines(&snapshot.items[1], &grid);
    assert_eq!(collapsed_lines.len(), 1, "折叠组只显示一行摘要");

    crate::kit::fold_overrides::insert(FoldKey::Group(group.key.clone()), FoldState::Expanded);
    crate::kit::acp_events::push_view_models(&mut state);
    let expanded = current_snapshot();
    let expanded_group = group_at(&expanded, 1);
    assert_eq!(expanded_group.fold, FoldState::Expanded);
    let expanded_lines = crate::kit::message_area::render::vm_to_lines(&expanded.items[1], &grid);
    assert_eq!(
        expanded_lines.len(),
        4,
        "组头一行 + 3 个成员各保留自身单行折叠，实际 {:?}",
        expanded_lines
            .iter()
            .map(|line| line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>())
            .collect::<Vec<_>>()
    );
    assert!(
        expanded_lines
            .iter()
            .any(|line| { line.spans.iter().any(|span| span.content.contains("t1.rs")) }),
        "展开后工具标题摘要可见"
    );
    for member in &expanded_group.view_models {
        let fold = match member {
            TuiRenderUnit::TuiToolCard(card) => card.fold,
            TuiRenderUnit::TuiAssistantBubble(bubble) => {
                bubble.reasoning.as_ref().expect("reasoning member").fold
            }
            other => panic!("unexpected group member {other:?}"),
        };
        assert_eq!(
            fold,
            FoldState::Collapsed,
            "展开组不强制展开成员（保留折叠 pass 的单行折叠）"
        );
    }
}

/// 成员覆盖：单独展开组内成员后重建，只有该成员展开；组摘要/失败计数不变。
#[test]
#[serial]
fn test_member_fold_override_survives_rebuild_without_touching_group() {
    let mut state = prompt_state();
    dispatch_all(
        &mut state,
        &[text("正文 A", "m1"), tool_started("t1"), tool_ended("t1")],
    );
    dispatch_all(
        &mut state,
        &[
            tool_started("t2"),
            tool_ended("t2"),
            reasoning("想一想", "m2"),
            text("正文 B", "m3"),
        ],
    );
    let snapshot = current_snapshot();
    let group = group_at(&snapshot, 1);
    assert_eq!(group.count, 3);
    assert_eq!(group.failed_count, 0);
    let title = group.title.clone();
    let key = group.key.clone();

    crate::kit::fold_overrides::insert(FoldKey::Tool("t2".into()), FoldState::Expanded);
    crate::kit::acp_events::push_view_models(&mut state);
    let rebuilt = current_snapshot();
    let group = group_at(&rebuilt, 1);
    assert_eq!(
        group.fold,
        FoldState::Collapsed,
        "组自身折叠态不受成员覆盖影响"
    );
    assert_eq!(group.key, key, "组身份稳定");
    assert_eq!(group.title, title, "组摘要不被成员折叠变化改写");
    assert_eq!(group.count, 3);
    assert_eq!(group.failed_count, 0);
    let folds: Vec<FoldState> = group
        .view_models
        .iter()
        .map(|member| match member {
            TuiRenderUnit::TuiToolCard(card) => card.fold,
            TuiRenderUnit::TuiAssistantBubble(bubble) => {
                bubble.reasoning.as_ref().expect("reasoning member").fold
            }
            other => panic!("unexpected group member {other:?}"),
        })
        .collect();
    assert_eq!(
        folds,
        vec![
            FoldState::Collapsed,
            FoldState::Expanded,
            FoldState::Collapsed
        ],
        "只有被点开的成员展开，其他成员保持默认折叠"
    );
}

/// 相邻正文之间没有过程单元 → 不产生组。
#[test]
#[serial]
fn test_adjacent_paragraphs_without_process_units_have_no_group() {
    let mut state = prompt_state();
    dispatch_all(&mut state, &[text("第一段", "m1"), text("第二段", "m2")]);
    let snapshot = current_snapshot();
    assert_eq!(snapshot.items.len(), 2);
    assert!(
        snapshot
            .items
            .iter()
            .all(|vm| !matches!(vm, TuiRenderUnit::TuiCollapsedGroup(_))),
        "相邻正文无过程单元时不得出组"
    );
}

/// 末尾正文之后的过程单元同样归入末尾组，默认一行。
#[test]
#[serial]
fn test_trailing_process_units_form_tail_group() {
    let mut state = prompt_state();
    dispatch_all(
        &mut state,
        &[
            text("回答", "m1"),
            tool_started("t1"),
            tool_ended("t1"),
            tool_started("t2"),
            tool_ended("t2"),
        ],
    );
    let snapshot = current_snapshot();
    assert_eq!(snapshot.items.len(), 2);
    assert_eq!(body_text(&snapshot.items[0]), Some("回答"));
    let group = group_at(&snapshot, 1);
    assert_eq!(group.count, 2);
    assert_eq!(group.fold, FoldState::Collapsed);
    assert_eq!(group.key, "tool:t1");
}

/// 流式追加：同一空档内的新过程项进入既有组，组身份与用户展开选择保持稳定。
#[test]
#[serial]
fn test_streaming_append_joins_same_group_and_keeps_key() {
    let mut state = prompt_state();
    dispatch_all(&mut state, &[text("开始", "m1"), tool_started("t1")]);
    let first = current_snapshot();
    assert_eq!(first.items.len(), 2, "items={:?}", first.items);
    let first_group = group_at(&first, 1);
    assert_eq!(first_group.count, 1);
    let key = first_group.key.clone();
    let first_hash = first_group.content_hash;

    dispatch_all(&mut state, &[tool_started("t2")]);
    let second = current_snapshot();
    assert_eq!(second.items.len(), 2, "追加过程项不得新建组槽位");
    let second_group = group_at(&second, 1);
    assert_eq!(second_group.count, 2, "新项进入同一组");
    assert_eq!(second_group.key, key, "组身份跨流式追加稳定");
    assert_ne!(
        second_group.content_hash, first_hash,
        "组成员变化必须改变内容哈希（渲染缓存失效）"
    );

    // 用户展开该组后继续追加：覆盖键不变，展开态保持。
    crate::kit::fold_overrides::insert(FoldKey::Group(key.clone()), FoldState::Expanded);
    crate::kit::acp_events::push_view_models(&mut state);
    dispatch_all(&mut state, &[tool_started("t3")]);
    let third = current_snapshot();
    let third_group = group_at(&third, 1);
    assert_eq!(third_group.count, 3);
    assert_eq!(third_group.key, key);
    assert_eq!(
        third_group.fold,
        FoldState::Expanded,
        "同一组的展开覆盖在流式追加后仍生效"
    );
}

/// 通知类单元（system note）与工具同属过程区，进入同一组。
#[test]
#[serial]
fn test_system_note_between_tools_is_grouped() {
    let mut state = prompt_state();
    dispatch_all(
        &mut state,
        &[text("正文 A", "m1"), tool_started("t1"), tool_ended("t1")],
    );
    state.inject_system_note("通知：模型已切换".into(), TuiNoteLevel::Info);
    dispatch_all(
        &mut state,
        &[tool_started("t2"), tool_ended("t2"), text("正文 B", "m2")],
    );
    let snapshot = current_snapshot();
    assert_eq!(snapshot.items.len(), 3);
    let group = group_at(&snapshot, 1);
    assert_eq!(group.count, 3, "工具 + 通知 + 工具同组");
    assert!(
        group
            .view_models
            .iter()
            .any(|vm| matches!(vm, TuiRenderUnit::TuiSystemNote(_))),
        "通知必须包含在组内"
    );
}

/// 详情目标（超大正文占位 note / 外置正文工具卡）必须保持独立槽位：
/// 组行只暴露一个槽位，藏进组内的多个详情目标无法分别打开。
#[test]
#[serial]
fn test_detail_target_note_stays_visible_instead_of_grouped() {
    use crate::kit::history_window::{DetailDescriptor, HistoryDetailTarget, HistorySource};

    let mut state = prompt_state();
    dispatch_all(
        &mut state,
        &[text("正文 A", "m1"), tool_started("t1"), tool_ended("t1")],
    );
    state.inject_system_note(
        "超大内容 · 8.0 KiB · 按需加载详情".into(),
        TuiNoteLevel::Info,
    );
    dispatch_all(
        &mut state,
        &[tool_started("t2"), tool_ended("t2"), text("正文 B", "m2")],
    );
    let note_hash = current_snapshot()
        .items
        .iter()
        .find_map(|vm| match vm {
            TuiRenderUnit::TuiCollapsedGroup(group) => {
                group.view_models.iter().find_map(|vm| match vm {
                    TuiRenderUnit::TuiSystemNote(note) => Some(note.content_hash),
                    _ => None,
                })
            }
            _ => None,
        })
        .expect("占位 note 在注册详情目标前应位于组内");

    crate::kit::atoms::HISTORY_DETAIL_TARGETS.set(vec![HistoryDetailTarget {
        content_hash: note_hash,
        descriptor: DetailDescriptor::new(
            HistorySource::Own,
            "s1:m1",
            8192,
            4096,
            "snap-1",
            "ticket-1",
        ),
    }]);
    crate::kit::acp_events::push_view_models(&mut state);
    let snapshot = current_snapshot();
    assert_eq!(
        snapshot.items.len(),
        5,
        "正文 A + 组(1) + 占位 note + 组(1) + 正文 B，实际 {:?}",
        snapshot.items
    );
    assert_eq!(group_at(&snapshot, 1).count, 1);
    assert!(
        matches!(&snapshot.items[2], TuiRenderUnit::TuiSystemNote(note) if note.content_hash == note_hash),
        "详情目标占位 note 必须独立可见"
    );
    assert_eq!(group_at(&snapshot, 3).count, 1);
    assert_eq!(body_text(&snapshot.items[4]), Some("正文 B"));

    crate::kit::atoms::HISTORY_DETAIL_TARGETS.set(Vec::new());
    crate::kit::acp_events::render::reset_body_gap_group_cache();
}

/// 冷重建一致：清空分组缓存后重新装配，快照逐条结构相等（组身份与成员不变）。
#[test]
#[serial]
fn test_group_snapshot_matches_cold_rebuild() {
    let mut state = prompt_state();
    dispatch_all(
        &mut state,
        &[
            text("正文 A", "m1"),
            tool_started("t1"),
            tool_ended("t1"),
            reasoning("想一想", "m2"),
            tool_started("t2"),
            tool_ended("t2"),
            text("正文 B", "m3"),
        ],
    );
    super::dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    let hot = current_snapshot();

    crate::kit::acp_events::render::reset_body_gap_group_cache();
    crate::kit::fold_overrides::clear();
    crate::kit::acp_events::push_view_models(&mut state);
    let cold = current_snapshot();

    assert_eq!(hot.items, cold.items, "冷重建必须与热缓存逐条一致");
    assert_eq!(group_at(&hot, 1).key, group_at(&cold, 1).key);
    assert_eq!(group_at(&cold, 1).count, 3);
}

// ---------------------------------------------------------------------------
// 尾部工作组标记（Working shimmer 的判定层）
// ---------------------------------------------------------------------------

fn group_indexes(snapshot: &ViewModelsSnapshot) -> Vec<usize> {
    snapshot
        .items
        .iter()
        .enumerate()
        .filter_map(|(idx, vm)| matches!(vm, TuiRenderUnit::TuiCollapsedGroup(_)).then_some(idx))
        .collect()
}

fn changed_from() -> usize {
    crate::kit::atoms::TRANSCRIPT_PUBLICATION.get().changed_from
}

/// 只有「turn 运行中 + 最后一台折叠组」带标记；正文开始流式后旧组立即清除，
/// 且 `changed_from` 覆盖旧组下标（否则缓存保留最后一帧扫光）。
#[test]
#[serial]
fn test_only_tail_group_is_working_and_old_group_is_cleared() {
    let mut state = prompt_state();
    dispatch_all(
        &mut state,
        &[
            text("正文 A", "m1"),
            tool_started("t1"),
            tool_ended("t1"),
            text("正文 B", "m2"),
            tool_started("t2"),
            tool_ended("t2"),
        ],
    );
    let snapshot = current_snapshot();
    let groups = group_indexes(&snapshot);
    assert_eq!(
        groups,
        vec![1, 3],
        "两个正文段之间各一台组：{:?}",
        snapshot.items
    );
    assert!(!group_at(&snapshot, 1).is_working, "前一台组不是尾组");
    assert!(group_at(&snapshot, 3).is_working, "尾组在 turn 运行中");

    // 正文 C 开始流式：旧尾组被挤下去。
    let old_index = 3;
    dispatch_all(&mut state, &[text("正文 C", "m3")]);
    let snapshot = current_snapshot();
    assert!(
        !group_at(&snapshot, old_index).is_working,
        "旧尾组必须失去标记"
    );
    assert!(
        changed_from() <= old_index,
        "旧组下标必须落在重扫区间内：changed_from={} old={old_index}",
        changed_from()
    );
}

/// 尾部是 todo 摘要时，跳过它仍点亮前一张组；正文开始流式后同样要清除标记
/// （缓存前缀尾部是 todo，清扫不能只看 `changed_from - 1`）。
#[test]
#[serial]
fn test_todo_summary_at_tail_does_not_hide_working_group() {
    let mut state = prompt_state();
    *crate::kit::atoms::TODO_ITEMS.state().write() = vec![crate::kit::message_area::TodoItem {
        status: crate::kit::message_area::TodoStatus::InProgress,
        content: "Running tests".into(),
    }];
    dispatch_all(
        &mut state,
        &[text("正文 A", "m1"), tool_started("t1"), tool_ended("t1")],
    );
    let snapshot = current_snapshot();
    assert_eq!(snapshot.items.len(), 3, "{:?}", snapshot.items);
    assert!(
        matches!(&snapshot.items[2], TuiRenderUnit::TuiTodoSummary(_)),
        "todo 摘要位于 turn 底部"
    );
    assert!(group_at(&snapshot, 1).is_working, "todo 不遮挡尾组");

    dispatch_all(&mut state, &[text("正文 B", "m2")]);
    let snapshot = current_snapshot();
    assert!(
        !group_at(&snapshot, 1).is_working,
        "旧组被正文挤下去后必须清除（即使前缀尾部是 todo）"
    );
    assert!(
        changed_from() <= 1,
        "旧的 working 组必须重新进入重扫区间：changed_from={}",
        changed_from()
    );
    *crate::kit::atoms::TODO_ITEMS.state().write() = Vec::new();
}

/// turn 结束后标记清除；下一轮只有新的尾组亮起。
#[test]
#[serial]
fn test_turn_done_clears_working_and_next_turn_marks_its_own_tail() {
    let mut state = prompt_state();
    dispatch_all(
        &mut state,
        &[text("正文 A", "m1"), tool_started("t1"), tool_ended("t1")],
    );
    assert!(group_at(&current_snapshot(), 1).is_working);

    dispatch_all(&mut state, &[AcpEventData::TurnDone]);
    let snapshot = current_snapshot();
    assert!(
        !group_at(&snapshot, 1).is_working,
        "TurnDone 归档后历史组不得继续亮"
    );

    dispatch_all(
        &mut state,
        &[
            AcpEventData::PromptSubmitted {
                request_id: Some("r2".into()),
            },
            text("正文 C", "m3"),
            tool_started("t2"),
            tool_ended("t2"),
        ],
    );
    let snapshot = current_snapshot();
    let groups = group_indexes(&snapshot);
    assert_eq!(groups.len(), 2, "{:?}", snapshot.items);
    assert!(
        !group_at(&snapshot, groups[0]).is_working,
        "上一轮组保持静态"
    );
    assert!(
        group_at(&snapshot, groups[1]).is_working,
        "本轮尾组才是工作组"
    );
}

/// 中断后（phase == Idle）标记立即清除。
#[test]
#[serial]
fn test_turn_interrupted_clears_working() {
    let mut state = prompt_state();
    dispatch_all(
        &mut state,
        &[text("正文 A", "m1"), tool_started("t1"), tool_ended("t1")],
    );
    assert!(group_at(&current_snapshot(), 1).is_working);

    dispatch_all(
        &mut state,
        &[AcpEventData::TurnInterrupted {
            reason: "cancel".into(),
            request_id: None,
        }],
    );
    let snapshot = current_snapshot();
    assert!(
        !group_at(&snapshot, 1).is_working,
        "中断后折叠组不得继续显示 Working"
    );
}

/// 历史回放（phase == ReplayingHistory）永不标记——即使 `current_turn` 里还留着
/// 上一帧的工作组。
#[test]
#[serial]
fn test_replay_phase_never_marks_a_working_group() {
    let mut state = prompt_state();
    dispatch_all(
        &mut state,
        &[text("正文 A", "m1"), tool_started("t1"), tool_ended("t1")],
    );
    assert!(group_at(&current_snapshot(), 1).is_working);

    state.phase = crate::kit::acp_events::SessionPhase::ReplayingHistory;
    crate::kit::acp_events::push_view_models(&mut state);
    let snapshot = current_snapshot();
    assert!(
        matches!(
            &snapshot.items[1],
            TuiRenderUnit::TuiCollapsedGroup(group) if !group.is_working
        ),
        "回放阶段不得点亮 Working：items={:?}",
        snapshot.items
    );
}
