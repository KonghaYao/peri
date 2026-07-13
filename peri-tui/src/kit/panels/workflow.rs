//! ratatui-kit WorkflowPanel component.
//!
//! Workflow 是 `@peri-workflow` npm CLI 工具驱动的多 agent 编排层——TUI 没有
//! 内嵌运行时数据源（workflow 状态由外部工具维护）。本面板作为只读信息面板，
//! 说明如何使用外部工具，并提供当前会话内可观察的 workflow hint（来自
//! VIEW_MODELS 中的 TuiSubAgentGroup 计数）。

use crate::app::panel_types::PanelKind;
use crate::i18n;
use crate::kit::atoms::{LANG_VERSION, VIEW_MODELS};
use crate::kit::list_nav::{next_selection, previous_selection};
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
pub fn WorkflowPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let cursor = hooks.use_state(|| 0usize);

    // 从 VIEW_MODELS 派生 subagent group 数量（间接显示 workflow 活跃度）
    let vm_store = hooks.use_atom(&VIEW_MODELS);
    let _ = hooks.use_atom(&LANG_VERSION);
    let subagent_count = vm_store
        .read()
        .items
        .iter()
        .filter(|vm| {
            matches!(
                vm,
                crate::kit::tui_render_unit::TuiRenderUnit::TuiSubAgentGroup(_)
            )
        })
        .count();
    let _ = vm_store;

    let rows: Vec<(String, String)> = vec![
        (
            i18n::tr("panel-workflow-label-engine"),
            i18n::tr("panel-workflow-value-engine"),
        ),
        (
            i18n::tr("panel-workflow-label-binary"),
            i18n::tr("panel-workflow-value-binary"),
        ),
        (
            i18n::tr("panel-workflow-label-subagents"),
            format!("{}", subagent_count),
        ),
        (
            i18n::tr("panel-workflow-label-self-check"),
            i18n::tr("panel-workflow-value-self-check"),
        ),
    ];
    let count = rows.len();

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
                    let mut c = cursor.write();
                    *c = previous_selection(*c);
                }
                KeyCode::Down => {
                    let mut c = cursor.write();
                    if count > 0 {
                        *c = next_selection(*c, count);
                    }
                }
                _ => {}
            }
            EventResult::Consumed
        }
    });

    let sel = *cursor.read();
    let mut lines: Vec<Line<'_>> = Vec::new();

    lines.push(Line::from(vec![Span::styled(
        i18n::tr("panel-workflow-title"),
        Style::new()
            .fg(theme_def.read().semantic.text.primary)
            .bold(),
    )]));
    lines.push(Line::from(vec![Span::styled(
        i18n::tr("panel-workflow-subtitle"),
        Style::new()
            .fg(theme_def.read().semantic.text.muted)
            .italic(),
    )]));
    lines.push(Line::from(""));

    for (i, (label, value)) in rows.iter().enumerate() {
        let is_selected = i == sel;
        let cursor_mark = if is_selected { ">" } else { " " };
        let label_style = if is_selected {
            Style::new()
                .fg(theme_def.read().component.panel.title)
                .bold()
        } else {
            Style::new().fg(theme_def.read().semantic.text.muted)
        };
        let value_style = if is_selected {
            Style::new()
                .fg(theme_def.read().semantic.text.primary)
                .bold()
        } else {
            Style::new().fg(theme_def.read().semantic.text.primary)
        };

        lines.push(Line::from(vec![
            Span::styled(
                format!(" {} ", cursor_mark),
                Style::new().fg(theme_def.read().component.panel.title),
            ),
            Span::styled(format!("{:<26}", format!("{}:", label)), label_style),
            Span::styled(value.chars().take(60).collect::<String>(), value_style),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(vec![Span::styled(
        i18n::tr("panel-workflow-info-1"),
        Style::new().fg(theme_def.read().semantic.text.dim),
    )]));
    lines.push(Line::from(vec![Span::styled(
        i18n::tr("panel-workflow-info-2"),
        Style::new().fg(theme_def.read().semantic.text.dim),
    )]));
    lines.push(Line::from(""));
    lines.push(
        Line::from(i18n::tr("common-nav-enter-close")).fg(theme_def.read().semantic.text.dim),
    );

    let content = Paragraph::new(ratatui::text::Text::from(lines));

    panel_shell!(PanelKind::Workflow, {
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
