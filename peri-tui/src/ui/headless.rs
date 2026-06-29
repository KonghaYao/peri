//! Headless 测试支持模块
//!
//! 提供 [`HeadlessHandle`]，允许在无真实终端的情况下对 TUI 渲染管道进行端到端集成测试。
//! 渲染路径（`main_ui::render`）与生产代码完全一致。
//!
//! ## 基本使用
//!
//! ```rust,ignore
//! let (mut app, mut handle) = App::new_headless(120, 30).await;
//! app.push_agent_event(AgentEvent::AssistantChunk { chunk: "Hello".into(), source_agent_id: None });
//! app.process_pending_events();
//! handle.render(&mut app).await.unwrap();
//! assert!(handle.contains("Hello"));
//! ```
//!
//! ## 输入模拟（E2E 测试）
//!
//! ```rust,ignore
//! let (mut app, mut handle) = App::new_headless(120, 30).await;
//! // 模拟用户输入 /help 并回车
//! HeadlessHandle::type_text(&mut app, "/help").unwrap();
//! HeadlessHandle::press_enter(&mut app).unwrap();
//! handle.render(&mut app).await.unwrap();
//! handle.assert_contains("Available commands", "应显示命令列表");
//! ```

use std::sync::Arc;

use anyhow::Result;
use ratatui::{
    backend::TestBackend,
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers},
    Terminal,
};
use tokio::sync::Notify;

use crate::{
    app::App,
    event::{keyboard, Action},
    ui::main_ui,
};

/// Headless 测试句柄，包含 TestBackend Terminal 和渲染通知
pub struct HeadlessHandle {
    pub terminal: Terminal<TestBackend>,
    pub render_notify: Arc<Notify>,
}

impl HeadlessHandle {
    // ── 渲染 ──────────────────────────────────────────────────────────────────

    /// 截取当前 buffer 为纯文本行列表（去除每行尾部空格，跳过宽字符填充 cell）
    pub fn snapshot(&self) -> Vec<String> {
        let buffer = self.terminal.backend().buffer();
        let width = buffer.area.width as usize;
        buffer
            .content
            .chunks(width)
            .map(|row| {
                // diff_option == Skip 的 cell 是宽字符的占位填充，直接跳过
                let line: String = row
                    .iter()
                    .filter_map(|cell| {
                        use ratatui::buffer::CellDiffOption;
                        if matches!(cell.diff_option, CellDiffOption::Skip) {
                            None
                        } else {
                            Some(cell.symbol())
                        }
                    })
                    .collect();
                line.trim_end().to_string()
            })
            .collect()
    }

    /// 检查任意行是否包含指定文本
    pub fn contains(&self, text: &str) -> bool {
        self.snapshot().iter().any(|line| line.contains(text))
    }

    /// 等待渲染线程完成一次渲染（内部 notify.notified().await，无 sleep）
    pub async fn wait_for_render(&self) {
        self.render_notify.notified().await;
    }

    /// 等待渲染 + 绘制到 TestBackend（便捷组合）
    ///
    /// 等效于 `handle.wait_for_render().await; handle.terminal.draw(|f| main_ui::render(f, &mut app, None))`。
    pub async fn render(&mut self, app: &mut App) -> Result<()> {
        self.wait_for_render().await;
        self.terminal.draw(|f| main_ui::render(f, app, None))?;
        Ok(())
    }

    /// 断言屏幕包含指定文本（失败时打印完整屏幕内容）
    #[track_caller]
    pub fn assert_contains(&self, text: &str, context: &str) {
        let snap = self.snapshot();
        assert!(
            self.contains(text),
            "{}\n期望包含: {}\n实际屏幕内容:\n{}",
            context,
            text,
            snap.join("\n")
        );
    }

    /// 断言屏幕不包含指定文本（失败时打印完整屏幕内容）
    #[track_caller]
    pub fn assert_not_contains(&self, text: &str, context: &str) {
        let snap = self.snapshot();
        assert!(
            !self.contains(text),
            "{}\n期望不包含: {}\n实际屏幕内容:\n{}",
            context,
            text,
            snap.join("\n")
        );
    }

    // ── 输入模拟 ──────────────────────────────────────────────────────────────

    /// 注入单个按键事件，调用完整的 `handle_key_event` 路径。
    ///
    /// 自动处理返回的 `Action`：
    /// - `Submit(text)` → 调用 `app.submit_message(text)`（触发 Agent）
    /// - `Redraw` / `Quit` → 忽略（由调用方决定重绘时机）
    pub fn press_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> Result<()> {
        let key_event = KeyEvent::new(code, modifiers);
        if let Some(action) = keyboard::handle_key_event(app, key_event)? {
            Self::apply_action(app, action);
        }
        Ok(())
    }

    /// 逐字符键入文本（模拟真实打字）。
    ///
    /// 每个字符作为独立的 `KeyCode::Char(c)` + `KeyModifiers::NONE` 注入。
    pub fn type_text(app: &mut App, text: &str) -> Result<()> {
        for c in text.chars() {
            Self::press_key(app, KeyCode::Char(c), KeyModifiers::NONE)?;
        }
        Ok(())
    }

    /// 按下 Enter 键（无修饰键）
    pub fn press_enter(app: &mut App) -> Result<()> {
        Self::press_key(app, KeyCode::Enter, KeyModifiers::NONE)
    }

    /// 按下 Esc 键
    pub fn press_escape(app: &mut App) -> Result<()> {
        Self::press_key(app, KeyCode::Esc, KeyModifiers::NONE)
    }

    /// 按下 Backspace 键
    pub fn press_backspace(app: &mut App) -> Result<()> {
        Self::press_key(app, KeyCode::Backspace, KeyModifiers::NONE)
    }

    /// 按下 Tab 键
    pub fn press_tab(app: &mut App) -> Result<()> {
        Self::press_key(app, KeyCode::Tab, KeyModifiers::NONE)
    }

    fn apply_action(app: &mut App, action: Action) {
        match action {
            Action::Submit(text) => app.submit_message(text),
            Action::Effects(_effects) => {
                // v2 effects from command dispatch — not wired in headless mode
            }
            Action::Quit | Action::Redraw => {}
        }
    }
}

#[cfg(test)]
#[path = "headless_test.rs"]
mod tests;
