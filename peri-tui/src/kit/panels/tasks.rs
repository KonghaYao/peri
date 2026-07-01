//! ratatui-kit TasksPanel component.
//!
//! Phase 6b: task list with cursor navigation (use_state + use_local_events).
//! Mock data; Phase 8 通过 Atom/props 注入真实任务列表。
//!
//! 旧版: panel/panels/tasks.rs (PanelState trait, CronTaskDto-based).

use crate::kit::theme;
use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind},
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction},
        style::{Style, Stylize},
        text::{Line, Span},
        widgets::Paragraph,
    },
};

/// Mock task entry (Phase 8: injected via Atom from real task sources).
#[allow(dead_code)]
struct TaskEntry {
    name: &'static str,
    status: TaskStatus,
}

#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum TaskStatus {
    Pending,
    Running,
    Done,
}

#[allow(dead_code)]
impl TaskStatus {
    fn label(&self) -> &'static str {
        match self {
            Self::Pending => "PENDING",
            Self::Running => "RUNNING",
            Self::Done => "DONE",
        }
    }

    fn color(&self) -> ratatui::style::Color {
        match self {
            Self::Pending => theme::WARNING,
            Self::Running => theme::ACCENT,
            Self::Done => theme::SAGE,
        }
    }
}

#[allow(dead_code)]
const TASK_ENTRIES: &[TaskEntry] = &[
    TaskEntry {
        name: "Refactor panel system",
        status: TaskStatus::Running,
    },
    TaskEntry {
        name: "Add search feature",
        status: TaskStatus::Pending,
    },
    TaskEntry {
        name: "Update documentation",
        status: TaskStatus::Pending,
    },
    TaskEntry {
        name: "Fix memory leak in executor",
        status: TaskStatus::Done,
    },
];

#[component]
pub fn TasksPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let selected = hooks.use_state(|| 0usize);

    hooks.use_local_events({
        let selected = selected.clone();
        let count = TASK_ENTRIES.len();
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
                        let mut s = selected.write();
                        *s = s.saturating_sub(1);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        let mut s = selected.write();
                        if count > 0 {
                            *s = (*s + 1).min(count - 1);
                        }
                    }
                    _ => {}
                }
            }
        }
    });

    let sel = *selected.read();
    let mut lines: Vec<Line<'_>> = Vec::new();

    // Task list
    if TASK_ENTRIES.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            "  No tasks",
            Style::new().fg(theme::MUTED),
        )]));
    } else {
        for (i, entry) in TASK_ENTRIES.iter().enumerate() {
            let is_selected = i == sel;
            let cursor = if is_selected { ">" } else { " " };
            let name_style = if is_selected {
                Style::new().fg(theme::THINKING).bold()
            } else {
                Style::new().fg(theme::TEXT)
            };
            let status_style = Style::new().fg(entry.status.color());

            lines.push(Line::from(vec![
                Span::styled(format!(" {} {}. ", cursor, i + 1), name_style),
                Span::styled(entry.name, name_style),
                Span::raw("  "),
                Span::styled(format!("[{}]", entry.status.label()), status_style),
            ]));
        }
    }

    // Bottom hint line
    lines.push(Line::from(""));
    lines.push(Line::from(vec![Span::styled(
        "  j/k) Navigate  q) Close",
        Style::new().fg(theme::MUTED),
    )]));

    let content = if lines.is_empty() {
        Paragraph::new(Line::from("  (empty)").fg(theme::MUTED))
    } else {
        Paragraph::new(ratatui::text::Text::from(lines))
    };

    element!(
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().fg(theme::BORDER),
            top_title: Line::from(" Tasks ")
                .fg(theme::THINKING)
                .bold()
                .centered(),
            width: Constraint::Length(50),
            height: Constraint::Length(14),
        ) {
            ScrollView(
                scroll_bars: ScrollBars::default(),
                width: Constraint::Fill(1),
                height: Constraint::Fill(1),
            ) {
                Text(text: content)
            }
        }
    )
}
