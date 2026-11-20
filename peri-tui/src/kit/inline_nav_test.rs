//! Tests
use super::InlineNavAction;
use super::KeyCode;
use super::KeyEvent;

use ratatui_kit::crossterm::event::{KeyEventKind, KeyEventState, KeyModifiers};
use ratatui_kit::prelude::EventResult;

#[must_use]
fn key(code: KeyCode) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

#[must_use]
fn key_with(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent {
        code,
        modifiers,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

#[test]
fn test_classify_inline_nav_supports_arrows_and_tab() {
    assert_eq!(
        super::classify_inline_nav(&key(KeyCode::Up)),
        Some(InlineNavAction::MoveUp)
    );
    assert_eq!(
        super::classify_inline_nav(&key(KeyCode::BackTab)),
        Some(InlineNavAction::MoveUp)
    );
    assert_eq!(
        super::classify_inline_nav(&key(KeyCode::Down)),
        Some(InlineNavAction::MoveDown)
    );
    assert_eq!(
        super::classify_inline_nav(&key(KeyCode::Tab)),
        Some(InlineNavAction::MoveDown)
    );
    assert_eq!(
        super::classify_inline_nav(&key(KeyCode::Enter)),
        Some(InlineNavAction::Confirm)
    );
    assert_eq!(
        super::classify_inline_nav(&key(KeyCode::Esc)),
        Some(InlineNavAction::Cancel)
    );
    assert_eq!(super::classify_inline_nav(&key(KeyCode::Char('x'))), None);
}

/// @ 与 / 的 Tab/BackTab 在端点停住，不循环到另一端。
#[test]
fn test_tab_navigation_stops_at_endpoints() {
    use peri_tui_components::list_nav;
    let down = super::classify_inline_nav(&key(KeyCode::Tab));
    let up = super::classify_inline_nav(&key(KeyCode::BackTab));
    assert_eq!(down, Some(InlineNavAction::MoveDown));
    assert_eq!(up, Some(InlineNavAction::MoveUp));

    assert_eq!(list_nav::next_selection(2, 3), 2, "Tab 在末项停住，不循环");
    assert_eq!(
        list_nav::previous_selection(0),
        0,
        "BackTab 在首项停住，不循环"
    );
    assert_eq!(list_nav::next_selection(0, 0), 0, "空列表不移动");
}

/// 分类忽略修饰键；消费口径与现状一致（方向/Tab/Enter/Esc 均 Consumed）。
#[test]
fn test_modifiers_do_not_change_classification_or_consumption() {
    for modifiers in [
        KeyModifiers::NONE,
        KeyModifiers::SHIFT,
        KeyModifiers::CONTROL,
        KeyModifiers::ALT,
    ] {
        assert_eq!(
            super::classify_inline_nav(&key_with(KeyCode::Tab, modifiers)),
            Some(InlineNavAction::MoveDown),
            "Tab modifiers={modifiers:?}"
        );
        assert_eq!(
            super::classify_inline_nav(&key_with(KeyCode::BackTab, modifiers)),
            Some(InlineNavAction::MoveUp),
            "BackTab modifiers={modifiers:?}"
        );
        assert_eq!(
            super::event_result_for_inline_nav(&key_with(KeyCode::Enter, modifiers)),
            EventResult::Consumed
        );
        assert_eq!(
            super::event_result_for_inline_nav(&key_with(KeyCode::Char('x'), modifiers)),
            EventResult::Ignored
        );
    }
}
