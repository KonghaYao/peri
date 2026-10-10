use super::*;

#[test]
fn virtual_scroll_keeps_full_history_beyond_terminal_coordinates() {
    let mut offset = 70_000;
    apply_pending_to_virtual(&mut offset, 3, 100_000, 20);
    assert_eq!(offset, 70_003);
    apply_pending_to_virtual(&mut offset, i32::MAX, 100_000, 20);
    assert_eq!(offset, 99_980);
}

#[test]
fn virtual_scroll_clamps_top_bottom_and_shrinking_history() {
    let mut offset = 2;
    apply_pending_to_virtual(&mut offset, -3, 100, 20);
    assert_eq!(offset, 0);
    apply_pending_to_virtual(&mut offset, 100, 100, 20);
    assert_eq!(offset, 80);
    apply_pending_to_virtual(&mut offset, 0, 30, 20);
    assert_eq!(offset, 10);
    apply_pending_to_virtual(&mut offset, 3, 10, 20);
    assert_eq!(offset, 0);
}

#[test]
fn virtual_scroll_handles_signed_and_usize_extremes() {
    let mut offset = usize::MAX - 1;
    apply_pending_to_virtual(&mut offset, 3, usize::MAX, 0);
    assert_eq!(offset, usize::MAX);
    apply_pending_to_virtual(&mut offset, i32::MIN, usize::MAX, 0);
    assert_eq!(offset, usize::MAX - i32::MIN.unsigned_abs() as usize);
}

#[test]
fn mixed_slot_hit_testing_preserves_order_and_exclusive_boundaries() {
    let areas = [Rect::new(0, 0, 40, 20), Rect::new(40, 0, 60, 20)];
    assert_eq!(hovered_area(areas, 5, 39), Some(0));
    assert_eq!(hovered_area(areas, 5, 40), Some(1));
    assert_eq!(hovered_area(areas, 20, 40), None);
}
