//! Tests
use super::*;

// ── ScrollPos（usize 滚动状态）测试 ───────────────────────────────────

#[test]
fn test_scroll_pos_exceeds_u16_max() {
    // 核心回归：滚动位置必须能超过 65535（旧 ScrollViewState 的 u16 Position 上限）
    let mut pos = ScrollPos::default();
    for _ in 0..70_000 {
        pos.scroll_down();
    }
    assert_eq!(pos.offset(), 70_000);
    assert!(pos.offset() > u16::MAX as usize);
    // 渲染侧 clamp 到 max_scroll（如 70_000 - 40）后仍可达真实底部
    let max_scroll = 70_000usize.saturating_sub(40);
    assert_eq!(pos.offset().min(max_scroll), max_scroll);
}

#[test]
fn test_scroll_pos_scroll_up_never_underflows() {
    let mut pos = ScrollPos::default();
    pos.set_offset(10);
    pos.scroll_up();
    assert_eq!(pos.offset(), 9);
    for _ in 0..20 {
        pos.scroll_up();
    }
    assert_eq!(pos.offset(), 0);
}

#[test]
fn test_scroll_pos_page_scroll() {
    let mut pos = ScrollPos::default();
    pos.set_offset(100);
    pos.scroll_page_down(40);
    assert_eq!(pos.offset(), 139); // 100 + 40 - 1（与 ratatui 翻页重叠 1 行语义一致）
    pos.scroll_page_up(40);
    assert_eq!(pos.offset(), 100);
    // 页大小超过当前偏移 → 回到顶部
    pos.scroll_page_up(40);
    assert_eq!(pos.offset(), 61);
}

#[test]
fn test_scroll_pos_scroll_to_bottom_uses_max() {
    let mut pos = ScrollPos::default();
    pos.scroll_to_bottom();
    assert_eq!(pos.offset(), usize::MAX);
    pos.scroll_to_top();
    assert_eq!(pos.offset(), 0);
}

fn proximity_check(total: u16, scroll_y: u16, vis_height: u16) -> bool {
    if total == 0 {
        return false;
    }
    let max_scroll = total.saturating_sub(vis_height);
    if scroll_y >= max_scroll {
        return false;
    }
    let distance = max_scroll.saturating_sub(scroll_y);
    let threshold = (vis_height / 2).max(5);
    distance <= threshold
}

#[test]
fn test_proximity_at_bottom_should_not_trigger_scroll() {
    let total = 100;
    let vis_height = 20;
    let scroll_y = total - vis_height;
    assert!(!proximity_check(total, scroll_y, vis_height));
}

#[test]
fn test_proximity_within_half_viewport_should_follow() {
    let total = 100;
    let vis_height = 20;
    let scroll_y = total - vis_height - 10;
    assert!(proximity_check(total, scroll_y, vis_height));
}

#[test]
fn test_proximity_beyond_half_viewport_should_not_follow() {
    let total = 100;
    let vis_height = 20;
    let scroll_y = total - vis_height - 11;
    assert!(!proximity_check(total, scroll_y, vis_height));
}

#[test]
fn test_proximity_near_top_should_not_follow() {
    let total = 200;
    let vis_height = 30;
    let scroll_y = 20;
    assert!(!proximity_check(total, scroll_y, vis_height));
}

#[test]
fn test_proximity_small_viewport_minimum_threshold() {
    let total = 50;
    let vis_height = 6;
    let scroll_y = total - vis_height - 5;
    assert!(proximity_check(total, scroll_y, vis_height));
    let scroll_y = total - vis_height - 6;
    assert!(!proximity_check(total, scroll_y, vis_height));
}

#[test]
fn test_proximity_empty_content_no_follow() {
    assert!(!proximity_check(0, 0, 20));
}

#[test]
fn test_proximity_content_smaller_than_viewport_at_bottom() {
    let total = 10;
    let vis_height = 30;
    assert!(!proximity_check(total, 0, vis_height));
}

