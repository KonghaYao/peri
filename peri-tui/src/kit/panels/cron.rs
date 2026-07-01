//! ratatui-kit CronPanel component.
//!
//! S6c：cron 任务列表从 `CRON_JOBS` atom 读取（由 `service_snapshot` 后台任务
//! 周期性从 ServiceRegistry.cron_scheduler 派生）。toggle/delete 操作 S11 解耦后
//! 通过 AcpClient 触发（暂留 TODO）。

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

use crate::kit::atoms::{CRON_JOBS, CronJobSummary};
use crate::kit::theme;

#[component]
pub fn CronPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let selected = hooks.use_state(|| 0usize);
    let confirm_delete = hooks.use_state(|| false);

    // S6c: 订阅 CRON_JOBS atom——后台 service_snapshot 2s 派生一次
    let jobs_store = hooks.use_store(*CRON_JOBS.get().unwrap());
    let jobs: Vec<CronJobSummary> = jobs_store.read().clone();
    let _ = jobs_store; // StoreState 是 Copy，无需显式 drop
    let count = jobs.len();

    hooks.use_local_events({
        let jobs_snapshot = jobs.clone();
        move |event: Event| {
            if let Event::Key(key) = event {
                if key.kind != KeyEventKind::Press {
                    return;
                }

                // Confirm-delete mode: Enter confirms, Esc cancels, others cancel
                if *confirm_delete.read() {
                    match key.code {
                        KeyCode::Enter => {
                            let sel = *selected.read();
                            if let Some(job) = jobs_snapshot.get(sel) {
                                cron_remove(&job.id);
                            }
                            *confirm_delete.write() = false;
                        }
                        KeyCode::Esc => {
                            *confirm_delete.write() = false;
                        }
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            return;
                        }
                        _ => {
                            *confirm_delete.write() = false;
                        }
                    }
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
                    KeyCode::Enter | KeyCode::Char(' ') => {
                        let sel = *selected.read();
                        if let Some(job) = jobs_snapshot.get(sel) {
                            cron_toggle(&job.id);
                        }
                    }
                    KeyCode::Char('d') if count > 0 => {
                        *confirm_delete.write() = true;
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {}
                    _ => {}
                }
            }
        }
    });

    let sel = *selected.read();
    let is_confirming = *confirm_delete.read();
    let enabled_count = jobs.iter().filter(|e| e.enabled).count();
    let mut lines: Vec<Line<'_>> = Vec::new();

    // Stats line
    if !jobs.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            format!("  {} configured, {} enabled", jobs.len(), enabled_count),
            Style::new().fg(theme::TEXT).bold(),
        )]));
    }

    // Hint / confirm-delete line
    if is_confirming {
        lines.push(Line::from(vec![Span::styled(
            "  Enter) Confirm delete  Esc) Cancel",
            Style::new().fg(theme::WARNING),
        )]));
    } else {
        lines.push(Line::from(vec![Span::styled(
            "  Enter/Space) Toggle  d) Delete  Esc) Close",
            Style::new().fg(theme::MUTED),
        )]));
    }
    lines.push(Line::from(""));

    // Task list
    if jobs.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            "  No cron tasks configured",
            Style::new().fg(theme::MUTED),
        )]));
        lines.push(Line::from(vec![Span::styled(
            "  Ask the agent to set up recurring tasks",
            Style::new().fg(theme::MUTED),
        )]));
    } else {
        for (i, entry) in jobs.iter().enumerate() {
            let is_selected = i == sel;
            let cursor = if is_selected { ">" } else { " " };
            let name_style = if is_selected {
                Style::new().fg(theme::THINKING).bold()
            } else {
                Style::new().fg(theme::TEXT)
            };
            let enabled_label = if entry.enabled { "ON" } else { "OFF" };
            let enabled_style = if entry.enabled {
                Style::new().fg(theme::SAGE)
            } else {
                Style::new().fg(theme::MUTED)
            };

            // Label line: cursor + num + schedule + [ON/OFF]
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} {}. {} ", cursor, i + 1, entry.expression),
                    name_style,
                ),
                Span::styled(format!("[{}]", enabled_label), enabled_style),
            ]));

            // Detail line: prompt summary (indented)
            let prompt_summary: String = entry
                .prompt
                .chars()
                .take(50)
                .chain(if entry.prompt.chars().count() > 50 {
                    Some('…')
                } else {
                    None
                })
                .collect();
            lines.push(Line::from(vec![Span::styled(
                format!("     {}", prompt_summary),
                Style::new().fg(theme::TEXT),
            )]));

            // Next fire timestamp (if available)
            if let Some(next) = entry.next_fire {
                lines.push(Line::from(vec![Span::styled(
                    format!("     next: {}", next.format("%Y-%m-%d %H:%M")),
                    Style::new().fg(theme::MUTED),
                )]));
            }
        }
    }

    let content = if lines.is_empty() {
        Paragraph::new(Line::from("  (empty)").fg(theme::MUTED))
    } else {
        Paragraph::new(ratatui::text::Text::from(lines))
    };

    element!(
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().fg(theme::BORDER),
            top_title: Line::from(" Cron ")
                .fg(theme::THINKING)
                .bold()
                .centered(),
            width: Constraint::Length(52),
            height: Constraint::Length(14),
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

/// H1g：toggle cron 任务启用状态。service_snapshot 下次 tick 自动刷新 UI（≤2s）。
fn cron_toggle(id: &str) {
    use crate::kit::atoms::CRON_SCHEDULER_HANDLE;
    if let Some(handle) = CRON_SCHEDULER_HANDLE.get() {
        let mut scheduler = handle.lock();
        scheduler.toggle(id);
        tracing::info!(cron_id = id, "CronPanel: toggled");
    }
}

/// H1g：删除 cron 任务。service_snapshot 下次 tick 自动刷新 UI（≤2s）。
fn cron_remove(id: &str) {
    use crate::kit::atoms::CRON_SCHEDULER_HANDLE;
    if let Some(handle) = CRON_SCHEDULER_HANDLE.get() {
        let mut scheduler = handle.lock();
        let removed = scheduler.remove(id);
        tracing::info!(cron_id = id, removed, "CronPanel: removed");
    }
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
