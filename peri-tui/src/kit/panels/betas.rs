//! ratatui-kit BetasPanel component.
//!
//! Phase 6a: toggle list with cursor navigation (use_state + use_local_events).
//! Mock data; Phase 8 通过 Atom/props 注入真实 feature 列表。

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

use crate::ui::theme;

/// Mock beta feature entries (Phase 8: injected via Atom).
#[allow(dead_code)]
struct BetaEntry {
    label: &'static str,
    description: &'static str,
    enabled: bool,
}

#[allow(dead_code)]
const BETA_ENTRIES: &[BetaEntry] = &[
    BetaEntry {
        label: "subagent_v2",
        description: "New sub-agent dispatch engine",
        enabled: true,
    },
    BetaEntry {
        label: "experimental_compact",
        description: "Experimental context compaction",
        enabled: false,
    },
    BetaEntry {
        label: "mcp_logging",
        description: "MCP tool call logging",
        enabled: false,
    },
    BetaEntry {
        label: "ui_v2",
        description: "New UI rendering engine",
        enabled: true,
    },
];

#[component]
fn BetasPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let selected = hooks.use_state(|| 0usize);

    hooks.use_local_events({
        let selected = selected;
        let count = BETA_ENTRIES.len();
        move |event: Event| {
            if let Event::Key(key) = event {
                if key.kind != KeyEventKind::Press {
                    return;
                }
                match key.code {
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
                    KeyCode::Char(' ') => {
                        // TODO Phase 8: toggle feature via Atom write
                    }
                    // Esc: Phase 8 通过 use_input_layer 实现模态面板关闭
                    _ => {}
                }
            }
        }
    });

    let sel = *selected.read();
    let mut lines: Vec<Line<'_>> = Vec::new();

    // Hint line
    lines.push(Line::from("  Changes take effect in new sessions").fg(theme::MUTED));
    lines.push(Line::from(""));

    for (i, entry) in BETA_ENTRIES.iter().enumerate() {
        let is_selected = i == sel;
        let cursor = if is_selected { "> " } else { "  " };
        let label_style = if is_selected {
            Style::new().fg(theme::THINKING).bold()
        } else {
            Style::new().fg(theme::TEXT)
        };
        let value_text = if entry.enabled { "on" } else { "off" };
        let value_style = if entry.enabled {
            Style::new().fg(theme::SAGE).bold()
        } else {
            Style::new().fg(theme::MUTED)
        };

        lines.push(Line::from(vec![
            Span::styled(cursor, Style::new().fg(theme::THINKING)),
            Span::styled(format!("{:<22}", entry.label), label_style),
            Span::styled(value_text, value_style),
        ]));
        lines.push(Line::from(Span::styled(
            format!("      {}", entry.description),
            Style::new().fg(theme::MUTED),
        )));
    }

    if BETA_ENTRIES.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from("  No active beta features").fg(theme::MUTED));
    }

    // Footer hints
    lines.push(Line::from(""));
    lines.push(Line::from("  Space) Toggle  Esc) Close").fg(theme::DIM));

    let content = if lines.is_empty() {
        Paragraph::new(Line::from("  (empty)").fg(theme::MUTED))
    } else {
        Paragraph::new(ratatui::text::Text::from(lines))
    };

    element!(
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().fg(theme::BORDER),
            top_title: Line::from(" Beta Features ")
                .fg(theme::THINKING)
                .bold()
                .centered(),
            width: Constraint::Length(44),
            height: Constraint::Length(14),
        ) {
            Text(text: content)
        }
    )
}
