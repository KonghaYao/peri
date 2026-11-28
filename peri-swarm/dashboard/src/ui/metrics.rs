//! Core-metrics and Agent-metrics pages.
//!
//! Both pages are mouse-first: the window chips, Refresh, and the pager buttons
//! are click regions. Keyboard equivalents are `1`/`2`/`3`, `r`, and
//! PgUp/PgDn. Every number comes from the worker; no placeholder values are
//! substituted for missing data.

use chrono::Utc;
use peri_tui_components::button::ButtonTone;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::text::Span;

use crate::api::{AgentMetricsSession, AgentMetricsTotals};
use crate::app::{ActionId, App, Focus, Intent};
use crate::metrics::TimeWindow;
use crate::ui::hit::{HitMap, Layer};
use crate::ui::text;
use crate::ui::widgets::{self, Rows};

/// Render the Core-metrics page.
pub fn render_core(
    buf: &mut Buffer,
    rows: &mut Rows,
    app: &App,
    hits: &mut HitMap,
) -> Option<Position> {
    let state = app.core_metrics();
    widgets::render_body(
        buf,
        rows.line(),
        text::CORE_METRICS_TITLE,
        widgets::title_style(),
    );
    rows.skip(1);
    render_toolbar(buf, rows, app, hits);

    if state.not_configured() {
        render_not_configured(buf, rows, hits);
        return None;
    }
    if let Some(error) = state.error() {
        render_wrapped(buf, rows, error, widgets::error_style());
    }
    if state.stale() {
        render_wrapped(buf, rows, text::STALE_DATA, widgets::warning_style());
    }
    if !state.ever_loaded() {
        if state.loading() {
            widgets::render_loading(
                buf,
                rows.line(),
                app.spinner_frame(),
                text::LOADING_CORE_METRICS,
                widgets::dim_style(),
            );
        } else if state.error().is_none() {
            render_core_empty(buf, rows);
        }
        return None;
    }
    let Some(snapshot) = state.snapshot() else {
        render_core_empty(buf, rows);
        return None;
    };
    if snapshot.is_empty() {
        render_core_empty(buf, rows);
        return None;
    }

    for (label, value) in [
        (
            text::LABEL_REQUESTS_PER_MINUTE,
            text::requests_per_minute_value(snapshot.requests_per_minute),
        ),
        (
            text::LABEL_ERROR_RATE,
            text::error_rate_value(snapshot.error_rate),
        ),
        (
            text::LABEL_P95_LATENCY,
            text::p95_value(snapshot.p95_seconds),
        ),
        (
            text::LABEL_TOTAL_REQUESTS,
            text::total_value(snapshot.total_requests),
        ),
    ] {
        let area = rows.line();
        widgets::render_label_value(buf, area, label, &value, widgets::title_style());
    }
    rows.skip(1);
    let range = text::data_range(snapshot.covered_start_us, snapshot.covered_end_us);
    widgets::render_label_value(
        buf,
        rows.line(),
        text::LABEL_DATA_RANGE,
        &range,
        widgets::muted_style(),
    );
    None
}

/// Render the Agent-metrics page.
pub fn render_agent(
    buf: &mut Buffer,
    rows: &mut Rows,
    app: &App,
    hits: &mut HitMap,
) -> Option<Position> {
    let state = app.agent_metrics();
    widgets::render_body(
        buf,
        rows.line(),
        text::AGENT_METRICS_TITLE,
        widgets::title_style(),
    );
    rows.skip(1);
    render_toolbar(buf, rows, app, hits);

    if let Some(error) = state.error() {
        render_wrapped(buf, rows, error, widgets::error_style());
    }
    if state.stale() {
        render_wrapped(buf, rows, text::STALE_DATA, widgets::warning_style());
    }
    if !state.ever_loaded() {
        if state.loading() {
            widgets::render_loading(
                buf,
                rows.line(),
                app.spinner_frame(),
                text::LOADING_AGENT_METRICS,
                widgets::dim_style(),
            );
        } else if state.error().is_none() {
            render_agent_empty(buf, rows);
        }
        return None;
    }
    if state.loaded() == 0 {
        render_agent_empty(buf, rows);
        return None;
    }
    if let Some(totals) = state.totals() {
        render_totals(buf, rows, totals, state.loaded());
    }
    rows.skip(1);
    render_agent_table(buf, rows, app);
    if state.truncated() {
        rows.skip(1);
        render_wrapped(
            buf,
            rows,
            text::AGENT_METRICS_TRUNCATED,
            widgets::warning_style(),
        );
    }
    rows.skip(1);
    render_pager(
        buf,
        rows,
        hits,
        state.page(),
        Some(state.page_count()),
        state.page() > 0,
        state.page() + 1 < state.page_count(),
    );
    None
}

