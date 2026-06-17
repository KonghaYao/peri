mod attachment;
pub(crate) mod bg_agent_bar;
pub(crate) mod message_area;
pub(crate) mod panels;
mod popups;
mod status_bar;
mod sticky_header;

pub(crate) use message_area::highlight_line_spans;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    widgets::{Padding, Paragraph},
    Frame,
};

#[cfg(target_os = "windows")]
use crate::app::textarea_cursor_pos;
use crate::{app::App, ui::theme};

pub fn render(f: &mut Frame, app: &mut App) {
    // Setup 向导：全屏覆盖，优先于所有正常界面
    if app.global_ui.setup_wizard.is_some() {
        popups::setup_wizard::render_setup_wizard(f, app);
        return;
    }

    let area = f.area();
    render_session_column(f, app, area);
}

/// 渲染单个 session 列（含垂直布局拆分）
fn render_session_column(f: &mut Frame, app: &mut App, area: Rect) {
    // 动态输入框高度
    let line_count = app.session_mgr.current_mut().ui.textarea.lines().len() as u16;
    let input_height = (line_count + 2).min(area.height * 2 / 5).max(3);

    // 缓冲消息高度（loading 时在输入框上方显示待发送消息）
    let pending_count = app
        .session_mgr
        .current_mut()
        .messages
        .pending_messages
        .len();
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
    let panel_height = active_panel_height(app, area.height, area.width);

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

    message_area::render_messages(f, app, chunks[0], chunks[1]);
    attachment::render_attachment_bar(f, app, chunks[2]);

    // 底部展开区
    if panel_height > 0 {
        let panel_area = chunks[3];
        match &app.session_mgr.current_mut().agent.interaction_prompt {
            Some(crate::app::InteractionPrompt::Approval(_)) => {
                popups::hitl::render_hitl_popup(f, app, panel_area);
            }
            Some(crate::app::InteractionPrompt::Questions(_)) => {
                popups::ask_user::render_ask_user_popup(f, app, panel_area);
            }
            Some(crate::app::InteractionPrompt::Rewind(_)) => {
                popups::rewind::render_rewind_popup(f, app, panel_area);
            }
            None => {}
        }
        if app.global_ui.oauth_prompt.is_some() {
            popups::oauth::render_oauth_popup(f, app, panel_area);
        }
        // PanelManager 统一渲染分发：session 面板优先，global 面板次之
        if app
            .session_mgr
            .current_mut()
            .agent
            .interaction_prompt
            .is_none()
            && app.global_ui.oauth_prompt.is_none()
        {
            if app.session_mgr.current_mut().session_panels.is_any_open() {
                let mut state = app
                    .session_mgr
                    .current_mut()
                    .session_panels
                    .take_active()
                    .expect("is_any_open was true");
                state.render(f, app, panel_area);
                app.session_mgr
                    .current_mut()
                    .session_panels
                    .put_active(state);
            } else if app.global_panels.is_any_open() {
                let mut state = app
                    .global_panels
                    .take_active()
                    .expect("is_any_open was true");
                state.render(f, app, panel_area);
                app.global_panels.put_active(state);
            }
        }
    }

    // 缓冲消息预览（loading 时在输入框上方显示待发送消息）
    if queued_height > 0 {
        let queued_area = chunks[4];
        let msgs = &app.session_mgr.current_mut().messages.pending_messages;
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
            let suffix = if msg.chars().count() > max_chars.saturating_sub(3) {
                "…"
            } else {
                ""
            };
            f.render_widget(
                Paragraph::new(format!("{}{}", display, suffix)).style(pending_style),
                line_area,
            );
        }
        if pending_count > visible_count {
            let more_area = Rect {
                x: queued_area.x + 2,
                y: queued_area.y + visible_count as u16,
                width: queued_area.width.saturating_sub(2),
                height: 1,
            };
            f.render_widget(
                Paragraph::new(format!("… +{} more", pending_count - visible_count))
                    .style(pending_style),
                more_area,
            );
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

    // Windows：将终端光标定位到输入框光标处，使 IME 合成窗口跟随输入位置
    // 仅在聚焦时设置（失焦时终端光标由 ratatui 自动隐藏）
    // macOS/Linux 不启用——保留 tui-textarea 默认 REVERSED buffer 光标，
    // 避免 textarea_cursor_pos 推断公式与 sticky scroll 不一致导致长行行尾光标不可见。
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
    } else {
        popups::hints::render_unified_hint(f, app, chunks[5]);
    }

    // 状态栏
    status_bar::render_status_bar(f, app, chunks[6]);
    if bg_bar_height_val > 0 {
        bg_agent_bar::render_bg_agent_bar(f, app, chunks[7]);
        app.session_mgr.current_mut().ui.bg_bar_area = Some(chunks[7]);
    } else {
        app.session_mgr.current_mut().ui.bg_bar_area = None;
    }
}

/// 计算底部展开区所需高度（无激活面板时返回 0）
fn active_panel_height(app: &App, screen_height: u16, screen_width: u16) -> u16 {
    // plugin 面板可以占 70%，AskUser 弹窗允许 75%（选项多/文字长需要更多空间），其他最多 60%
    let is_plugin_panel = app.global_panels.is_active(crate::app::PanelKind::Plugin);
    let has_ask_user = matches!(
        &app.session_mgr.current().agent.interaction_prompt,
        Some(crate::app::InteractionPrompt::Questions(_))
    );
    let max_h = if is_plugin_panel {
        screen_height * 70 / 100
    } else if has_ask_user {
        screen_height * 3 / 4
    } else {
        screen_height * 3 / 5
    };
    let raw = if let Some(h) = app
        .session_mgr
        .current()
        .session_panels
        .dispatch_desired_height(screen_height, screen_width)
    {
        h
    } else if let Some(h) = app
        .global_panels
        .dispatch_desired_height(screen_height, screen_width)
    {
        h
    } else if let Some(crate::app::InteractionPrompt::Approval(p)) =
        &app.session_mgr.current().agent.interaction_prompt
    {
        (p.items.len() as u16 * 2 + 5).max(5)
    } else if app.global_ui.oauth_prompt.is_some() {
        9 // 标题1 + 提示1 + URL1 + 空行1 + 输入框1 + 错误1 + 快捷键1 + 边框2
    } else if let Some(crate::app::InteractionPrompt::Questions(p)) =
        &app.session_mgr.current().agent.interaction_prompt
    {
        let cur = &p.questions[p.active_tab];
        // BorderedPanel 无左右边框，内容区宽度 = screen_width；滚动条占 1 列
        let panel_width = screen_width.saturating_sub(1) as usize;
        popups::ask_user_height::ask_user_content_height(&cur.data, panel_width).max(8)
    } else if let Some(crate::app::InteractionPrompt::Rewind(p)) =
        &app.session_mgr.current().agent.interaction_prompt
    {
        let base = p.items.len() as u16 + 3;
        let confirm_extra = if p.mode == crate::app::RewindMode::ConfirmRevert {
            let selected = &p.items[p.cursor];
            (selected.file_changes.len() as u16 + 3).min(10)
        } else {
            0
        };
        (base + confirm_extra).max(5)
    } else {
        0
    };
    raw.min(max_h)
}
