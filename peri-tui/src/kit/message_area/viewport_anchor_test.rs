//! `viewport_anchor` 单元测试：锚点身份 / 单元内位移 / 重定位与安全方向。
//!
//! 全部走生产函数（`ViewportAnchor::capture` / `relocate` / `PrependRelocation::target_for`）
//! 与真实 `SlotIndex`（同 `scroll_test.rs::index_of` 的构造口径：按槽位给视觉行高），
//! 不另造坐标换算。身份重定位的判据只有一条：**同一个可见单元在屏幕上的行号不变**，
//! 屏幕行 = `slot_visual_start(单元) − 视口偏移`。

use super::*;
use crate::kit::entry_render_cache::{EntryRenderContext, EntrySurface};
use crate::kit::message_area::grid::GridSpec;
use crate::kit::message_area::selection::{SlotLines, WrappedLineInfo};
use crate::kit::tui_render_unit::{FoldState, TuiToolCard, TuiToolPresentation, TuiUserBubble};
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::ratatui::text::Line;
use std::sync::Arc;

fn context() -> EntryRenderContext {
    crate::kit::atoms::init_atoms();
    crate::i18n::init(Some("en"));
    EntryRenderContext {
        theme: Arc::clone(&THEME_ATOM.state().read()),
        language: crate::kit::atoms::LANG_VERSION.get(),
        surface: EntrySurface::Message,
        occurrence: 0,
    }
}

/// 一屏索引：`heights[slot]` = 该槽位的视觉行数（模拟 wrap 后的布局），
/// `items[slot]` = 该槽位的渲染单元（锚点身份的来源）。
fn index(items: Vec<TuiRenderUnit>, heights: &[usize]) -> SlotIndex {
    assert_eq!(items.len(), heights.len(), "items 与槽位高必须一一对应");
    let grid = GridSpec::grid_for(80);
    let mut index = SlotIndex::default();
    index.set_source(im::Vector::from(items), grid, context(), 0);
    for (slot, height) in heights.iter().copied().enumerate() {
        let map = vec![WrappedLineInfo {
            logical_idx: 0,
            visual_start: 0,
            visual_end: height,
            slot_index: slot,
        }];
        index.set_slot(
            slot,
            Arc::new(SlotLines::single(Arc::new(vec![Line::from(format!(
                "slot-{slot}"
            ))]))),
            &map,
            true,
        );
    }
    index
}

fn bubble(text: &str) -> TuiRenderUnit {
    TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(text.to_string()))
}

fn tool_card(tool_id: &str) -> TuiRenderUnit {
    TuiRenderUnit::TuiToolCard(TuiToolCard {
        tool_id: tool_id.to_string(),
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
    })
}

/// 折叠组：成员 tool id 序列就是它的身份（`FoldKey::Group`，与折叠 pass 同口径）。
fn collapsed_group(member_ids: &[&str]) -> TuiRenderUnit {
    use crate::kit::tui_render_unit::TuiCollapsedGroup;
    let mut group = TuiCollapsedGroup {
        title: "Read".into(),
        count: member_ids.len() as u32,
        failed_count: 0,
        view_models: member_ids.iter().map(|id| tool_card(id)).collect(),
        fold: FoldState::Collapsed,
        content_hash: 0,
    };
    group.recompute_hash();
    TuiRenderUnit::TuiCollapsedGroup(group)
}

/// 屏幕行 = 单元视觉起始 − 视口偏移（对本测试的索引：起始即单元首行的视觉行号）。
fn screen_row(index: &SlotIndex, slot: usize, offset: usize) -> isize {
    index.slot_visual_start(slot).expect("单元视觉起始") as isize - offset as isize
}

// ── ① 身份 + 单元内位移：插入块把锚点单元整体下推 ──────────────────────────

