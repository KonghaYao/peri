//! 正文空档折叠组展开后的两级折叠：成员默认保持折叠 pass 给出的 fold，
//! 组内成员标题行可单独点击切换（组头点击仍切换整组）。
//!
//! 这里锁定渲染产出的「成员标题逻辑行 → FoldKey」命中表，并用真实 wrap map +
//! `SlotIndex` 走 Down 冻结路径复原视觉行命中，覆盖滚动偏移后的行映射。

use std::sync::Arc;

use ratatui_kit::prelude::EventResult;
use ratatui_kit::ratatui::text::Line;
use serial_test::serial;

use crate::kit::message_area::entry_nav::EntryClickHit;
use crate::kit::message_area::grid::{GridSpec, TextLineSource, ZoneTransition};
use crate::kit::message_area::render;
use crate::kit::message_area::scroll::freeze_down_index;
use crate::kit::message_area::selection::{SlotLines, build_wrap_map};
use crate::kit::message_area::slot_index::SlotIndex;
use crate::kit::tui_render_unit::{
    EntryStatus, FoldKey, FoldState, TuiAssistantBubble, TuiCollapsedGroup, TuiReasoningBlock,
    TuiRenderUnit, TuiSystemReminder, TuiToolCard, TuiToolPresentation,
};

fn tool(id: &str, fold: FoldState) -> TuiRenderUnit {
    let mut card = TuiToolCard {
        tool_id: id.to_string(),
        tool_name: "Read".to_string(),
        input_summary: format!("{id}.rs"),
        output_summary: "file contents".to_string(),
        is_error: false,
        is_running: false,
        running_duration_ms: None,
        completed_duration_ms: Some(400),
        diff: None,
        presentation: TuiToolPresentation::Generic,
        fold,
        user_modified: false,
        tool_calls_count: 0,
        content_hash: 1,
    };
    card.recompute_hash();
    TuiRenderUnit::TuiToolCard(card)
}

fn reasoning(id: &str, fold: FoldState) -> TuiRenderUnit {
    TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
            started_at: None,
            duration_ms: None,
            text: String::new(),
            reasoning: Some(TuiReasoningBlock {
                text: "先读一下再回答".into(),
                fold,
                status: EntryStatus::Completed,
                is_running: false,
                started_at: None,
                duration_ms: Some(12_000),
            }),
            message_id: Some(id.to_string()),
            content_hash: 1,
        }
        .into(),
    )
}

fn reminder(text: &str) -> (TuiRenderUnit, u64) {
    let vm = TuiSystemReminder::legacy(text.into());
    let id = vm.reminder_id;
    (TuiRenderUnit::TuiSystemReminder(vm), id)
}

fn group(members: Vec<TuiRenderUnit>, fold: FoldState) -> TuiCollapsedGroup {
    let mut group = TuiCollapsedGroup {
        key: "tool:t1".into(),
        title: "Read 2 · Thought 1".into(),
        count: members.len() as u32,
        failed_count: 0,
        view_models: members,
        fold,
        content_hash: 0,
    };
    group.recompute_hash();
    group
}

fn rendered(group: &TuiCollapsedGroup) -> (Vec<Line<'static>>, Vec<(usize, FoldKey)>) {
    let grid = GridSpec::grid_for(120);
    let (lines, _, _, _, _, _, fold_hits) = render::vm_to_lines_cached_with_layout(
        &TuiRenderUnit::TuiCollapsedGroup(group.clone()),
        &grid,
        &mut Default::default(),
        None,
        false,
        ZoneTransition::None,
    );
    (lines, fold_hits)
}

/// 展开组后，成员保持折叠 pass 给出的 Collapsed（每项一行），命中表只覆盖
/// 成员标题行。
#[test]
fn test_expanded_group_keeps_member_folds_and_maps_title_lines() {
    let (note, note_id) = reminder("提醒正文");
    let group = group(
        vec![
            tool("t1", FoldState::Collapsed),
            reasoning("msg_r1", FoldState::Collapsed),
            note,
        ],
        FoldState::Expanded,
    );
    let (lines, hits) = rendered(&group);

    assert_eq!(
        lines.len(),
        4,
        "组头一行 + 3 个折叠成员各一行，实际 {:?}",
        lines.iter().map(text_of).collect::<Vec<_>>()
    );
    assert_eq!(
        hits,
        vec![
            (1, FoldKey::Tool("t1".into())),
            (2, FoldKey::Reasoning("msg_r1".into())),
            (3, FoldKey::SystemReminder(note_id)),
        ],
        "命中表 = 每个成员标题的逻辑行"
    );
    assert!(
        text_of(&lines[1]).contains("t1.rs"),
        "成员标题保持可见，实际 {:?}",
        text_of(&lines[1])
    );
}

