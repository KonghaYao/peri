//! ratatui-kit McpPanel component.
//!
//! H1d（Iteration 14）：从 MCP_SERVERS atom 读取真实 MCP server 列表（由
//! service_snapshot 从 mcp_pool.all_server_infos 派生）。结合 SERVICE_SNAPSHOT.mcp
//! 显示初始化阶段摘要。只读面板——MCP 配置通过 ~/.claude/settings.json 管理。

use crate::kit::atoms::{MCP_SERVERS, McpServerSummary, SERVICE_SNAPSHOT};
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

#[component]
pub fn McpPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let selected = hooks.use_state(|| 0usize);
    let store = hooks.use_store(*MCP_SERVERS.get().unwrap());
    let servers: Vec<McpServerSummary> = store.read().clone();
    let _ = store;

    let snap_store = hooks.use_store(*SERVICE_SNAPSHOT.get().unwrap());
    let init_phase = snap_store.read().mcp.init_phase;
    let connected_total = snap_store.read().mcp.connected;
    let config_total = snap_store.read().mcp.total;
    let _ = snap_store;

    let count = servers.len();

    hooks.use_local_events({
        let selected = selected.clone();
        let count = count;
        move |event: Event| {
            if let Event::Key(key) = event {
                if key.kind != KeyEventKind::Press {
                    return;
                }
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => close_panel(),
                    KeyCode::Up | KeyCode::Char('k') => {
                        *selected.write() = selected.read().saturating_sub(1);
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

    // 摘要头：init phase / connected / total
    let phase_label = match init_phase {
        crate::kit::atoms::McpInitPhase::Pending => "pending",
        crate::kit::atoms::McpInitPhase::Initializing => "initializing",
        crate::kit::atoms::McpInitPhase::Ready => "ready",
        crate::kit::atoms::McpInitPhase::Failed => "failed",
    };
    lines.push(Line::from(vec![
        Span::styled("  MCP Pool: ", Style::new().fg(theme::MUTED)),
        Span::styled(phase_label, Style::new().fg(theme::ACCENT).bold()),
        Span::styled(
            format!("   {}/{} connected", connected_total, config_total),
            Style::new().fg(theme::TEXT),
        ),
    ]));
    lines.push(Line::from(""));

    if servers.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            "  No MCP servers configured",
            Style::new().fg(theme::MUTED),
        )]));
        lines.push(Line::from(vec![Span::styled(
            "  Add servers via ~/.claude/settings.json (mcpServers)",
            Style::new().fg(theme::MUTED),
        )]));
    } else {
        for (i, s) in servers.iter().enumerate() {
            let is_selected = i == sel;
            let cursor = if is_selected { ">" } else { " " };
            let name_style = if is_selected {
                Style::new().fg(theme::THINKING).bold()
            } else {
                Style::new().fg(theme::TEXT)
            };
            let (status_icon, status_color) = derive_status_style(&s.status);

            lines.push(Line::from(vec![
                Span::styled(format!(" {} ", cursor), Style::new().fg(theme::THINKING)),
                Span::styled(s.name.clone(), name_style),
                Span::styled(format!("  {}", status_icon), Style::new().fg(status_color)),
                Span::styled(format!(" {}", s.status), Style::new().fg(status_color)),
            ]));
            lines.push(Line::from(vec![Span::styled(
                format!("     transport: {}  tools: {}", s.transport, s.tools_count),
                Style::new().fg(theme::DIM),
            )]));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from("  j/k) Navigate  Esc) Close").fg(theme::DIM));

    let content = Paragraph::new(ratatui::text::Text::from(lines));

    element!(
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().fg(theme::BORDER),
            top_title: Line::from(" MCP Servers ")
                .fg(theme::THINKING)
                .bold()
                .centered(),
            width: Constraint::Length(80),
            height: Constraint::Length(20),
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

fn derive_status_style(status: &str) -> (&'static str, ratatui::style::Color) {
    if status.contains("connected") {
        ("\u{2714}", theme::SAGE)
    } else if status.contains("error") || status.contains("failed") {
        ("\u{2717}", theme::ERROR)
    } else {
        ("\u{25ef}", theme::MUTED)
    }
}

fn close_panel() {
    use crate::kit::atoms::{ACTIVE_PANEL, OPEN_PANELS};
    if let Some(atom) = ACTIVE_PANEL.get() {
        *atom.write() = None;
    }
    if let Some(atom) = OPEN_PANELS.get() {
        atom.write().clear();
    }
}
