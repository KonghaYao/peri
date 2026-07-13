//! ratatui-kit WorkflowPanel kanban component.
//!
//! Displays workflow run status in a kanban-style layout: run tabs at top,
//! phases on left, agents on right, footer shortcuts at bottom.

use crate::app::panel_types::PanelKind;
use crate::i18n;
use crate::kit::atoms::{LANG_VERSION, WORKFLOW_SNAPSHOT};
use crate::kit::list_nav::{cycle_next, cycle_previous, previous_selection};
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind},
    prelude::*,
    ratatui::{
        style::{Style, Stylize},
        text::{Line, Span},
        widgets::Paragraph,
    },
};

#[component]
pub fn WorkflowPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    // ALL hooks BEFORE any logic
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let _lang = hooks.use_atom(&LANG_VERSION);
    let snapshot_store = hooks.use_atom(&WORKFLOW_SNAPSHOT);
    let snapshot = snapshot_store.read().clone();
    let _ = snapshot_store;

    let active_run = hooks.use_state(|| 0usize);
    let focus_left = hooks.use_state(|| true);
    let phase_sel = hooks.use_state(|| 0usize);
    let agent_sel = hooks.use_state(|| 0usize);

    // Determine panel state
    let runs = match &snapshot {
        None => {
            // Loading state
            let msg = Paragraph::new(Line::from(vec![Span::styled(
                format!(
                    "  {} {}...",
                    i18n::tr("common-loading"),
                    i18n::tr("workflow-loading-runs")
                ),
                Style::new().fg(theme_def.read().semantic.text.muted),
            )]));
            return panel_shell!(PanelKind::Workflow, { Text(text: msg) });
        }
        Some(s) => &s.runs,
    };

    if runs.is_empty() {
        // Empty state
        let msg = Paragraph::new(Line::from(vec![Span::styled(
            format!("  {}", i18n::tr("workflow-no-runs")),
            Style::new().fg(theme_def.read().semantic.text.muted),
        )]));
        return panel_shell!(PanelKind::Workflow, { Text(text: msg) });
    }

    let run_count = runs.len();

    // ── Keyboard event handling ──────────────────────────────────────────
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
                    return EventResult::Consumed;
                }
                KeyCode::Tab => {
                    let mut r = active_run.write();
                    *r = cycle_next(*r, run_count);
                    *phase_sel.write() = 0;
                    *agent_sel.write() = 0;
                    return EventResult::Consumed;
                }
                KeyCode::BackTab => {
                    let mut r = active_run.write();
                    *r = cycle_previous(*r, run_count);
                    *phase_sel.write() = 0;
                    *agent_sel.write() = 0;
                    return EventResult::Consumed;
                }
                KeyCode::Left => {
                    *focus_left.write() = true;
                    return EventResult::Consumed;
                }
                KeyCode::Right => {
                    *focus_left.write() = false;
                    return EventResult::Consumed;
                }
                KeyCode::Up => {
                    if *focus_left.read() {
                        let mut p = phase_sel.write();
                        *p = previous_selection(*p);
                    } else {
                        let mut a = agent_sel.write();
                        *a = previous_selection(*a);
                    }
                    return EventResult::Consumed;
                }
                KeyCode::Down => {
                    if *focus_left.read() {
                        let mut p = phase_sel.write();
                        *p = p.saturating_add(1);
                    } else {
                        let mut a = agent_sel.write();
                        *a = a.saturating_add(1);
                    }
                    return EventResult::Consumed;
                }
                KeyCode::Enter => {
                    // MVP: no-op
                    return EventResult::Consumed;
                }
                _ => {}
            }
            EventResult::Ignored
        }
    });

    let sel_run = *active_run.read();
    let current_run = &runs[sel_run];

    // ── Selection clamping (during render, not event handler) ────────────
    // Gate writes with a change check to avoid infinite re-render loops
    let phase_count = current_run.phases.len();
    let agent_count = current_run.agents.len();
    let clamped_phase = (*phase_sel.read()).min(phase_count.saturating_sub(1));
    let clamped_agent = (*agent_sel.read()).min(agent_count.saturating_sub(1));
    if *phase_sel.read() != clamped_phase {
        *phase_sel.write() = clamped_phase;
    }
    if *agent_sel.read() != clamped_agent {
        *agent_sel.write() = clamped_agent;
    }

    let sel_phase = *phase_sel.read();
    let sel_agent = *agent_sel.read();
    let focus = *focus_left.read();

    // ── Tab bar ──────────────────────────────────────────────────────────
    let theme = theme_def.read();
    let tab_bar_spans: Vec<Span<'_>> = runs
        .iter()
        .enumerate()
        .map(|(i, run)| {
            let is_selected = i == sel_run;
            let emoji = status_emoji_for_run(&run.status);
            let name = &run.workflow_name;
            let text = format!(" {emoji} {name} ");
            if is_selected {
                Span::styled(
                    text,
                    Style::new()
                        .fg(theme.component.panel.title)
                        .bg(theme.semantic.status.running)
                        .bold(),
                )
            } else {
                Span::styled(text, Style::new().fg(theme.semantic.text.muted))
            }
        })
        .collect();
    let tab_bar = Paragraph::new(Line::from(tab_bar_spans));

    // ── Phase lines ──────────────────────────────────────────────────────
    let mut phase_lines: Vec<Line<'_>> = Vec::new();
    // Phase header
    phase_lines.push(Line::from(vec![Span::styled(
        " Phases",
        Style::new().fg(theme.semantic.text.muted).bold(),
    )]));
    for (pi, phase) in current_run.phases.iter().enumerate() {
        let is_sel = focus && pi == sel_phase;
        let arrow = if is_sel { ">" } else { " " };
        let arrow_style = Style::new().fg(theme.component.panel.title).bold();
        let emoji = status_emoji_for_phase(&phase.status);
        let emoji_color = phase_status_color(&phase.status, &theme);
        let name = &phase.title;
        let name_style = if is_sel {
            Style::new().fg(theme.component.panel.title).bold()
        } else {
            Style::new().fg(theme.semantic.text.primary)
        };
        let agent_count = current_run
            .agents
            .iter()
            .filter(|a| a.phase.as_deref() == Some(&phase.title))
            .count();
        let agent_tag = if agent_count > 0 {
            format!(" [{agent_count}]")
        } else {
            String::new()
        };
        let tag_style = Style::new().fg(theme.semantic.text.muted);

        phase_lines.push(Line::from(vec![
            Span::styled(arrow, arrow_style),
            Span::styled(format!(" {emoji} "), emoji_color),
            Span::styled(name.chars().take(28).collect::<String>(), name_style),
            Span::styled(agent_tag, tag_style),
        ]));
    }
    // If no phases, show placeholder
    if current_run.phases.is_empty() {
        phase_lines.push(Line::from(vec![Span::styled(
            "  (no phases)",
            Style::new().fg(theme.semantic.text.muted),
        )]));
    }

    // ── Agent lines ──────────────────────────────────────────────────────
    let mut agent_lines: Vec<Line<'_>> = Vec::new();
    agent_lines.push(Line::from(vec![Span::styled(
        " Agents",
        Style::new().fg(theme.semantic.text.muted).bold(),
    )]));
    for (ai, agent) in current_run.agents.iter().enumerate() {
        let is_sel = !focus && ai == sel_agent;
        let arrow = if is_sel { ">" } else { " " };
        let arrow_style = Style::new().fg(theme.component.panel.title).bold();
        let emoji = status_emoji_for_agent(&agent.status);
        let emoji_color = agent_status_color(&agent.status, &theme);
        let name = agent
            .label
            .as_deref()
            .unwrap_or("?")
            .chars()
            .take(16)
            .collect::<String>();
        let name_style = if is_sel {
            Style::new().fg(theme.component.panel.title).bold()
        } else {
            Style::new().fg(theme.semantic.text.primary)
        };
        let phase_tag = agent.phase.as_deref().unwrap_or("-");
        let tag_style = Style::new().fg(theme.semantic.text.muted);
        let tokens = abbreviate_count(agent.token_count.unwrap_or(0));
        let tools = format!("{}", agent.tool_count.unwrap_or(0));
        let dim_style = Style::new().fg(theme.semantic.text.dim);

        agent_lines.push(Line::from(vec![
            Span::styled(arrow, arrow_style),
            Span::styled(format!(" {emoji} "), emoji_color),
            Span::styled(format!("{name:16}"), name_style),
            Span::styled(format!(" [{phase_tag}]"), tag_style),
            Span::styled(format!(" {tokens:>8}"), dim_style),
            Span::styled(format!("  {tools:>8}"), dim_style),
        ]));
    }
    if current_run.agents.is_empty() {
        agent_lines.push(Line::from(vec![Span::styled(
            "  (no agents)",
            Style::new().fg(theme.semantic.text.muted),
        )]));
    }

    // ── Interleave phases and agents side by side ────────────────────────
    let max_rows = phase_lines.len().max(agent_lines.len());
    let mut body_lines: Vec<Line<'_>> = Vec::new();
    let sep_span = Span::styled(" │ ", Style::new().fg(theme.semantic.text.dim));

    for row in 0..max_rows {
        let phase_span = if row < phase_lines.len() {
            phase_lines[row].clone()
        } else {
            Line::from("")
        };
        let agent_span = if row < agent_lines.len() {
            agent_lines[row].clone()
        } else {
            Line::from("")
        };

        let mut combined_spans: Vec<Span<'_>> = Vec::new();
        // Add phase spans (pad to ~30 wide)
        combined_spans.extend(phase_span.spans);
        combined_spans.push(sep_span.clone());
        combined_spans.extend(agent_span.spans);
        body_lines.push(Line::from(combined_spans));
    }

    drop(theme);

    // ── Footer ───────────────────────────────────────────────────────────
    let footer =
        Line::from(i18n::tr("workflow-footer-shortcuts")).fg(theme_def.read().semantic.text.dim);

    let content = Paragraph::new(ratatui::text::Text::from({
        let mut all: Vec<Line> = Vec::new();
        all.push(Line::from(""));
        all.extend(body_lines);
        all.push(Line::from(""));
        all.push(footer);
        all
    }));

    panel_shell!(PanelKind::Workflow, {
        Text(text: tab_bar)
        Text(text: content)
    })
}

