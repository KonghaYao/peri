//! [F5] 历史前插后按稳定身份重定位选区起点。

use super::*;
use ratatui_kit::ratatui::text::Span;

fn make_line(s: &str) -> Line<'static> {
    Line::from(vec![Span::raw(s.to_string())])
}

// ── [F5] 历史前插后按稳定身份重定位选区起点 ─────────────────────────────

fn anchor_test_context() -> crate::kit::entry_render_cache::EntryRenderContext {
    use crate::kit::atoms::LANG_VERSION;
    use crate::kit::entry_render_cache::{EntryRenderContext, EntrySurface};
    use peri_theme::atoms::THEME_ATOM;
    crate::kit::atoms::init_atoms();
    crate::i18n::init(Some("en"));
    EntryRenderContext {
        theme: Arc::clone(&THEME_ATOM.state().read()),
        language: LANG_VERSION.get(),
        surface: EntrySurface::Message,
        occurrence: 0,
    }
}

fn anchor_test_bubble(text: &str) -> crate::kit::tui_render_unit::TuiRenderUnit {
    crate::kit::tui_render_unit::TuiRenderUnit::TuiUserBubble(
        crate::kit::tui_render_unit::TuiUserBubble::new(text.to_string()),
    )
}

/// 每个 slot 恰好一个视觉行；行内容由调用方给出（身份来自 `items[slot]`）。
fn anchored_index(
    items: Vec<crate::kit::tui_render_unit::TuiRenderUnit>,
    lines: Vec<Vec<Line<'static>>>,
) -> SlotIndex {
    assert_eq!(items.len(), lines.len());
    let grid = GridSpec::grid_for(80);
    let mut index = SlotIndex::default();
    index.set_source(im::Vector::from(items), grid, anchor_test_context(), 0);
    for (slot, slot_lines) in lines.into_iter().enumerate() {
        let map = vec![WrappedLineInfo {
            logical_idx: 0,
            visual_start: 0,
            visual_end: 1,
            slot_index: slot,
        }];
        index.set_slot(
            slot,
            Arc::new(SlotLines::single(Arc::new(slot_lines))),
            &map,
            true,
        );
    }
    index
}

#[test]
#[serial_test::serial]
fn test_selection_anchor_relocates_after_history_prepend() {
    let before = anchored_index(
        vec![anchor_test_bubble("history"), anchor_test_bubble("old")],
        vec![vec![make_line("HIST")], vec![make_line("ABCDEF")]],
    );
    let anchor = capture_selection_anchor(&before, (1, 1)).expect("有 canonical items");
    let mut selection = TextSelection::new();
    selection.start_drag_anchored(1, 1, Some(anchor));
    selection.update_drag(1, 5);

    let mut items = (0..10)
        .map(|i| anchor_test_bubble(&format!("HIST_{i:02}")))
        .collect::<Vec<_>>();
    items.push(anchor_test_bubble("history"));
    items.push(anchor_test_bubble("old"));
    let mut lines = (0..10)
        .map(|i| vec![make_line(&format!("HIST_{i:02}"))])
        .collect::<Vec<_>>();
    lines.push(vec![make_line("HIST")]);
    lines.push(vec![make_line("ABCDEF")]);
    let after = anchored_index(items, lines);

    assert!(matches!(
        relocate_selection_start(&after, &mut selection),
        AnchorResolution::Resolved
    ));
    assert_eq!(selection.start, Some((11, 1)), "原单元下移 10 行");
    // Up 按当前滚动偏移重算终点：用户指针未动，同一屏幕位置对应新的
    // 视觉行（原内容下移 10 行）。
    selection.update_drag(11, 5);
    let ((sr, sc), (er, ec)) = resolved_bounds(&after, &selection).unwrap();
    assert_eq!(
        extract_visual_range_index(&after, (sr, sc), (er, ec), 80, None, None).as_deref(),
        Some("BCDE"),
        "同一屏幕位置释放仍复制原文字，而不是前插的历史内容"
    );
}

#[test]
#[serial_test::serial]
fn test_selection_anchor_missing_or_ambiguous_cancels() {
    let anchor = {
        let index = anchored_index(
            vec![anchor_test_bubble("history"), anchor_test_bubble("old")],
            vec![vec![make_line("HIST")], vec![make_line("ABCDEF")]],
        );
        capture_selection_anchor(&index, (1, 1)).expect("anchor")
    };

    // 身份不存在（单元被淘汰/替换）→ 明确取消。
    let missing = anchored_index(
        vec![anchor_test_bubble("history")],
        vec![vec![make_line("HIST")]],
    );
    assert_eq!(resolve_selection_anchor(&missing, &anchor), None);
    let mut selection = TextSelection::new();
    selection.start_drag_anchored(1, 1, Some(anchor.clone()));
    assert!(matches!(
        relocate_selection_start(&missing, &mut selection),
        AnchorResolution::Lost
    ));

    // 同一身份出现两次（两条内容相同的单元）→ 歧义，不猜。
    let ambiguous = anchored_index(
        vec![
            anchor_test_bubble("old"),
            anchor_test_bubble("history"),
            anchor_test_bubble("old"),
        ],
        vec![
            vec![make_line("ABCDEF")],
            vec![make_line("HIST")],
            vec![make_line("ABCDEF")],
        ],
    );
    assert_eq!(resolve_selection_anchor(&ambiguous, &anchor), None);
}

#[test]
#[serial_test::serial]
fn test_selection_anchor_rejects_row_offset_beyond_unit() {
    let anchor = {
        SelectionAnchor {
            unit_kind: crate::kit::message_area::window_identity_token(&anchor_test_bubble(
                "history",
            ))
            .kind,
            unit_hash: crate::kit::message_area::window_identity_token(&anchor_test_bubble(
                "history",
            ))
            .hash,
            row_offset: 3,
            column: 0,
        }
    };
    let index = anchored_index(
        vec![anchor_test_bubble("history")],
        vec![vec![make_line("HIST")]],
    );
    assert_eq!(
        resolve_selection_anchor(&index, &anchor),
        None,
        "单元变矮后偏移越界 → 取消"
    );
}

#[test]
fn test_text_selection_clear_drops_anchor() {
    let mut selection = TextSelection::new();
    selection.start_drag_anchored(
        1,
        1,
        Some(SelectionAnchor {
            unit_kind: 1,
            unit_hash: 2,
            row_offset: 0,
            column: 1,
        }),
    );
    assert!(selection.anchor.is_some());
    selection.clear();
    assert!(selection.anchor.is_none());
    assert!(!selection.dragging);
    assert_eq!(selection.start, None);
}
