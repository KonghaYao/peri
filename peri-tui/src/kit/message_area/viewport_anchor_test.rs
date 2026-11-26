//! `viewport_anchor` 单元测试：锚点身份 / 单元内位移 / 重定位与安全方向。
//!
//! 全部走生产函数（`ViewportAnchor::capture` / `relocate` / `PrependRelocation::target_for`）
//! 与真实 `SlotIndex`（同 `scroll_test.rs::index_of` 的构造口径：按槽位给视觉行高），
//! 不另造坐标换算。身份重定位的判据只有一条：**同一个可见单元在屏幕上的行号不变**，
//! 屏幕行 = `slot_visual_start(单元) − 视口偏移`。

use super::PrependRelocation;
use super::SlotIndex;
use super::TuiRenderUnit;
use super::ViewportAnchor;

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
    {
        let iteration_items = heights.iter().copied().enumerate();
        for (slot, height) in iteration_items {
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
    }
    index
}

fn bubble(text: &str) -> TuiRenderUnit {
    TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(text.to_string()))
}

fn tool_card(tool_id: &str) -> TuiRenderUnit {
    TuiRenderUnit::TuiToolCard(TuiToolCard {
        images: Vec::new(),
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

// ── ③ 折叠：相邻工具卡收束为单行后上方高度减少（身份重定位不看槽位号）────

/// 相邻工具卡从展开收束为单行（每个工具高度变小）：tail 的屏幕行不变（A20）。
#[test]
#[serial_test::serial]
fn relocate_survives_neighbouring_tool_cards_collapsing_to_single_lines() {
    // 发布前：`[tool t1(3 行), tool t2(3 行), tail(3 行)]`，视口顶行 7 落在 tail 内。
    let before = index(
        vec![tool_card("t1"), tool_card("t2"), bubble("tail")],
        &[3, 3, 3],
    );
    let anchor = ViewportAnchor::capture(&before, 7).expect("锚点落在 tail 内");
    assert_eq!(anchor.offset_within_unit, 1, "7 − tail 起始 6");

    // 发布后：两张工具卡分别收束为 1 行——槽位数不变，但上方高度减少。
    let after = index(
        vec![tool_card("t1"), tool_card("t2"), bubble("tail")],
        &[1, 1, 3],
    );
    let target = anchor.relocate(&after).expect("tail 身份在新窗口里唯一");
    assert_eq!(target, 2 + 1, "新起始 2 + 单元内位移 1");
    assert_eq!(
        screen_row(&after, 2, target),
        screen_row(&before, 2, 7),
        "tail 屏幕行不变（A20）——尽管上方工具高度变小"
    );
    assert_eq!(target as isize - 7, -4, "重定位量 = 上方两张卡各压掉 2 行");
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

// ── 同条消息内的阅读锚点（图完成/缩放/异步结果） ─────────────────────────

use super::ReadingAnchor;
use crate::kit::markdown::{
    FenceClose, FenceKind, MermaidFence, MermaidFenceKey, MermaidMessageIdentity, MermaidMessageRef,
};
use crate::kit::mermaid::FenceSpan;
use crate::kit::tui_render_unit::TuiAssistantBubble;

fn fence_key(name: &str, open_start: usize) -> MermaidFenceKey {
    MermaidFenceKey {
        message: MermaidMessageIdentity {
            session_id: "session".to_string(),
            message: MermaidMessageRef::Persistent(name.to_string()),
        },
        open_start,
    }
}

fn fence_span(name: &str, open_start: usize, start: usize, end: usize) -> FenceSpan {
    FenceSpan {
        key: fence_key(name, open_start),
        fence: Arc::new(MermaidFence {
            kind: FenceKind::Backtick,
            marker_len: 3,
            language: "mermaid".to_string(),
            info: "mermaid".to_string(),
            open_start,
            body_start: 0,
            body_end: 0,
            end: 0,
            close: FenceClose::Closed,
            opener_confirmed: true,
            opening_line: "```mermaid".to_string(),
            raw: String::new(),
            source: "graph TD\nA-->B".to_string(),
        }),
        width: 80,
        frozen: true,
        logical: start..end,
        layout_bytes: 0,
        prefix: 4,
        h_view: None,
    }
}

fn reading_index(
    lines: usize,
    text: &str,
) -> (SlotIndex, super::super::vm_cache_slot::VmCacheSlot) {
    let vm = TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
            started_at: None,
            duration_ms: None,
            text: text.to_string(),
            reasoning: None,
            message_id: Some("reading".to_string()),
            content_hash: 7,
        }
        .into(),
    );
    let grid = GridSpec::grid_for(80);
    let mut index = SlotIndex::default();
    index.set_source(im::Vector::from(vec![vm]), grid, context(), 0);
    let slot_lines: Vec<Line<'static>> = (0..lines)
        .map(|line| Line::from(format!("line-{line}")))
        .collect();
    let map: Vec<WrappedLineInfo> = (0..lines)
        .map(|line| WrappedLineInfo {
            logical_idx: line,
            visual_start: line,
            visual_end: line + 1,
            slot_index: 0,
        })
        .collect();
    index.set_slot(
        0,
        Arc::new(SlotLines::single(Arc::new(slot_lines))),
        &map,
        true,
    );
    let cache = super::super::vm_cache_slot::VmCacheSlot {
        wrap_map: Arc::new(map),
        ..Default::default()
    };
    (index, cache)
}

#[test]
fn reading_anchor_keeps_a_text_line_after_a_diagram_grows() {
    let (before, mut cache) = reading_index(10, "text");
    cache.mermaid_fences = vec![fence_span("m", 10, 2, 5)];
    // 顶行 3 在围栏内第 1 行。
    let anchor = ReadingAnchor::capture(&before, 3, &[cache]).expect("anchor");
    assert_eq!(
        anchor.relocate(
            &before,
            &[{
                let (_, mut c) = reading_index(10, "text");
                c.mermaid_fences = vec![fence_span("m", 10, 2, 5)];
                c
            }]
        ),
        Some(3)
    );

    // 围栏长到 5..9，下面的正文整体下移 4 行；顶行若是正文（原第 5 行
    // 之前的围栏外行）保持相对围栏的位置。
    let (grown, mut grown_cache) = reading_index(14, "text");
    grown_cache.mermaid_fences = vec![fence_span("m", 10, 2, 9)];
    // 原顶行 1 = 围栏前第 1 行；新布局仍应是围栏前第 1 行。
    let before_fence = ReadingAnchor::capture(
        &before,
        1,
        &[{
            let (_, mut c) = reading_index(10, "text");
            c.mermaid_fences = vec![fence_span("m", 10, 2, 5)];
            c
        }],
    )
    .expect("before-fence anchor");
    assert_eq!(before_fence.relocate(&grown, &[grown_cache]), Some(1));
}

#[test]
fn reading_anchor_finds_the_same_paragraph_after_80_40_80_roundtrip() {
    let (wide, mut wide_cache) = reading_index(12, "text");
    wide_cache.mermaid_fences = vec![
        fence_span("first", 1, 2, 5),
        fence_span("second", 20, 8, 10),
    ];
    // 顶行 10 在第二张图之后第 0 行。
    let wide_caches = vec![wide_cache];
    let anchor = ReadingAnchor::capture(&wide, 10, &wide_caches).expect("anchor");

    let (narrow, mut narrow_cache) = reading_index(30, "text");
    narrow_cache.mermaid_fences = vec![
        fence_span("first", 1, 2, 9),
        fence_span("second", 20, 20, 26),
    ];
    let narrow_caches = vec![narrow_cache];
    assert_eq!(
        anchor.relocate(&narrow, &narrow_caches),
        Some(26),
        "40 列时仍落在第二张图后的第一行"
    );

    assert_eq!(
        anchor.relocate(&wide, &wide_caches),
        Some(10),
        "回到 80 列时回到原行"
    );
    let text = crate::kit::message_area::selection::extract_visual_range_index(
        &wide,
        (10, 0),
        (10, 10),
        80,
        wide.canonical_items(),
        Some(GridSpec::grid_for(80)),
    );
    assert!(
        text.is_some_and(|text| text.contains("line-10")),
        "同一阅读位置仍可选中原段落"
    );
}
