//! Tests
use super::*;
use ratatui_kit::crossterm::event::{KeyEventKind, KeyEventState, KeyModifiers};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

#[test]
fn test_classify_inline_nav_supports_arrows_and_tab() {
    assert_eq!(
        classify_inline_nav(&key(KeyCode::Up)),
        Some(InlineNavAction::MoveUp)
    );
    assert_eq!(
        classify_inline_nav(&key(KeyCode::BackTab)),
        Some(InlineNavAction::MoveUp)
    );
    assert_eq!(
        classify_inline_nav(&key(KeyCode::Down)),
        Some(InlineNavAction::MoveDown)
    );
    assert_eq!(
        classify_inline_nav(&key(KeyCode::Tab)),
        Some(InlineNavAction::MoveDown)
    );
    assert_eq!(
        classify_inline_nav(&key(KeyCode::Enter)),
        Some(InlineNavAction::Confirm)
    );
    assert_eq!(
        classify_inline_nav(&key(KeyCode::Esc)),
        Some(InlineNavAction::Cancel)
    );
    assert_eq!(classify_inline_nav(&key(KeyCode::Char('x'))), None);
}

#[test]
fn test_selection_helpers_are_bounded() {
    assert_eq!(clamp_selection(5, 0), 0);
    assert_eq!(clamp_selection(5, 3), 2);
    assert_eq!(next_selection(0, 0), 0);
    assert_eq!(next_selection(0, 3), 1);
    assert_eq!(next_selection(2, 3), 2);
    assert_eq!(previous_selection(0), 0);
    assert_eq!(previous_selection(2), 1);
}