/// 已展开的成员只把标题行当命中行：正文行留给文本选区。
#[test]
fn test_member_hit_covers_title_line_only() {
    let group = group(
        vec![
            tool("t1", FoldState::Expanded),
            tool("t2", FoldState::Collapsed),
        ],
        FoldState::Expanded,
    );
    let (lines, hits) = rendered(&group);
    let second_member = hits[1].0;
    assert_eq!(
        hits,
        vec![
            (1, FoldKey::Tool("t1".into())),
            (second_member, FoldKey::Tool("t2".into())),
        ],
        "展开成员只有标题行进入命中表"
    );
    assert!(
        second_member > 2,
        "t1 展开后占多行（标题 + 内容），t2 标题顺延，实际 {second_member} 行"
    );
    assert_eq!(lines.len(), second_member + 1, "t2 折叠只有一行");
}

/// 真实 Buffer 行映射：视觉行 → 组头 / 成员标题 / 正文行 三种结果；
/// 前置槽位偏移后仍按绝对视觉行命中（滚动偏移就是这个行号的来源）。
#[test]
fn test_entry_click_target_maps_header_member_and_content_rows() {
    let mut card = match tool("t1", FoldState::Expanded) {
        TuiRenderUnit::TuiToolCard(card) => card,
        _ => unreachable!(),
    };
    card.output_summary = "line one\nline two\nline three".into();
    card.recompute_hash();
    let group = group(
        vec![
            TuiRenderUnit::TuiToolCard(card),
            reasoning("msg_r1", FoldState::Collapsed),
        ],
        FoldState::Expanded,
    );
    let (group_lines, hits) = rendered(&group);
    let header_row = 5usize;
    let member_row = header_row + hits[0].0;
    let content_row = header_row + hits[0].0 + 1;

    let prefix: Vec<Line<'static>> = (0..5).map(|i| Line::from(format!("前文 {i}"))).collect();
    let mut slots = vec![
        SlotLines::single(Arc::new(prefix.clone())),
        SlotLines::single(Arc::new(group_lines)).with_fold_hits(Arc::new(hits.clone())),
    ];
    let mut maps: Vec<Arc<Vec<_>>> = Vec::new();
    for slot in &mut slots {
        let lines: Vec<Line<'static>> = (0..slot.len())
            .filter_map(|i| slot.line(i).cloned())
            .collect();
        maps.push(Arc::new(build_wrap_map(&lines, 120).1));
    }
    let index = SlotIndex::new(slots, maps);

    assert_eq!(index.slot_visual_start(1), Some(header_row));
    assert_eq!(
        hit_at(&index, header_row),
        Some(EntryClickHit::Header { slot: 1 }),
        "组头命中 = 整组切换"
    );
    assert_eq!(
        hit_at(&index, member_row),
        Some(EntryClickHit::GroupMember {
            slot: 1,
            key: FoldKey::Tool("t1".into()),
        }),
        "成员标题行命中 = 只切换该成员（滚动偏移后的绝对视觉行）"
    );
    assert_eq!(
        hit_at(&index, content_row),
        None,
        "成员正文行不参与折叠切换"
    );
    assert_eq!(
        hit_at(&index, header_row + hits[1].0),
        Some(EntryClickHit::GroupMember {
            slot: 1,
            key: FoldKey::Reasoning("msg_r1".into()),
        }),
        "第二个成员标题同样可命中"
    );
    assert_eq!(
        hit_at(&index, 0),
        Some(EntryClickHit::Header { slot: 0 }),
        "前置槽位不受影响"
    );
}

/// 入口台阶占第 0 行（文本 → 过程区转场）时，组头与成员标题一起顺延，
/// 命中表与 `header_local` 同步偏移。
#[test]
fn test_entry_click_target_after_zone_step_shift() {
    let group = group(
        vec![
            tool("t1", FoldState::Collapsed),
            reasoning("msg_r1", FoldState::Collapsed),
        ],
        FoldState::Expanded,
    );
    let grid = GridSpec::grid_for(120);
    let (lines, _, _, _, _, _, hits) = render::vm_to_lines_cached_with_layout(
        &TuiRenderUnit::TuiCollapsedGroup(group),
        &grid,
        &mut Default::default(),
        None,
        false,
        ZoneTransition::EnterProcess(TextLineSource::User),
    );
    assert!(render::is_step_row(&lines[0]), "首行是入口台阶");
    assert_eq!(
        hits,
        vec![
            (2, FoldKey::Tool("t1".into())),
            (3, FoldKey::Reasoning("msg_r1".into())),
        ],
        "台阶行使成员标题逻辑行 +1"
    );

    let (_, map) = build_wrap_map(&lines, 120);
    let index = SlotIndex::new(
        vec![SlotLines::single(Arc::new(lines)).with_fold_hits(Arc::new(hits))],
        vec![Arc::new(map)],
    );
    assert_eq!(index.header_local(0), 1, "台阶行占第 0 行 → 组头是第 1 行");
    assert_eq!(
        hit_at(&index, 1),
        Some(EntryClickHit::Header { slot: 0 }),
        "组头点击"
    );
    assert_eq!(
        hit_at(&index, 2),
        Some(EntryClickHit::GroupMember {
            slot: 0,
            key: FoldKey::Tool("t1".into()),
        }),
        "成员标题点击"
    );
}

