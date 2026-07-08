//! ratatui-kit TasksPanel component.
//!
//! H1g（Iteration 14）：聚合多源任务视图——
//! - Cron 任务（从 CRON_JOBS atom）
//! - SubAgent 运行时（从 VIEW_MODELS 扫描 TuiSubAgentGroup）
//!
//! 只读面板。Cron 任务的 enable/disable/delete 在 Cron 面板；SubAgent 详情
//! 在 Agent 面板。本面板提供跨调度源的"任务总览"。

use crate::app::panel_types::PanelKind;
use crate::kit::atoms::{BG_TASKS, CRON_JOBS, VIEW_MODELS};
use crate::kit::list_nav::{next_selection, previous_selection};
use crate::kit::theme;
use crate::kit::tui_render_unit::{TuiRenderUnit, TuiSubAgentGroup};
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
pub fn TasksPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let selected = hooks.use_state(|| 0usize);

    // Background Tasks（从 BG_TASKS atom）
    let bg_store = hooks.use_atom(&BG_TASKS);
    let bg_tasks = bg_store.read().clone();
    let _ = bg_store;

    // Cron
    let cron_store = hooks.use_atom(&CRON_JOBS);
    let cron_jobs = cron_store.read().clone();
    let _ = cron_store;

    // SubAgent（从 VIEW_MODELS 扫描）
    let vm_store = hooks.use_atom(&VIEW_MODELS);
    let subagents = collect_subagents(&vm_store.read());
    let _ = vm_store;

    let bg_count = bg_tasks.len();
    let cron_count = cron_jobs.len();
    let subagent_count = subagents.len();
    let total = bg_count + cron_count + subagent_count;

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
                KeyCode::Enter => {
                    // 如果选中了 bg task，尝试取消
                    let sel = *selected.read();
                    let bg_count = BG_TASKS.state().read().len();
                    if sel < bg_count {
                        if let Some(task) = BG_TASKS.state().read().get(sel) {
                            tracing::info!(
                                task_id = %task.task_id,
                                kind = %task.kind,
                                "tasks panel: cancel bg task (RPC not yet wired)"
                            );
                        }
                    }
                    close_panel()
                }
                KeyCode::Up => {
                    let mut s = selected.write();
                    *s = previous_selection(*s);
                }
                KeyCode::Down => {
                    let mut s = selected.write();
                    let total = BG_TASKS.state().read().len()
                        + CRON_JOBS.state().read().len()
                        + collect_subagents(&VIEW_MODELS.state().read()).len();
                    if total > 0 {
                        *s = next_selection(*s, total);
                    }
                }
                _ => {}
            }
            EventResult::Consumed
        }
    });

    let sel = *selected.read();
    let mut lines: Vec<Line<'_>> = Vec::new();

    // 摘要
    lines.push(Line::from(vec![
        Span::styled("  Total: ", Style::new().fg(theme::semantic().text.muted)),
        Span::styled(
            format!("{}", total),
            Style::new().fg(theme::semantic().text.primary).bold(),
        ),
        Span::styled(
            format!(
                "   ({} bg, {} cron, {} subagent)",
                bg_count, cron_count, subagent_count
            ),
            Style::new().fg(theme::semantic().text.dim),
        ),
    ]));
    lines.push(Line::from(""));

    // Background Tasks section
    if !bg_tasks.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            format!("  ▼ Background Tasks ({})", bg_count),
            Style::new().fg(theme::semantic().border.active).bold(),
        )]));
        for (i, task) in bg_tasks.iter().enumerate() {
            let row_idx = i;
            let is_selected = row_idx == sel;
            let cursor = if is_selected { ">" } else { " " };
            let name_style = if is_selected {
                Style::new().fg(theme::component().panel.title).bold()
            } else {
                Style::new().fg(theme::semantic().text.primary)
            };
            let kind_str = match task.kind.as_str() {
                "shell" => "[sh]",
                "agent" => "[ag]",
                "workflow" => "[wf]",
                _ => "[?]",
            };
            let pid_str = task.pid.map(|p| format!(" pid:{}", p)).unwrap_or_default();
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} ", cursor),
                    Style::new().fg(theme::component().panel.title),
                ),
                Span::styled(kind_str, Style::new().fg(theme::semantic().text.dim)),
                Span::styled(format!(" {} ", task.task_id), name_style),
                Span::styled(
                    format!(
                        "{}{}",
                        task.summary.chars().take(60).collect::<String>(),
                        pid_str
                    ),
                    Style::new().fg(theme::semantic().text.muted),
                ),
            ]));
        }
        lines.push(Line::from(""));
    }

    // Cron section
    if !cron_jobs.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            format!("  ▼ Cron Jobs ({})", cron_count),
            Style::new().fg(theme::semantic().border.active).bold(),
        )]));
        for (i, job) in cron_jobs.iter().enumerate() {
            let row_idx = bg_count + i;
            let is_selected = row_idx == sel;
            let cursor = if is_selected { ">" } else { " " };
            let name_style = if is_selected {
                Style::new().fg(theme::component().panel.title).bold()
            } else {
                Style::new().fg(theme::semantic().text.primary)
            };
            let (status_icon, status_color) = if job.enabled {
                ("\u{25cf}", theme::semantic().status.success)
            } else {
                ("\u{25cb}", theme::semantic().text.muted)
            };
            let next_str = job
                .next_fire
                .map(|t| t.format("%H:%M:%S").to_string())
                .unwrap_or_else(|| "—".to_string());

            let prompt_preview: String = job.prompt.chars().take(50).collect();
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} ", cursor),
                    Style::new().fg(theme::component().panel.title),
                ),
                Span::styled(status_icon, Style::new().fg(status_color)),
                Span::styled(
                    format!(" {} ", job.expression),
                    Style::new().fg(theme::semantic().text.dim),
                ),
                Span::styled(prompt_preview, name_style),
                Span::styled(
                    format!("  @{}", next_str),
                    Style::new().fg(theme::semantic().text.muted),
                ),
            ]));
        }
        lines.push(Line::from(""));
    }

    // SubAgent section
    if !subagents.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            format!("  ▼ SubAgents ({})", subagent_count),
            Style::new().fg(theme::semantic().border.active).bold(),
        )]));
        for (i, sa) in subagents.iter().enumerate() {
            let row_idx = bg_count + cron_count + i;
            let is_selected = row_idx == sel;
            let cursor = if is_selected { ">" } else { " " };
            let name_style = if is_selected {
                Style::new().fg(theme::component().panel.title).bold()
            } else {
                Style::new().fg(theme::semantic().text.primary)
            };
            let collapsed_marker = if sa.collapsed {
                Span::styled(
                    " (collapsed)",
                    Style::new().fg(theme::semantic().text.muted),
                )
            } else {
                Span::styled(" (live)", Style::new().fg(theme::semantic().status.success))
            };
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} ", cursor),
                    Style::new().fg(theme::component().panel.title),
                ),
                Span::styled(sa.agent_name.clone(), name_style),
                Span::styled(
                    format!("  [{}]", sa.agent_id),
                    Style::new().fg(theme::semantic().text.dim),
                ),
                collapsed_marker,
                Span::styled(
                    format!("  {} msgs", sa.view_models.len()),
                    Style::new().fg(theme::semantic().text.muted),
                ),
            ]));
        }
    }

    if total == 0 {
        lines.push(Line::from(vec![Span::styled(
            "  No active tasks",
            Style::new().fg(theme::semantic().text.muted).italic(),
        )]));
        lines.push(Line::from(vec![Span::styled(
            "  Cron jobs are scheduled via /loop command;",
            Style::new().fg(theme::semantic().text.dim),
        )]));
        lines.push(Line::from(vec![Span::styled(
            "  SubAgents are spawned by Task / SubAgent tools.",
            Style::new().fg(theme::semantic().text.dim),
        )]));
    }

    lines.push(Line::from(""));
    lines.push(
        Line::from("  ↑/↓::navigate  Enter::open  Esc::close").fg(theme::semantic().text.dim),
    );

    let content = Paragraph::new(ratatui::text::Text::from(lines));

    panel_shell!(PanelKind::Tasks, {
            ScrollView(
                scrollbars: crate::kit::panel_registry::clean_scrollbars(),
                width: Constraint::Fill(1),
                height: Constraint::Fill(1),
            ) {
                Text(text: content)
            }
    })
}

/// 从 ViewModelsSnapshot 派生去重 SubAgent 列表。
fn collect_subagents(snap: &crate::kit::atoms::ViewModelsSnapshot) -> Vec<TuiSubAgentGroup> {
    let mut out: Vec<TuiSubAgentGroup> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for vm in snap.items.iter() {
        scan_vm_for_subagents(vm, &mut out, &mut seen);
    }
    out
}

fn scan_vm_for_subagents(
    vm: &TuiRenderUnit,
    out: &mut Vec<TuiSubAgentGroup>,
    seen: &mut std::collections::HashSet<String>,
) {
    if let TuiRenderUnit::TuiSubAgentGroup(d) = vm {
        if seen.insert(d.agent_id.clone()) {
            out.push(d.clone());
        }
        for child in d.view_models.iter() {
            scan_vm_for_subagents(child, out, seen);
        }
    } else if let TuiRenderUnit::TuiCollapsedGroup(g) = vm {
        for child in g.view_models.iter() {
            scan_vm_for_subagents(child, out, seen);
        }
    }
}

fn close_panel() {
    // I19-A: 弹栈而非清空整个栈，避免同时打开多个不同组面板时关闭一个会全部关闭
    crate::kit::panel_registry::close_active_panel();
}
