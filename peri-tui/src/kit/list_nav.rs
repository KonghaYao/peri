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

/// 计算列表视口的 `scroll_start`，让 `selected` 项保持在上 1/3 处可见。
///
/// 仿 `slash_completion.rs` 的「选中项保持在上 1/3 处可见」模式：
/// 当 selected 接近视口顶部时（< visible_items / 3），不滚动；
/// 超过时把视口下沿推到 `selected - visible_items / 3`，但不超过 `max_scroll`。
///
/// 当 `item_count <= visible_items` 时返回 0（列表全可见，无需滚动）。
pub fn scroll_start_for_selected(
    selected: usize,
    item_count: usize,
    visible_items: usize,
) -> usize {
    if visible_items == 0 || item_count <= visible_items {
        0
    } else {
        let max_scroll = item_count - visible_items;
        selected.saturating_sub(visible_items / 3).min(max_scroll)
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
#[path = "list_nav_test.rs"]
mod tests;
