// [TRAP] 终端光标 vs Buffer 光标
// 禁用 tui-textarea 默认 REVERSED buffer 级光标改用终端光标时，必须有准确的 textarea 水平
// 滚动偏移读数。tui-textarea-2 的 Viewport::scroll_top() 是 pub(crate)，外部无法读取真实 top_col。
// 推断公式在 sticky scroll 场景下会算错，导致光标被钉在视口最右列→完全消失。
// 当前用 buffer 后处理方案（扫描 REVERSED 空格，移除 REVERSED + 设 bg=TEXT）等效还原光标。
// 未来恢复 IME 终端光标时必须用方案 A（UI state 跨帧 sticky），禁止复刻 PR #34 的简单推断公式。
// 详见 spec/global/domains/tui.md#issue_2026-06-17-main-textarea-cursor-invisible-long-line
mod attachment;
pub(crate) mod bg_agent_bar;
pub(crate) mod message_area;
mod popups;
mod status_bar;
mod sticky_header;

#[cfg(not(target_os = "windows"))]
use ratatui::buffer::CellDiffOption;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    widgets::{Padding, Paragraph},
};

#[cfg(target_os = "windows")]
use crate::app::textarea_cursor_pos;

use crate::{app::App, ui::theme};

pub fn render(
    f: &mut Frame,
    app: &mut App,
    v2_panel_height: Option<u16>,
    v2_view_models: Option<&[peri_acp_types::view_model::ViewModel]>,
) {
    // Setup 向导由 v2 state machine Modal handler 渲染（Phase 2.6）。
    // 此处仅做快速返回，布局无需占用。
    if app.global_ui.setup_wizard.is_some() {
        return;
    }

    let area = f.area();
    render_session_column(f, app, area, v2_panel_height, v2_view_models);
}

