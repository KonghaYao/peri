//! Tests for focus_router

#[cfg(test)]
use super::*;

#[cfg(test)]
use crate::app::panel_types::PanelKind;
#[cfg(test)]
use crate::kit::atoms;
#[cfg(test)]
use crate::kit::panel_registry::{close_all_panels, open_panel};
#[cfg(test)]
use ratatui_kit::crossterm::event::{KeyEventKind, KeyEventState, KeyModifiers};
#[cfg(test)]
use serial_test::serial;

#[cfg(test)]
fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent {
        code,
        modifiers,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

#[cfg(test)]
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

#[test]
fn test_cycle_model_macos_alt_m() {
    let key = key(KeyCode::Char('µ'), KeyModifiers::NONE);
    assert_eq!(
        classify_global_shortcut(&key),
        Some(GlobalShortcut::CycleModel)
    );
}

#[test]
fn test_cycle_model_ctrl_t() {
    let key = key(KeyCode::Char('t'), KeyModifiers::CONTROL);
    assert_eq!(
        classify_global_shortcut(&key),
        Some(GlobalShortcut::CycleModel)
    );
}

#[test]
fn test_cycle_provider_ctrl_shift_t() {
    let key = key(
        KeyCode::Char('t'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    );
    assert_eq!(
        classify_global_shortcut(&key),
        Some(GlobalShortcut::CycleProvider)
    );
}

#[test]
fn test_cycle_provider_macos_alt_shift_m() {
    let key = key(KeyCode::Char('Â'), KeyModifiers::NONE);
    assert_eq!(
        classify_global_shortcut(&key),
        Some(GlobalShortcut::CycleProvider)
    );
}
