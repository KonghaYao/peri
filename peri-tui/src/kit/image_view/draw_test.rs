//! 共享绘制几何：水平居中、区域越界拒绝、视口矩形折算。

use ratatui::layout::Rect;

use super::draw::{centered_rect, centered_x, screen_rect};

#[test]
fn centered_rect_centers_horizontally_and_aligns_top() {
    let area = Rect::new(5, 3, 40, 10);
    assert_eq!(
        centered_rect(area, 20, 4),
        Some(Rect::new(15, 3, 20, 4)),
        "水平居中、顶部对齐"
    );
    assert_eq!(centered_x(40, 20), 10);
    assert_eq!(centered_x(20, 40), 0, "不产生负偏移");
}

#[test]
fn centered_rect_rejects_overflow_and_empty() {
    let area = Rect::new(0, 0, 10, 4);
    assert_eq!(centered_rect(area, 11, 2), None, "宽超出区域");
    assert_eq!(centered_rect(area, 2, 5), None, "高超出区域");
    assert_eq!(centered_rect(area, 0, 2), None);
    assert_eq!(centered_rect(area, 2, 0), None);
}

#[test]
fn screen_rect_maps_viewport_rows_and_rejects_edges() {
    let area = Rect::new(2, 4, 60, 20);
    assert_eq!(
        screen_rect(area, 6, 3, 10, 20, 3),
        Some(Rect::new(12, 7, 20, 3)),
        "屏幕行 = 区域顶 + 视觉行 - 滚动偏移"
    );
    assert_eq!(screen_rect(area, 2, 3, 0, 20, 3), None, "滚出视口上方不画");
    assert_eq!(screen_rect(area, 22, 0, 0, 20, 3), None, "越过区域底部不画");
    assert_eq!(screen_rect(area, 4, 0, 50, 20, 3), None, "越过区域右侧不画");
    assert_eq!(screen_rect(area, 4, 0, 0, 0, 3), None);
}
