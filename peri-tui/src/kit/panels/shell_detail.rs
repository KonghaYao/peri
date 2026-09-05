//! ratatui-kit ShellDetailPanel — 后台 shell 任务轻量详情（只读）。

use crate::app::panel_types::PanelKind;
use crate::i18n;
use crate::kit::acp_types::BgTaskEntry;
use crate::kit::atoms::{
    BG_DISPLAY, BG_LIVE_DETAIL, BG_TASKS, BgDisplayEntry, BgLiveDetail, BgLiveStatus, LANG_VERSION,
    SELECTED_BG_TASK_ID,
};
use peri_theme::atoms::THEME_ATOM;
use peri_theme::theme::ThemeDefinition;
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
pub fn ShellDetailPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let _lang_ver = hooks.use_atom(&LANG_VERSION);
    let sv = hooks.use_state(ScrollViewState::default);

    let selected_id = SELECTED_BG_TASK_ID.state().read().clone();
    let tasks_store = hooks.use_atom(&BG_TASKS);
    let tasks = tasks_store.read().clone();
    let _ = tasks_store;

    let display_store = hooks.use_atom(&BG_DISPLAY);
    let display = display_store.read().clone();
    let _ = display_store;

    let live_store = hooks.use_atom(&BG_LIVE_DETAIL);
    let live = live_store.read().clone();
    let _ = live_store;

    hooks.use_event_handler(EventScope::Current, EventPriority::Normal, move |event| {
        let Event::Key(key) = event else {
            return EventResult::Ignored;
        };
        if key.kind != KeyEventKind::Press {
            return EventResult::Ignored;
        }
        if key.code == KeyCode::Esc {
            close_panel();
        }
        EventResult::Consumed
    });

    let area = hooks.use_previous_size();
    let theme = theme_def.read();
    let mut lines =
        build_shell_detail_lines(selected_id.as_deref(), &tasks, &display, &live, &theme);

    if lines.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            i18n::tr("shell-detail-not-found"),
            Style::new().fg(theme.semantic.text.muted),
        )]));
    }
    drop(theme);

    let content = Paragraph::new(ratatui::text::Text::from(lines));
    crate::kit::panel_scroll::register_panel_scroll(PanelKind::ShellDetail, area, sv);

    panel_shell!(PanelKind::ShellDetail, {
        ScrollView(
            scrollbars: crate::kit::panel_registry::clean_scrollbars(),
            state: Some(sv),
            width: Constraint::Fill(1),
            height: Constraint::Fill(1),
        ) {
            Text(text: content)
        }
    })
}

fn build_shell_detail_lines(
    selected_id: Option<&str>,
    tasks: &[BgTaskEntry],
    display: &[BgDisplayEntry],
    live: &std::collections::HashMap<String, BgLiveDetail>,
    theme: &ThemeDefinition,
) -> Vec<Line<'static>> {
    let Some(ctx) = resolve_shell_detail_context(tasks, display, live, selected_id) else {
        return Vec::new();
    };

    let display_row = display.iter().find(|e| e.id == ctx.task_id);
    let live_row = live.get(&ctx.task_id);

    let mut lines = Vec::new();
    let title_style = Style::new().fg(theme.component.panel.title).bold();
    let dim = Style::new().fg(theme.semantic.text.dim);
    let muted = Style::new().fg(theme.semantic.text.muted);

    lines.push(Line::from(vec![Span::styled(
        ctx.summary.clone(),
        title_style,
    )]));
    lines.push(Line::from(vec![Span::styled(
        format!("  id: {}", ctx.task_id),
        dim,
    )]));
    if let Some(pid) = ctx.pid {
        lines.push(Line::from(vec![Span::styled(format!("  pid: {pid}"), dim)]));
    }
    if let Some(started) = ctx.started_at.as_deref().filter(|s| !s.is_empty()) {
        lines.push(Line::from(vec![Span::styled(
            format!("  started: {started}"),
            dim,
        )]));
    }

    let status_line = status_label(display_row, live_row);
    lines.push(Line::from(vec![Span::styled(status_line, muted)]));
    lines.push(Line::from(""));

    match output_section(live_row, display_row) {
        OutputSection::Preview(text) => {
            lines.push(Line::from(vec![Span::styled(
                i18n::tr("shell-detail-output-preview"),
                dim,
            )]));
            for line in text.lines() {
                lines.push(Line::from(format!("  {line}")));
            }
        }
        OutputSection::RunningNoStream => {
            lines.push(Line::from(vec![Span::styled(
                i18n::tr("shell-detail-running"),
                muted,
            )]));
            lines.push(Line::from(vec![Span::styled(
                i18n::tr("shell-detail-no-output"),
                muted,
            )]));
        }
        OutputSection::NoOutput => {
            lines.push(Line::from(vec![Span::styled(
                i18n::tr("shell-detail-no-output"),
                muted,
            )]));
        }
    }

    lines
}

