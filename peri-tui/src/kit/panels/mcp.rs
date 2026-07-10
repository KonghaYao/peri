//! ratatui-kit McpPanel component.
//!
//! H1d（Iteration 14）：从 MCP_SERVERS atom 读取真实 MCP server 列表（由
//! service_snapshot 从 mcp_pool.all_server_infos 派生）。结合 SERVICE_SNAPSHOT.mcp
//! 显示初始化阶段摘要。只读面板——MCP 配置通过 ~/.claude/settings.json 管理。

use crate::app::panel_types::PanelKind;
use crate::kit::atoms::{MCP_SERVERS, McpServerSummary, SERVICE_SNAPSHOT};
use crate::kit::list_nav::{next_selection, previous_selection, scroll_start_for_selected};
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind},
    prelude::*,
    ratatui::{
        layout::Constraint,
        style::{Style, Stylize},
        text::{Line, Span},
        widgets::Paragraph,
    },
};

#[component]
pub fn McpPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let selected = hooks.use_state(|| 0usize);
    let store = hooks.use_atom(&MCP_SERVERS);
    let servers: Vec<McpServerSummary> = store.read().clone();
    let _ = store;

    let snap_store = hooks.use_atom(&SERVICE_SNAPSHOT);
    let init_phase = snap_store.read().mcp.init_phase;
    let connected_total = snap_store.read().mcp.connected;
    let config_total = snap_store.read().mcp.total;
    let _ = snap_store;

    hooks.use_event_handler(EventScope::Current, EventPriority::Normal, {
        move |event| {
            let Event::Key(key) = event else {
                return EventResult::Ignored;
            };
            if key.kind != KeyEventKind::Press {
                return EventResult::Ignored;
            }
            match key.code {
                KeyCode::Esc => close_panel(),
                KeyCode::Enter => close_panel(),
                KeyCode::Up => {
                    let mut s = selected.write();
                    *s = previous_selection(*s);
                }
                KeyCode::Down => {
                    let mut s = selected.write();
                    let count = MCP_SERVERS.state().read().len();
                    if count > 0 {
                        *s = next_selection(*s, count);
                    }
                }
                _ => {}
            }
            EventResult::Consumed
        }
    });

    let sel = *selected.read();
    let mut lines: Vec<Line<'_>> = Vec::new();

    // 视口跟随：让选中项始终可见（issue 2026-07-06-panels-selection-no-scroll-follow）。
    // panel 高度 18 - border 2 - header 2 - footer 2 = 12 行；每项 2 行 → 可见 6 个。
    const VISIBLE_ITEMS: usize = 6;
    let scroll_start = scroll_start_for_selected(sel, servers.len(), VISIBLE_ITEMS);

    // 摘要头：init phase / connected / total
    let phase_label = match init_phase {
        crate::kit::atoms::McpInitPhase::Pending => "pending",
        crate::kit::atoms::McpInitPhase::Initializing => "initializing",
        crate::kit::atoms::McpInitPhase::Ready => "ready",
        crate::kit::atoms::McpInitPhase::Failed => "failed",
    };
    lines.push(Line::from(vec![
        Span::styled(
            "  MCP Pool: ",
            Style::new().fg(theme_def.read().semantic.text.muted),
        ),
        Span::styled(
            phase_label,
            Style::new()
                .fg(theme_def.read().semantic.border.active)
                .bold(),
        ),
        Span::styled(
            format!("   {}/{} connected", connected_total, config_total),
            Style::new().fg(theme_def.read().semantic.text.primary),
        ),
    ]));
    lines.push(Line::from(""));

    if servers.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            "  No MCP servers configured",
            Style::new().fg(theme_def.read().semantic.text.muted),
        )]));
        lines.push(Line::from(vec![Span::styled(
            "  Add servers via ~/.claude/settings.json (mcpServers)",
            Style::new().fg(theme_def.read().semantic.text.muted),
        )]));
    } else {
        for (i, s) in servers
            .iter()
            .enumerate()
            .skip(scroll_start)
            .take(VISIBLE_ITEMS)
        {
            let is_selected = i == sel;
            let cursor = if is_selected { ">" } else { " " };
            let name_style = if is_selected {
                Style::new()
                    .fg(theme_def.read().component.panel.title)
                    .bold()
            } else {
                Style::new().fg(theme_def.read().semantic.text.primary)
            };
            let (status_icon, status_color) = derive_status_style(&s.status);

            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} ", cursor),
                    Style::new().fg(theme_def.read().component.panel.title),
                ),
                Span::styled(s.name.clone(), name_style),
                Span::styled(format!("  {}", status_icon), Style::new().fg(status_color)),
                Span::styled(format!(" {}", s.status), Style::new().fg(status_color)),
            ]));
            lines.push(Line::from(vec![Span::styled(
                format!("     transport: {}  tools: {}", s.transport, s.tools_count),
                Style::new().fg(theme_def.read().semantic.text.dim),
            )]));
        }
    }

    lines.push(Line::from(""));
    lines.push(
        Line::from("  ↑/↓::navigate  Enter::open  Esc::close").fg(theme_def
            .read()
            .semantic
            .text
            .dim),
    );

    let content = Paragraph::new(ratatui::text::Text::from(lines));

    panel_shell!(PanelKind::Mcp, {
            ScrollView(
                scrollbars: crate::kit::panel_registry::clean_scrollbars(),
                width: Constraint::Fill(1),
                height: Constraint::Fill(1),
            ) {
                Text(text: content)
            }
    })
}

fn derive_status_style(status: &str) -> (&'static str, ratatui::style::Color) {
    if status.contains("connected") {
        (
            "\u{2714}",
            THEME_ATOM.state().read().semantic.status.success,
        )
    } else if status.contains("error") || status.contains("failed") {
        ("\u{2717}", THEME_ATOM.state().read().semantic.status.error)
    } else {
        ("\u{25ef}", THEME_ATOM.state().read().semantic.text.muted)
    }
}

fn close_panel() {
    // I19-A: 弹栈而非清空整个栈，避免同时打开多个不同组面板时关闭一个会全部关闭
    crate::kit::panel_registry::close_active_panel();
}
