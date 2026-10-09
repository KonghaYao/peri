use super::*;
use crate::kit::message_area::selection::{build_wrap_map, extract_visual_range_index};
use ratatui_kit::ratatui::text::Line;
use std::sync::Arc;

fn pending() -> GesturePending {
    GesturePending {
        screen: (20, 10),
        visual: (105, 10),
        entry_hit: None,
    }
}

#[test]
fn pending_selection_captures_drag_outside_message_area() {
    assert!(selection_captures_pointer(
        Some(pending()),
        false,
        MouseEventKind::Drag(MouseButton::Left),
    ));
}

#[test]
fn armed_selection_captures_release_outside_message_area() {
    assert!(selection_captures_pointer(
        None,
        true,
        MouseEventKind::Up(MouseButton::Left),
    ));
}

#[test]
fn idle_selection_does_not_capture_pointer() {
    assert!(!selection_captures_pointer(
        None,
        false,
        MouseEventKind::Drag(MouseButton::Left),
    ));
}

#[test]
fn selection_does_not_capture_other_gestures() {
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Drag(MouseButton::Right),
        MouseEventKind::ScrollDown,
        MouseEventKind::Moved,
    ] {
        assert!(!selection_captures_pointer(Some(pending()), true, kind));
    }
}

#[test]
fn selection_position_uses_centered_area_and_scroll_offset() {
    assert_eq!(
        selection_position(Rect::new(10, 5, 40, 10), (25, 8), 70_000),
        (70_003, 15),
    );
}

#[test]
fn selection_position_clamps_outside_viewport() {
    let area = Rect::new(10, 5, 40, 10);
    assert_eq!(selection_position(area, (0, 0), 100), (100, 0));
    assert_eq!(selection_position(area, (70, 30), 100), (109, 40));
}

#[test]
fn selection_release_uses_endpoint_even_when_last_drag_was_throttled() {
    let mut selection = TextSelection::new();
    selection.start_drag(105, 10);
    selection.update_drag(105, 15);
    assert_eq!(drag_step(None, (35, 10), true), DragAction::Throttled);

    let visual = selection_position(Rect::new(10, 5, 40, 10), (35, 10), 100);
    assert_eq!(
        selection_bounds_on_release(&mut selection, visual),
        Some(((105, 10), (105, 25))),
    );
}

#[test]
fn selection_drag_outside_viewport_can_be_released_and_cleared() {
    let pending = pending();
    let screen = (70, 30);
    let DragAction::Upgrade(pending) = drag_step(Some(pending), screen, true) else {
        panic!("drag outside viewport must upgrade the pending gesture");
    };
    let mut selection = TextSelection::new();
    selection.start_drag(pending.visual.0, pending.visual.1);
    let visual = selection_position(Rect::new(10, 5, 40, 10), screen, 100);
    assert_eq!(
        selection_bounds_on_release(&mut selection, visual),
        Some(((105, 10), (109, 40))),
    );
    selection.clear();
    assert!(!selection.dragging);
    assert_eq!(selection.normalized_bounds(), None);
}

#[test]
fn reverse_selection_release_normalizes_final_endpoint() {
    let mut selection = TextSelection::new();
    selection.start_drag(109, 30);
    selection.update_drag(107, 10);
    assert_eq!(
        selection_bounds_on_release(&mut selection, (100, 0)),
        Some(((100, 0), (109, 30))),
    );
}

#[test]
fn release_without_drag_does_not_create_selection() {
    let mut selection = TextSelection::new();
    assert_eq!(selection_bounds_on_release(&mut selection, (105, 10)), None);
    assert!(!selection.is_active());
}

#[test]
fn fast_selection_copies_through_release_endpoint_with_wide_characters() {
    let lines = vec![Line::from("你好abcdef")];
    let (_, map) = build_wrap_map(&lines, 20);
    let index = SlotIndex::new(vec![Arc::new(lines)], vec![Arc::new(map)]);
    let mut selection = TextSelection::new();
    selection.start_drag(0, 0);
    selection.update_drag(0, 4);
    let visual = selection_position(Rect::new(10, 5, 20, 2), (20, 5), 0);
    let (start, end) = selection_bounds_on_release(&mut selection, visual).unwrap();

    assert_eq!(
        extract_visual_range_index(&index, start, end, 20, None, None),
        Some("你好abcdef".to_string()),
    );
}

#[test]
fn selection_released_outside_viewport_copies_visible_lines() {
    let lines = vec![Line::from("你好"), Line::from("abcdef")];
    let (_, map) = build_wrap_map(&lines, 20);
    let index = SlotIndex::new(vec![Arc::new(lines)], vec![Arc::new(map)]);
    let mut selection = TextSelection::new();
    selection.start_drag(0, 0);
    let visual = selection_position(Rect::new(10, 5, 20, 2), (99, 99), 0);
    let (start, end) = selection_bounds_on_release(&mut selection, visual).unwrap();

    assert_eq!(
        extract_visual_range_index(&index, start, end, 20, None, None),
        Some("你好\nabcdef".to_string()),
    );
}
