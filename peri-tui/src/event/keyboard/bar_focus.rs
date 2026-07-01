use ratatui::crossterm::event::{KeyCode, KeyEventKind};

use super::super::Action;
use crate::app::App;

/// Bar 焦点模式拦截：bg_bar_cursor 有值时，所有按键转发到 handle_bar_key_event
pub(super) fn handle_bar_focus(
    app: &mut App,
    key_event: &ratatui::crossterm::event::KeyEvent,
) -> Option<Action> {
    let cursor = app.session_mgr.current_mut().ui.bg_bar_cursor;
    if cursor.is_some() {
        return Some(handle_bar_key_event(app, *key_event));
    }
    None
}

/// 聚焦只读模式拦截：focused_instance_id 有值时，仅 Esc 退出
pub(super) fn handle_focused_only(
    app: &mut App,
    key_event: &ratatui::crossterm::event::KeyEvent,
) -> Option<Action> {
    let focused = app.session_mgr.current_mut().focused_instance_id.is_some();
    if focused {
        if matches!(key_event.code, KeyCode::Esc) {
            app.session_mgr.current_mut().focused_instance_id = None;
            app.session_mgr.current_mut().ui.bg_bar_cursor = None;
        }
        return Some(Action::Redraw);
    }
    None
}

/// Bar 焦点模式下的键盘处理
pub(super) fn handle_bar_key_event(
    app: &mut App,
    key_event: ratatui::crossterm::event::KeyEvent,
) -> Action {
    if key_event.kind == KeyEventKind::Release {
        return Action::Redraw;
    }

    let agents_len = app.session_mgr.current_mut().background_agents.len();
    let total_items = 1 + agents_len.min(4);

    let raw_cursor = app.session_mgr.current_mut().ui.bg_bar_cursor.unwrap_or(0);
    // Clamp cursor to valid range (agents may have been removed)
    let cursor = raw_cursor.min(total_items.saturating_sub(1));

    match key_event.code {
        KeyCode::Esc => {
            app.session_mgr.current_mut().ui.bg_bar_cursor = None;
            Action::Redraw
        }
        KeyCode::Up => {
            let new_cursor = if cursor > 0 {
                cursor - 1
            } else {
                total_items - 1
            };
            app.session_mgr.current_mut().ui.bg_bar_cursor = Some(new_cursor);
            Action::Redraw
        }
        KeyCode::Down => {
            let new_cursor = (cursor + 1) % total_items;
            app.session_mgr.current_mut().ui.bg_bar_cursor = Some(new_cursor);
            Action::Redraw
        }
        KeyCode::Enter => {
            if cursor == 0 {
                // 选中 main → 退出聚焦
                app.session_mgr.current_mut().focused_instance_id = None;
            } else {
                // 选中后台 agent → 聚焦
                let agents = &app.session_mgr.current_mut().background_agents;
                if let Some(agent) = agents.get(cursor - 1) {
                    app.session_mgr.current_mut().focused_instance_id =
                        Some(agent.instance_id.clone());
                }
            }
            app.session_mgr.current_mut().ui.bg_bar_cursor = None;
            Action::Redraw
        }
        _ => Action::Redraw,
    }
}