#[test]
#[serial_test::serial]
fn capture_and_relocate_keep_the_anchored_unit_on_its_screen_row() {
    // 发布前：`[继承前缀(1 行), 自有(5 行)]`，视口顶行（scroll_y=3）落在自有单元内。
    let before = index(vec![bubble("继承"), bubble("own")], &[1, 5]);
    let anchor = ViewportAnchor::capture(&before, 3).expect("锚点可取");
    assert_eq!(anchor.offset_within_unit, 2, "3 − 单元起始 1");

    // 发布后：更早条目（4 行）插到继承前缀之后 → 自有单元整体下推 4 行。
    let after = index(
        vec![bubble("继承"), bubble("old"), bubble("own")],
        &[1, 4, 5],
    );
    let target = anchor.relocate(&after).expect("身份在新窗口里唯一");
    assert_eq!(target, 5 + 2, "新起始 5 + 单元内位移 2");
    assert_eq!(
        screen_row(&after, 2, target),
        screen_row(&before, 1, 3),
        "同一可见单元（自有）屏幕行不变（A20）"
    );
    // 重定位量恰好等于插入块高度（1 槽 = 4 行），不是别的几何量。
    assert_eq!(target as isize - 3, 4);
}

// ── ② 插入块在视口下方：重定位量 0（视口本就没被推动）────────────────────

#[test]
#[serial_test::serial]
fn relocate_is_zero_when_the_inserted_block_is_below_the_viewport() {
    // 发布前：`[A(4), B(4), D(4)]`，视口顶行 0 在 A。
    let before = index(vec![bubble("a"), bubble("b"), bubble("d")], &[4, 4, 4]);
    let anchor = ViewportAnchor::capture(&before, 0).expect("锚点可取");
    assert_eq!(anchor.offset_within_unit, 0);

    // 发布后：中间缺口页插到 B 之后 → A 的位置完全没变。
    let after = index(
        vec![bubble("a"), bubble("b"), bubble("mid"), bubble("d")],
        &[4, 4, 4, 4],
    );
    let target = anchor.relocate(&after).expect("身份仍唯一");
    assert_eq!(target, 0, "A 仍在视觉行 0 → 重定位量 0（不动视口）");
    assert_eq!(screen_row(&after, 0, target), screen_row(&before, 0, 0));
}

// ── ③ 折叠组：相邻工具卡压成一组导致槽位数变化（S7d 反例的本体）──────────

#[test]
#[serial_test::serial]
fn relocate_survives_neighbouring_tool_cards_collapsing_into_one_group() {
    // 发布前：`[tool t1(2), tool t2(2), tail(3)]`（3 槽），视口顶行 5 落在 tail 内。
    let before = index(
        vec![tool_card("t1"), tool_card("t2"), bubble("tail")],
        &[2, 2, 3],
    );
    let anchor = ViewportAnchor::capture(&before, 5).expect("锚点可取");
    assert_eq!(anchor.offset_within_unit, 1, "5 − tail 起始 4");

    // 发布后：t1/t2 被压成一张折叠组（2 槽）——**槽位数变少**，旧「桥接槽位号索引
    // 渲染槽位」的口径在这里必然错位；身份重定位不看槽位号，只看身份。
    let after = index(
        vec![collapsed_group(&["t1", "t2"]), bubble("tail")],
        &[3, 3],
    );
    let target = anchor.relocate(&after).expect("tail 身份在新窗口里唯一");
    assert_eq!(target, 3 + 1, "新起始 3 + 单元内位移 1");
    assert_eq!(
        screen_row(&after, 1, target),
        screen_row(&before, 2, 5),
        "tail 屏幕行不变（A20）——尽管槽位数从 3 变 2"
    );
    assert_eq!(target as isize - 5, -1, "重定位量 = 折叠把上方压掉的 1 行");
}

