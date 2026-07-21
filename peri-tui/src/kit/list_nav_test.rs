//! Tests
use super::*;
use ratatui_kit::crossterm::event::{KeyEventKind, KeyEventState};

fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent {
        code,
        modifiers,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

#[test]
fn test_classify_list_nav_supports_list_keys() {
    assert_eq!(
        classify_list_nav(&key(KeyCode::Up, KeyModifiers::NONE)),
        Some(ListNavAction::MoveUp)
    );
    assert_eq!(
        classify_list_nav(&key(KeyCode::Down, KeyModifiers::NONE)),
        Some(ListNavAction::MoveDown)
    );
    assert_eq!(
        classify_list_nav(&key(KeyCode::Tab, KeyModifiers::NONE)),
        Some(ListNavAction::CycleForward)
    );
    assert_eq!(
        classify_list_nav(&key(KeyCode::BackTab, KeyModifiers::SHIFT)),
        Some(ListNavAction::CycleBackward)
    );
    assert_eq!(
        classify_list_nav(&key(KeyCode::Enter, KeyModifiers::NONE)),
        Some(ListNavAction::Confirm)
    );
    assert_eq!(
        classify_list_nav(&key(KeyCode::Esc, KeyModifiers::NONE)),
        Some(ListNavAction::Cancel)
    );
    assert_eq!(
        classify_list_nav(&key(KeyCode::Down, KeyModifiers::CONTROL)),
        None
    );
}

#[test]
fn test_selection_helpers_are_bounded_and_cyclic() {
    assert_eq!(clamp_selection(4, 0), 0);
    assert_eq!(clamp_selection(4, 3), 2);
    assert_eq!(next_selection(2, 3), 2);
    assert_eq!(previous_selection(0), 0);
    assert_eq!(cycle_next(2, 3), 0);
    assert_eq!(cycle_next(0, 0), 0);
    assert_eq!(cycle_previous(0, 3), 2);
    assert_eq!(cycle_previous(0, 0), 0);
}

#[test]
fn test_scroll_start_for_selected_keeps_selected_in_upper_third() {
    // 列表全可见时不滚动
    assert_eq!(scroll_start_for_selected(0, 3, 3), 0);
    assert_eq!(scroll_start_for_selected(2, 3, 3), 0);
    // visible_items=0 视为无滚动
    assert_eq!(scroll_start_for_selected(5, 10, 0), 0);
    // selected 在上 1/3 区间时不滚动
    assert_eq!(scroll_start_for_selected(0, 10, 3), 0);
    assert_eq!(scroll_start_for_selected(1, 10, 3), 0);
    // selected 超过上 1/3（visible_items / 3 = 1），从 sel - 1 开始
    assert_eq!(scroll_start_for_selected(2, 10, 3), 1);
    assert_eq!(scroll_start_for_selected(5, 10, 3), 4);
    // 不超过 max_scroll = item_count - visible_items
    assert_eq!(scroll_start_for_selected(9, 10, 3), 7);
    assert_eq!(scroll_start_for_selected(100, 10, 3), 7);
}
