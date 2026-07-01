//! ratatui-kit WorkflowPanel component.
//!
//! Phase 6b 第2批: workflow list with cursor navigation (use_state + use_local_events).
//! Mock data; Phase 8 通过 Atom/props 注入真实 workflow snapshot 列表。
//!
//! 旧版: panel/panels/workflow.rs (PanelState trait, WorkflowRunEntry-based).

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

/// Mock workflow snapshot entry.
#[allow(dead_code)]
struct WorkflowEntry {
    name: &'static str,
    status: WorkflowStatus,
    agent_count: u32,
    tool_count: u32,
    started: &'static str,
    finished: Option<&'static str>,
}

#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum WorkflowStatus {
    Running,
    Done,
    Error,
    Failed,
}

#[allow(dead_code)]
impl WorkflowStatus {
    fn label(&self) -> &'static str {
        match self {
            Self::Running => "RUNNING",
            Self::Done => "DONE",
            Self::Error => "ERROR",
            Self::Failed => "FAILED",
        }
    }

    fn color(&self) -> ratatui::style::Color {
        match self {
            Self::Running => theme::ACCENT,
            Self::Done => theme::SAGE,
            Self::Error => theme::WARNING,
            Self::Failed => theme::ERROR,
        }
    }
}

#[allow(dead_code)]
const WORKFLOW_ENTRIES: &[WorkflowEntry] = &[
    WorkflowEntry {
        name: "PR Review Pipeline",
        status: WorkflowStatus::Running,
        agent_count: 3,
        tool_count: 24,
        started: "2026-07-01 14:32",
        finished: None,
    },
    WorkflowEntry {
        name: "Code Generation Suite",
        status: WorkflowStatus::Done,
        agent_count: 4,
        tool_count: 18,
        started: "2026-07-01 10:15",
        finished: Some("2026-07-01 11:02"),
    },
    WorkflowEntry {
        name: "Deploy Staging",
        status: WorkflowStatus::Failed,
        agent_count: 2,
        tool_count: 8,
        started: "2026-06-30 22:10",
        finished: Some("2026-06-30 22:18"),
    },
];

#[component]
pub fn WorkflowPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let cursor = hooks.use_state(|| 0usize);

    hooks.use_local_events({
        let cursor = cursor.clone();
        let count = WORKFLOW_ENTRIES.len();
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
                        // TODO Phase 8: open workflow detail / resume
                    }
                    _ => {}
                }
            }
        }
    });

    let sel = *cursor.read();
    let mut lines: Vec<Line<'_>> = Vec::new();

    if WORKFLOW_ENTRIES.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            "  No active workflows",
            Style::new().fg(theme::MUTED),
        )]));
    } else {
        for (i, entry) in WORKFLOW_ENTRIES.iter().enumerate() {
            let is_selected = i == sel;
            let cursor_marker = if is_selected { ">" } else { " " };
            let name_style = if is_selected {
                Style::new().fg(theme::THINKING).bold()
            } else {
                Style::new().fg(theme::TEXT)
            };
            let status_style = Style::new().fg(entry.status.color());

            // Line 1: cursor + name + status badge
            lines.push(Line::from(vec![
                Span::styled(format!(" {} {} ", cursor_marker, entry.name), name_style),
                Span::styled(format!("[{}]", entry.status.label()), status_style),
            ]));

            // Line 2: agents, tool calls, time range
            let time_range = match entry.finished {
                Some(f) => format!("{} → {}", entry.started, f),
                None => format!("{} → ...", entry.started),
            };
            lines.push(Line::from(vec![Span::styled(
                format!(
                    "    {} agents  {} tool calls  {}",
                    entry.agent_count, entry.tool_count, time_range,
                ),
                Style::new().fg(theme::MUTED),
            )]));

            // Blank separator line
            lines.push(Line::from(""));
        }
    }

    // Bottom hint line
    lines.push(Line::from(vec![Span::styled(
        "  j/k) Navigate  Enter) View  q) Close",
        Style::new().fg(theme::MUTED),
    )]));

    let content = if WORKFLOW_ENTRIES.is_empty() {
        Paragraph::new(Line::from("  (empty)").fg(theme::MUTED))
    } else {
        Paragraph::new(ratatui::text::Text::from(lines))
    };

    element!(
        Border(
            flex_direction: ratatui_kit::ratatui::layout::Direction::Vertical,
            border_style: Style::new().fg(theme::BORDER),
            top_title: Line::from(" Workflow ")
                .fg(theme::THINKING)
                .bold()
                .centered(),
            width: ratatui_kit::ratatui::layout::Constraint::Length(60),
            height: ratatui_kit::ratatui::layout::Constraint::Length(14),
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
