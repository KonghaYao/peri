//! `shared::list` 视口与命中的一致性回归。

use ratatui_kit::ratatui::layout::Rect;

use super::{ListMetrics, ListViewport, content_rows};
use crate::kit::panel_mouse::hit_row;
use crate::kit::panel_overlay::panel_content_rows;

/// 面板区域（含上下边框），命中行号以它为准。
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
    for (slot, expected) in visible.iter().enumerate() {
        let visual_row = layout.header_rows + (slot as u16) * layout.item_rows;
        let mouse_row = rect.y + 1 + visual_row;
        assert_eq!(
            hit_row(mouse_row, rect, layout),
            Some(*expected),
            "第 {slot} 个可见槽位命中索引应与 window 一致"
        );
    }
}

#[test]
fn row_beyond_rendered_items_does_not_hit() {
    let rect = area(14);
    // 列表只有 2 项：footer 从第 3 个槽位起不可点
    let vp = ListViewport::for_content_rows(12, METRICS, 2, 0);
    let layout = vp.hit_layout();
    let third_slot_row = rect.y + 1 + layout.header_rows + 2 * layout.item_rows;
    assert_eq!(hit_row(third_slot_row, rect, layout), None);
}

#[test]
fn first_frame_prediction_is_not_empty() {
    // 首帧 AreaTracker 未测量（Rect::default()）：不能按 0 行渲染空面板
    let predicted = content_rows(None, 30);
    assert_eq!(predicted, panel_content_rows(30));
    assert!(predicted > 0);
}

#[test]
fn measured_height_wins_over_prediction() {
    let measured = Some(area(18));
    assert_eq!(content_rows(measured, 30), 16);
}
