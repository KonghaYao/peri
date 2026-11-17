//! `shared::list` 视口与命中的一致性回归。

use ratatui_kit::ratatui::layout::Rect;

use super::ListMetrics;
use super::ListViewport;

/// 面板区域（含上下边框），命中行号以它为准。
#[must_use]
fn area(height: u16) -> Rect {
    Rect::new(0, 0, 60, height)
}

const METRICS: ListMetrics = ListMetrics::new(3, 3, 0);

#[test]
fn visible_items_follow_content_rows() {
    // 内容区 12 行：3 行 header + 3×3 行 item
    let short = ListViewport::for_content_rows(12, METRICS, 20, 0);
    assert_eq!(short.visible_items, 3);

    // 终端变高 → 可见项数必须跟着变，不能停在写死的常量上
    let tall = ListViewport::for_content_rows(24, METRICS, 20, 0);
    assert_eq!(tall.visible_items, 7);
    assert!(tall.visible_items > short.visible_items);
}

#[test]
fn visible_items_saturate_on_tiny_panel() {
    let tiny = ListViewport::for_content_rows(3, METRICS, 20, 0);
    assert_eq!(tiny.visible_items, 0);
    assert_eq!(tiny.window(&(0..20).collect::<Vec<_>>()).count(), 0);
}

#[test]
fn selection_follows_and_clamps_after_shrink() {
    // selected 超出刷新后的列表长度：收敛到末项，不留下越界选择
    let vp = ListViewport::for_content_rows(12, METRICS, 3, 9);
    assert_eq!(vp.scroll_start, 0);
    assert_eq!(vp.window(&[0, 1, 2]).count(), 3);

    let long = ListViewport::for_content_rows(12, METRICS, 20, 19);
    assert_eq!(long.scroll_start, 17);
}

#[test]
fn window_and_hit_layout_agree() {
    let rect = area(14); // 内容区 12 行
    let items: Vec<usize> = (0..10).collect();
    let vp = ListViewport::for_content_rows(12, METRICS, items.len(), 5);
    let layout = vp.hit_layout();

    let visible: Vec<usize> = vp.window(&items).map(|(i, _)| i).collect();
    assert_eq!(visible, vec![4, 5, 6]);

    // 每个可见项的第一行命中同一个全量索引——绘制窗口与命中布局同源
    {
        let iteration_items = visible.iter().enumerate();
        for (slot, expected) in iteration_items {
            let visual_row = layout.header_rows + (slot as u16) * layout.item_rows;
            let mouse_row = rect.y + 1 + visual_row;
            assert_eq!(
                crate::panel_mouse::hit_row(mouse_row, rect, layout),
                Some(*expected),
                "第 {slot} 个可见槽位命中索引应与 window 一致"
            );
        }
    }
}

#[test]
fn row_beyond_rendered_items_does_not_hit() {
    let rect = area(14);
    // 列表只有 2 项：footer 从第 3 个槽位起不可点
    let vp = ListViewport::for_content_rows(12, METRICS, 2, 0);
    let layout = vp.hit_layout();
    let third_slot_row = rect.y + 1 + layout.header_rows + 2 * layout.item_rows;
    assert_eq!(
        crate::panel_mouse::hit_row(third_slot_row, rect, layout),
        None
    );
}

#[test]
fn zero_row_height_normalizes_for_both_drawing_and_hits() {
    let items: Vec<usize> = (0..20).collect();
    let rect = area(8);
    let viewport = ListViewport::for_content_rows(6, ListMetrics::new(1, 0, 1), 20, 2);
    let layout = viewport.hit_layout();
    let visible: Vec<usize> = viewport.window(&items).map(|(index, _)| index).collect();
    assert_eq!(visible, vec![1, 2, 3, 4]);
    assert_eq!(layout.item_rows, 1);
    for (slot, expected) in visible.into_iter().enumerate() {
        let row = rect.y + 1 + layout.header_rows + slot as u16;
        assert_eq!(
            crate::panel_mouse::hit_row(row, rect, layout),
            Some(expected)
        );
    }
    let footer_row = rect.y + 1 + layout.header_rows + layout.visible_items;
    assert_eq!(crate::panel_mouse::hit_row(footer_row, rect, layout), None);
}

#[test]
fn oversized_header_and_footer_leave_no_drawable_or_hittable_rows() {
    let viewport =
        ListViewport::for_content_rows(12, ListMetrics::new(u16::MAX, 1, u16::MAX), 10, 0);
    assert_eq!(viewport.visible_items, 0);
    assert_eq!(viewport.window(&[(); 10]).count(), 0);
    let rect = area(14);
    for row in rect.y..rect.y + rect.height {
        assert_eq!(
            crate::panel_mouse::hit_row(row, rect, viewport.hit_layout()),
            None
        );
    }
}

#[test]
fn large_list_indices_survive_hit_testing_without_terminal_coordinate_truncation() {
    let items = vec![(); 70_010];
    let rect = area(12);
    let viewport =
        ListViewport::for_content_rows(10, ListMetrics::new(0, 1, 0), items.len(), 70_000);
    assert!(viewport.scroll_start > usize::from(u16::MAX));
    for (slot, (expected, _)) in viewport.window(&items).enumerate() {
        let row = rect.y + 1 + slot as u16;
        assert_eq!(
            crate::panel_mouse::hit_row(row, rect, viewport.hit_layout()),
            Some(expected),
        );
    }
}