enum OutputSection {
    Preview(String),
    RunningNoStream,
    NoOutput,
}

fn output_section(live: Option<&BgLiveDetail>, display: Option<&BgDisplayEntry>) -> OutputSection {
    if let Some(preview) = live
        .and_then(|d| d.output_preview.as_ref())
        .filter(|s| !s.is_empty())
    {
        return OutputSection::Preview(preview.clone());
    }

    let running = live
        .map(|d| d.status == BgLiveStatus::Running)
        .unwrap_or_else(|| display.map(|d| d.is_active).unwrap_or(true));

    if running {
        OutputSection::RunningNoStream
    } else {
        OutputSection::NoOutput
    }
}

fn status_label(display: Option<&BgDisplayEntry>, live: Option<&BgLiveDetail>) -> String {
    if let Some(d) = live {
        let base = match d.status {
            BgLiveStatus::Running => i18n::tr("shell-detail-status-running"),
            BgLiveStatus::Succeeded => i18n::tr("shell-detail-status-succeeded"),
            BgLiveStatus::Failed => i18n::tr("shell-detail-status-failed"),
            BgLiveStatus::Cancelled => i18n::tr("shell-detail-status-cancelled"),
        };
        if let Some(reason) = d.cancel_reason.as_deref().filter(|s| !s.is_empty()) {
            return format!("{base} ({reason})");
        }
        return base;
    }
    if let Some(row) = display {
        if row.is_error {
            return i18n::tr("shell-detail-status-failed");
        }
        if row.is_active {
            return i18n::tr("shell-detail-status-running");
        }
        return i18n::tr("shell-detail-status-succeeded");
    }
    i18n::tr("shell-detail-status-running")
}

/// 抽屉展示用的 shell 上下文（运行中来自 `BG_TASKS`；完成后仍可从 display / live 投影读取）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShellDetailContext {
    pub task_id: String,
    pub summary: String,
    pub pid: Option<u32>,
    pub started_at: Option<String>,
}

/// 按 `SELECTED_BG_TASK_ID` 解析 shell：先 `BG_TASKS`，再 `BG_DISPLAY` / `BG_LIVE_DETAIL`。
pub(crate) fn resolve_shell_detail_context(
    tasks: &[BgTaskEntry],
    display: &[BgDisplayEntry],
    live: &std::collections::HashMap<String, BgLiveDetail>,
    task_id: Option<&str>,
) -> Option<ShellDetailContext> {
    let id = task_id?;
    if let Some(t) = tasks.iter().find(|t| t.task_id == id && t.kind == "shell") {
        return Some(ShellDetailContext {
            task_id: t.task_id.clone(),
            summary: t.summary.clone(),
            pid: t.pid,
            started_at: Some(t.started_at.clone()),
        });
    }
    if let Some(d) = display
        .iter()
        .find(|e| e.id == id && e.agent_type == "shell")
    {
        let live_row = live.get(id);
        return Some(ShellDetailContext {
            task_id: id.to_string(),
            summary: d.desc.clone(),
            pid: live_row.and_then(|l| l.pid),
            started_at: None,
        });
    }
    if let Some(l) = live.get(id).filter(|l| l.kind == "shell") {
        return Some(ShellDetailContext {
            task_id: id.to_string(),
            summary: l.summary.clone(),
            pid: l.pid,
            started_at: None,
        });
    }
    None
}

/// 按 `SELECTED_BG_TASK_ID` 在 `BG_TASKS` 中查找 shell 任务（仅活跃列表）。
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn resolve_shell_task<'a>(
    tasks: &'a [BgTaskEntry],
    task_id: Option<&str>,
) -> Option<&'a BgTaskEntry> {
    let id = task_id?;
    tasks.iter().find(|t| t.task_id == id && t.kind == "shell")
}

fn close_panel() {
    crate::kit::panel_registry::close_active_panel();
}

#[cfg(test)]
#[path = "shell_detail_test.rs"]
mod tests;