// ── Helpers ─────────────────────────────────────────────────────────────

fn close_panel() {
    // I19-A: 弹栈而非清空整个栈，避免同时打开多个不同组面板时关闭一个会全部关闭
    crate::kit::panel_registry::close_active_panel();
}

fn status_emoji_for_run(status: &str) -> &'static str {
    match status {
        "running" => "\u{25cf}",           // ●
        "completed" => "\u{2713}",         // ✓
        "failed" | "killed" => "\u{2717}", // ✗
        _ => "\u{25cb}",                   // ○
    }
}

fn status_emoji_for_phase(status: &str) -> &'static str {
    match status {
        "active" => "\u{25cf}",  // ●
        "done" => "\u{2713}",    // ✓
        "pending" => "\u{25cb}", // ○
        _ => "\u{25cb}",         // ○
    }
}

fn status_emoji_for_agent(status: &str) -> &'static str {
    match status {
        "running" => "\u{25cf}",          // ●
        "done" => "\u{2713}",             // ✓
        "pending" => "\u{25cb}",          // ○
        "dead" | "skipped" => "\u{2717}", // ✗
        _ => "\u{25cb}",                  // ○
    }
}

fn phase_status_color(
    status: &str,
    theme: &peri_theme::theme::ThemeDefinition,
) -> ratatui::style::Style {
    Style::new().fg(match status {
        "active" => theme.semantic.status.running,
        "done" => theme.semantic.status.success,
        "failed" => theme.semantic.status.error,
        _ => theme.semantic.text.muted,
    })
}

fn agent_status_color(
    status: &str,
    theme: &peri_theme::theme::ThemeDefinition,
) -> ratatui::style::Style {
    Style::new().fg(match status {
        "running" => theme.semantic.status.running,
        "done" => theme.semantic.status.success,
        "dead" | "skipped" => theme.semantic.status.error,
        _ => theme.semantic.text.muted,
    })
}

/// Abbreviate a count into a human-readable short form.
fn abbreviate_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{}k", n / 1_000)
    } else {
        format!("{n}")
    }
}
