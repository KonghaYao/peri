//! Session log page: toolbar, status chips, search, the adaptive table and the
//! page indicator.
//!
//! Counts, filters and search all operate on the loaded set; the table shows one
//! display page of it and the pager row pages through the loaded rows. The
//! footer scope note names the scope so the page never pretends to show
//! gateway-wide totals.

use chrono::Utc;
use peri_tui_components::button::ButtonTone;
use peri_tui_components::pagination::{
    pagination_layout, render_pagination, PageAction, PaginationLabels, PaginationState,
};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::text::Span;

use crate::app::{ActionId, App, Focus, Intent, MAX_SESSIONS};
use crate::ui::hit::{HitMap, Layer};
use crate::ui::text;
use crate::ui::widgets::{self, Rows};
use crate::view_model::{self, Column, StatusBucket};

/// Render the session log body.
pub fn render(buf: &mut Buffer, rows: &mut Rows, app: &App, hits: &mut HitMap) -> Option<Position> {
    let state = app.session_log();
    widgets::render_body(
        buf,
        rows.line(),
        text::SESSION_LOG_TITLE,
        widgets::title_style(),
    );
    rows.skip(1);

    render_toolbar(buf, rows, app, hits);
    render_chips(buf, rows, app, hits);
    let cursor = render_search(buf, rows, app, hits);

    if let Some(error) = state.error() {
        render_wrapped(buf, rows, error, widgets::error_style());
    }
    if !state.ever_loaded() {
        if state.loading() {
            if app.is_paused() {
                render_wrapped(buf, rows, text::POLLING_PAUSED, widgets::warning_style());
            } else {
                widgets::render_loading(
                    buf,
                    rows.line(),
                    app.spinner_frame(),
                    text::LOADING_SESSIONS,
                    widgets::dim_style(),
                );
            }
        }
        return cursor;
    }

    let visible = state.page_indices();
    if state.sessions().is_empty() {
        render_wrapped(
            buf,
            rows,
            text::EMPTY_SESSIONS_TITLE,
            widgets::title_style(),
        );
        render_wrapped(buf, rows, text::EMPTY_SESSIONS_BODY, widgets::muted_style());
    } else {
        render_table(buf, rows, app, hits, &visible);
        if visible.is_empty() {
            render_wrapped(buf, rows, text::NO_SESSIONS_MATCH, widgets::muted_style());
        }
    }
    rows.skip(1);
    render_pager(buf, rows, app, hits);
    cursor
}

fn render_toolbar(buf: &mut Buffer, rows: &mut Rows, app: &App, hits: &mut HitMap) {
    let row = rows.line();
    let mut x = row.x;
    for (action, tone) in [
        (ActionId::NewSession, ButtonTone::Primary),
        (ActionId::RefreshSessions, ButtonTone::Secondary),
    ] {
        let label = text::action_label(action);
        let rect = widgets::button_rect(row, x, label, tone);
        if rect.width == 0 {
            break;
        }
        widgets::render_button(buf, rect, label, app.focus() == Focus::Action(action), tone);
        hits.region(Layer::Base, rect, Intent::Activate(action));
        x = rect.right().saturating_add(2);
    }
}

fn render_chips(buf: &mut Buffer, rows: &mut Rows, app: &App, hits: &mut HitMap) {
    let state = app.session_log();
    let counts = state.counts();
    let mut row = rows.line();
    let mut x = row.x;
    for bucket in StatusBucket::ALL {
        let chip = text::chip_label(bucket, counts.get(bucket));
        let width = u16::try_from(Span::raw(chip.as_str()).width()).unwrap_or(u16::MAX);
        if x > row.x && x.saturating_add(width) > row.right() {
            row = rows.line();
            x = row.x;
        }
        let rect = Rect::new(x, row.y, width.min(row.right().saturating_sub(x)), 1);
        if rect.width == 0 {
            break;
        }
        let style = if state.filter() == bucket {
            widgets::selected_style()
        } else {
            widgets::tone_style(bucket.tone())
        };
        buf.set_string(rect.x, rect.y, widgets::fit(&chip, rect.width), style);
        hits.region(Layer::Base, rect, Intent::SetFilter(bucket));
        x = rect.right().saturating_add(2);
    }
}

fn render_search(
    buf: &mut Buffer,
    rows: &mut Rows,
    app: &App,
    hits: &mut HitMap,
) -> Option<Position> {
    let width = rows.width();
    let label = text::field_label(crate::app::FieldId::SessionSearch);
    let hint = format!("{label} · {}", text::SEARCH_SCOPE_HINT);
    let hint_height = widgets::wrapped_height(&hint, width);
    widgets::render_body(buf, rows.take(hint_height), &hint, widgets::section_style());
    let rect = rows.take(3);
    let focused = app.focus() == Focus::Field(crate::app::FieldId::SessionSearch);
    let cursor = widgets::render_field(
        buf,
        rect,
        app.session_log().search(),
        focused,
        false,
        text::SEARCH_PLACEHOLDER,
    );
    hits.region(
        Layer::Base,
        rect,
        Intent::FocusField {
            field: crate::app::FieldId::SessionSearch,
            click: None,
        },
    );
    cursor
}

