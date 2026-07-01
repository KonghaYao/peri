//! ratatui-kit ThreadBrowserPanel component.
//!
//! Phase 6b 第2批: session list with cursor navigation (use_state + use_local_events).
//! Mock data; Phase 8 通过 Atom/props 注入真实 session 列表。
//!
//! 旧版: panel/panels/thread_browser.rs (PanelState trait, ThreadMeta-based).

use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind},
    prelude::*,
    ratatui::{
        style::{Style, Stylize},
        text::{Line, Span},
        widgets::Paragraph,
    },
};

use crate::ui::theme;

/// Mock session entry.
#[allow(dead_code)]
struct SessionEntry {
    id: &'static str,
    name: &'static str,
    time: &'static str,
    messages: u32,
}

#[allow(dead_code)]
const SESSION_ENTRIES: &[SessionEntry] = &[
    SessionEntry {
        id: "session-a3f2d1c0",
        name: "Refactor panel system",
        time: "2026-07-01 15:42",
        messages: 34,
    },
    SessionEntry {
        id: "session-b1c4e5d2",
        name: "Fix memory leak in executor",
        time: "2026-06-30 09:18",
        messages: 52,
    },
    SessionEntry {
        id: "session-d5e6f7a8",
        name: "Add search feature",
        time: "2026-06-29 16:55",
        messages: 27,
    },
    SessionEntry {
        id: "session-f7a8b9c0",
        name: "Update documentation",
        time: "2026-06-28 11:30",
        messages: 18,
    },
    SessionEntry {
        id: "session-c9b0d1e2",
        name: "Code review pipeline",
        time: "2026-06-27 14:05",
        messages: 41,
    },
];

#[component]
fn ThreadBrowserPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let cursor = hooks.use_state(|| 0usize);

    hooks.use_local_events({
        let cursor = cursor.clone();
        let count = SESSION_ENTRIES.len();
        move |event: Event| {
            if let Event::Key(key) = event {
                if key.kind != KeyEventKind::Press {
                    return;
                }
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => {
                        // TODO Phase 8: close panel via use_input_layer
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
                        // TODO Phase 8: switch session via use_input_layer
                    }
                    _ => {}
                }
            }
        }
    });

    let sel = *cursor.read();
    let mut lines: Vec<Line<'_>> = Vec::new();

    if SESSION_ENTRIES.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            "  No sessions",
            Style::new().fg(theme::MUTED),
        )]));
    } else {
        for (i, entry) in SESSION_ENTRIES.iter().enumerate() {
            let is_selected = i == sel;
            let cursor_marker = if is_selected { ">" } else { " " };
            let name_style = if is_selected {
                Style::new().fg(theme::THINKING).bold()
            } else {
                Style::new().fg(theme::TEXT)
            };

            // Line 1: cursor + name
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} {} ", cursor_marker, entry.name),
                    name_style,
                ),
            ]));

            // Line 2: id, time, messages
            lines.push(Line::from(vec![Span::styled(
                format!(
                    "    {}  {}  {} messages",
                    entry.id, entry.time, entry.messages,
                ),
                Style::new().fg(theme::MUTED),
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

    let content = if SESSION_ENTRIES.is_empty() {
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
            width: ratatui_kit::ratatui::layout::Constraint::Length(56),
            height: ratatui_kit::ratatui::layout::Constraint::Length(16),
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
