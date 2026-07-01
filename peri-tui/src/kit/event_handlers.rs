//! 事件处理器——Global + Root 层事件监听注册。
//!
//! 替代 `event/keyboard/normal_keys.rs` 的键盘 fallback。
//! 分为 Global Layer（不可阻断）和 Root Layer（被子层阻断）。
//!
//! ## S6 快捷键映射（避开 Ctrl+C 全局 quit 和 Ctrl+K cycle permission mode）
//!
//! | 快捷键       | 面板           |
//! |-------------|----------------|
//! | Ctrl+M      | Model          |
//! | Ctrl+T      | ThreadBrowser  |
//! | Ctrl+R      | Cron           |
//! | Ctrl+S      | Status         |
//! | Ctrl+L      | Login          |
//! | Ctrl+H      | Hooks          |
//! | Ctrl+J      | Tasks          |
//! | Ctrl+B      | Betas          |
//! | Ctrl+P      | Plugin         |
//! | Ctrl+G      | Agent          |
//! | Ctrl+F      | Config         |
//! | Ctrl+W      | Workflow       |
//! | Ctrl+N      | Memory         |
//! | Ctrl+X      | Mcp            |

use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers},
    prelude::*,
};

use super::atoms::{
    ACTIVE_PANEL, AT_MENTION_ACTIVE, MODE_HIGHLIGHT_UNTIL, MODEL_HIGHLIGHT_UNTIL, OPEN_PANELS,
    PROVIDER_HIGHLIGHT_UNTIL, SLASH_HINT_ACTIVE,
};
use crate::kit::panel_registry::{close_active_panel, from_key_code, open_panel};

/// Global Layer: 不可阻断的快捷键。
///
/// 注册监听 Ctrl+C / Ctrl+O 等顶级快捷键。
pub fn register_global_handlers(hooks: &mut Hooks, mut exit: Handler<'static, ()>) {
    hooks.use_events(move |event| {
        let Event::Key(key) = event else { return };
        if key.kind != KeyEventKind::Press {
            return;
        }

        match key.code {
            // Ctrl+C: quit
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                exit(());
            }
            // Ctrl+O: toggle diff
            KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                // Phase 5: 切换 diff 视图
            }
            _ => {}
        }
    });
}

/// Root Layer: 可被子层阻断的快捷键。
///
/// S6 起注册：
/// - 14 个 Ctrl+字母 快捷键 → open_panel(kind)
/// - Esc → 关闭 popup / @mention / slash_hint / 当前激活面板
/// - Ctrl+K → cycle permission mode（保留）
pub fn register_root_handlers(hooks: &mut Hooks) {
    hooks.use_events(move |event| {
        let Event::Key(key) = event else { return };
        if key.kind != KeyEventKind::Press {
            return;
        }

        // ── Ctrl+字母：14 面板开关 ──
        // 仅在 Ctrl 按下、无 Shift/Alt 时触发；Ctrl+C/K 由上层保留。
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && !key.modifiers.contains(KeyModifiers::SHIFT)
            && !key.modifiers.contains(KeyModifiers::ALT)
        {
            if let Some(kind) = from_key_code(key.code) {
                // toggle 行为：已打开则关闭，否则打开。这样 Ctrl+M 第二次按下关闭 Model。
                let is_open = OPEN_PANELS
                    .get()
                    .map(|a| a.read().contains(&kind))
                    .unwrap_or(false);
                if is_open {
                    crate::kit::panel_registry::close_panel(kind);
                } else {
                    open_panel(kind);
                }
                return;
            }
        }

        match key.code {
            // Ctrl+K: cycle permission mode（保留）
            KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                *MODE_HIGHLIGHT_UNTIL.get().unwrap().write() =
                    Some(std::time::Instant::now() + std::time::Duration::from_secs(2));
            }

            // Esc: 关闭优先级 popup → @mention/slash → 激活面板
            KeyCode::Esc => {
                // 先关 @mention / slash_hint
                let mention = AT_MENTION_ACTIVE.get().map(|a| *a.read()).unwrap_or(false);
                let slash = SLASH_HINT_ACTIVE.get().map(|a| *a.read()).unwrap_or(false);
                if mention || slash {
                    if let Some(a) = AT_MENTION_ACTIVE.get() {
                        *a.write() = false;
                    }
                    if let Some(a) = SLASH_HINT_ACTIVE.get() {
                        *a.write() = false;
                    }
                    return;
                }

                // 再关激活面板
                let has_active = ACTIVE_PANEL
                    .get()
                    .map(|a| a.read().is_some())
                    .unwrap_or(false);
                if has_active {
                    close_active_panel();
                }
            }

            // 其他按键: 交由子组件处理（InputArea/panels 等）
            _ => {}
        }
    });
}

// ── 编译期静默 unused 警告（保留 atoms 导入便于将来扩展） ───────────────────
#[allow(dead_code)]
fn _silence_unused_atoms_warnings() {
    let _ = *MODEL_HIGHLIGHT_UNTIL.get().unwrap();
    let _ = *PROVIDER_HIGHLIGHT_UNTIL.get().unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_atoms() {
        crate::kit::atoms::init_atoms();
        *OPEN_PANELS.get().unwrap().write() = Vec::new();
        *ACTIVE_PANEL.get().unwrap().write() = None;
    }

    #[test]
    fn test_from_key_code_maps_all_14_panels() {
        // 验证所有 14 个快捷键字母都能映射到对应面板
        use crate::app::panel_types::PanelKind;
        let cases = [
            (KeyCode::Char('m'), PanelKind::Model),
            (KeyCode::Char('t'), PanelKind::ThreadBrowser),
            (KeyCode::Char('r'), PanelKind::Cron),
            (KeyCode::Char('s'), PanelKind::Status),
            (KeyCode::Char('l'), PanelKind::Login),
            (KeyCode::Char('h'), PanelKind::Hooks),
            (KeyCode::Char('j'), PanelKind::Tasks),
            (KeyCode::Char('b'), PanelKind::Betas),
            (KeyCode::Char('p'), PanelKind::Plugin),
            (KeyCode::Char('g'), PanelKind::Agent),
            (KeyCode::Char('f'), PanelKind::Config),
            (KeyCode::Char('w'), PanelKind::Workflow),
            (KeyCode::Char('n'), PanelKind::Memory),
            (KeyCode::Char('x'), PanelKind::Mcp),
        ];
        for (code, expected) in cases {
            assert_eq!(
                from_key_code(code),
                Some(expected),
                "key {:?} should map to {:?}",
                code,
                expected
            );
        }
    }

    #[test]
    fn test_from_key_code_preserves_reserved_shortcuts() {
        // Ctrl+C 和 Ctrl+K 应保留（返回 None）
        assert_eq!(from_key_code(KeyCode::Char('c')), None);
        assert_eq!(from_key_code(KeyCode::Char('k')), None);
    }

    #[test]
    fn test_setup_atoms_initializes_empty() {
        setup_atoms();
        assert!(OPEN_PANELS.get().unwrap().read().is_empty());
        assert!(ACTIVE_PANEL.get().unwrap().read().is_none());
    }
}