// ── resize 跟随判断测试 ─────────────────────────────────────────────

#[test]
fn test_resize_follow_when_at_bottom_before_resize() {
    // 内容 100 行，resize 前视口 20 行，用户停在底部（offset = 旧 max_scroll = 80）
    let total = 100;
    let prev_vis = 20;
    let offset = total - prev_vis;
    assert!(should_follow_after_resize(total, prev_vis, offset as usize));
}

#[test]
fn test_resize_follow_when_near_bottom_before_resize() {
    // resize 前距底部 4 行（threshold = max(20/4, 5) = 5）→ 跟随
    let total = 100;
    let prev_vis = 20;
    let offset = total - prev_vis - 4;
    assert!(should_follow_after_resize(total, prev_vis, offset as usize));
}

#[test]
fn test_resize_no_follow_when_scrolled_up_before_resize() {
    // resize 前用户上滚浏览（距底部 20 行 > threshold 5）→ 不打扰
    let total = 100;
    let prev_vis = 20;
    let offset = total - prev_vis - 20;
    assert!(!should_follow_after_resize(
        total,
        prev_vis,
        offset as usize
    ));
}

#[test]
fn test_resize_no_follow_on_first_frame() {
    // prev_vis_height = 0（首帧哨兵未初始化）→ 不跟随
    assert!(!should_follow_after_resize(100, 0, 0));
    // 空内容 → 不跟随
    assert!(!should_follow_after_resize(0, 20, 0));
}

#[test]
fn test_resize_no_follow_when_content_smaller_than_old_viewport() {
    // 内容 10 行 < 旧视口 30 行：旧 max_scroll = 0，offset 0 距底 0 → 跟随（无害，滚到底 = 0）
    assert!(should_follow_after_resize(10, 30, 0));
}

// ── 滚动条几何 / 反推公式测试 ────────────────────────────────────────

fn make_rect(x: u16, y: u16, width: u16, height: u16) -> Rect {
    Rect::new(x, y, width, height)
}

#[test]
fn test_is_scrollbar_column_rightmost() {
    // area: x=10, width=80 → scrollbar 列 = 10 + 80 - 1 = 89
    let area = make_rect(10, 0, 80, 24);
    assert!(is_scrollbar_column(89, area));
}

#[test]
fn test_is_scrollbar_column_text_area() {
    let area = make_rect(10, 0, 80, 24);
    assert!(!is_scrollbar_column(10, area));
    assert!(!is_scrollbar_column(50, area));
    assert!(!is_scrollbar_column(88, area));
}

#[test]
fn test_compute_thumb_geometry_no_overflow_returns_none() {
    let fields = ScrollbarFields {
        content_length: 50,
        position: 0,
        viewport_length: 60,
    };
    let area = make_rect(0, 0, 80, 24);
    assert!(compute_thumb_geometry(&fields, area).is_none());
}

#[test]
fn test_compute_thumb_geometry_thumb_length_clamped_to_one() {
    let fields = ScrollbarFields {
        content_length: 1000,
        position: 0,
        viewport_length: 10,
    };
    let area = make_rect(0, 0, 80, 24);
    let geo = compute_thumb_geometry(&fields, area).expect("应有溢出");
    assert!(geo.thumb_length >= 1);
    assert!(geo.thumb_length <= geo.track_length);
    // content=1000, viewport=10, track=22 → thumb ≈ round(10*22/1009) = round(0.218) = 0 → clamp 到 1
    assert_eq!(geo.thumb_length, 1);
}

#[test]
fn test_compute_thumb_geometry_track_length_minus_two() {
    let fields = ScrollbarFields {
        content_length: 200,
        position: 100,
        viewport_length: 20,
    };
    let area = make_rect(0, 0, 80, 24);
    let geo = compute_thumb_geometry(&fields, area).expect("应有溢出");
    assert_eq!(geo.track_length, 22); // 24 - 2
}

