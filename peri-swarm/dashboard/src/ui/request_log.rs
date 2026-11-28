//! Request-log page: window toolbar, access-log table and server-side pager.

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};

use crate::app::App;
use crate::metrics::RequestLogRow;
use crate::ui::hit::HitMap;
use crate::ui::metrics;
use crate::ui::text;
use crate::ui::widgets::{self, Rows, StatusTone};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Column {
    Time,
    Method,
    Route,
    Status,
    Duration,
    Session,
}

impl Column {
    fn header(self) -> &'static str {
        match self {
            Column::Time => text::REQUEST_COL_TIME,
            Column::Method => text::REQUEST_COL_METHOD,
            Column::Route => text::REQUEST_COL_ROUTE,
            Column::Status => text::REQUEST_COL_STATUS,
            Column::Duration => text::REQUEST_COL_DURATION,
            Column::Session => text::REQUEST_COL_SESSION,
        }
    }
}

/// Render the request-log body.
pub fn render(buf: &mut Buffer, rows: &mut Rows, app: &App, hits: &mut HitMap) -> Option<Position> {
    let state = app.request_log();
    widgets::render_body(
        buf,
        rows.line(),
        text::REQUEST_LOG_TITLE,
        widgets::title_style(),
    );
    rows.skip(1);
    metrics::render_toolbar(buf, rows, app, hits);

    if state.not_configured() {
        metrics::render_not_configured(buf, rows, hits);
        return None;
    }
    if let Some(error) = state.error() {
        metrics::render_wrapped(buf, rows, error, widgets::error_style());
    }
    if state.stale() {
        metrics::render_wrapped(buf, rows, text::STALE_DATA, widgets::warning_style());
    }
    if !state.ever_loaded() {
        if state.loading() {
            widgets::render_loading(
                buf,
                rows.line(),
                app.spinner_frame(),
                text::LOADING_REQUEST_LOG,
                widgets::dim_style(),
            );
        } else if state.error().is_none() {
            render_empty(buf, rows);
        }
        return None;
    }
    if state.pending_page() || (state.rows().is_empty() && state.loading()) {
        widgets::render_loading(
            buf,
            rows.line(),
            app.spinner_frame(),
            text::LOADING_REQUEST_LOG,
            widgets::dim_style(),
        );
        return None;
    }
    if state.rows().is_empty() {
        render_empty(buf, rows);
        if state.page() > 0 {
            rows.skip(1);
            metrics::render_pager(
                buf,
                rows,
                hits,
                state.page(),
                None,
                state.has_previous(),
                state.can_next(),
            );
        }
        return None;
    }

    render_table(buf, rows, app);
    rows.skip(1);
    metrics::render_pager(
        buf,
        rows,
        hits,
        state.page(),
        None,
        state.has_previous(),
        state.can_next(),
    );
    None
}

fn render_empty(buf: &mut Buffer, rows: &mut Rows) {
    metrics::render_wrapped(buf, rows, text::EMPTY_REQUEST_LOG, widgets::title_style());
    metrics::render_wrapped(
        buf,
        rows,
        text::EMPTY_REQUEST_LOG_HINT,
        widgets::muted_style(),
    );
}

fn render_table(buf: &mut Buffer, rows: &mut Rows, app: &App) {
    let width = rows.width();
    let breakpoint = app.viewport_width().max(width);
    let columns = columns(breakpoint);
    let widths = column_widths(&columns, width);
    if columns.is_empty() || widths.iter().all(|width| *width == 0) {
        return;
    }
    render_header(buf, rows.line(), &columns, &widths);
    for row in app.request_log().rows() {
        let area = rows.line();
        if area.is_empty() {
            break;
        }
        render_row(buf, area, row, &columns, &widths);
    }
}

fn columns(width: u16) -> Vec<Column> {
    let mut columns = vec![Column::Time, Column::Method, Column::Route, Column::Status];
    if width >= 72 {
        columns.push(Column::Duration);
    }
    if width >= 98 {
        columns.push(Column::Session);
    }
    columns
}

/// Fixed widths except the route column, which receives the remainder.
fn column_widths(columns: &[Column], width: u16) -> Vec<u16> {
    if columns.is_empty() || width == 0 {
        return Vec::new();
    }
    let fixed: u16 = columns
        .iter()
        .map(|column| match column {
            Column::Time => 14,
            Column::Method => 7,
            Column::Status => 6,
            Column::Duration => 9,
            Column::Session => 12,
            Column::Route => 0,
        })
        .fold(0, u16::saturating_add);
    let separators = u16::try_from(columns.len().saturating_sub(1)).unwrap_or(u16::MAX);
    let remaining = width.saturating_sub(fixed).saturating_sub(separators);
    let flex = columns
        .iter()
        .filter(|column| **column == Column::Route)
        .count();
    let flex_width = if flex == 0 {
        0
    } else {
        remaining / u16::try_from(flex).unwrap_or(1)
    };
    columns
        .iter()
        .map(|column| match column {
            Column::Route => flex_width,
            Column::Time => 14,
            Column::Method => 7,
            Column::Status => 6,
            Column::Duration => 9,
            Column::Session => 12,
        })
        .collect()
}

fn render_header(buf: &mut Buffer, area: Rect, columns: &[Column], widths: &[u16]) {
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
            widgets::fit(column.header(), cell.width),
            widgets::section_style(),
        );
        x = cell.right().saturating_add(1);
    }
}

fn render_row(
    buf: &mut Buffer,
    area: Rect,
    row: &RequestLogRow,
    columns: &[Column],
    widths: &[u16],
) {
    let mut x = area.x;
    for (column, width) in columns.iter().zip(widths) {
        let cell = Rect::new(x, area.y, *width, 1);
        if cell.is_empty() {
            break;
        }
        let (value, style) = match column {
            Column::Time => (text::request_time(row.timestamp_us), widgets::dim_style()),
            Column::Method => (
                text::request_optional(row.method.as_deref()),
                widgets::muted_style(),
            ),
            Column::Route => (
                text::request_optional(row.route.as_deref()),
                widgets::muted_style(),
            ),
            Column::Status => (
                text::request_status(row.status),
                widgets::tone_style(status_tone(row.status)),
            ),
            Column::Duration => (
                text::request_duration(row.duration_ms),
                widgets::muted_style(),
            ),
            Column::Session => (
                text::request_optional(row.session_id.as_deref()),
                widgets::dim_style(),
            ),
        };
        buf.set_string(cell.x, cell.y, widgets::fit(&value, cell.width), style);
        x = cell.right().saturating_add(1);
    }
}

fn status_tone(status: Option<u16>) -> StatusTone {
    match status {
        Some(200..=299) => StatusTone::Ok,
        Some(400..=499) => StatusTone::Warning,
        Some(500..=599) => StatusTone::Error,
        _ => StatusTone::Muted,
    }
}

#[cfg(test)]
#[path = "request_log_test.rs"]
mod tests;
