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

// ── Slice 2：entry 导航仲裁（message_nav_accepts）────────────────────────

#[test]
#[serial]
fn test_message_nav_alt_up_down_always_claimed_in_input_layer() {
    // Alt+方向键恒归属消息区（移 entry 焦点）——裁决 C3。
    reset_focus_atoms();
    assert!(message_nav_accepts(
        &key(KeyCode::Up, KeyModifiers::ALT),
        false
    ));
    assert!(message_nav_accepts(
        &key(KeyCode::Down, KeyModifiers::ALT),
        false
    ));
    // 焦点未激活时 Enter/Space 不归属消息区（输入态不抢占）
    assert!(!message_nav_accepts(
        &key(KeyCode::Enter, KeyModifiers::NONE),
        false
    ));
    assert!(!message_nav_accepts(
        &key(KeyCode::Char(' '), KeyModifiers::NONE),
        false
    ));
    // 非 Alt 方向键与 Ctrl 方向键（滚动）不归属导航
    assert!(!message_nav_accepts(
        &key(KeyCode::Up, KeyModifiers::NONE),
        false
    ));
    assert!(!message_nav_accepts(
        &key(KeyCode::Up, KeyModifiers::CONTROL),
        false
    ));
    assert!(!message_nav_accepts(
        &key(KeyCode::Down, KeyModifiers::CONTROL),
        false
    ));
}

#[test]
#[serial]
fn test_message_nav_enter_space_claimed_when_entry_focused() {
    // entry 焦点激活后 Enter/Space 归属消息区（切 Collapsed/Expanded / Preview）。
    reset_focus_atoms();
    assert!(message_nav_accepts(
        &key(KeyCode::Enter, KeyModifiers::NONE),
        true
    ));
    assert!(message_nav_accepts(
        &key(KeyCode::Char(' '), KeyModifiers::NONE),
        true
    ));
    // 带修饰符的 Enter/Space 不归属（Alt+Enter 是输入区换行）
    assert!(!message_nav_accepts(
        &key(KeyCode::Enter, KeyModifiers::ALT),
        true
    ));
    assert!(!message_nav_accepts(
        &key(KeyCode::Enter, KeyModifiers::SHIFT),
        true
    ));
}

#[test]
#[serial]
fn test_message_nav_rejected_when_popup_or_panel_active() {
    // popup / inline-completion / panel 激活时不响应（active_layer 仲裁）。
    reset_focus_atoms();
    *POPUP_KIND.state().write() = Some(PopupKind::Rewind);
    assert!(!message_nav_accepts(
        &key(KeyCode::Up, KeyModifiers::ALT),
        true
    ));
    assert!(!message_nav_accepts(
        &key(KeyCode::Enter, KeyModifiers::NONE),
        true
    ));
    *POPUP_KIND.state().write() = None;
    *SLASH_HINT_ACTIVE.state().write() = true;
    assert!(!message_nav_accepts(
        &key(KeyCode::Up, KeyModifiers::ALT),
        true
    ));
    *SLASH_HINT_ACTIVE.state().write() = false;
    open_panel(PanelKind::Model);
    assert!(!message_nav_accepts(
        &key(KeyCode::Down, KeyModifiers::ALT),
        true
    ));
    close_all_panels();
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

/// [Slice 4 §6.8] entry 焦点激活时 Tab/←/→ 归属消息区（interaction option
/// 切换）——Tab 未被全局分类（BackTab 才是 CyclePermissionMode）；焦点未激活
/// 时不抢占（输入区 Tab 语义不受影响）。
#[test]
#[serial]
fn test_message_nav_tab_arrows_claimed_when_entry_focused() {
    reset_focus_atoms();
    assert!(message_nav_accepts(
        &key(KeyCode::Tab, KeyModifiers::NONE),
        true
    ));
    assert!(message_nav_accepts(
        &key(KeyCode::Left, KeyModifiers::NONE),
        true
    ));
    assert!(message_nav_accepts(
        &key(KeyCode::Right, KeyModifiers::NONE),
        true
    ));
    // 焦点未激活 → 不归属（Tab 继续传给输入区）
    assert!(!message_nav_accepts(
        &key(KeyCode::Tab, KeyModifiers::NONE),
        false
    ));
    assert!(!message_nav_accepts(
        &key(KeyCode::Left, KeyModifiers::NONE),
        false
    ));
    // 带修饰符不归属（Shift+Tab=BackTab 是权限模式循环；Ctrl+方向键是滚动）
    assert!(!message_nav_accepts(
        &key(KeyCode::BackTab, KeyModifiers::NONE),
        true
    ));
    assert!(!message_nav_accepts(
        &key(KeyCode::Tab, KeyModifiers::SHIFT),
        true
    ));
    assert!(!message_nav_accepts(
        &key(KeyCode::Left, KeyModifiers::CONTROL),
        true
    ));
}

/// [Slice 4] 全局快捷键分类不吞 Tab（仅 BackTab 归类为权限模式循环）。
#[test]
fn test_classify_global_shortcut_tab_not_classified() {
    assert!(classify_global_shortcut(&key(KeyCode::Tab, KeyModifiers::NONE)).is_none());
    assert_eq!(
        classify_global_shortcut(&key(KeyCode::BackTab, KeyModifiers::NONE)),
        Some(GlobalShortcut::CyclePermissionMode)
    );
}
