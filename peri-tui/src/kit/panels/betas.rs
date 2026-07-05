//! ratatui-kit BetasPanel component.
//!
//! Phase 6a: toggle list with cursor navigation (use_state + use_event_handler).
//! Mock data; Phase 8 通过 Atom/props 注入真实 feature 列表。

use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind},
    prelude::*,
    ratatui::{
        style::{Style, Stylize},
        text::{Line, Span},
        widgets::Paragraph,
    },
};

use crate::app::panel_types::PanelKind;
use crate::kit::list_nav::{next_selection, previous_selection};
use crate::kit::theme;

/// Mock beta feature entries (Phase 8: injected via Atom).
struct BetaEntry {
    label: &'static str,
    description: &'static str,
    enabled: bool,
}

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
pub fn BetasPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let selected = hooks.use_state(|| 0usize);

    hooks.use_event_handler(EventScope::Current, EventPriority::Normal, {
        let count = BETA_ENTRIES.len();
        move |event| {
            let Event::Key(key) = event else {
                return EventResult::Ignored;
            };
            if key.kind != KeyEventKind::Press {
                return EventResult::Ignored;
            }
            match key.code {
                KeyCode::Esc => {
                    close_panel();
                    EventResult::Consumed
                }
                KeyCode::Enter => {
                    close_panel();
                    EventResult::Consumed
                }
                KeyCode::Up => {
                    let mut s = selected.write();
                    *s = previous_selection(*s);
                    EventResult::Consumed
                }
                KeyCode::Down => {
                    let mut s = selected.write();
                    if count > 0 {
                        *s = next_selection(*s, count);
                    }
                    EventResult::Consumed
                }
                _ => EventResult::Ignored,
            }
        }
    });

    let sel = *selected.read();
    let mut lines: Vec<Line<'_>> = Vec::new();

    // Hint line
    lines.push(
        Line::from("  (read-only — feature flags are configured at build time)")
            .fg(theme::semantic().text.muted),
    );
    lines.push(Line::from(""));

    for (i, entry) in BETA_ENTRIES.iter().enumerate() {
        let is_selected = i == sel;
        let cursor = if is_selected { "> " } else { "  " };
        let label_style = if is_selected {
            Style::new().fg(theme::component().panel.title).bold()
        } else {
            Style::new().fg(theme::semantic().text.primary)
        };
        let value_text = if entry.enabled { "on" } else { "off" };
        let value_style = if entry.enabled {
            Style::new().fg(theme::semantic().status.success).bold()
        } else {
            Style::new().fg(theme::semantic().text.muted)
        };

        lines.push(Line::from(vec![
            Span::styled(cursor, Style::new().fg(theme::component().panel.title)),
            Span::styled(format!("{:<22}", entry.label), label_style),
            Span::styled(value_text, value_style),
        ]));
        lines.push(Line::from(Span::styled(
            format!("      {}", entry.description),
            Style::new().fg(theme::semantic().text.muted),
        )));
    }

    if BETA_ENTRIES.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from("  No active beta features").fg(theme::semantic().text.muted));
    }

    // Footer hints
    lines.push(Line::from(""));
    lines.push(
        Line::from("  ↑/↓::navigate  Enter::open  Esc::close").fg(theme::semantic().text.dim),
    );

    let content = if lines.is_empty() {
        Paragraph::new(Line::from("  (empty)").fg(theme::semantic().text.muted))
    } else {
        Paragraph::new(ratatui::text::Text::from(lines))
    };

    panel_shell!(PanelKind::Betas, {
        Text(text: content)
    })
}

fn close_panel() {
    // I19-A: 弹栈而非清空整个栈，避免同时打开多个不同组面板时关闭一个会全部关闭
    crate::kit::panel_registry::close_active_panel();
}
