//! ratatui-kit WorkflowPanel component.
//!
//! Workflow 是 `@peri-workflow` npm CLI 工具驱动的多 agent 编排层——TUI 没有
//! 内嵌运行时数据源（workflow 状态由外部工具维护）。本面板作为只读信息面板，
//! 说明如何使用外部工具，并提供当前会话内可观察的 workflow hint（来自
//! VIEW_MODELS 中的 SubAgentGroup 计数）。

use crate::app::panel_types::PanelKind;
use crate::kit::atoms::VIEW_MODELS;
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
pub fn WorkflowPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let cursor = hooks.use_state(|| 0usize);

    // 从 VIEW_MODELS 派生 subagent group 数量（间接显示 workflow 活跃度）
    let vm_store = hooks.use_atom(&VIEW_MODELS);
    let subagent_count = vm_store
        .read()
        .committed
        .iter()
        .chain(vm_store.read().current_turn.iter())
        .filter(|vm| matches!(vm, peri_acp_types::view_model::ViewModel::SubAgentGroup(_)))
        .count();
    let _ = vm_store;

    let rows: Vec<(&str, String)> = vec![
        ("Engine", "@peri-workflow (external CLI)".to_string()),
        ("Binary", "peri-workflow".to_string()),
        ("Current session sub-agents", format!("{}", subagent_count)),
        (
            "Self-check",
            "Run `which peri-workflow` to verify install".to_string(),
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
        "  Workflow Engine",
        Style::new().fg(theme::semantic().text.primary).bold(),
    )]));
    lines.push(Line::from(vec![Span::styled(
        "  Multi-agent orchestration via @peri-workflow CLI",
        Style::new().fg(theme::semantic().text.muted).italic(),
    )]));
    lines.push(Line::from(""));

    for (i, (label, value)) in rows.iter().enumerate() {
        let is_selected = i == sel;
        let cursor_mark = if is_selected { ">" } else { " " };
        let label_style = if is_selected {
            Style::new().fg(theme::component().panel.title).bold()
        } else {
            Style::new().fg(theme::semantic().text.muted)
        };
        let value_style = if is_selected {
            Style::new().fg(theme::semantic().text.primary).bold()
        } else {
            Style::new().fg(theme::semantic().text.primary)
        };

        lines.push(Line::from(vec![
            Span::styled(
                format!(" {} ", cursor_mark),
                Style::new().fg(theme::component().panel.title),
            ),
            Span::styled(format!("{:<26}", format!("{}:", label)), label_style),
            Span::styled(value.chars().take(60).collect::<String>(), value_style),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(vec![Span::styled(
        "  Workflows are spawned from agent prompts;",
        Style::new().fg(theme::semantic().text.dim),
    )]));
    lines.push(Line::from(vec![Span::styled(
        "  progress surfaces here as SubAgent groups in the message stream.",
        Style::new().fg(theme::semantic().text.dim),
    )]));
    lines.push(Line::from(""));
    lines.push(
        Line::from("  ↑/↓::navigate  Enter::open  Esc::close").fg(theme::semantic().text.dim),
    );

    let content = Paragraph::new(ratatui::text::Text::from(lines));

    panel_shell!(PanelKind::Workflow, {
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
