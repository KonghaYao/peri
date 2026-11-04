use super::*;

#[test]
fn virtual_scroll_keeps_full_history_beyond_terminal_coordinates() {
    let mut offset = 70_000;
    apply_scroll_to_virtual(&mut offset, 3, 100_000, 20);
    assert_eq!(offset, 70_003);
    apply_scroll_to_virtual(&mut offset, i32::MAX, 100_000, 20);
    assert_eq!(offset, 99_980);
}

#[test]
fn virtual_scroll_clamps_top_bottom_and_shrinking_history() {
    let mut offset = 2;
    apply_scroll_to_virtual(&mut offset, -3, 100, 20);
    assert_eq!(offset, 0);
    apply_scroll_to_virtual(&mut offset, 100, 100, 20);
    assert_eq!(offset, 80);
    apply_scroll_to_virtual(&mut offset, 0, 30, 20);
    assert_eq!(offset, 10);
    apply_scroll_to_virtual(&mut offset, 3, 10, 20);
    assert_eq!(offset, 0);
}

#[test]
fn virtual_scroll_handles_signed_and_usize_extremes() {
    let mut offset = usize::MAX - 1;
    apply_scroll_to_virtual(&mut offset, 3, usize::MAX, 0);
    assert_eq!(offset, usize::MAX);
    apply_scroll_to_virtual(&mut offset, i32::MIN, usize::MAX, 0);
    assert_eq!(offset, usize::MAX - i32::MIN.unsigned_abs() as usize);
}

/// 关键回归：同一毫秒到达的两个滚轮事件必须各落地一次 `SCROLL_LINES`，
/// 而不是按时间窗合并成一次 2×`SCROLL_LINES` 跳变。
/// 边界：这里验证驱动层逐事件调用语义（每次调用只应用传入 delta）；
/// 事件分发与注册表校验由 `handle_panel_scroll` 承担，需真实 State，未在单测覆盖。
#[test]
fn two_wheel_events_land_two_steps_not_one_merged_jump() {
    let mut offset = 40;
    apply_scroll_to_virtual(&mut offset, SCROLL_LINES as i32, 100, 20);
    assert_eq!(offset, 40 + SCROLL_LINES as usize, "第一个事件立即落地");
    apply_scroll_to_virtual(&mut offset, SCROLL_LINES as i32, 100, 20);
    assert_eq!(
        offset,
        40 + 2 * SCROLL_LINES as usize,
        "第二个事件再落地一步"
    );
}

#[test]
fn mixed_slot_hit_testing_preserves_order_and_exclusive_boundaries() {
    let areas = [Rect::new(0, 0, 40, 20), Rect::new(40, 0, 60, 20)];
    assert_eq!(hovered_area(areas, 5, 39), Some(0));
    assert_eq!(hovered_area(areas, 5, 40), Some(1));
    assert_eq!(hovered_area(areas, 20, 40), None);
}
