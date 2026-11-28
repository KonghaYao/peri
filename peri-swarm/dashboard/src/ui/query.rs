//! Query page: SQL editor, window chips, Run and a read-only result table.
//!
//! Queries run only on an explicit Run (Ctrl-Enter) or a pager click; there is
//! no polling. The editor is the shared multiline text area, so clicks place the
//! caret and Enter inserts a newline. Result columns come from the response when
//! the server reports them and fall back to the JSON keys of the rows; columns
//! that do not fit are cropped with a notice instead of overflowing.

use peri_tui_components::button::{ButtonState, ButtonTone};
use peri_tui_components::textarea::{render_text_area_with_cursor, TextAreaStyles};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::text::Span;

use crate::app::{ActionId, App, FieldId, Focus, Intent, TextAreaClick};
use crate::metrics::TimeWindow;
use crate::ui::hit::{HitMap, Layer};
use crate::ui::widgets::{self, Rows};
use crate::ui::{metrics, text};

/// Inner rows of the SQL editor.
const EDITOR_ROWS: u16 = 5;
/// Narrowest result column worth drawing.
const MIN_COLUMN_WIDTH: u16 = 12;
/// Width one result column aims for; the visible count follows from it.
const PREFERRED_COLUMN_WIDTH: u16 = 20;

/// Render the Query page body.
pub fn render(buf: &mut Buffer, rows: &mut Rows, app: &App, hits: &mut HitMap) -> Option<Position> {
    let state = app.query();
    widgets::render_body(buf, rows.line(), text::QUERY_TITLE, widgets::title_style());
    rows.skip(1);
    render_toolbar(buf, rows, app, hits);
    rows.skip(1);
    let cursor = render_editor(buf, rows, app, hits);
    render_wrapped(buf, rows, text::QUERY_HINT, widgets::dim_style());

    if let Some(error) = state.validation_error() {
        render_wrapped(buf, rows, error, widgets::error_style());
    }
    if state.not_configured() {
        metrics::render_not_configured(buf, rows, hits);
        return cursor;
    }
    if let Some(error) = state.error() {
        render_wrapped(buf, rows, error, widgets::error_style());
    }
    if state.loading() {
        widgets::render_loading(
            buf,
            rows.line(),
            app.spinner_frame(),
            text::QUERY_RUNNING,
            widgets::dim_style(),
        );
        return cursor;
    }
    if !state.ever_loaded() {
        return cursor;
    }
    if state.rows().is_empty() {
        render_empty(buf, rows);
        if state.applied_page() > 0 {
            rows.skip(1);
            render_pager(buf, rows, app, hits);
        }
        return cursor;
    }

    rows.skip(1);
    render_table(buf, rows, app);
    rows.skip(1);
    render_pager(buf, rows, app, hits);
    cursor
}

