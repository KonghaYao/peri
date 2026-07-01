//! ratatui-kit AgentPanel component.
//!
//! Phase 6c batch 1: agent session info display with cursor navigation
//! (use_state + use_local_events). Mock data; Phase 8 通过 Atom/props 注入
//! 真实 agent session 状态。
//!
//! 旧版: panel/panels/agent.rs (PanelState trait, agent selection).

use crate::ui::theme;
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

/// Mock agent session info row.
#[allow(dead_code)]
struct AgentInfoRow {
    label: &'static str,
    value: &'static str,
}

/// Mock agent session data (Phase 8: injected via Atom from live session).
#[allow(dead_code)]
const AGENT_INFO_ROWS: &[AgentInfoRow] = &[
    AgentInfoRow {
        label: "Model",
        value: "claude-sonnet-4-20250514",
    },
    AgentInfoRow {
        label: "Provider",
        value: "Anthropic",
    },
    AgentInfoRow {
        label: "Context Window",
        value: "200K tokens (standard)",
    },
    AgentInfoRow {
        label: "Token Usage",
        value: "34.2k input / 8.1k output",
    },
    AgentInfoRow {
        label: "Session ID",
        value: "sess_01JXYZ...",
    },
    AgentInfoRow {
        label: "Subagent Count",
        value: "2 active (code-review, test-gen)",
    },
];

#[component]
pub fn AgentPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let cursor = hooks.use_state(|| 0usize);

    hooks.use_local_events({
        let cursor = cursor.clone();
        let count = AGENT_INFO_ROWS.len();
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
                    _ => {}
                }
            }
        }
    });

    let sel = *cursor.read();
    let mut lines: Vec<Line<'_>> = Vec::new();

    // Header
    lines.push(Line::from(vec![Span::styled(
        "  Current Agent Session",
        Style::new().fg(theme::TEXT).bold(),
    )]));
    lines.push(Line::from(vec![Span::styled(
        "  ----------------------",
        Style::new().fg(theme::DIM),
    )]));
    lines.push(Line::from(""));

    // Agent info rows
    for (i, row) in AGENT_INFO_ROWS.iter().enumerate() {
        let is_selected = i == sel;
        let cursor_mark = if is_selected { ">" } else { " " };
        let label_style = if is_selected {
            Style::new().fg(theme::THINKING).bold()
        } else {
            Style::new().fg(theme::MUTED)
        };
        let value_style = if is_selected {
            Style::new().fg(theme::TEXT).bold()
        } else {
            Style::new().fg(theme::TEXT)
        };

        lines.push(Line::from(vec![
            Span::styled(
                format!(" {} ", cursor_mark),
                Style::new().fg(theme::THINKING),
            ),
            Span::styled(format!("{:<18}", format!("{}:", row.label)), label_style),
            Span::styled(row.value, value_style),
        ]));
    }

    // Footer
    lines.push(Line::from(""));
    lines.push(Line::from("  j/k) Navigate  q) Close").fg(theme::DIM));

    let content = Paragraph::new(ratatui::text::Text::from(lines));

    element!(
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().fg(theme::BORDER),
            top_title: Line::from(" Agent ")
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
