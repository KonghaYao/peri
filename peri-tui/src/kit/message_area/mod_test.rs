//! Tests

use super::*;
use ratatui_kit::ratatui::layout::Rect;

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