fn render_table(
    buf: &mut Buffer,
    rows: &mut Rows,
    app: &App,
    hits: &mut HitMap,
    visible: &[usize],
) {
    let state = app.session_log();
    let pane_width = rows.width();
    let terminal_width = app.viewport_width().max(pane_width);
    let columns = view_model::columns(terminal_width, pane_width);
    let widths = view_model::column_widths(&columns, pane_width);
    if columns.is_empty() || widths.iter().all(|width| *width == 0) {
        return;
    }
    // Metadata is real data, so its absence from the table is never silent:
    // the detail page carries the full values.
    if !columns.contains(&Column::Metadata)
        && state
            .sessions()
            .iter()
            .any(|session| !session.metadata.is_empty())
    {
        render_wrapped(buf, rows, text::METADATA_HIDDEN, widgets::dim_style());
    }
    render_header(buf, rows.line(), &columns, &widths);
    let now = Utc::now().timestamp();
    let cursor_row = state.row_cursor();
    let content_focused = app.focus() == Focus::Content;
    let selected = state.selected().map(str::to_owned);

    for (row_index, session_index) in visible.iter().enumerate() {
        let area = rows.line();
        if area.is_empty() {
            continue;
        }
        let Some(session) = state.sessions().get(*session_index) else {
            continue;
        };
        let highlighted = selected.as_deref() == Some(session.id.as_str())
            || (content_focused && row_index == cursor_row);
        if highlighted {
            buf.set_style(area, widgets::selected_style());
        }
        render_row(buf, area, &columns, &widths, session, now);
        hits.region(Layer::Base, area, Intent::OpenSession(session.id.clone()));
    }
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
    columns: &[Column],
    widths: &[u16],
    session: &crate::api::Session,
    now_unix: i64,
) {
    let mut x = area.x;
    for (column, width) in columns.iter().zip(widths) {
        let cell = Rect::new(x, area.y, *width, 1);
        if cell.is_empty() {
            break;
        }
        render_cell(buf, cell, *column, session, now_unix);
        x = cell.right().saturating_add(1);
    }
}

fn render_cell(
    buf: &mut Buffer,
    cell: Rect,
    column: Column,
    session: &crate::api::Session,
    now_unix: i64,
) {
    match column {
        Column::Session => {
            let (id, model) = view_model::session_cell(session, cell.width);
            buf.set_string(cell.x, cell.y, &id, widgets::title_style());
            let used = u16::try_from(Span::raw(id.as_str()).width()).unwrap_or(u16::MAX);
            if !model.is_empty() && used < cell.width {
                let rest = Rect::new(
                    cell.x.saturating_add(used),
                    cell.y,
                    cell.width.saturating_sub(used),
                    1,
                );
                buf.set_string(rest.x, rest.y, &model, widgets::dim_style());
            }
        }
        Column::Status => {
            let bucket = view_model::bucket(session);
            buf.set_string(
                cell.x,
                cell.y,
                widgets::fit(bucket.label(), cell.width),
                widgets::tone_style(bucket.tone()),
            );
        }
        Column::Approvals => {
            let count = view_model::approvals_count(session);
            let style = if count == 0 {
                widgets::muted_style()
            } else {
                widgets::warning_style()
            };
            buf.set_string(
                cell.x,
                cell.y,
                widgets::fit(&text::approvals_summary(count), cell.width),
                style,
            );
        }
        Column::LastActive => {
            buf.set_string(
                cell.x,
                cell.y,
                widgets::fit(
                    &text::relative_time(session.last_active_at, now_unix),
                    cell.width,
                ),
                widgets::muted_style(),
            );
        }
        Column::Metadata => {
            buf.set_string(
                cell.x,
                cell.y,
                widgets::fit(&view_model::metadata_summary(session), cell.width),
                widgets::dim_style(),
            );
        }
    }
}

/// Draw the page indicator and register one hit region per enabled control.
fn render_pager(buf: &mut Buffer, rows: &mut Rows, app: &App, hits: &mut HitMap) {
    let state = app.session_log();
    if !state.ever_loaded() || state.visible_len() == 0 {
        return;
    }
    let row = rows.line();
    if row.is_empty() {
        return;
    }
    let state_view = PaginationState {
        page: state.page(),
        page_count: state.page_count(),
        has_more: state.has_more() && state.limit() < MAX_SESSIONS,
    };
    let layout = pagination_layout(
        row,
        state_view,
        PaginationLabels {
            previous: text::PAGE_PREV,
            next: text::PAGE_NEXT,
        },
    );
    render_pagination(buf, &layout, widgets::theme());
    for control in &layout.controls {
        if !control.enabled {
            continue;
        }
        let intent = match control.action {
            PageAction::Previous => Intent::SessionPagePrev,
            PageAction::Next => Intent::SessionPageNext,
            PageAction::Page(page) => Intent::GoToSessionPage(page),
            PageAction::Gap => continue,
        };
        hits.region(Layer::Base, control.rect, intent);
    }
}

fn render_wrapped(buf: &mut Buffer, rows: &mut Rows, body: &str, style: ratatui::style::Style) {
    let height = widgets::wrapped_height(body, rows.width());
    widgets::render_body(buf, rows.take(height), body, style);
}

#[cfg(test)]
#[path = "session_log_test.rs"]
mod tests;
