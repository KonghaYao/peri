//! Headless 测试支持模块
//!
//! 提供 [`HeadlessHandle`]，允许在无真实终端的情况下对 TUI 渲染管道进行端到端集成测试。
//! 渲染路径（`main_ui::render`）与生产代码完全一致。
//!
//! ## 基本使用
//!
//! ```rust,ignore
//! let (mut app, mut handle) = App::new_headless(120, 30).await;
//! app.push_agent_event(AgentEvent::AssistantChunk);
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

use anyhow::Result;
use ratatui::{
    Terminal,
    backend::TestBackend,
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers},
};

use crate::{app::App, ui::main_ui};

/// Headless 测试句柄，包含 TestBackend Terminal
/// P5: render_notify removed — sync rendering from state machine
pub struct HeadlessHandle {
    pub terminal: Terminal<TestBackend>,
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

    /// P5: Yield to allow pending tasks to complete (sync rendering, no render thread)
    pub async fn wait_for_render(&self) {
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
    }

    /// 等待渲染 + 绘制到 TestBackend（便捷组合）
    ///
    /// Phase 2.6 step 7e.9: 从 `v2_test_views` 构造 v2 ViewModels，
    /// 让测试走与生产一致的 v2 渲染路径。
    ///
    /// Phase 2.6 step 1：同时构造 `SessionSubAgentProbe` 并通过 thread-local
    /// 注入，让测试覆盖与生产 `draw_now` 完全一致的 SubAgent 渲染路径
    /// （包括 child_messages 权威源注入）。
    ///
    /// Phase 2.6 step 6：headless 路径不经过 ACP ViewCommit，因此
    /// SubAgentGroup 占位符不会由 view_mapper 自动产生。这里从
    /// `subagent_status` 合成 v2 SubAgentGroupData 占位符并追加到 v2_vms，
    /// 模拟生产中 ACP 层对 "Agent" 工具调用的处理，让
    /// `render_subagent_group` 能找到插槽并通过 probe 注入 child_messages。
    pub async fn render(&mut self, app: &mut App) -> Result<()> {
        self.wait_for_render().await;
        let v2_views = &app.session_mgr.current().messages.v2_test_views;
        let mut v2_vms: Vec<peri_acp_types::view_model::ViewModel> = v2_views.clone();
        // 合成 SubAgentGroup 占位符（headless 路径无 ACP ViewCommit）
        let session = app.session_mgr.current();
        for (instance_id, status) in session.subagent_status.iter() {
            v2_vms.push(peri_acp_types::view_model::ViewModel::SubAgentGroup(
                peri_acp_types::view_model::SubAgentGroupData {
                    agent_id: status.agent_id.clone(),
                    agent_name: status.agent_id.clone(),
                    view_models: Vec::new(),
                    collapsed: !status.is_error && !status.is_running,
                },
            ));
            let _ = instance_id;
        }
        let probe = crate::app::SessionSubAgentProbe::new(session.subagent_status.clone());
        let status_probe: std::rc::Rc<dyn crate::render::view_render::SubAgentStatusProbe> =
            std::rc::Rc::new(probe);
        crate::render::view_render::with_status_probe(status_probe, || {
            self.terminal
                .draw(|f| main_ui::render(f, app, None, Some(&v2_vms)))
        })?;
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

    /// 注入单个按键事件，直接写入 textarea（键盘 fallback 已删除，Phase 2.6）。
    ///
    /// 自动处理回车提交：
    /// - Enter → 调用 `app.submit_message(text)`（触发 Agent）
    /// - 其他按键 → 直接插入 textarea
    pub fn press_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> Result<()> {
        use tui_textarea::Input;
        let input = Input::from(KeyEvent::new(code, modifiers));
        let textarea = &mut app.session_mgr.current_mut().ui.textarea;
        match input {
            Input {
                key: tui_textarea::Key::Enter,
                ..
            } => {
                let text: String = textarea.lines().join("\n");
                textarea.delete_str(textarea.lines().len());
                app.submit_message(text);
            }
            Input {
                key: tui_textarea::Key::Esc,
                ..
            } => {
                textarea.delete_str(textarea.lines().len());
            }
            _ => {
                textarea.input(input);
            }
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
}

#[cfg(test)]
#[path = "headless_test.rs"]
mod tests;
