//! ratatui-kit PluginPanel component.
//!
//! Phase 6c batch 2: plugin list with cursor navigation
//! (use_state + use_local_events). Mock data with 4 plugins
//! (claude-md-management, frontend-design, skill-creator, supergoal);
//! Phase 8 通过 Atom/props 注入真实 plugin 状态。
//!
//! 旧版: panel/panels/plugin.rs (PanelState trait).

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

// ---------------------------------------------------------------------------
// Mock plugin data
// ---------------------------------------------------------------------------

/// Mock plugin entry (Phase 8: from real plugin registry).
#[allow(dead_code)]
struct PluginEntry {
    name: &'static str,
    version: &'static str,
    enabled: bool,
    description: &'static str,
}

#[allow(dead_code)]
const PLUGINS: &[PluginEntry] = &[
    PluginEntry {
        name: "claude-md-management",
        version: "1.0.0",
        enabled: true,
        description: "Audit and improve CLAUDE.md files in repositories",
    },
    PluginEntry {
        name: "frontend-design",
        version: "unknown",
        enabled: true,
        description: "Create distinctive, production-grade frontend interfaces",
    },
    PluginEntry {
        name: "skill-creator",
        version: "unknown",
        enabled: true,
        description: "Create new skills, modify and improve existing skills",
    },
    PluginEntry {
        name: "supergoal",
        version: "0.6.1",
        enabled: false,
        description: "Plan and autonomously build a software task end-to-end",
    },
];

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

#[component]
fn PluginPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let cursor = hooks.use_state(|| 0usize);

    hooks.use_local_events({
        let cursor = cursor.clone();
        let count = PLUGINS.len();
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
                        // TODO Phase 8: toggle/enter detail for selected plugin
                    }
                    _ => {}
                }
            }
        }
    });

    let sel = *cursor.read();
    let mut lines: Vec<Line<'_>> = Vec::new();

    // Header
    lines.push(Line::from(""));
    // Plugin rows
    for (i, entry) in PLUGINS.iter().enumerate() {
        let is_cursor = i == sel;
        let cursor_mark = if is_cursor { "\u{276f}" } else { " " };

        let name_style = if is_cursor {
            Style::new().fg(theme::THINKING).bold()
        } else {
            Style::new().fg(theme::TEXT)
        };

        // Status indicator
        let (status_icon, status_style) = if entry.enabled {
            ("\u{2714}", Style::new().fg(theme::SAGE))
        } else {
            ("\u{25cb}", Style::new().fg(theme::MUTED))
        };

        lines.push(Line::from(vec![
            Span::styled(
                format!(" {} ", cursor_mark),
                Style::new().fg(theme::THINKING),
            ),
            Span::styled(format!("{:<28}", entry.name), name_style),
            Span::styled(
                format!("v{}  ", entry.version),
                Style::new().fg(theme::MUTED),
            ),
            Span::styled(status_icon, status_style),
            Span::styled(
                if entry.enabled {
                    " enabled"
                } else {
                    " disabled"
                },
                status_style,
            ),
        ]));

        // Description line (indented)
        lines.push(Line::from(vec![
            Span::styled("     ", Style::new()),
            Span::styled(entry.description, Style::new().fg(theme::MUTED)),
        ]));
        lines.push(Line::from(""));
    }

    // Footer hint
    lines.push(Line::from(vec![Span::styled(
        "  j/k) Navigate  Enter) Detail  q) Close",
        Style::new().fg(theme::DIM),
    )]));

    let content = Paragraph::new(ratatui::text::Text::from(lines));

    element!(
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().fg(theme::BORDER),
            top_title: Line::from(" Plugins ")
                .fg(theme::THINKING)
                .bold()
                .centered(),
            width: Constraint::Length(54),
            height: Constraint::Length(16),
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