/// 成员点击只切换被点的成员；组身份、摘要与失败计数不变。
#[test]
#[serial]
fn test_member_toggle_changes_only_clicked_member() {
    crate::kit::atoms::init_atoms();
    crate::kit::fold_overrides::clear();
    let group = group(
        vec![
            tool("t1", FoldState::Collapsed),
            tool("t2", FoldState::Collapsed),
            reasoning("msg_r1", FoldState::Collapsed),
        ],
        FoldState::Expanded,
    );
    let key_before = group.key.clone();
    let title_before = group.title.clone();
    let hash_before = group.content_hash;
    let mut snapshot = crate::kit::atoms::ViewModelsSnapshot {
        items: im::Vector::from(vec![TuiRenderUnit::TuiCollapsedGroup(group)]),
        generation: 0,
    };

    assert_eq!(
        crate::kit::message_area::apply_group_member_toggle(
            &mut snapshot,
            0,
            &FoldKey::Tool("t1".into()),
        ),
        EventResult::Consumed,
    );
    let updated = group_of(&snapshot);
    assert_eq!(member_fold(updated, 0), FoldState::Expanded, "被点成员展开");
    assert_eq!(
        member_fold(updated, 1),
        FoldState::Collapsed,
        "同组其他成员不变"
    );
    assert_eq!(
        member_fold(updated, 2),
        FoldState::Collapsed,
        "同组其他成员不变"
    );
    assert_eq!(updated.fold, FoldState::Expanded, "整组展开态不变");
    assert_eq!(updated.key, key_before, "组身份稳定");
    assert_eq!(updated.title, title_before, "组摘要不因成员折叠变化");
    assert_eq!(updated.count, 3);
    assert_eq!(updated.failed_count, 0);
    assert_ne!(
        updated.content_hash, hash_before,
        "成员折叠变化必须失效渲染缓存"
    );
    assert_eq!(
        crate::kit::atoms::FOLD_OVERRIDES
            .state()
            .read()
            .get(&FoldKey::Tool("t1".into()))
            .copied(),
        Some(FoldState::Expanded),
        "成员选择写入持久覆盖表"
    );

    // 再次点击同一成员 → 收起；其他成员仍不变。
    assert_eq!(
        crate::kit::message_area::apply_group_member_toggle(
            &mut snapshot,
            0,
            &FoldKey::Tool("t1".into()),
        ),
        EventResult::Consumed,
    );
    let updated = group_of(&snapshot);
    assert_eq!(
        member_fold(updated, 0),
        FoldState::Collapsed,
        "再次点击收起"
    );
    assert_eq!(member_fold(updated, 1), FoldState::Collapsed);
    assert_eq!(
        crate::kit::atoms::FOLD_OVERRIDES
            .state()
            .read()
            .get(&FoldKey::Tool("t1".into()))
            .copied(),
        Some(FoldState::Collapsed),
    );
}

/// 组头语义保持现状：点击组头（entry 首行）切换整组，成员 fold 不变。
#[test]
#[serial]
fn test_group_header_toggle_still_toggles_whole_group() {
    crate::kit::atoms::init_atoms();
    crate::kit::fold_overrides::clear();
    let group = group(
        vec![
            tool("t1", FoldState::Collapsed),
            tool("t2", FoldState::Collapsed),
        ],
        FoldState::Expanded,
    );
    let mut snapshot = crate::kit::atoms::ViewModelsSnapshot {
        items: im::Vector::from(vec![TuiRenderUnit::TuiCollapsedGroup(group)]),
        generation: 0,
    };
    assert_eq!(
        crate::kit::message_area::apply_fold_toggle(&mut snapshot, 0, false),
        EventResult::Consumed
    );
    let updated = group_of(&snapshot);
    assert_eq!(updated.fold, FoldState::Collapsed, "组头点击收起整组");
    assert_eq!(
        member_fold(updated, 0),
        FoldState::Collapsed,
        "成员 fold 不被改写"
    );
    assert_eq!(
        member_fold(updated, 1),
        FoldState::Collapsed,
        "成员 fold 不被改写"
    );
}

fn hit_at(index: &SlotIndex, visual_row: usize) -> Option<EntryClickHit> {
    freeze_down_index((0, 0), (visual_row, 0), index, true).entry_hit
}

fn text_of(line: &Line<'static>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

fn group_of(snapshot: &crate::kit::atoms::ViewModelsSnapshot) -> &TuiCollapsedGroup {
    match &snapshot.items[0] {
        TuiRenderUnit::TuiCollapsedGroup(group) => group,
        other => panic!("expected TuiCollapsedGroup, got {other:?}"),
    }
}

fn member_fold(group: &TuiCollapsedGroup, index: usize) -> FoldState {
    match &group.view_models[index] {
        TuiRenderUnit::TuiToolCard(card) => card.fold,
        TuiRenderUnit::TuiAssistantBubble(bubble) => {
            bubble.reasoning.as_ref().expect("reasoning member").fold
        }
        other => panic!("unexpected member {other:?}"),
    }
}
