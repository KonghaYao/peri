use ratatui_kit::{
    crossterm::event::{KeyCode, KeyEvent},
    prelude::EventResult,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineNavAction {
    MoveUp,
    MoveDown,
    Confirm,
    Cancel,
}

pub fn classify_inline_nav(key: &KeyEvent) -> Option<InlineNavAction> {
    match key.code {
        KeyCode::Up | KeyCode::BackTab => Some(InlineNavAction::MoveUp),
        KeyCode::Down | KeyCode::Tab => Some(InlineNavAction::MoveDown),
        KeyCode::Enter => Some(InlineNavAction::Confirm),
        KeyCode::Esc => Some(InlineNavAction::Cancel),
        _ => None,
    }
}

pub fn clamp_selection(selection: usize, item_count: usize) -> usize {
    if item_count == 0 {
        0
    } else {
        selection.min(item_count - 1)
    }
}

pub fn next_selection(selection: usize, item_count: usize) -> usize {
    if item_count == 0 {
        0
    } else {
        selection.saturating_add(1).min(item_count - 1)
    }
}

pub fn previous_selection(selection: usize) -> usize {
    selection.saturating_sub(1)
}

pub fn event_result_for_inline_nav(key: &KeyEvent) -> EventResult {
    if classify_inline_nav(key).is_some() {
        EventResult::Consumed
    } else {
        EventResult::Ignored
    }
}

#[cfg(test)]
mod tests {
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
}
