use ratatui_kit::{
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers},
    prelude::EventResult,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListNavAction {
    MoveUp,
    MoveDown,
    CycleForward,
    CycleBackward,
    Confirm,
    Cancel,
}

pub fn classify_list_nav(key: &KeyEvent) -> Option<ListNavAction> {
    match (key.modifiers, key.code) {
        (KeyModifiers::NONE, KeyCode::Up) => Some(ListNavAction::MoveUp),
        (KeyModifiers::NONE, KeyCode::Down) => Some(ListNavAction::MoveDown),
        (KeyModifiers::NONE, KeyCode::Tab) => Some(ListNavAction::CycleForward),
        (KeyModifiers::SHIFT, KeyCode::BackTab) | (KeyModifiers::NONE, KeyCode::BackTab) => {
            Some(ListNavAction::CycleBackward)
        }
        (KeyModifiers::NONE, KeyCode::Enter) => Some(ListNavAction::Confirm),
        (KeyModifiers::NONE, KeyCode::Esc) => Some(ListNavAction::Cancel),
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

pub fn cycle_next(selection: usize, item_count: usize) -> usize {
    if item_count == 0 {
        0
    } else {
        (selection + 1) % item_count
    }
}

pub fn cycle_previous(selection: usize, item_count: usize) -> usize {
    if item_count == 0 {
        0
    } else {
        selection.checked_sub(1).unwrap_or(item_count - 1)
    }
}

pub fn event_result_for_list_nav(key: &KeyEvent) -> EventResult {
    if classify_list_nav(key).is_some() {
        EventResult::Consumed
    } else {
        EventResult::Ignored
    }
}

#[cfg(test)]
mod tests {
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
}
