//! ratatui-kit CronPanel component.
//!
//! Phase 6a: cron task list with cursor navigation, toggle, and delete
//! (use_state + use_local_events). Mock data; Phase 8 通过 Atom/props 注入真实
//! cron 任务列表。
//!
//! 旧版: panel/panels/cron.rs (PanelState trait, CronTaskDto-based).

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

use crate::ui::theme;

/// Mock cron entry (Phase 8: injected via Atom from CronTaskDto).
#[allow(dead_code)]
struct CronEntry {
    schedule: &'static str,
    prompt: &'static str,
    enabled: bool,
}

#[allow(dead_code)]
const CRON_ENTRIES: &[CronEntry] = &[
    CronEntry {
        schedule: "*/5 * * * *",
        prompt: "Check deploy status and report",
        enabled: true,
    },
    CronEntry {
        schedule: "0 * * * *",
        prompt: "Hourly health check",
        enabled: true,
    },
    CronEntry {
        schedule: "0 0 * * *",
        prompt: "Daily backup rotation",
        enabled: false,
    },
    CronEntry {
        schedule: "0 9 * * 1",
        prompt: "Weekly summary report",
        enabled: true,
    },
];

#[component]
fn CronPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let selected = hooks.use_state(|| 0usize);
    let confirm_delete = hooks.use_state(|| false);

    hooks.use_local_events({
        let selected = selected;
        let confirm_delete = confirm_delete;
        let count = CRON_ENTRIES.len();
        move |event: Event| {
            if let Event::Key(key) = event {
                if key.kind != KeyEventKind::Press {
                    return;
                }

                // Confirm-delete mode: Enter confirms, Esc cancels, others cancel
                if *confirm_delete.read() {
                    match key.code {
                        KeyCode::Enter => {
                            // TODO Phase 8: emit SendToAcp delete_cron_task
                            *confirm_delete.write() = false;
                        }
                        KeyCode::Esc => {
                            *confirm_delete.write() = false;
                        }
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            // Ctrl+C: don't consume, let upper layers handle
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
                        // TODO Phase 8: close panel via use_input_layer
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
                        // TODO Phase 8: emit SendToAcp toggle_cron_task
                    }
                    KeyCode::Char('d') => {
                        if !CRON_ENTRIES.is_empty() {
                            *confirm_delete.write() = true;
                        }
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        // Ctrl+C: don't consume
                        return;
                    }
                    _ => {}
                }
            }
        }
    });

    let sel = *selected.read();
    let is_confirming = *confirm_delete.read();
    let enabled_count = CRON_ENTRIES.iter().filter(|e| e.enabled).count();
    let mut lines: Vec<Line<'_>> = Vec::new();

    // Stats line
    if !CRON_ENTRIES.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            format!(
                "  {} configured, {} enabled",
                CRON_ENTRIES.len(),
                enabled_count
            ),
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
    if CRON_ENTRIES.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            "  No cron tasks configured",
            Style::new().fg(theme::MUTED),
        )]));
        lines.push(Line::from(vec![Span::styled(
            "  Ask the agent to set up recurring tasks",
            Style::new().fg(theme::MUTED),
        )]));
    } else {
        for (i, entry) in CRON_ENTRIES.iter().enumerate() {
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
                    format!(" {} {}. {} ", cursor, i + 1, entry.schedule),
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