/// 渲染单个 session 列（含垂直布局拆分）
fn render_session_column(
    f: &mut Frame,
    app: &mut App,
    area: Rect,
    v2_panel_height: Option<u16>,
    v2_view_models: Option<&[peri_acp_types::view_model::ViewModel]>,
) {
    // 动态输入框高度
    let line_count = app.session_mgr.current_mut().ui.textarea.lines().len() as u16;
    let input_height = (line_count + 2).min(area.height * 2 / 5).max(3);

    // 缓冲消息区域（pending_messages：loading 期间用户输入的缓存，Agent 完成后自动提交）
    let pending_count = app.session_mgr.current().messages.pending_messages.len();
    let queued_height: u16 = if pending_count > 0 && app.session_mgr.current_mut().ui.loading {
        (pending_count as u16).min(3)
    } else {
        0
    };

    // 附件栏高度
    let attachment_height: u16 = if app
        .session_mgr
        .current_mut()
        .metadata
        .pending_attachments
        .is_empty()
    {
        0
    } else {
        3
    };

    // 底部展开区高度
    let panel_height = active_panel_height(app, area.height, area.width, v2_panel_height);

    // Sticky header 高度
    let sticky_header_height: u16 = app
        .session_mgr
        .current()
        .metadata
        .last_human_message
        .as_ref()
        .map(|msg| {
            let width = area.width.saturating_sub(2).max(1);
            let lines = sticky_header::estimate_header_lines(msg, width);
            lines as u16
        })
        .unwrap_or(0);

    let status_bar_height: u16 = 3;

    let bg_bar_height_val = bg_agent_bar::bg_bar_height(app);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(sticky_header_height),
            Constraint::Min(1),
            Constraint::Length(attachment_height),
            Constraint::Length(panel_height),
            Constraint::Length(queued_height),
            Constraint::Length(input_height),
            Constraint::Length(status_bar_height),
            Constraint::Length(bg_bar_height_val),
        ])
        .split(area);

    message_area::render_messages(f, app, v2_view_models, chunks[0], chunks[1]);
    attachment::render_attachment_bar(f, app, chunks[2]);

    // 底部展开区
    if panel_height > 0 {
        let panel_area = chunks[3];
        // v2 Modal: popup rendering is handled by v2 state machine Modal handler.
        // Legacy popup renderers (hitl/ask_user/rewind/oauth) have been removed (Phase 2.6).
        if v2_panel_height.is_some() {
            app.session_mgr.current_mut().ui.panel_area = Some(panel_area);
        }
    }

    // 输入框样式：Bar 焦点变暗 / 聚焦只读模式 / 正常模式
    let bar_focused = app.session_mgr.current_mut().ui.bg_bar_cursor.is_some();
    let focused_id = app.session_mgr.current_mut().focused_instance_id.clone();

    let popup_active = app.global_ui.oauth_prompt.is_some()
        || app
            .session_mgr
            .current_mut()
            .agent
            .interaction_prompt
            .is_some();

    if bar_focused || popup_active {
        // Bar 焦点模式：输入框变暗
        let block = ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::TOP | ratatui::widgets::Borders::BOTTOM)
            .border_style(ratatui::style::Style::default().fg(ratatui::style::Color::DarkGray))
            .padding(Padding::new(2, 0, 0, 0));
        app.session_mgr.current_mut().ui.textarea.set_block(block);
    } else if let Some(ref id) = focused_id {
        // 聚焦只读模式：彩色边框 + agent 名称标签 + 暗色文本
        let agents = &app.session_mgr.current_mut().background_agents;
        let color_idx = agents.iter().position(|a| a.instance_id == *id);
        let color = color_idx
            .map(bg_agent_bar::agent_color)
            .unwrap_or(ratatui::style::Color::Cyan);
        let agent_name = agents
            .iter()
            .find(|a| a.instance_id == *id)
            .map(|a| a.agent_name.as_str())
            .unwrap_or("agent");
        let title = format!("[{}]", agent_name);
        let block = ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::TOP | ratatui::widgets::Borders::BOTTOM)
            .border_style(ratatui::style::Style::default().fg(color))
            .padding(Padding::new(2, 0, 0, 0))
            .title(title);
        app.session_mgr.current_mut().ui.textarea.set_block(block);
        app.session_mgr
            .current_mut()
            .ui
            .textarea
            .set_style(ratatui::style::Style::default().fg(ratatui::style::Color::DarkGray));
    } else {
        // 正常模式：恢复与 build_textarea 一致的边框样式
        let border_color = theme::MUTED;
        let block = ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::TOP | ratatui::widgets::Borders::BOTTOM)
            .border_style(ratatui::style::Style::default().fg(border_color))
            .padding(Padding::new(2, 0, 0, 0));
        app.session_mgr.current_mut().ui.textarea.set_block(block);
        app.session_mgr
            .current_mut()
            .ui
            .textarea
            .set_style(ratatui::style::Style::default().fg(theme::TEXT));
    }

    // 缓冲消息渲染（pending_messages：loading 期间缓存，Agent 完成后自动提交）
    if queued_height > 0 {
        let queued_area = chunks[4];
        let msgs = &app.session_mgr.current().messages.pending_messages;
        let visible_count = (pending_count).min(queued_height as usize);
        let pending_style = Style::default().fg(theme::MUTED).bg(theme::USER_BG);
        for (i, msg) in msgs.iter().take(visible_count).enumerate() {
            let line_area = Rect {
                x: queued_area.x + 2,
                y: queued_area.y + i as u16,
                width: queued_area.width.saturating_sub(2),
                height: 1,
            };
            // 截断到可用宽度（字符级安全）
            let max_chars = line_area.width as usize;
            let display: String = msg.chars().take(max_chars.saturating_sub(3)).collect();
            f.render_widget(Paragraph::new(display).style(pending_style), line_area);
        }
        if pending_count > visible_count {
            let more_line = Rect {
                x: queued_area.x + 2,
                y: queued_area.y + visible_count as u16,
                width: queued_area.width.saturating_sub(2),
                height: 1,
            };
            f.render_widget(
                Paragraph::new(format!("… +{} more", pending_count - visible_count))
                    .style(pending_style),
                more_line,
            );
        }
    }

    // 输入框渲染
    // 应用失焦时隐藏光标
    let should_hide_cursor = !app.focused;
    if should_hide_cursor {
        let mut ta = app.session_mgr.current_mut().ui.textarea.clone();
        ta.set_cursor_style(Style::default().fg(theme::DIM));
        f.render_widget(&ta, chunks[5]);
    } else {
        f.render_widget(&app.session_mgr.current_mut().ui.textarea, chunks[5]);
    }
    app.session_mgr.current_mut().ui.textarea_area = Some(chunks[5]);
    // 通过 textarea 的 Block 计算 inner 区域，避免硬编码 padding/border 偏移
    // （padding 改动会自动同步，无失配风险）
    let inner = app
        .session_mgr
        .current()
        .ui
        .textarea
        .block()
        .map(|b| b.inner(chunks[5]))
        .unwrap_or(chunks[5]);

    // macOS/Linux：cursor_at_end 残影修复——扫描 textarea 渲染区域内
    // 所有 REVERSED 空格，用 bg=TEXT（白色背景）替换 REVERSED 以达到
    // 相同视觉效果（光标块），同时避免 REVERSED 跨帧残留。
    // 原始 REVERSED 空格 = 白色背景块；设 bg=TEXT 完全等效且高度自然。
    // 详见 spec/issues/2026-06-17-main-textarea-cursor-invisible-long-line.md。
    //
    // [TRAP] CJK 删除残影：删除 CJK wide char 时，wide char 的 trailing cell
    // 在 ratatui buffer 中保持 EMPTY（None），Paragraph 不覆盖；两帧之间该 cell
    // 状态相同（None, fg=TEXT, bg=Reset），ratatui diff 检测无变化不输出更新，
    // 但 VSCode xterm.js 等终端保留 wide char trailing 视觉状态（前一帧像素
    // 或 bg 颜色残留）→ 用户看到"白色块"残影。修复：对 inner 区域所有 cell
    // 设置 AlwaysUpdate，每帧强制重发整个 textarea 区域，确保终端状态严格同步。
    #[cfg(not(target_os = "windows"))]
    if app.focused {
        let buf = f.buffer_mut();
        for y in inner.y..inner.bottom() {
            for x in inner.x..inner.right() {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    if cell.symbol() == " " && cell.modifier.contains(Modifier::REVERSED) {
                        cell.modifier.remove(Modifier::REVERSED);
                        cell.bg = theme::TEXT;
                    }
                    cell.set_diff_option(CellDiffOption::AlwaysUpdate);
                }
            }
        }
    }

    // Windows：将终端光标定位到输入框光标处，使 IME 合成窗口跟随输入位置
    // 仅在聚焦时设置（失焦时终端光标由 ratatui 自动隐藏）
    // macOS/Linux 不启用——保留 tui-textarea 默认 REVERSED buffer 光标，
    // cursor_at_end 残影由渲染后处理修复。
    #[cfg(target_os = "windows")]
    if app.focused {
        if let Some((cx, cy)) = textarea_cursor_pos(&app.session_mgr.current().ui.textarea, inner) {
            f.set_cursor_position((cx, cy));
        }
    }

    // Prediction placeholder 叠加（textarea 为空 + 有 prediction 时显示）
    if let Some(ref pred) = app.session_mgr.current().ui.prediction {
        let textarea_empty = app
            .session_mgr
            .current()
            .ui
            .textarea
            .lines()
            .iter()
            .all(|l| l.is_empty());
        if textarea_empty {
            let pred_area = ratatui::layout::Rect {
                x: inner.x,
                y: inner.y,
                width: inner.width,
                height: 1,
            };
            let pred_text = ratatui::text::Line::from(ratatui::text::Span::styled(
                &pred.text,
                ratatui::style::Style::default().fg(theme::DIM),
            ));
            f.render_widget(ratatui::widgets::Paragraph::new(pred_text), pred_area);
        }
    }

    // ❯ 前缀（x 位于 Block 的 left padding 区域，y 与文本第一行对齐）
    let prompt_area = Rect {
        x: chunks[5].x,
        y: inner.y,
        width: 2,
        height: 1,
    };
    let loading = app.session_mgr.current_mut().ui.loading;
    let prompt_color = if loading { theme::MUTED } else { theme::TEXT };
    let prompt_style = Style::default().fg(prompt_color).add_modifier(if loading {
        Modifier::empty()
    } else {
        Modifier::BOLD
    });
    f.render_widget(Paragraph::new("❯").style(prompt_style), prompt_area);

    // 统一命令/Skills 提示条 或 @ 提及弹窗（互斥）
    if app.session_mgr.current_mut().ui.at_mention.active {
        crate::app::at_mention::popup::render_at_mention_popup(
            f,
            &app.session_mgr.current_mut().ui.at_mention,
            chunks[5],
        );
    }
    // Legacy hints renderer removed (Phase 2.6). v2 slash_completion and
    // command hints are now rendered by the state machine or kit components.

    // 状态栏
    status_bar::render_status_bar(f, app, chunks[6]);
    if bg_bar_height_val > 0 {
        bg_agent_bar::render_bg_agent_bar(f, app, chunks[7]);
        app.session_mgr.current_mut().ui.bg_bar_area = Some(chunks[7]);
    } else {
        app.session_mgr.current_mut().ui.bg_bar_area = None;
    }
}

/// 计算底部展开区所需高度（无激活面板时返回 0）。
///
/// v2 路径：高度由 `draw()` 预计算（`handler.desired_height()`
/// 或 `panel.desired_height()`），此处仅做上限约束。
/// v1 fallback：仍为少数 headless test 场景用 flat percentage。
fn active_panel_height(
    app: &App,
    screen_height: u16,
    _screen_width: u16,
    v2_panel_height: Option<u16>,
) -> u16 {
    // v2 Modal 面板/交互弹窗：用 desired_height 的结果，受 60% 上限约束。
    if let Some(h) = v2_panel_height {
        return h.min(screen_height * 3 / 5).max(1);
    }
    // v1 popup fallback: 仍有少数场景（headless test）未迁移到 v2 handler，
    // 保守返回 60% 高度，让 legacy popup rendering 路径继续工作。
    let has_popup = app.session_mgr.current().agent.interaction_prompt.is_some()
        || app.global_ui.oauth_prompt.is_some();
    if has_popup {
        (screen_height * 3 / 5).max(1)
    } else {
        0
    }
}