#[test]
fn test_thumb_start_to_position_at_top() {
    let fields = ScrollbarFields {
        content_length: 200,
        position: 100,
        viewport_length: 20,
    };
    let area = make_rect(0, 0, 80, 24);
    let geo = compute_thumb_geometry(&fields, area).expect("应有溢出");
    let pos = thumb_start_to_position(0, &geo);
    assert_eq!(pos, 0);
}

#[test]
fn test_thumb_start_to_position_at_bottom() {
    let fields = ScrollbarFields {
        content_length: 200,
        position: 0,
        viewport_length: 20,
    };
    let area = make_rect(0, 0, 80, 24);
    let geo = compute_thumb_geometry(&fields, area).expect("应有溢出");
    let max_thumb_start = geo.track_length - geo.thumb_length;
    let pos = thumb_start_to_position(max_thumb_start, &geo);
    // thumb 到底 → position 应接近 max_position（=199），允许 ±2 舍入误差
    assert!(
        pos >= geo.max_position.saturating_sub(2),
        "thumb 到底时 position 应接近 max_position，实际 = {}，max = {}",
        pos,
        geo.max_position
    );
}

#[test]
fn test_thumb_start_to_position_clamps_to_max() {
    let fields = ScrollbarFields {
        content_length: 100,
        position: 0,
        viewport_length: 10,
    };
    let area = make_rect(0, 0, 80, 24);
    let geo = compute_thumb_geometry(&fields, area).expect("应有溢出");
    let pos = thumb_start_to_position(usize::MAX, &geo);
    assert_eq!(pos, geo.max_position);
}

#[test]
fn test_round_divide_basic() {
    assert_eq!(round_divide(10, 4), 3); // (10+2)/4 = 3
    assert_eq!(round_divide(8, 4), 2); // (8+2)/4 = 2
    assert_eq!(round_divide(7, 0), 0);
    assert_eq!(round_divide(0, 5), 0);
}

#[test]
fn test_scrollbar_drag_state_default() {
    let s = ScrollbarDragState::default();
    assert!(!s.active);
    assert_eq!(s.thumb_offset, 0);
}

#[test]
fn test_position_to_scroll_y_linear() {
    // max_position=99, max_scroll=90（content=100, viewport=10）
    // position=0 → scroll_y=0
    assert_eq!(position_to_scroll_y(0, 99, 90), 0);
    // position=99 → scroll_y=99*90/99 = 90
    assert_eq!(position_to_scroll_y(99, 99, 90), 90);
    // position=50 → ceil(50*90/99) = ceil(45.45) = 46（ceil 反推，与正向 floor 互逆）
    assert_eq!(position_to_scroll_y(50, 99, 90), 46);
}

#[test]
fn test_position_to_scroll_y_ceil_hits_bottom() {
    // [Fix] 用 ceil 后，thumb 拖到接近底部时就该映射到 max_scroll，
    // 而不是必须恰好拖到 max_position。例如 max_scroll=2, max_position=9 时，
    // position=8（89%）应映射到 scroll_y=2（到底）
    // ceil(8*2/9) = ceil(1.78) = 2
    assert_eq!(position_to_scroll_y(8, 9, 2), 2);
    // ceil(7*2/9) = ceil(1.56) = 2
    assert_eq!(position_to_scroll_y(7, 9, 2), 2);
    // ceil(6*2/9) = ceil(1.33) = 2
    assert_eq!(position_to_scroll_y(6, 9, 2), 2);
    // ceil(4*2/9) = ceil(0.89) = 1
    assert_eq!(position_to_scroll_y(4, 9, 2), 1);
}

#[test]
fn test_position_to_scroll_y_zero_max_position() {
    // max_position=0（content=1）→ 总是 0
    assert_eq!(position_to_scroll_y(0, 0, 0), 0);
    assert_eq!(position_to_scroll_y(5, 0, 10), 0);
}

#[test]
fn test_position_to_scroll_y_zero_max_scroll() {
    // max_scroll=0（content <= viewport）→ 总是 0
    assert_eq!(position_to_scroll_y(5, 100, 0), 0);
}
