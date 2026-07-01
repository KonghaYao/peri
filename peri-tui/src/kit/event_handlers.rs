//! 事件处理器——Global + Root 层事件监听注册。
//!
//! 替代 `event/keyboard/normal_keys.rs` 的键盘 fallback。
//! 分为 Global Layer（不可阻断）和 Root Layer（被子层阻断）。
//! Phase 4 编译桩——Phase 5-6 逐步填充快捷键逻辑。

use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers},
    prelude::*,
};

use super::atoms::{
    ACP_STATE, SCROLL_OFFSET, MODEL_HIGHLIGHT_UNTIL, PROVIDER_HIGHLIGHT_UNTIL,
    MODE_HIGHLIGHT_UNTIL, AT_MENTION_ACTIVE, SLASH_HINT_ACTIVE,
};

/// Global Layer: 不可阻断的快捷键。
///
/// 注册监听 Ctrl+C / Ctrl+O 等顶级快捷键。
pub fn register_global_handlers(
    hooks: &mut Hooks,
    _exit: Handler<'static, ()>,
) {
    hooks.use_events(move |event| {
        let Event::Key(key) = event else { return };
        if key.kind != KeyEventKind::Press {
            return;
        }

        match key.code {
            // Ctrl+C: quit
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                // Phase 5: 三级优先级（清空输入 → 中断 agent → 退出）
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
/// 注册 Ctrl+T/M/P/K 轮换、Esc 关闭、Enter 提交等快捷键。
pub fn register_root_handlers(
    hooks: &mut Hooks,
) {
    hooks.use_events(move |event| {
        let Event::Key(key) = event else { return };
        if key.kind != KeyEventKind::Press {
            return;
        }

        match key.code {
            // ── 模型/提供者/权限 轮换 ──
            KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if key.modifiers.contains(KeyModifiers::SHIFT) {
                    // Ctrl+Shift+T: cycle provider
                    PROVIDER_HIGHLIGHT_UNTIL.set(Some(
                        std::time::Instant::now() + std::time::Duration::from_secs(2),
                    ));
                } else {
                    // Ctrl+T: cycle model
                    MODEL_HIGHLIGHT_UNTIL.set(Some(
                        std::time::Instant::now() + std::time::Duration::from_secs(2),
                    ));
                }
            }

            // Ctrl+P: open model panel
            KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                // Phase 5: OPEN_PANEL effect
            }

            // Ctrl+K: cycle permission mode
            KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                MODE_HIGHLIGHT_UNTIL.set(Some(
                    std::time::Instant::now() + std::time::Duration::from_secs(2),
                ));
            }

            // Esc: 关闭 popup / @mention / slash_hint
            KeyCode::Esc => {
                AT_MENTION_ACTIVE.set(false);
                SLASH_HINT_ACTIVE.set(false);
            }

            // Enter: 提交消息
            KeyCode::Enter if !key.modifiers.contains(KeyModifiers::SHIFT)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                // Phase 5: SM 处理提交
            }

            // Shift/Alt+Enter: 换行
            KeyCode::Enter => {
                // Phase 5: textarea 插入换行
            }

            // 方向键: history 浏览 → textarea 光标
            KeyCode::Up | KeyCode::Down => {
                // Phase 5: SM 处理 scroll + 光标
            }

            // 其他按键: 交由 Phase 6 全接管
            _ => {}
        }
    });
}
