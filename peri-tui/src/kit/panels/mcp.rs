//! ratatui-kit McpPanel component.
//!
//! Phase 6c batch 2: MCP server list with cursor navigation
//! (use_state + use_local_events). Mock data with 4 MCP servers
//! (filesystem, github, slack, web-search); Phase 8 通过 Atom/props 注入
//! 真实 MCP server 状态。
//!
//! 旧版: panel/panels/mcp.rs (PanelState trait).

use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind},
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction},
        style::{Color, Style, Stylize},
        text::{Line, Span},
        widgets::Paragraph,
    },
};
use crate::ui::theme;

// ---------------------------------------------------------------------------
// Mock MCP data
// ---------------------------------------------------------------------------

/// MCP server status enum.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum McpStatus {
    Connected,
    Disconnected,
    Error,
}

#[allow(dead_code)]
impl McpStatus {
    fn icon(&self) -> &'static str {
        match self {
            Self::Connected => "\u{2714}",
            Self::Disconnected => "\u{25ef}",
            Self::Error => "\u{2717}",
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Self::Connected => "connected",
            Self::Disconnected => "offline",
            Self::Error => "error",
        }
    }

    fn color(&self) -> Color {
        match self {
            Self::Connected => theme::SAGE,
            Self::Disconnected => theme::MUTED,
            Self::Error => theme::ERROR,
        }
    }
}

/// Mock MCP server entry (Phase 8: from real pool).
#[allow(dead_code)]
struct McpServerEntry {
    name: &'static str,
    status: McpStatus,
    tool_count: usize,
    enabled: bool,
}

#[allow(dead_code)]
const MCP_SERVERS: &[McpServerEntry] = &[
    McpServerEntry {
        name: "filesystem",
        status: McpStatus::Connected,
        tool_count: 8,
        enabled: true,
    },
    McpServerEntry {
        name: "github",
        status: McpStatus::Connected,
        tool_count: 12,
        enabled: true,
    },
    McpServerEntry {
        name: "slack",
        status: McpStatus::Disconnected,
        tool_count: 4,
        enabled: false,
    },
    McpServerEntry {
        name: "web-search",
        status: McpStatus::Error,
        tool_count: 2,
        enabled: true,
    },
];

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

#[component]
fn McpPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let cursor = hooks.use_state(|| 0usize);

    hooks.use_local_events({
        let cursor = cursor.clone();
        let count = MCP_SERVERS.len();
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
                    // Space: toggle enabled status
                    KeyCode::Char(' ') => {
                        // TODO Phase 8: toggle server via ACP
                    }
                    // r: refresh server list
                    KeyCode::Char('r') => {
                        // TODO Phase 8: refresh from live pool
                    }
                    // n: add new server
                    KeyCode::Char('n') => {
                        // TODO Phase 8: open new server dialog
                    }
                    _ => {}
                }
            }
        }
    });

    let sel = *cursor.read();
    let mut lines: Vec<Line<'_>> = Vec::new();

    // Header: server count
    lines.push(Line::from(vec![Span::styled(
        format!("  {} servers", MCP_SERVERS.len()),
        Style::new().fg(theme::MUTED),
    )]));
    lines.push(Line::from(""));

    // Server rows
    for (i, entry) in MCP_SERVERS.iter().enumerate() {
        let is_cursor = i == sel;
        let cursor_mark = if is_cursor { "\u{276f}" } else { " " };

        let name_style = if is_cursor {
            Style::new().fg(theme::THINKING).bold()
        } else {
            Style::new().fg(theme::TEXT)
        };

        let status_style = Style::new().fg(entry.status.color());

        // Enabled/disabled indicator
        let toggle = if entry.enabled { "\u{25c9}" } else { "\u{25cb}" };
        let toggle_style = if entry.enabled {
            Style::new().fg(theme::SAGE)
        } else {
            Style::new().fg(theme::MUTED)
        };

        lines.push(Line::from(vec![
            Span::styled(
                format!(" {} ", cursor_mark),
                Style::new().fg(theme::THINKING),
            ),
            Span::styled(toggle.to_string(), toggle_style),
            Span::styled(
                format!(" {:<20}", entry.name),
                name_style,
            ),
            Span::styled(entry.status.icon(), status_style),
            Span::styled(
                format!(" {}", entry.status.label()),
                status_style,
            ),
            Span::styled(
                format!("  {} tools", entry.tool_count),
                Style::new().fg(theme::MUTED),
            ),
        ]));
    }

    // Footer hint
    lines.push(Line::from(""));
    lines.push(Line::from(vec![Span::styled(
        "  j/k) Nav  Space) Toggle  r) Refresh  n) New  q) Close",
        Style::new().fg(theme::DIM),
    )]));

    let content = Paragraph::new(ratatui::text::Text::from(lines));

    element!(
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().fg(theme::BORDER),
            top_title: Line::from(" MCP ")
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