pub(crate) fn render_toolbar(buf: &mut Buffer, rows: &mut Rows, app: &App, hits: &mut HitMap) {
    let row = rows.line();
    if row.is_empty() {
        return;
    }
    let selected = match app.page() {
        crate::app::Page::CoreMetrics => app.core_metrics().window(),
        crate::app::Page::AgentMetrics => app.agent_metrics().window(),
        crate::app::Page::RequestLog => app.request_log().window(),
        _ => TimeWindow::FifteenMinutes,
    };
    let mut x = row.x;
    for window in TimeWindow::ALL {
        let tone = if window == selected {
            ButtonTone::Primary
        } else {
            ButtonTone::Secondary
        };
        let label = text::window_label(window);
        let rect = widgets::button_rect(row, x, label, tone);
        if rect.width == 0 {
            break;
        }
        widgets::render_button(buf, rect, label, false, tone);
        hits.region(Layer::Base, rect, Intent::SetMetricsWindow(window));
        x = rect.right().saturating_add(1);
    }
    let action = refresh_action(app.page());
    let label = text::action_label(action);
    let rect = widgets::button_rect(row, x.saturating_add(1), label, ButtonTone::Secondary);
    if rect.width > 0 {
        widgets::render_button(
            buf,
            rect,
            label,
            app.focus() == Focus::Action(action),
            ButtonTone::Secondary,
        );
        hits.region(Layer::Base, rect, Intent::Activate(action));
    }
}

fn refresh_action(page: crate::app::Page) -> ActionId {
    match page {
        crate::app::Page::AgentMetrics => ActionId::RefreshAgentMetrics,
        crate::app::Page::RequestLog => ActionId::RefreshRequestLog,
        _ => ActionId::RefreshCoreMetrics,
    }
}

pub(crate) fn render_not_configured(buf: &mut Buffer, rows: &mut Rows, hits: &mut HitMap) {
    rows.skip(1);
    render_wrapped(
        buf,
        rows,
        text::OBSERVABILITY_NOT_CONFIGURED_MESSAGE,
        widgets::title_style(),
    );
    render_wrapped(
        buf,
        rows,
        text::OBSERVABILITY_NOT_CONFIGURED_HINT,
        widgets::muted_style(),
    );
    rows.skip(1);
    render_settings_button(buf, rows, hits);
}

fn render_settings_button(buf: &mut Buffer, rows: &mut Rows, hits: &mut HitMap) {
    let row = rows.line();
    if row.is_empty() {
        return;
    }
    let action = ActionId::OpenSettings;
    let label = text::action_label(action);
    let rect = widgets::button_rect(row, row.x, label, ButtonTone::Secondary);
    if rect.width == 0 {
        return;
    }
    widgets::render_button(buf, rect, label, false, ButtonTone::Secondary);
    hits.region(Layer::Base, rect, Intent::Activate(action));
}

fn render_core_empty(buf: &mut Buffer, rows: &mut Rows) {
    render_wrapped(buf, rows, text::EMPTY_CORE_METRICS, widgets::title_style());
    render_wrapped(
        buf,
        rows,
        text::EMPTY_CORE_METRICS_HINT,
        widgets::muted_style(),
    );
}

fn render_agent_empty(buf: &mut Buffer, rows: &mut Rows) {
    render_wrapped(buf, rows, text::EMPTY_AGENT_METRICS, widgets::title_style());
    render_wrapped(
        buf,
        rows,
        text::EMPTY_AGENT_METRICS_HINT,
        widgets::muted_style(),
    );
}

