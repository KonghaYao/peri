//! ratatui-kit WorkflowPanel kanban component.
//!
//! Displays workflow run status in a kanban-style layout: run tabs at top,
//! phases on left, agents on right, footer shortcuts at bottom.

use crate::app::panel_types::PanelKind;
use crate::i18n;
use crate::kit::atoms::{LANG_VERSION, WORKFLOW_SNAPSHOT};
use crate::kit::list_nav::{
    cycle_next, cycle_previous, previous_selection, scroll_start_for_selected,
};
use peri_theme::atoms::THEME_ATOM;
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
use unicode_width::UnicodeWidthStr;

#[component]
pub fn WorkflowPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    // ALL hooks BEFORE any logic
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let _lang = hooks.use_atom(&LANG_VERSION);
    let snapshot_store = hooks.use_atom(&WORKFLOW_SNAPSHOT);
    let snapshot = snapshot_store.read().clone();
    let _ = snapshot_store;

    let active_run = hooks.use_state(|| 0usize);
    // 外部滚动状态——面板滚轮仲裁（panel_scroll.rs）驱动，统一 3 行/格 + 节流
    let sv_phase = hooks.use_state(ScrollViewState::default);
    let sv_agent = hooks.use_state(ScrollViewState::default);
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
    // 先 clamp phase 选择 → 据此过滤 agents → 再 clamp agent 选择
    let phase_count = current_run.phases.len();
    let clamped_phase = (*phase_sel.read()).min(phase_count.saturating_sub(1));
    if *phase_sel.read() != clamped_phase {
        *phase_sel.write() = clamped_phase;
    }

    let sel_phase = *phase_sel.read();
    let sel_phase_title = current_run
        .phases
        .get(sel_phase)
        .map(|p| p.title.as_str())
        .unwrap_or("");

    let agent_count = if sel_phase_title.is_empty() {
        current_run.agents.len()
    } else {
        current_run
            .agents
            .iter()
            .filter(|a| a.phase.as_deref() == Some(sel_phase_title))
            .count()
    };
    let clamped_agent = (*agent_sel.read()).min(agent_count.saturating_sub(1));
    if *agent_sel.read() != clamped_agent {
        *agent_sel.write() = clamped_agent;
    }

    let sel_agent = *agent_sel.read();
    let focus = *focus_left.read();

    // ── Tab bar ──────────────────────────────────────────────────────────
    let theme = theme_def.read();
    let tab_bar_spans: Vec<Span<'_>> = runs
        .iter()
        .enumerate()
        .map(|(i, run)| {
            let is_selected = i == sel_run;
            let emoji: String = if run.status == "running" {
                running_indicator()
            } else {
                status_emoji_for_run(&run.status).to_string()
            };
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
    for (pi, phase) in current_run.phases.iter().enumerate() {
        let is_sel = focus && pi == sel_phase;
        let arrow = if is_sel { ">" } else { " " };
        let arrow_style = Style::new().fg(theme.component.panel.title).bold();
        let emoji: String = if phase.status == "active" {
            running_indicator()
        } else {
            status_emoji_for_phase(&phase.status).to_string()
        };
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

    // ── Agent lines（按选中 phase 过滤，去除重复 phase 标签）──────────
    let mut agent_lines: Vec<Line<'_>> = Vec::new();
    let filtered_agents: Vec<_> = current_run
        .agents
        .iter()
        .filter(|a| {
            if sel_phase_title.is_empty() {
                true
            } else {
                a.phase.as_deref() == Some(sel_phase_title)
            }
        })
        .collect();
    for (ai, agent) in filtered_agents.iter().enumerate() {
        let is_sel = !focus && ai == sel_agent;
        let arrow = if is_sel { ">" } else { " " };
        let arrow_style = Style::new().fg(theme.component.panel.title).bold();
        let emoji: String = if agent.status == "running" {
            running_indicator()
        } else {
            status_emoji_for_agent(&agent.status).to_string()
        };
        let emoji_color = agent_status_color(&agent.status, &theme);
        let name = agent
            .label
            .as_deref()
            .unwrap_or("?")
            .chars()
            .take(18)
            .collect::<String>();
        // 使用 unicode_width 计算终端列宽，而非 Rust char 计数
        let name_display_width = UnicodeWidthStr::width(name.as_str());
        let name_pad = if name_display_width < 18 {
            " ".repeat(18 - name_display_width)
        } else {
            String::new()
        };
        let tokens_padded = format!("{:>8}", abbreviate_count(agent.token_count.unwrap_or(0)));
        let tools_padded = format!("{:>4}", agent.tool_count.unwrap_or(0));
        let name_style = if is_sel {
            Style::new().fg(theme.component.panel.title).bold()
        } else {
            Style::new().fg(theme.semantic.text.primary)
        };
        let dim_style = Style::new().fg(theme.semantic.text.dim);

        agent_lines.push(Line::from(vec![
            Span::styled(arrow, arrow_style),
            Span::styled(format!(" {emoji} "), emoji_color),
            Span::styled(format!("{name}{name_pad}"), name_style),
            Span::styled(format!(" {tokens_padded}"), dim_style),
            Span::styled(format!("  {tools_padded}"), dim_style),
        ]));
    }
    if filtered_agents.is_empty() {
        agent_lines.push(Line::from(vec![Span::styled(
            "  (no agents for this phase)",
            Style::new().fg(theme.semantic.text.muted),
        )]));
    }

    // ── Two-column layout via ratatui View + ScrollView ──────────────────────
    //
    // 左侧 Phase 列 40%，中部 │ 分隔线，右侧 Agents 列 60%。
    // 每列独立 ScrollView，选中项跟随滚动。

    // 各列占满剩余空间，ScrollView 动态裁剪可见项。
    const VISIBLE_ITEMS: usize = 20;

    let phase_scroll = scroll_start_for_selected(sel_phase, phase_lines.len(), VISIBLE_ITEMS);
    let agent_scroll = scroll_start_for_selected(sel_agent, filtered_agents.len(), VISIBLE_ITEMS);

    // Build phases Paragraph: header + visible slice
    let mut phase_text: Vec<Line<'_>> = Vec::new();
    phase_text.push(Line::from(Span::styled(
        " Phases",
        Style::new().fg(theme.semantic.text.muted).bold(),
    )));
    phase_text.extend(
        phase_lines
            .into_iter()
            .skip(phase_scroll)
            .take(VISIBLE_ITEMS),
    );

    // Build agents Paragraph: header + visible slice
    let mut agent_text: Vec<Line<'_>> = Vec::new();
    agent_text.push(Line::from(Span::styled(
        " Agents                 Tokens Tools",
        Style::new().fg(theme.semantic.text.muted).bold(),
    )));
    agent_text.extend(
        agent_lines
            .into_iter()
            .skip(agent_scroll)
            .take(VISIBLE_ITEMS),
    );

    // Divider —— 渲染垂直 │ 线；高度固定为 body viewport
    let divider_style = Style::new().fg(theme.semantic.border.default);
    let divider_lines: Vec<Line<'_>> = (0..30_usize)
        .map(|_| Line::from(Span::styled("│", divider_style)))
        .collect();

    let phase_para = Paragraph::new(ratatui::text::Text::from(phase_text));
    let agent_para = Paragraph::new(ratatui::text::Text::from(agent_text));
    let divider_para = Paragraph::new(ratatui::text::Text::from(divider_lines));

    drop(theme);

    // 面板滚轮仲裁注册（双栏：按 40% 切分左右区域，divider 列并入右侧）
    let (phase_area, agent_area) =
        crate::kit::panel_scroll::split_vertical(hooks.use_previous_size(), 40);
    crate::kit::panel_scroll::register_panel_scrolls(
        PanelKind::Workflow,
        vec![
            crate::kit::panel_scroll::PanelScrollSlot {
                area: phase_area,
                state: sv_phase,
            },
            crate::kit::panel_scroll::PanelScrollSlot {
                area: agent_area,
                state: sv_agent,
            },
        ],
    );

    // ── Footer ───────────────────────────────────────────────────────────
    let footer =
        Line::from(i18n::tr("workflow-footer-shortcuts")).fg(theme_def.read().semantic.text.dim);

    panel_shell!(PanelKind::Workflow, {
        View(height: Constraint::Length(1)) {
            Text(text: tab_bar)
        }
        View(height: Constraint::Length(1)) {}
        View(
            flex_direction: Direction::Horizontal,
            width: Constraint::Fill(1),
            height: Constraint::Fill(1),
        ) {
            View(width: Constraint::Percentage(40), height: Constraint::Fill(1)) {
                ScrollView(
                    scrollbars: crate::kit::panel_registry::clean_scrollbars(),
                    state: Some(sv_phase),
                    width: Constraint::Fill(1),
                    height: Constraint::Fill(1),
                ) {
                    Text(text: phase_para)
                }
            }
            View(width: Constraint::Length(1), height: Constraint::Fill(1)) {
                Text(text: divider_para)
            }
            View(width: Constraint::Fill(1), height: Constraint::Fill(1)) {
                ScrollView(
                    scrollbars: crate::kit::panel_registry::clean_scrollbars(),
                    state: Some(sv_agent),
                    width: Constraint::Fill(1),
                    height: Constraint::Fill(1),
                ) {
                    Text(text: agent_para)
                }
            }
        }
        View(height: Constraint::Length(1)) {
            Text(text: Paragraph::new(ratatui::text::Text::from(footer)))
        }
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

/// 壁钟驱动的运行中动画帧指示器。每 100ms 推进一帧。
fn running_indicator() -> String {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    const FRAMES: &[char] = &[
        '✳', '✴', '✵', '✶', '✷', '✸', '✹', '✺', '✻', '✼', '❃', '❊', '✼', '✻', '✺', '✸',
    ];
    let start = START.get_or_init(Instant::now);
    let tick = (start.elapsed().as_millis() / 100) as usize;
    let frame = FRAMES[tick % FRAMES.len()];
    format!("{frame}")
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