/// Window chips and the Run button.
fn render_toolbar(buf: &mut Buffer, rows: &mut Rows, app: &App, hits: &mut HitMap) {
    let row = rows.line();
    if row.is_empty() {
        return;
    }
    let state = app.query();
    let mut x = row.x;
    for window in TimeWindow::ALL {
        let selected = window == state.window();
        let tone = if selected {
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

    let action = ActionId::RunQuery;
    let label = text::action_label(action);
    let rect = widgets::button_rect(row, x.saturating_add(2), label, ButtonTone::Primary);
    if rect.width == 0 {
        return;
    }
    let button = if state.loading() {
        ButtonState::Disabled
    } else if app.focus() == Focus::Action(action) {
        ButtonState::Focused
    } else {
        ButtonState::Normal
    };
    widgets::render_button_state(buf, rect, label, button, ButtonTone::Primary);
    if !state.loading() {
        hits.region(Layer::Base, rect, Intent::Activate(action));
    }
}

/// SQL editor; returns the caret position in content coordinates.
fn render_editor(
    buf: &mut Buffer,
    rows: &mut Rows,
    app: &App,
    hits: &mut HitMap,
) -> Option<Position> {
    widgets::render_body(
        buf,
        rows.line(),
        text::QUERY_SQL_LABEL,
        widgets::section_style(),
    );
    let state = app.query();
    let rect = rows.take(EDITOR_ROWS.saturating_add(2));
    let focused = app.focus() == Focus::Field(FieldId::QuerySql);
    let styles = TextAreaStyles {
        cursor: widgets::selected_style(),
        selection: widgets::selected_style(),
        placeholder: widgets::dim_style(),
        base: widgets::muted_style(),
    };
    let cursor = render_text_area_with_cursor(
        buf,
        rect,
        state.sql(),
        focused,
        widgets::border_style(focused),
        styles,
        Some(text::QUERY_SQL_PLACEHOLDER),
    );
    hits.region(
        Layer::Base,
        rect,
        Intent::FocusTextAreaAt {
            field: FieldId::QuerySql,
            click: TextAreaClick {
                row: 0,
                column: 0,
                width: rect.width.saturating_sub(2),
                height: rect.height.saturating_sub(2),
            },
        },
    );
    cursor
}

fn render_empty(buf: &mut Buffer, rows: &mut Rows) {
    render_wrapped(buf, rows, text::QUERY_EMPTY, widgets::title_style());
    render_wrapped(buf, rows, text::QUERY_EMPTY_HINT, widgets::muted_style());
}

fn render_table(buf: &mut Buffer, rows: &mut Rows, app: &App) {
    let state = app.query();
    let all = state.columns();
    let width = rows.width();
    let (visible, widths) = column_layout(all.len(), width);
    if visible == 0 {
        return;
    }
    let area = rows.line();
    if area.is_empty() {
        return;
    }
    render_header(buf, area, &all[..visible], &widths);
    for hit in state.rows() {
        let area = rows.line();
        if area.is_empty() {
            break;
        }
        render_row(buf, area, hit, &all[..visible], &widths);
    }
    if visible < all.len() {
        rows.skip(1);
        render_wrapped(
            buf,
            rows,
            &text::query_columns_truncated(visible, all.len()),
            widgets::muted_style(),
        );
    }
}

/// Visible column count and widths.
///
/// A column aims for [`PREFERRED_COLUMN_WIDTH`] cells; narrower terminals show
/// fewer columns rather than crushing every cell. The remainder is handed to the
/// last visible column so the table always uses the available row.
fn column_layout(count: usize, width: u16) -> (usize, Vec<u16>) {
    if count == 0 || width < MIN_COLUMN_WIDTH {
        return (0, Vec::new());
    }
    let visible = (usize::from(width) / usize::from(PREFERRED_COLUMN_WIDTH))
        .max(1)
        .min(count);
    let visible_u16 = u16::try_from(visible).unwrap_or(u16::MAX);
    let total = width.saturating_sub(visible_u16.saturating_sub(1));
    let base = total / visible_u16;
    let remainder = total % visible_u16;
    let mut widths: Vec<u16> = (0..visible).map(|_| base).collect();
    if let Some(last) = widths.last_mut() {
        *last = last.saturating_add(remainder);
    }
    (visible, widths)
}

fn render_header(buf: &mut Buffer, area: Rect, columns: &[String], widths: &[u16]) {
    let mut x = area.x;
    for (column, width) in columns.iter().zip(widths) {
        let cell = Rect::new(x, area.y, *width, 1);
        if cell.is_empty() {
            break;
        }
        buf.set_string(
            cell.x,
            cell.y,
            widgets::fit(column, cell.width),
            widgets::section_style(),
        );
        x = cell.right().saturating_add(1);
    }
}

fn render_row(
    buf: &mut Buffer,
    area: Rect,
    row: &serde_json::Value,
    columns: &[String],
    widths: &[u16],
) {
    let object = row.as_object();
    let mut x = area.x;
    for (column, width) in columns.iter().zip(widths) {
        let cell = Rect::new(x, area.y, *width, 1);
        if cell.is_empty() {
            break;
        }
        let value = object.and_then(|object| object.get(column));
        let rendered = text::query_cell(value);
        buf.set_string(
            cell.x,
            cell.y,
            widgets::fit(&rendered, cell.width),
            widgets::muted_style(),
        );
        x = cell.right().saturating_add(1);
    }
}

fn render_pager(buf: &mut Buffer, rows: &mut Rows, app: &App, hits: &mut HitMap) {
    let row = rows.line();
    if row.is_empty() {
        return;
    }
    let state = app.query();
    let mut x = row.x;
    let previous = text::PAGE_PREV;
    let rect = widgets::button_rect(row, x, previous, ButtonTone::Secondary);
    if rect.width > 0 {
        let enabled = state.has_previous() && !state.loading();
        widgets::render_button_state(
            buf,
            rect,
            previous,
            if enabled {
                ButtonState::Normal
            } else {
                ButtonState::Disabled
            },
            ButtonTone::Secondary,
        );
        if enabled {
            hits.region(Layer::Base, rect, Intent::QueryPagePrev);
        }
        x = rect.right().saturating_add(2);
    }
    let indicator = text::request_page_indicator(state.applied_page());
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
    if rect.width == 0 {
        return;
    }
    let enabled = state.can_next();
    widgets::render_button_state(
        buf,
        rect,
        next,
        if enabled {
            ButtonState::Normal
        } else {
            ButtonState::Disabled
        },
        ButtonTone::Secondary,
    );
    if enabled {
        hits.region(Layer::Base, rect, Intent::QueryPageNext);
    }
}

fn render_wrapped(buf: &mut Buffer, rows: &mut Rows, body: &str, style: ratatui::style::Style) {
    let height = widgets::wrapped_height(body, rows.width());
    widgets::render_body(buf, rows.take(height), body, style);
}

#[cfg(test)]
#[path = "query_test.rs"]
mod tests;