/// 反向形态：折叠组把**锚点自己**吞掉（原单元身份在新窗口里不存在）→
/// 不补偿（安全方向）。组身份是成员序列，与被吞掉的工具卡身份不同。
#[test]
#[serial_test::serial]
fn relocate_refuses_when_the_anchored_unit_was_absorbed_into_a_group() {
    let before = index(
        vec![tool_card("t1"), tool_card("t2"), bubble("tail")],
        &[2, 2, 3],
    );
    let anchor = ViewportAnchor::capture(&before, 3).expect("锚点落在 t2 内");
    assert_eq!(
        identity_of(&before.canonical_items().unwrap()[1]),
        UnitIdentity::Fold(FoldKey::Tool("t2".into())),
        "工具卡身份 = 既有折叠键"
    );

    let after = index(
        vec![collapsed_group(&["t1", "t2"]), bubble("tail")],
        &[3, 3],
    );
    assert_eq!(
        anchor.relocate(&after),
        None,
        "锚点被并入折叠组后身份消失 → 不补偿（宁可少补一次）"
    );
}

// ── ④ 安全方向：身份认不出 / 身份歧义 → 不补偿 ───────────────────────────

#[test]
#[serial_test::serial]
fn relocate_refuses_when_identity_is_missing_or_ambiguous() {
    let before = index(vec![bubble("继承"), bubble("own")], &[1, 5]);
    let anchor = ViewportAnchor::capture(&before, 3).expect("锚点可取");

    // 身份消失（rewind 替换 / 单元被移除）。
    let gone = index(vec![bubble("继承"), bubble("replaced")], &[1, 5]);
    assert_eq!(anchor.relocate(&gone), None, "身份不在新窗口 → 不补偿");

    // 身份歧义（新窗口里出现两个内容完全相同的单元）→ 不猜。
    let ambiguous = index(
        vec![bubble("继承"), bubble("own"), bubble("own")],
        &[1, 5, 5],
    );
    assert_eq!(anchor.relocate(&ambiguous), None, "身份不唯一 → 不补偿");

    // 空窗口（索引还没有内容）→ 不补偿。
    assert_eq!(
        ViewportAnchor::capture(&SlotIndex::default(), 0),
        None,
        "首帧 / 无快照 → 锚点取不到"
    );
    assert_eq!(anchor.relocate(&SlotIndex::default()), None);
}

/// 锚点单元在新布局里变矮到装不下原位移：不夹取、不额外补偿（有意保留的语义，
/// 见模块文档「仍未覆盖的边界」2.）。
#[test]
#[serial_test::serial]
fn relocate_does_not_clamp_a_within_unit_offset_that_outgrew_the_unit() {
    let before = index(vec![bubble("big")], &[10]);
    let anchor = ViewportAnchor::capture(&before, 9).expect("锚点可取");
    assert_eq!(anchor.offset_within_unit, 9);

    let after = index(vec![bubble("big")], &[2]);
    assert_eq!(
        anchor.relocate(&after),
        Some(9),
        "位移原样保留（视口顶行落到单元之外），不在重定位里偷偷夹取"
    );
}

// ── ⑤ 消费端口径：跟随态 / 底部哨兵不换算 ───────────────────────────────

#[test]
#[serial_test::serial]
fn target_for_skips_follow_bottom_and_bottom_sentinel() {
    let relocation = PrependRelocation {
        from_offset: 5,
        target_offset: 8,
    };
    assert_eq!(
        relocation.target_for(false, 5),
        Some(8),
        "浏览态：落到新目标"
    );
    assert_eq!(
        relocation.target_for(false, 6),
        Some(9),
        "相对量口径：frame 之间用户又滚了 1 行也不丢"
    );
    assert_eq!(
        relocation.target_for(false, 0),
        Some(3),
        "重定位量为负也按相对量走"
    );
    assert_eq!(
        PrependRelocation {
            from_offset: 5,
            target_offset: 3,
        }
        .target_for(false, 0),
        Some(0),
        "目标为负 → 下界 0（不 panic、不 wrap）"
    );
    assert_eq!(
        relocation.target_for(true, 5),
        None,
        "跟随态看的是尾部 → 不换算（吸底自会处理）"
    );
    assert_eq!(
        relocation.target_for(false, usize::MAX),
        None,
        "usize::MAX 是「跟随到底」哨兵 → 不参与偏移换算"
    );
}
