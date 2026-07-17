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
use crate::i18n;
use crate::kit::atoms::{HOOK_LIST, HookSummary, LANG_VERSION};
use crate::kit::list_nav::{next_selection, previous_selection, scroll_start_for_selected};
use fluent_bundle::FluentValue;
use peri_theme::atoms::THEME_ATOM;

/// Map event name to human-readable description。
fn event_description(event: &str) -> String {
    match event {
        "pretooluse" => i18n::tr("hook-event-before-tool"),
        "posttooluse" => i18n::tr("hook-event-after-tool"),
        "posttoolusefailure" => i18n::tr("hook-event-after-tool-fail"),
        "permissionrequest" => i18n::tr("hook-event-before-auto-mode"),
        "userpromptsubmit" => i18n::tr("hook-event-user-submit"),
        "sessionstart" => i18n::tr("hook-event-session-start"),
        "sessionend" => i18n::tr("hook-event-session-end"),
        "stop" => i18n::tr("hook-event-agent-stop"),
        "stopfailure" => i18n::tr("hook-event-agent-stop-fail"),
        "posttoolbatch" => i18n::tr("hook-event-parallel-tools-done"),
        "subagentstart" => i18n::tr("hook-event-subagent-start"),
        "subagentstop" => i18n::tr("hook-event-subagent-stop"),
        "precompact" => i18n::tr("hook-event-before-compact"),
        "postcompact" => i18n::tr("hook-event-after-compact"),
        "notification" => i18n::tr("hook-event-needs-input"),
        _ => String::new(),
    }
}

#[component]
pub fn HooksPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let selected = hooks.use_state(|| 0usize);
    let store = hooks.use_atom(&HOOK_LIST);
    let hook_list: Vec<HookSummary> = store.read().clone();
    let _ = store;
    let _lang_ver = hooks.use_atom(&LANG_VERSION);
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
        i18n::tr_args(
            "hooks-configured-count",
            &[("count".to_string(), FluentValue::from(count as i64))],
        ),
        Style::new()
            .fg(theme_def.read().semantic.text.primary)
            .bold(),
    )]));
    lines.push(Line::from(vec![Span::styled(
        i18n::tr("hooks-readonly-hint"),
        Style::new()
            .fg(theme_def.read().semantic.text.muted)
            .italic(),
    )]));
    lines.push(Line::from(""));

    if hook_list.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            i18n::tr("hooks-no-hooks"),
            Style::new().fg(theme_def.read().semantic.text.muted),
        )]));
        lines.push(Line::from(vec![Span::styled(
            i18n::tr("hooks-no-hooks-hint"),
            Style::new().fg(theme_def.read().semantic.text.muted),
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
                Style::new()
                    .fg(theme_def.read().component.panel.title)
                    .bold()
            } else {
                Style::new().fg(theme_def.read().semantic.text.primary)
            };
            let desc = event_description(&entry.event);

            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} {}. {} ", cursor, i + 1, entry.event),
                    name_style,
                ),
                Span::styled(
                    format!("  via {}", entry.plugin_name),
                    Style::new().fg(theme_def.read().semantic.border.active),
                ),
                Span::styled(
                    format!("  {}", desc),
                    Style::new().fg(theme_def.read().semantic.text.muted),
                ),
            ]));

            if let Some(m) = &entry.matcher {
                lines.push(Line::from(vec![Span::styled(
                    format!("     matcher: {}", m),
                    Style::new().fg(theme_def.read().semantic.text.dim),
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
                Style::new().fg(theme_def.read().semantic.text.primary),
            )]));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(i18n::tr("panel-hooks-nav-hint")).fg(theme_def.read().semantic.text.dim));

    let content = Paragraph::new(ratatui::text::Text::from(lines));

    panel_shell!(PanelKind::Hooks, {
            ScrollView(
                scrollbars: crate::kit::panel_registry::clean_scrollbars(),
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
