//! Focus/Shortcut Router：集中描述 kit v2 的键盘焦点优先级。
//!
//! 组件仍各自注册 `use_event_handler`，但所有跨层优先级判断必须先经过这里。
//! 目标优先级：Popup > InlineCompletion > Panel > Input > Message。

use crate::kit::atoms::{
    ACTIVE_PANEL, AT_MENTION_ACTIVE, POPUP_KIND, PopupKind, SLASH_HINT_ACTIVE,
};
use ratatui_kit::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusLayer {
    Popup(PopupKind),
    InlineCompletion,
    Panel,
    Input,
    Message,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlobalShortcut {
    Quit,
    ToggleDiff,
    CyclePermissionMode,
}

pub fn active_layer() -> FocusLayer {
    if let Some(kind) = *POPUP_KIND.state().read() {
        FocusLayer::Popup(kind)
    } else if *AT_MENTION_ACTIVE.state().read() || *SLASH_HINT_ACTIVE.state().read() {
        FocusLayer::InlineCompletion
    } else if ACTIVE_PANEL.state().read().is_some() {
        FocusLayer::Panel
    } else {
        FocusLayer::Input
    }
}

pub fn classify_global_shortcut(key: &KeyEvent) -> Option<GlobalShortcut> {
    if !key.modifiers.contains(KeyModifiers::CONTROL) {
        return None;
    }
    match key.code {
        KeyCode::Char('c') => Some(GlobalShortcut::Quit),
        KeyCode::Char('o') => Some(GlobalShortcut::ToggleDiff),
        KeyCode::Char('k') => Some(GlobalShortcut::CyclePermissionMode),
        _ => None,
    }
}

pub fn message_accepts_key(key: &KeyEvent) -> bool {
    matches!(
        key.code,
        KeyCode::Up | KeyCode::Down | KeyCode::Home | KeyCode::End
    ) && key.modifiers.contains(KeyModifiers::CONTROL)
}

pub fn input_accepts_key(key: &KeyEvent) -> bool {
    if matches!(active_layer(), FocusLayer::Popup(_) | FocusLayer::Panel) {
        return false;
    }
    !message_accepts_key(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::panel_types::PanelKind;
    use crate::kit::atoms;
    use crate::kit::panel_registry::{close_all_panels, open_panel};
    use ratatui_kit::crossterm::event::{KeyEventKind, KeyEventState, KeyModifiers};
    use serial_test::serial;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn reset_focus_atoms() {
        atoms::init_atoms();
        *POPUP_KIND.state().write() = None;
        *AT_MENTION_ACTIVE.state().write() = false;
        *SLASH_HINT_ACTIVE.state().write() = false;
        close_all_panels();
    }

    #[test]
    #[serial]
    fn test_active_layer_priority_popup_over_inline_and_panel() {
        reset_focus_atoms();
        open_panel(PanelKind::Model);
        *SLASH_HINT_ACTIVE.state().write() = true;
        *POPUP_KIND.state().write() = Some(PopupKind::Rewind);
        assert_eq!(active_layer(), FocusLayer::Popup(PopupKind::Rewind));
    }

    #[test]
    #[serial]
    fn test_active_layer_priority_inline_over_panel() {
        reset_focus_atoms();
        open_panel(PanelKind::Model);
        *AT_MENTION_ACTIVE.state().write() = true;
        assert_eq!(active_layer(), FocusLayer::InlineCompletion);
    }

    #[test]
    #[serial]
    fn test_active_layer_panel_before_input() {
        reset_focus_atoms();
        open_panel(PanelKind::Model);
        assert_eq!(active_layer(), FocusLayer::Panel);
    }

    #[test]
    #[serial]
    fn test_active_layer_defaults_to_input() {
        reset_focus_atoms();
        assert_eq!(active_layer(), FocusLayer::Input);
    }

    #[test]
    fn test_classify_global_shortcut_ctrl_only() {
        assert_eq!(
            classify_global_shortcut(&key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(GlobalShortcut::Quit)
        );
        assert_eq!(
            classify_global_shortcut(&key(KeyCode::Char('c'), KeyModifiers::NONE)),
            None
        );
    }

    #[test]
    fn test_message_accepts_only_ctrl_navigation_keys() {
        assert!(message_accepts_key(&key(
            KeyCode::Up,
            KeyModifiers::CONTROL
        )));
        assert!(!message_accepts_key(&key(KeyCode::Up, KeyModifiers::NONE)));
        assert!(!message_accepts_key(&key(
            KeyCode::Char('x'),
            KeyModifiers::CONTROL
        )));
    }

    #[test]
    #[serial]
    fn test_input_rejects_when_panel_or_popup_active() {
        reset_focus_atoms();
        open_panel(PanelKind::Model);
        assert!(!input_accepts_key(&key(
            KeyCode::Char('x'),
            KeyModifiers::NONE
        )));
        close_all_panels();
        *POPUP_KIND.state().write() = Some(PopupKind::Hitl);
        assert!(!input_accepts_key(&key(
            KeyCode::Char('x'),
            KeyModifiers::NONE
        )));
    }
}
