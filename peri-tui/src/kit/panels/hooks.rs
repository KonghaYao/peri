//! ratatui-kit HooksPanel component.
//!
//! H1b（Iteration 14）：从 HOOK_LIST atom 读取真实 hooks 列表（由
//! service_snapshot 从 plugin_data.all_hooks 派生）。只读面板——hooks 在
//! 插件 hooks/<event>.json 中声明，UI 不修改。

use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers},
    prelude::*,
    ratatui::{
        layout::Constraint,
        style::{Style, Stylize},
        text::{Line, Span},
        widgets::Paragraph,
    },
};

use crate::app::panel_types::PanelKind;
use crate::kit::atoms::{HOOK_LIST, HookSummary};
use crate::kit::list_nav::{next_selection, previous_selection, scroll_start_for_selected};
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
    let store = hooks.use_atom(&HOOK_LIST);
    let hook_list: Vec<HookSummary> = store.read().clone();
    let _ = store;
    let count = hook_list.len();

    hooks.use_event_handler(EventScope::Current, EventPriority::Normal, {
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
                }
                KeyCode::Enter => {
                    close_panel();
                }
                KeyCode::Up => {
                    let mut s = selected.write();
                    *s = previous_selection(*s);
                }
                KeyCode::Down => {
                    let mut s = selected.write();
                    let count = HOOK_LIST.state().read().len();
                    if count > 0 {
                        *s = next_selection(*s, count);
                    }
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return EventResult::Ignored;
                }
                _ => {}
            }
            EventResult::Consumed
        }
    });

    let sel = *selected.read();
    let mut lines: Vec<Line<'_>> = Vec::new();

    // 视口跟随：让选中项始终可见（issue 2026-07-06-panels-selection-no-scroll-follow）。
    // panel 高度 18 - border 2 - header 3 - footer 2 = 11 行；每项 3 行 → 可见 3 个。
    // matcher 缺失时占位空行，保证每项固定 3 行（视口计算依赖）。
    const VISIBLE_ITEMS: usize = 3;
    let scroll_start = scroll_start_for_selected(sel, hook_list.len(), VISIBLE_ITEMS);

    // Stats line
    lines.push(Line::from(vec![Span::styled(
        format!("  {} hooks registered", count),
        Style::new().fg(theme::semantic().text.primary).bold(),
    )]));
    lines.push(Line::from(vec![Span::styled(
        "  (read-only — configured via plugins)",
        Style::new().fg(theme::semantic().text.muted).italic(),
    )]));
    lines.push(Line::from(""));

    if hook_list.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            "  No hooks configured",
            Style::new().fg(theme::semantic().text.muted),
        )]));
        lines.push(Line::from(vec![Span::styled(
            "  Add hooks/<event>.json files to a plugin",
            Style::new().fg(theme::semantic().text.muted),
        )]));
    } else {
        for (i, entry) in hook_list
            .iter()
            .enumerate()
            .skip(scroll_start)
            .take(VISIBLE_ITEMS)
        {
            let is_selected = i == sel;
            let cursor = if is_selected { ">" } else { " " };
            let name_style = if is_selected {
                Style::new().fg(theme::component().panel.title).bold()
            } else {
                Style::new().fg(theme::semantic().text.primary)
            };
            let desc = event_description(&entry.event);

            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} {}. {} ", cursor, i + 1, entry.event),
                    name_style,
                ),
                Span::styled(
                    format!("  via {}", entry.plugin_name),
                    Style::new().fg(theme::semantic().border.active),
                ),
                Span::styled(
                    format!("  {}", desc),
                    Style::new().fg(theme::semantic().text.muted),
                ),
            ]));

            if let Some(m) = &entry.matcher {
                lines.push(Line::from(vec![Span::styled(
                    format!("     matcher: {}", m),
                    Style::new().fg(theme::semantic().text.dim),
                )]));
            } else {
                // matcher 缺失时占位空行，保证每项固定 3 行（视口计算依赖）
                lines.push(Line::from(""));
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
                Style::new().fg(theme::semantic().text.primary),
            )]));
        }
    }

    lines.push(Line::from(""));
    lines.push(
        Line::from("  ↑/↓::navigate  Enter::open  Esc::close").fg(theme::semantic().text.dim),
    );

    let content = Paragraph::new(ratatui::text::Text::from(lines));

    panel_shell!(PanelKind::Hooks, {
            ScrollView(
                scroll_bars: crate::kit::panel_registry::clean_scrollbars(),
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
