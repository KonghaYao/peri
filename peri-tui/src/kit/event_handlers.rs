//! 事件处理器——Global + Root 层事件监听注册。
//!
//! 替代 `event/keyboard/normal_keys.rs` 的键盘 fallback。
//! 分为 Global Layer（不可阻断）和 Root Layer（被子层阻断）。
//!
//! ## 保留快捷键
//!
//! | 快捷键 | 功能 |
//! |--------|------|
//! | Ctrl+C | 三级优先级链（中断→双击退出） |
//! | Ctrl+O | Diff 视图切换 |
//! | Ctrl+K | 权限模式循环 |
//! | Esc    | 关闭 popup / 面板 / mention / slash |

use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind},
    prelude::*,
};

use super::atoms::{
    ACP_STATE, INPUT_AREA_ESC_PREFIX, INPUT_BUFFER, LAST_ESC_TIME, MODE_HIGHLIGHT_UNTIL,
    MODEL_HIGHLIGHT_UNTIL, PROVIDER_HIGHLIGHT_UNTIL, QUIT_PENDING_SINCE, REWIND_PREVIEW,
};
use crate::kit::atoms::PopupKind;
use crate::kit::focus_router::{
    FocusLayer, GlobalShortcut, active_layer, classify_global_shortcut,
};
use crate::kit::panel_registry::close_active_panel;
use crate::kit::popup_overlay::{close_popup, open_popup};
use tracing::info;

/// Global Layer: 不可阻断的快捷键。
///
/// 注册监听 Ctrl+C / Ctrl+O 等顶级快捷键。
pub fn register_global_handlers(hooks: &mut Hooks, mut exit: Handler<'static, ()>) {
    hooks.use_event_handler(EventScope::Global, EventPriority::High, move |event| {
        tracing::info!(?event, "kit raw input event");
        let Event::Key(key) = event else {
            return EventResult::Ignored;
        };
        if key.kind != KeyEventKind::Press {
            return EventResult::Ignored;
        }

        match classify_global_shortcut(&key) {
            Some(GlobalShortcut::Quit) => {
                let loading = ACP_STATE.state().read().is_loading;
                if loading {
                    INPUT_BUFFER.state().write().clear();
                    return EventResult::Consumed;
                }

                let now = std::time::Instant::now();
                let pending = *QUIT_PENDING_SINCE.state().read();
                match pending {
                    None => {
                        *QUIT_PENDING_SINCE.state().write() = Some(now);
                        info!("再次按 Ctrl+C 退出");
                    }
                    Some(t) if now.duration_since(t) < std::time::Duration::from_secs(1) => {
                        exit(());
                    }
                    Some(_) => {
                        *QUIT_PENDING_SINCE.state().write() = Some(now);
                    }
                }
                EventResult::Consumed
            }
            Some(GlobalShortcut::ToggleDiff) => {
                let diff_visible = crate::kit::atoms::DIFF_VISIBLE.state();
                let mut g = diff_visible.write();
                *g = !*g;
                tracing::info!(diff_visible = *g, "Ctrl+O: 切换 diff 视图");
                EventResult::Consumed
            }
            _ => EventResult::Ignored,
        }
    });
}

/// Root Layer: 可被子层阻断的快捷键。
///
/// 注册：
/// - Esc → 关闭 popup / @mention / slash_hint / 当前激活面板
/// - Ctrl+K → cycle permission mode（保留）
pub fn register_root_handlers(hooks: &mut Hooks) {
    hooks.use_event_handler(EventScope::Current, EventPriority::Normal, move |event| {
        let Event::Key(key) = event else {
            return EventResult::Ignored;
        };
        if key.kind != KeyEventKind::Press {
            return EventResult::Ignored;
        }

        match classify_global_shortcut(&key) {
            Some(GlobalShortcut::CyclePermissionMode) => {
                *MODE_HIGHLIGHT_UNTIL.state().write() =
                    Some(std::time::Instant::now() + std::time::Duration::from_secs(2));
                EventResult::Consumed
            }
            _ => match key.code {
                // Esc: 双击触发 Rewind popup，否则走关闭优先级链
                KeyCode::Esc => {
                    // 跳过由 InputArea 检测到的 Alt+key ESC 前缀
                    let is_alt_prefix = *INPUT_AREA_ESC_PREFIX.state().read();
                    if is_alt_prefix {
                        return EventResult::Ignored;
                    }

                    match active_layer() {
                        FocusLayer::Popup(_) => {
                            close_popup();
                            return EventResult::Consumed;
                        }
                        FocusLayer::InlineCompletion => return EventResult::Ignored,
                        FocusLayer::Panel => {
                            close_active_panel();
                            return EventResult::Consumed;
                        }
                        FocusLayer::Input | FocusLayer::Message => {}
                    }

                    let now = std::time::Instant::now();
                    let last_esc = *LAST_ESC_TIME.state().read();
                    let is_double_esc = last_esc
                        .map(|t| now.duration_since(t) < std::time::Duration::from_millis(500))
                        .unwrap_or(false);

                    *LAST_ESC_TIME.state().write() = Some(now);

                    if is_double_esc {
                        let _ = REWIND_PREVIEW.state();
                        open_popup(PopupKind::Rewind);
                        return EventResult::Consumed;
                    }

                    EventResult::Ignored
                }
                _ => EventResult::Ignored,
            },
        }
    });
}

#[allow(dead_code)]
fn _silence_unused_atoms_warnings() {
    let _ = MODEL_HIGHLIGHT_UNTIL.state();
    let _ = PROVIDER_HIGHLIGHT_UNTIL.state();
}

#[cfg(test)]
mod tests {
    use crate::kit::atoms::{ACTIVE_PANEL, OPEN_PANELS};

    fn setup_atoms() {
        crate::kit::atoms::init_atoms();
        *OPEN_PANELS.state().write() = Vec::new();
        *ACTIVE_PANEL.state().write() = None;
    }

    #[test]
    fn test_setup_atoms_initializes_empty() {
        setup_atoms();
        assert!(OPEN_PANELS.state().read().is_empty());
        assert!(ACTIVE_PANEL.state().read().is_none());
    }
}
