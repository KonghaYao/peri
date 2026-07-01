use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph},
};

use crate::{app::App, ui::theme};

/// 待发送附件栏（有附件时显示在输入框上方）
pub(crate) fn render_attachment_bar(f: &mut Frame, app: &App, area: Rect) {
    if area.height == 0 {
        return;
    }

    let block = Block::default()
        .title(Span::styled(
            " 待发送附件 ",
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::ACCENT));
    f.render_widget(&block, area);

    let inner = block.inner(area);

    // 第 1 行：所有附件标签
    let tags: String = app
        .session_mgr
        .current()
        .metadata
        .pending_attachments
        .iter()
        .map(|att| {
            let size_kb = (att.size_bytes / 1024).max(1);
            format!("[img {} {}KB]", att.label, size_kb)
        })
        .collect::<Vec<_>>()
        .join("  ");

    let lines = vec![
        Line::from(Span::styled(tags, Style::default().fg(theme::TEXT))),
        Line::from(Span::styled(
            "Del: 删除最后一张",
            Style::default().fg(theme::MUTED),
        )),
    ];

    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}
