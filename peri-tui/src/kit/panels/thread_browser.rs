//! ratatui-kit ThreadBrowserPanel component.
//!
//! S6c：thread 列表从 `THREAD_LIST` atom 读取（由 `service_snapshot` 后台任务
//! 周期性从 ServiceRegistry.thread_store 派生）。Enter 切换 thread 操作 S11
//! 解耦后通过 AcpClient 触发（暂留 TODO）。

use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind},
    prelude::*,
    ratatui::{
        style::{Style, Stylize},
        text::{Line, Span},
        widgets::Paragraph,
    },
};

use crate::kit::atoms::{THREAD_LIST, ThreadSummary};
use crate::ui::theme;

#[component]
pub fn ThreadBrowserPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let cursor = hooks.use_state(|| 0usize);

    // S6c: 订阅 THREAD_LIST atom——后台 service_snapshot 2s 派生一次
    let threads_store = hooks.use_store(*THREAD_LIST.get().unwrap());
    let threads: Vec<ThreadSummary> = threads_store.read().clone();
    let _ = threads_store; // StoreState 是 Copy，无需显式 drop
    let count = threads.len();

    hooks.use_local_events({
        let cursor = cursor.clone();
        let count = count;
        move |event: Event| {
            if let Event::Key(key) = event {
                if key.kind != KeyEventKind::Press {
                    return;
                }
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => {
                        // 由 PanelOverlay 上层 Esc 处理关闭
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        let mut c = cursor.write();
                        *c = c.saturating_sub(1);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        let mut c = cursor.write();
                        if count > 0 {
                            *c = (*c + 1).min(count - 1);
                        }
                    }
                    KeyCode::Enter => {
                        // S11 TODO: 通过 AcpClient 切换 thread
                    }
                    _ => {}
                }
            }
        }
    });

    let sel = *cursor.read();
    let mut lines: Vec<Line<'_>> = Vec::new();

    if threads.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            "  No threads",
            Style::new().fg(theme::MUTED),
        )]));
    } else {
        for (i, entry) in threads.iter().enumerate() {
            let is_selected = i == sel;
            let cursor_marker = if is_selected { ">" } else { " " };
            let name_style = if is_selected {
                Style::new().fg(theme::THINKING).bold()
            } else {
                Style::new().fg(theme::TEXT)
            };

            let id_short: String = entry.id.chars().take(8.min(entry.id.len())).collect();
            let title = entry
                .title
                .clone()
                .unwrap_or_else(|| format!("(untitled {})", id_short));

            // Line 1: cursor + title
            lines.push(Line::from(vec![Span::styled(
                format!(" {} {} ", cursor_marker, title),
                name_style,
            )]));

            // Line 2: id, updated_at, message_count
            let updated = entry
                .updated_at
                .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_else(|| "-".to_string());
            lines.push(Line::from(vec![Span::styled(
                format!(
                    "    {}  {}  {} messages",
                    entry.id, updated, entry.message_count,
                ),
                Style::new().fg(theme::MUTED),
            )]));

            // Line 3: cwd (truncated for narrow viewports)
            let cwd: String = entry.cwd.chars().take(54).collect();
            lines.push(Line::from(vec![Span::styled(
                format!("    {}", cwd),
                Style::new().fg(theme::DIM),
            )]));

            // Blank separator line
            lines.push(Line::from(""));
        }
    }

    // Bottom hint line
    lines.push(Line::from(vec![Span::styled(
        "  j/k) Navigate  Enter) Switch  q) Close",
        Style::new().fg(theme::MUTED),
    )]));

    let content = if threads.is_empty() {
        Paragraph::new(Line::from("  (empty)").fg(theme::MUTED))
    } else {
        Paragraph::new(ratatui::text::Text::from(lines))
    };

    element!(
        Border(
            flex_direction: ratatui_kit::ratatui::layout::Direction::Vertical,
            border_style: Style::new().fg(theme::BORDER),
            top_title: Line::from(" Threads ")
                .fg(theme::THINKING)
                .bold()
                .centered(),
            width: ratatui_kit::ratatui::layout::Constraint::Length(60),
            height: ratatui_kit::ratatui::layout::Constraint::Length(18),
        ) {
            ScrollView(
                scroll_bars: ScrollBars::default(),
                width: ratatui_kit::ratatui::layout::Constraint::Fill(1),
                height: ratatui_kit::ratatui::layout::Constraint::Fill(1),
            ) {
                Text(text: content)
            }
        }
    )
}
