//! ratatui-kit PluginPanel component.
//!
//! H1c（Iteration 14）：从 PLUGIN_LIST atom 读取真实插件列表（由
//! service_snapshot 从 plugin_data.plugins 派生）。只读面板——插件启用/禁用
//! 通过修改 ~/.claude/plugins/config.json，UI 暂不实现切换。

use crate::app::panel_types::PanelKind;
use crate::kit::atoms::{PLUGIN_LIST, PluginSummary};
use crate::kit::list_nav::{next_selection, previous_selection};
use crate::kit::theme;
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
pub fn PluginPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let selected = hooks.use_state(|| 0usize);
    let store = hooks.use_atom(&PLUGIN_LIST);
    let plugins: Vec<PluginSummary> = store.read().clone();
    let _ = store;
    let count = plugins.len();

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
                KeyCode::Up => {
                    *selected.write() = previous_selection(*selected.read());
                }
                KeyCode::Down => {
                    let mut s = selected.write();
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

    lines.push(Line::from(vec![Span::styled(
        format!("  {} plugins loaded", count),
        Style::new().fg(theme::semantic().text.primary).bold(),
    )]));
    lines.push(Line::from(vec![Span::styled(
        "  (read-only — toggle via ~/.claude/plugins/config.json)",
        Style::new().fg(theme::semantic().text.muted).italic(),
    )]));
    lines.push(Line::from(""));

    if plugins.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            "  No plugins installed",
            Style::new().fg(theme::semantic().text.muted),
        )]));
        lines.push(Line::from(vec![Span::styled(
            "  Install via: agm install <name>",
            Style::new().fg(theme::semantic().text.muted),
        )]));
    } else {
        for (i, p) in plugins.iter().enumerate() {
            let is_selected = i == sel;
            let cursor = if is_selected { ">" } else { " " };
            let name_style = if is_selected {
                Style::new().fg(theme::component().panel.title).bold()
            } else {
                Style::new().fg(theme::semantic().text.primary)
            };

            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} ", cursor),
                    Style::new().fg(theme::component().panel.title),
                ),
                Span::styled(p.name.clone(), name_style),
                Span::styled(
                    format!(
                        " v{}",
                        if p.version.is_empty() {
                            "?"
                        } else {
                            &p.version
                        }
                    ),
                    Style::new().fg(theme::semantic().text.muted),
                ),
            ]));
            if !p.description.is_empty() {
                lines.push(Line::from(vec![Span::styled(
                    format!("     {}", p.description),
                    Style::new().fg(theme::semantic().text.dim),
                )]));
            }
            // 截断 root 路径显示
            let root: String = p.root.chars().take(76).collect();
            lines.push(Line::from(vec![Span::styled(
                format!("     {}", root),
                Style::new().fg(theme::semantic().text.dim),
            )]));
            lines.push(Line::from(""));
        }
    }

    lines.push(
        Line::from("  ↑/↓::navigate  Enter::open  Esc::close").fg(theme::semantic().text.dim),
    );

    let content = Paragraph::new(ratatui::text::Text::from(lines));

    panel_shell!(PanelKind::Plugin, {
            ScrollView(
                scroll_bars: ScrollBars::default(),
                width: Constraint::Fill(1),
                height: Constraint::Fill(1),
            ) {
                Text(text: content)
            }
    })
}

fn close_panel() {
    // I19-A: 弹栈而非清空整个栈，避免同时打开多个不同组面板时关闭一个会全部关闭
    crate::kit::panel_registry::close_active_panel();
}
