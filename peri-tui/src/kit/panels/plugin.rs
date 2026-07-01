//! ratatui-kit PluginPanel component.
//!
//! H1c（Iteration 14）：从 PLUGIN_LIST atom 读取真实插件列表（由
//! service_snapshot 从 plugin_data.plugins 派生）。只读面板——插件启用/禁用
//! 通过修改 ~/.claude/plugins/config.json，UI 暂不实现切换。

use crate::kit::atoms::{PLUGIN_LIST, PluginSummary};
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
pub fn PluginPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let selected = hooks.use_state(|| 0usize);
    let store = hooks.use_store(*PLUGIN_LIST.get().unwrap());
    let plugins: Vec<PluginSummary> = store.read().clone();
    let _ = store;
    let count = plugins.len();

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

    lines.push(Line::from(vec![Span::styled(
        format!("  {} plugins loaded", count),
        Style::new().fg(theme::TEXT).bold(),
    )]));
    lines.push(Line::from(vec![Span::styled(
        "  (read-only — toggle via ~/.claude/plugins/config.json)",
        Style::new().fg(theme::MUTED).italic(),
    )]));
    lines.push(Line::from(""));

    if plugins.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            "  No plugins installed",
            Style::new().fg(theme::MUTED),
        )]));
        lines.push(Line::from(vec![Span::styled(
            "  Install via: agm install <name>",
            Style::new().fg(theme::MUTED),
        )]));
    } else {
        for (i, p) in plugins.iter().enumerate() {
            let is_selected = i == sel;
            let cursor = if is_selected { ">" } else { " " };
            let name_style = if is_selected {
                Style::new().fg(theme::THINKING).bold()
            } else {
                Style::new().fg(theme::TEXT)
            };

            lines.push(Line::from(vec![
                Span::styled(format!(" {} ", cursor), Style::new().fg(theme::THINKING)),
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
                    Style::new().fg(theme::MUTED),
                ),
            ]));
            if !p.description.is_empty() {
                lines.push(Line::from(vec![Span::styled(
                    format!("     {}", p.description),
                    Style::new().fg(theme::DIM),
                )]));
            }
            // 截断 root 路径显示
            let root: String = p.root.chars().take(76).collect();
            lines.push(Line::from(vec![Span::styled(
                format!("     {}", root),
                Style::new().fg(theme::DIM),
            )]));
            lines.push(Line::from(""));
        }
    }

    lines.push(Line::from("  j/k) Navigate  Esc) Close").fg(theme::DIM));

    let content = Paragraph::new(ratatui::text::Text::from(lines));

    element!(
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().fg(theme::BORDER),
            top_title: Line::from(" Plugins ")
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

fn close_panel() {
    use crate::kit::atoms::{ACTIVE_PANEL, OPEN_PANELS};
    if let Some(atom) = ACTIVE_PANEL.get() {
        *atom.write() = None;
    }
    if let Some(atom) = OPEN_PANELS.get() {
        atom.write().clear();
    }
}
