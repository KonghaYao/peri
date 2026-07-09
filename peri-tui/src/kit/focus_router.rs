//! Focus/Shortcut Router：集中描述 kit v2 的键盘焦点优先级。
//!
//! 组件仍各自注册 `use_event_handler`，但所有跨层优先级判断必须先经过这里。
//! 目标优先级：Popup > InlineCompletion > Panel > Input > Message。

use crate::kit::atoms::{
    ACTIVE_PANEL, AT_MENTION_ACTIVE, POPUP_KIND, PopupKind, SLASH_HINT_ACTIVE,
};
use ratatui_kit::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// 跨平台快捷键绑定——同时匹配 macOS Option 合成字符和标准 Ctrl+字母路径。
///
/// macOS 终端按下 Option+字母时发送合成 Unicode 字符（无 modifier 标志位），
/// 标准终端使用 Ctrl+字母（带 CONTROL modifier）。
pub struct KeyBinding {
    /// macOS Option 键产生的合成 Unicode 字符（如 Alt+M = 'µ'）
    macos_char: Option<char>,
    /// 标准 Ctrl+字母 路径
    ctrl_letter: Option<char>,
    /// 标准 Ctrl+字母且带 Shift 的变体路径（如 Ctrl+Shift+T）
    ctrl_letter_shifted: bool,
}

impl KeyBinding {
    /// 仅匹配标准 Ctrl+字母
    pub const fn ctrl(c: char) -> Self {
        Self {
            macos_char: None,
            ctrl_letter: Some(c),
            ctrl_letter_shifted: false,
        }
    }

    /// macOS Option 字符 + 标准 Ctrl+字母 双重匹配
    pub const fn dual(macos: char, ctrl: char) -> Self {
        Self {
            macos_char: Some(macos),
            ctrl_letter: Some(ctrl),
            ctrl_letter_shifted: false,
        }
    }

    /// macOS Option+Shift 字符 + 标准 Ctrl+Shift+字母 双重匹配
    pub const fn dual_shift(macos: char, ctrl: char) -> Self {
        Self {
            macos_char: Some(macos),
            ctrl_letter: Some(ctrl),
            ctrl_letter_shifted: true,
        }
    }

    pub fn matches(&self, key: &KeyEvent) -> bool {
        // macOS 路径：无修饰符（或仅有 ALT）+ 合成字符匹配
        if let Some(ch) = self.macos_char {
            let mods = key.modifiers;
            if mods == KeyModifiers::NONE || mods == KeyModifiers::ALT {
                if let KeyCode::Char(c) = key.code {
                    if c == ch {
                        return true;
                    }
                }
            }
        }
        // 标准 Ctrl+(+Shift)+字母 路径
        if let Some(ch) = self.ctrl_letter {
            let expected_mods = if self.ctrl_letter_shifted {
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            } else {
                KeyModifiers::CONTROL
            };
            if key.modifiers == expected_mods {
                if let KeyCode::Char(c) = key.code {
                    if c.eq_ignore_ascii_case(&ch) {
                        return true;
                    }
                }
            }
        }
        false
    }
}

/// 模型别名循环：macOS Alt+M (µ) / 标准 Ctrl+T
pub const CYCLE_MODEL: KeyBinding = KeyBinding::dual('µ', 't');

/// Provider 循环：macOS Alt+Shift+M (Â) / 标准 Ctrl+Shift+T
pub const CYCLE_PROVIDER: KeyBinding = KeyBinding::dual_shift('Â', 't');

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
    CycleModel,
    CycleProvider,
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
    // BackTab (Shift+Tab) → 权限模式循环
    // crossterm 发送 BackTab 时 modifiers 可能为 SHIFT 或 NONE，两者都处理
    if key.code == KeyCode::BackTab {
        return Some(GlobalShortcut::CyclePermissionMode);
    }
    // Ctrl+letter shortcuts (standard terminal)
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('c') => return Some(GlobalShortcut::Quit),
            KeyCode::Char('o') => return Some(GlobalShortcut::ToggleDiff),
            KeyCode::Char('t') => {
                if key.modifiers.contains(KeyModifiers::SHIFT) {
                    return Some(GlobalShortcut::CycleProvider);
                }
                return Some(GlobalShortcut::CycleModel);
            }
            _ => {}
        }
    }
    // macOS Option+letter shortcuts (synthetic Unicode without modifier flags)
    if CYCLE_MODEL.matches(key) {
        return Some(GlobalShortcut::CycleModel);
    }
    if CYCLE_PROVIDER.matches(key) {
        return Some(GlobalShortcut::CycleProvider);
    }
    None
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
}
