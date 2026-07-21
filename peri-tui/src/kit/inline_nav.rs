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
#[path = "inline_nav_test.rs"]
mod tests;
