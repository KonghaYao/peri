//! ratatui-kit TasksPanel component.
//!
//! H1g（Iteration 14）：聚合多源任务视图——
//! - Cron 任务（从 CRON_JOBS atom）
//! - SubAgent 运行时（从 VIEW_MODELS 扫描 SubAgentGroup）
//!
//! 只读面板。Cron 任务的 enable/disable/delete 在 Cron 面板；SubAgent 详情
//! 在 Agent 面板。本面板提供跨调度源的"任务总览"。

use crate::kit::atoms::{CRON_JOBS, VIEW_MODELS};
use crate::kit::theme;
use peri_acp_types::view_model::{SubAgentGroupData, ViewModel};
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
pub fn TasksPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let selected = hooks.use_state(|| 0usize);

    // Cron
    let cron_store = hooks.use_store(*CRON_JOBS.get().unwrap());
    let cron_jobs = cron_store.read().clone();
    let _ = cron_store;

    // SubAgent（从 VIEW_MODELS 扫描）
    let vm_store = hooks.use_store(*VIEW_MODELS.get().unwrap());
    let subagents = collect_subagents(&vm_store.read());
    let _ = vm_store;

    let cron_count = cron_jobs.len();
    let subagent_count = subagents.len();
    let total = cron_count + subagent_count;

    hooks.use_local_events({
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
                        if total > 0 {
                            *s = (*s + 1).min(total - 1);
                        }
                    }
                    _ => {}
                }
            }
        }
    });

    let sel = *selected.read();
    let mut lines: Vec<Line<'_>> = Vec::new();

    // 摘要
    lines.push(Line::from(vec![
        Span::styled("  Total: ", Style::new().fg(theme::MUTED)),
        Span::styled(format!("{}", total), Style::new().fg(theme::TEXT).bold()),
        Span::styled(
            format!("   ({} cron, {} subagent)", cron_count, subagent_count),
            Style::new().fg(theme::DIM),
        ),
    ]));
    lines.push(Line::from(""));

    // Cron section
    if !cron_jobs.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            format!("  ▼ Cron Jobs ({})", cron_count),
            Style::new().fg(theme::ACCENT).bold(),
        )]));
        for (i, job) in cron_jobs.iter().enumerate() {
            let is_selected = i == sel;
            let cursor = if is_selected { ">" } else { " " };
            let name_style = if is_selected {
                Style::new().fg(theme::THINKING).bold()
            } else {
                Style::new().fg(theme::TEXT)
            };
            let (status_icon, status_color) = if job.enabled {
                ("\u{25cf}", theme::SAGE)
            } else {
                ("\u{25cb}", theme::MUTED)
            };
            let next_str = job
                .next_fire
                .map(|t| t.format("%H:%M:%S").to_string())
                .unwrap_or_else(|| "—".to_string());

            let prompt_preview: String = job.prompt.chars().take(50).collect();
            lines.push(Line::from(vec![
                Span::styled(format!(" {} ", cursor), Style::new().fg(theme::THINKING)),
                Span::styled(status_icon, Style::new().fg(status_color)),
                Span::styled(format!(" {} ", job.expression), Style::new().fg(theme::DIM)),
                Span::styled(prompt_preview, name_style),
                Span::styled(format!("  @{}", next_str), Style::new().fg(theme::MUTED)),
            ]));
        }
        lines.push(Line::from(""));
    }

    // SubAgent section
    if !subagents.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            format!("  ▼ SubAgents ({})", subagent_count),
            Style::new().fg(theme::ACCENT).bold(),
        )]));
        for (i, sa) in subagents.iter().enumerate() {
            let row_idx = cron_count + i;
            let is_selected = row_idx == sel;
            let cursor = if is_selected { ">" } else { " " };
            let name_style = if is_selected {
                Style::new().fg(theme::THINKING).bold()
            } else {
                Style::new().fg(theme::TEXT)
            };
            let collapsed_marker = if sa.collapsed {
                Span::styled(" (collapsed)", Style::new().fg(theme::MUTED))
            } else {
                Span::styled(" (live)", Style::new().fg(theme::SAGE))
            };
            lines.push(Line::from(vec![
                Span::styled(format!(" {} ", cursor), Style::new().fg(theme::THINKING)),
                Span::styled(sa.agent_name.clone(), name_style),
                Span::styled(format!("  [{}]", sa.agent_id), Style::new().fg(theme::DIM)),
                collapsed_marker,
                Span::styled(
                    format!("  {} msgs", sa.view_models.len()),
                    Style::new().fg(theme::MUTED),
                ),
            ]));
        }
    }

    if total == 0 {
        lines.push(Line::from(vec![Span::styled(
            "  No active tasks",
            Style::new().fg(theme::MUTED).italic(),
        )]));
        lines.push(Line::from(vec![Span::styled(
            "  Cron jobs are scheduled via /loop command;",
            Style::new().fg(theme::DIM),
        )]));
        lines.push(Line::from(vec![Span::styled(
            "  SubAgents are spawned by Task / SubAgent tools.",
            Style::new().fg(theme::DIM),
        )]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from("  j/k) Navigate  Esc) Close").fg(theme::DIM));

    let content = Paragraph::new(ratatui::text::Text::from(lines));

    element!(
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().fg(theme::BORDER),
            top_title: Line::from(" Tasks ")
                .fg(theme::THINKING)
                .bold()
                .centered(),
            width: Constraint::Length(80),
            height: Constraint::Length(22),
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

/// 从 ViewModelsSnapshot 派生去重 SubAgent 列表。
fn collect_subagents(snap: &crate::kit::atoms::ViewModelsSnapshot) -> Vec<SubAgentGroupData> {
    let mut out: Vec<SubAgentGroupData> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for vm in snap.committed.iter().chain(snap.current_turn.iter()) {
        scan_vm_for_subagents(vm, &mut out, &mut seen);
    }
    out
}

fn scan_vm_for_subagents(
    vm: &ViewModel,
    out: &mut Vec<SubAgentGroupData>,
    seen: &mut std::collections::HashSet<String>,
) {
    if let ViewModel::SubAgentGroup(d) = vm {
        if seen.insert(d.agent_id.clone()) {
            out.push(d.clone());
        }
        for child in d.view_models.iter() {
            scan_vm_for_subagents(child, out, seen);
        }
    } else if let ViewModel::CollapsedGroup(g) = vm {
        for child in g.view_models.iter() {
            scan_vm_for_subagents(child, out, seen);
        }
    }
}

fn close_panel() {
    // I19-A: 弹栈而非清空整个栈，避免同时打开多个不同组面板时关闭一个会全部关闭
    crate::kit::panel_registry::close_active_panel();
}