fn render_totals(buf: &mut Buffer, rows: &mut Rows, totals: &AgentMetricsTotals, sessions: usize) {
    let line = text::agent_totals_line(totals, sessions);
    render_wrapped(buf, rows, &line, widgets::section_style());
}

/// Columns that fit the available width, in gateway order.
fn agent_columns(width: u16) -> Vec<(&'static str, u16)> {
    let mut columns: Vec<(&'static str, u16)> = vec![(text::AGENT_COL_SESSION, 0)];
    if width >= 108 {
        columns.push((text::AGENT_COL_MODEL, 14));
        columns.push((text::AGENT_COL_INPUT, 9));
        columns.push((text::AGENT_COL_OUTPUT, 9));
        columns.push((text::AGENT_COL_CACHE, 9));
    } else if width >= 92 {
        columns.push((text::AGENT_COL_INPUT, 9));
        columns.push((text::AGENT_COL_OUTPUT, 9));
        columns.push((text::AGENT_COL_CACHE, 9));
    }
    columns.push((text::AGENT_COL_TOTAL, 9));
    if width >= 64 {
        columns.push((text::AGENT_COL_TURNS, 6));
    }
    if width >= 76 {
        columns.push((text::AGENT_COL_LAST_ACTIVE, 13));
    }
    columns
}

fn render_agent_table(buf: &mut Buffer, rows: &mut Rows, app: &App) {
    let state = app.agent_metrics();
    let width = rows.width();
    // Breakpoints follow the whole terminal, like the session-log columns, so a
    // wide terminal keeps every column even though the sidebar narrows the pane.
    let breakpoint = app.viewport_width().max(width);
    let columns = agent_columns(breakpoint);
    let widths = column_widths(&columns, width);
    if widths.is_empty() || widths.iter().all(|width| *width == 0) {
        return;
    }
    render_header(buf, rows.line(), &columns, &widths);
    let now = Utc::now().timestamp();
    for session in state.page_rows() {
        let area = rows.line();
        if area.is_empty() {
            break;
        }
        render_agent_row(buf, area, session, &columns, &widths, now);
    }
}

fn render_agent_row(
    buf: &mut Buffer,
    area: Rect,
    session: &AgentMetricsSession,
    columns: &[(&'static str, u16)],
    widths: &[u16],
    now_unix: i64,
) {
    let mut x = area.x;
    for ((header, _), width) in columns.iter().zip(widths) {
        let cell = Rect::new(x, area.y, *width, 1);
        if cell.is_empty() {
            break;
        }
        match *header {
            text::AGENT_COL_SESSION => {
                buf.set_string(
                    cell.x,
                    cell.y,
                    widgets::fit(&session.session_id, cell.width),
                    widgets::title_style(),
                );
            }
            text::AGENT_COL_MODEL => {
                buf.set_string(
                    cell.x,
                    cell.y,
                    widgets::fit(&text::model_value(session.model.as_deref()), cell.width),
                    widgets::muted_style(),
                );
            }
            text::AGENT_COL_INPUT => {
                buf.set_string(
                    cell.x,
                    cell.y,
                    widgets::fit(&text::token_value(session.input_tokens), cell.width),
                    widgets::muted_style(),
                );
            }
            text::AGENT_COL_OUTPUT => {
                buf.set_string(
                    cell.x,
                    cell.y,
                    widgets::fit(&text::token_value(session.output_tokens), cell.width),
                    widgets::muted_style(),
                );
            }
            text::AGENT_COL_CACHE => {
                buf.set_string(
                    cell.x,
                    cell.y,
                    widgets::fit(
                        &text::cache_value(
                            session.cache_read_tokens,
                            session.cache_creation_tokens,
                        ),
                        cell.width,
                    ),
                    widgets::muted_style(),
                );
            }
            text::AGENT_COL_TOTAL => {
                buf.set_string(
                    cell.x,
                    cell.y,
                    widgets::fit(&text::token_value(session.total_tokens), cell.width),
                    widgets::title_style(),
                );
            }
            text::AGENT_COL_TURNS => {
                buf.set_string(
                    cell.x,
                    cell.y,
                    widgets::fit(&session.turns.to_string(), cell.width),
                    widgets::muted_style(),
                );
            }
            text::AGENT_COL_LAST_ACTIVE => {
                buf.set_string(
                    cell.x,
                    cell.y,
                    widgets::fit(
                        &text::agent_last_active(&session.last_active_at, now_unix),
                        cell.width,
                    ),
                    widgets::muted_style(),
                );
            }
            _ => {}
        }
        x = cell.right().saturating_add(1);
    }
}

/// Assign fixed widths, then give the remaining space to the first column.
fn column_widths(columns: &[(&'static str, u16)], width: u16) -> Vec<u16> {
    if columns.is_empty() || width == 0 {
        return Vec::new();
    }
    let separators = u16::try_from(columns.len().saturating_sub(1)).unwrap_or(u16::MAX);
    let fixed: u16 = columns
        .iter()
        .skip(1)
        .map(|(_, width)| *width)
        .fold(0, u16::saturating_add);
    let remaining = width.saturating_sub(fixed).saturating_sub(separators);
    columns
        .iter()
        .enumerate()
        .map(
            |(index, (_, column_width))| {
                if index == 0 {
                    remaining
                } else {
                    *column_width
                }
            },
        )
        .collect()
}

fn render_header(buf: &mut Buffer, area: Rect, columns: &[(&'static str, u16)], widths: &[u16]) {
    if area.is_empty() {
        return;
    }
    let mut x = area.x;
    for (column, width) in columns.iter().zip(widths) {
        let cell = Rect::new(x, area.y, *width, 1);
        if cell.is_empty() {
            break;
        }
        buf.set_string(
            cell.x,
            cell.y,
            widgets::fit(column.0, cell.width),
            widgets::section_style(),
        );
        x = cell.right().saturating_add(1);
    }
}

/// Pager for client-side (Agent metrics) and server-side (Request log) pages.
pub(crate) fn render_pager(
    buf: &mut Buffer,
    rows: &mut Rows,
    hits: &mut HitMap,
    page: usize,
    page_count: Option<usize>,
    has_previous: bool,
    has_next: bool,
) {
    let row = rows.line();
    if row.is_empty() {
        return;
    }
    let mut x = row.x;
    let previous = text::PAGE_PREV;
    let rect = widgets::button_rect(row, x, previous, ButtonTone::Secondary);
    if rect.width > 0 {
        widgets::render_button_state(
            buf,
            rect,
            previous,
            if has_previous {
                peri_tui_components::button::ButtonState::Normal
            } else {
                peri_tui_components::button::ButtonState::Disabled
            },
            ButtonTone::Secondary,
        );
        if has_previous {
            hits.region(Layer::Base, rect, Intent::MetricsPagePrev);
        }
        x = rect.right().saturating_add(2);
    }
    let indicator = match page_count {
        Some(count) => text::page_indicator(page, count),
        None => text::request_page_indicator(page),
    };
    let width = u16::try_from(Span::raw(indicator.as_str()).width()).unwrap_or(u16::MAX);
    let available = row.right().saturating_sub(x);
    if available > 0 {
        buf.set_string(
            x,
            row.y,
            widgets::fit(&indicator, available.min(width)),
            widgets::muted_style(),
        );
        x = x.saturating_add(width).saturating_add(2);
    }
    let next = text::PAGE_NEXT;
    let rect = widgets::button_rect(row, x, next, ButtonTone::Secondary);
    if rect.width > 0 {
        widgets::render_button_state(
            buf,
            rect,
            next,
            if has_next {
                peri_tui_components::button::ButtonState::Normal
            } else {
                peri_tui_components::button::ButtonState::Disabled
            },
            ButtonTone::Secondary,
        );
        if has_next {
            hits.region(Layer::Base, rect, Intent::MetricsPageNext);
        }
    }
}

pub(crate) fn render_wrapped(
    buf: &mut Buffer,
    rows: &mut Rows,
    body: &str,
    style: ratatui::style::Style,
) {
    let height = widgets::wrapped_height(body, rows.width());
    widgets::render_body(buf, rows.take(height), body, style);
}

#[cfg(test)]
#[path = "metrics_test.rs"]
mod tests;
