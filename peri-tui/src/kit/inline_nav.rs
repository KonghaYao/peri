//! 内联补全（`@` / `/`）的按键分类。
//!
//! 选择钳制与移动原语来自 `peri_tui_components::list_nav`，与通用列表共用；
//! 这里只保留不区分修饰键的分类（Tab/BackTab 在端点停住）。

use ratatui_kit::crossterm::event::KeyCode;
use ratatui_kit::crossterm::event::KeyEvent;
use ratatui_kit::prelude::EventResult;

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
