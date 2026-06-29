use peri_widgets::{BorderedPanel, ScrollState, ScrollableArea};
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span, Text},
    Frame,
};

use crate::{
    app::{App, CronPanel},
    ui::{main_ui::highlight_line_spans, theme},
};

/// CronPanel 渲染
pub(crate) fn render_cron_panel(f: &mut Frame, panel: &CronPanel, app: &mut App, area: Rect) {
    let title = " 定时任务 ";
    let inner = BorderedPanel::new(Span::styled(
        title,
        Style::default()
            .fg(theme::THINKING)
            .add_modifier(Modifier::BOLD),
    ))
    .border_style(Style::default().fg(theme::BORDER))
    .render(f, area);
    let mut lines: Vec<Line> = Vec::new();

    for (i, task) in panel.tasks().iter().enumerate() {
        let is_cursor = i == panel.cursor();
        let cursor_char = if is_cursor { "❯ " } else { "  " };
        let status_icon = if task.enabled {
            "✓启用"
        } else {
            "✗禁用"
        };
        let next = task
            .next_fire
            .map(|t| {
                // Convert UTC to local time display
                let local: chrono::DateTime<chrono::Local> = t.into();
                local.format("%H:%M:%S").to_string()
            })
            .unwrap_or_else(|| "N/A".to_string());

        let prompt_truncated: String = task.prompt.chars().take(30).collect();
        let prompt_display = if task.prompt.chars().count() > 30 {
            format!("{}…", prompt_truncated)
        } else {
            prompt_truncated
        };

        let style = if is_cursor {
            Style::default()
                .fg(theme::THINKING)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::TEXT)
        };

        let status_style = if task.enabled {
            Style::default().fg(theme::SAGE)
        } else {
            Style::default().fg(theme::MUTED)
        };

        lines.push(Line::from(vec![
            Span::styled(
                cursor_char.to_string(),
                Style::default().fg(theme::THINKING),
            ),
            Span::styled(format!("[{}] ", status_icon), status_style),
            Span::styled(format!("{} ", task.expression), style),
            Span::styled(format!("| {} | ", next), Style::default().fg(theme::MUTED)),
            Span::styled(prompt_display, style),
        ]));
    }

    // 空列表引导
    if panel.tasks().is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  （无定时任务，使用 /loop 命令创建）",
            Style::default().fg(theme::MUTED),
        )));
    }

    // 存储面板元数据供鼠标选区使用
    app.session_mgr.current_mut().ui.panel_area = Some(inner);
    app.session_mgr.current_mut().ui.panel_scroll_offset = panel.scroll_offset();
    app.session_mgr.current_mut().ui.panel_plain_lines = lines
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect();

    // 应用面板选区高亮
    if app.session_mgr.current_mut().ui.panel_selection.is_active() {
        let sel = &app.session_mgr.current_mut().ui.panel_selection;
        if let (Some(start), Some(end)) = (sel.start, sel.end) {
            let ((sr, sc), (er, ec)) = if start <= end {
                (start, end)
            } else {
                (end, start)
            };
            let scroll = app.session_mgr.current_mut().ui.panel_scroll_offset as usize;
            let visible_start = scroll;
            let visible_end = scroll + inner.height as usize;
            for line_idx in sr as usize..=er as usize {
                if line_idx < visible_start || line_idx >= visible_end {
                    continue;
                }
                let visual_idx = line_idx - visible_start;
                if visual_idx >= lines.len() {
                    continue;
                }
                let (cs, ce) = if line_idx == sr as usize && line_idx == er as usize {
                    (sc as usize, ec as usize)
                } else if line_idx == sr as usize {
                    (sc as usize, usize::MAX)
                } else if line_idx == er as usize {
                    (0, ec as usize)
                } else {
                    (0, usize::MAX)
                };
                let spans = std::mem::take(&mut lines[visual_idx].spans);
                lines[visual_idx] = Line::from(highlight_line_spans(spans, cs, ce));
            }
        }
    }

    let mut scroll_state = ScrollState::with_offset(panel.scroll_offset());
    app.session_mgr.current_mut().ui.panel_scrollbar_metrics =
        ScrollableArea::new(Text::from(lines))
            .scrollbar_style(Style::default().fg(theme::MUTED))
            .render(f, inner, &mut scroll_state);
}

#[cfg(test)]
mod tests {
    use crate::app::{App, CronPanel};

    async fn render_headless_cron_empty() -> (App, crate::ui::headless::HeadlessHandle) {
        let (mut app, mut handle) = App::new_headless(120, 30).await;
        app.global_panels
            .open(crate::app::panel_manager::PanelState::Cron(CronPanel::new(
                vec![],
            )));
        handle
            .terminal
            .draw(|f| crate::ui::main_ui::render(f, &mut app, None))
            .unwrap();
        (app, handle)
    }

    #[tokio::test]
    async fn test_cron_empty_shows_guide() {
        let (_, handle) = render_headless_cron_empty().await;
        let snap = handle.snapshot().join("\n");
        assert!(
            snap.contains("loop"),
            "空 Cron 面板应显示 /loop 创建引导，实际:\n{}",
            snap
        );
    }
}
