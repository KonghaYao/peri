//! ratatui-kit CronPanel component.
//!
//! S6c：cron 任务列表从 `CRON_JOBS` atom 读取（由 `service_snapshot` 后台任务
//! 周期性从 ServiceRegistry.cron_scheduler 派生）。toggle/delete 操作 S11 解耦后
//! 通过 AcpClient 触发（暂留 TODO）。

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
use crate::kit::atoms::{CRON_JOBS, CronJobSummary, LANG_VERSION};
use crate::kit::list_nav::{next_selection, previous_selection, scroll_start_for_selected};
use fluent_bundle::FluentValue;
use peri_theme::atoms::THEME_ATOM;

#[component]
pub fn CronPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let selected = hooks.use_state(|| 0usize);
    let confirm_delete = hooks.use_state(|| false);

    // S6c: 订阅 CRON_JOBS atom——后台 service_snapshot 2s 派生一次
    let jobs_store = hooks.use_atom(&CRON_JOBS);
    let jobs: Vec<CronJobSummary> = jobs_store.read().clone();
    let _ = jobs_store; // StoreState 是 Copy，无需显式 drop
    let _ = hooks.use_atom(&LANG_VERSION);

    hooks.use_event_handler(EventScope::Current, EventPriority::Normal, {
        move |event| {
            let Event::Key(key) = event else {
                return EventResult::Ignored;
            };
            if key.kind != KeyEventKind::Press {
                return EventResult::Ignored;
            }

            // Confirm-delete mode: Enter confirms, Esc cancels, others cancel
            if *confirm_delete.read() {
                match key.code {
                    KeyCode::Enter => {
                        let sel = *selected.read();
                        let jobs = CRON_JOBS.state().read().clone();
                        if let Some(job) = jobs.get(sel) {
                            cron_remove(&job.id);
                        }
                        *confirm_delete.write() = false;
                    }
                    KeyCode::Esc => {
                        *confirm_delete.write() = false;
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return EventResult::Ignored;
                    }
                    _ => {
                        *confirm_delete.write() = false;
                    }
                }
                return EventResult::Consumed;
            }

            match key.code {
                KeyCode::Esc => {
                    close_panel();
                }
                KeyCode::Up => {
                    let mut s = selected.write();
                    *s = previous_selection(*s);
                }
                KeyCode::Down => {
                    let mut s = selected.write();
                    let count = CRON_JOBS.state().read().len();
                    if count > 0 {
                        *s = next_selection(*s, count);
                    }
                }
                KeyCode::Enter | KeyCode::Char(' ') => {
                    let sel = *selected.read();
                    let jobs = CRON_JOBS.state().read().clone();
                    if let Some(job) = jobs.get(sel) {
                        cron_toggle(&job.id);
                    }
                }
                KeyCode::Char('d') if !CRON_JOBS.state().read().is_empty() => {
                    *confirm_delete.write() = true;
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
    let is_confirming = *confirm_delete.read();
    let enabled_count = jobs.iter().filter(|e| e.enabled).count();
    let mut lines: Vec<Line<'_>> = Vec::new();

    // 视口跟随：让选中项始终可见（issue 2026-07-06-panels-selection-no-scroll-follow）。
    // panel 高度 18 - border 2 - header 3 = 13 行；每项 3 行 → 可见 4 个。
    // next_fire 缺失时占位空行，保证每项固定 3 行（视口计算依赖）。
    const VISIBLE_ITEMS: usize = 4;
    let scroll_start = scroll_start_for_selected(sel, jobs.len(), VISIBLE_ITEMS);

    // Stats line
    if !jobs.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            i18n::tr_args(
                "panel-cron-stats",
                &[
                    ("configured".to_string(), FluentValue::from(jobs.len() as i64)),
                    ("enabled".to_string(), FluentValue::from(enabled_count as i64)),
                ],
            ),
            Style::new()
                .fg(theme_def.read().semantic.text.primary)
                .bold(),
        )]));
    }

    // Hint / confirm-delete line
    if is_confirming {
        lines.push(Line::from(vec![Span::styled(
            i18n::tr("panel-cron-confirm-hint"),
            Style::new().fg(theme_def.read().semantic.status.warning),
        )]));
    } else {
        lines.push(Line::from(vec![Span::styled(
            i18n::tr("panel-cron-nav-hint"),
            Style::new().fg(theme_def.read().semantic.text.muted),
        )]));
    }
    lines.push(Line::from(""));

    // Task list
    if jobs.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            i18n::tr("panel-cron-empty"),
            Style::new().fg(theme_def.read().semantic.text.muted),
        )]));
        lines.push(Line::from(vec![Span::styled(
            i18n::tr("panel-cron-empty-hint"),
            Style::new().fg(theme_def.read().semantic.text.muted),
        )]));
    } else {
        for (i, entry) in jobs
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
            let enabled_label = if entry.enabled {
                i18n::tr("panel-cron-status-on")
            } else {
                i18n::tr("panel-cron-status-off")
            };
            let enabled_style = if entry.enabled {
                Style::new().fg(theme_def.read().semantic.status.success)
            } else {
                Style::new().fg(theme_def.read().semantic.text.muted)
            };

            // Label line: cursor + num + schedule + [ON/OFF]
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} {}. {} ", cursor, i + 1, entry.expression),
                    name_style,
                ),
                Span::styled(
                    i18n::tr_args("panel-cron-status-format", &[("status".to_string(), FluentValue::from(enabled_label))]), enabled_style),
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
                Style::new().fg(theme_def.read().semantic.text.primary),
            )]));

            // Next fire timestamp (if available)
            if let Some(next) = entry.next_fire {
                let ts = next.format("%Y-%m-%d %H:%M").to_string();
                lines.push(Line::from(vec![Span::styled(
                    i18n::tr_args("panel-cron-next-fire", &[("time".to_string(), FluentValue::from(ts))]),                    Style::new().fg(theme_def.read().semantic.text.muted),
                )]));
            } else {
                // next_fire 缺失时占位空行，保证每项固定 3 行（视口计算依赖）
                lines.push(Line::from(""));
            }
        }
    }

    let content = if lines.is_empty() {
        Paragraph::new(Line::from(i18n::tr("common-empty")).fg(theme_def.read().semantic.text.muted))
    } else {
        Paragraph::new(ratatui::text::Text::from(lines))
    };

    panel_shell!(PanelKind::Cron, {
            ScrollView(
                scrollbars: crate::kit::panel_registry::clean_scrollbars(),
                width: Constraint::Fill(1),
                height: Constraint::Fill(1),
            ) {
                Text(text: content)
            }
    })
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
    // I19-A: 弹栈而非清空整个栈，避免同时打开多个不同组面板时关闭一个会全部关闭
    crate::kit::panel_registry::close_active_panel();
}
