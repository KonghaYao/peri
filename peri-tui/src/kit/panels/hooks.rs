//! ratatui-kit HooksPanel component.
//!
//! H1b（Iteration 14）：从 HOOK_LIST atom 读取真实 hooks 列表（由
//! service_snapshot 从 plugin_data.all_hooks 派生）。只读面板——hooks 在
//! 插件 hooks/<event>.json 中声明，UI 不修改。

use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers},
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction},
        style::{Style, Stylize},
        text::{Line, Span},
        widgets::Paragraph,
    },
};

use crate::kit::atoms::{HOOK_LIST, HookSummary};
use crate::kit::theme;

/// Map event name to human-readable description。
fn event_description(event: &str) -> &'static str {
    match event {
        "pretooluse" => "Before tool execution",
        "posttooluse" => "After tool execution",
        "posttoolusefailure" => "After tool execution fails",
        "permissionrequest" => "Before auto mode classifier decides",
        "userpromptsubmit" => "When user submits a prompt",
        "sessionstart" => "When a new session starts",
        "sessionend" => "When a session ends",
        "stop" => "When agent stops",
        "stopfailure" => "When agent stops with failure",
        "posttoolbatch" => "When all parallel tools complete",
        "subagentstart" => "When a subagent starts",
        "subagentstop" => "When a subagent stops",
        "precompact" => "Before context compaction",
        "postcompact" => "After context compaction",
        "notification" => "When agent needs user input",
        _ => "",
    }
}

#[component]
pub fn HooksPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let selected = hooks.use_state(|| 0usize);
    let store = hooks.use_store(*HOOK_LIST.get().unwrap());
    let hook_list: Vec<HookSummary> = store.read().clone();
    let _ = store;
    let count = hook_list.len();

    hooks.use_local_events({
        move |event: Event| {
            if let Event::Key(key) = event {
                if key.kind != KeyEventKind::Press {
                    return;
                }
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => {
                        close_panel();
                    }
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
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {}
                    _ => {}
                }
            }
        }
    });

    let sel = *selected.read();
    let mut lines: Vec<Line<'_>> = Vec::new();

    // Stats line
    lines.push(Line::from(vec![Span::styled(
        format!("  {} hooks registered", count),
        Style::new().fg(theme::TEXT).bold(),
    )]));
    lines.push(Line::from(vec![Span::styled(
        "  (read-only — configured via plugins)",
        Style::new().fg(theme::MUTED).italic(),
    )]));
    lines.push(Line::from(""));

    if hook_list.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            "  No hooks configured",
            Style::new().fg(theme::MUTED),
        )]));
        lines.push(Line::from(vec![Span::styled(
            "  Add hooks/<event>.json files to a plugin",
            Style::new().fg(theme::MUTED),
        )]));
    } else {
        for (i, entry) in hook_list.iter().enumerate() {
            let is_selected = i == sel;
            let cursor = if is_selected { ">" } else { " " };
            let name_style = if is_selected {
                Style::new().fg(theme::THINKING).bold()
            } else {
                Style::new().fg(theme::TEXT)
            };
            let desc = event_description(&entry.event);

            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} {}. {} ", cursor, i + 1, entry.event),
                    name_style,
                ),
                Span::styled(
                    format!("  via {}", entry.plugin_name),
                    Style::new().fg(theme::ACCENT),
                ),
                Span::styled(format!("  {}", desc), Style::new().fg(theme::MUTED)),
            ]));

            if let Some(m) = &entry.matcher {
                lines.push(Line::from(vec![Span::styled(
                    format!("     matcher: {}", m),
                    Style::new().fg(theme::DIM),
                )]));
            }

            let cmd_summary: String = entry
                .command
                .chars()
                .take(70)
                .chain(if entry.command.chars().count() > 70 {
                    Some('…')
                } else {
                    None
                })
                .collect();
            lines.push(Line::from(vec![Span::styled(
                format!("     {}", cmd_summary),
                Style::new().fg(theme::TEXT),
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
            top_title: Line::from(" Hooks ")
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
    // I19-A: 弹栈而非清空整个栈，避免同时打开多个不同组面板时关闭一个会全部关闭
    crate::kit::panel_registry::close_active_panel();
}
