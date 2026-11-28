//! Page-specific empty states for the pages this build does not implement.

use ratatui::buffer::Buffer;

use crate::app::Page;
use crate::ui::text;
use crate::ui::widgets::{self, Rows};

/// Render the empty state for `page`.
pub fn render(page: Page, buf: &mut Buffer, rows: &mut Rows) {
    let width = rows.width();
    let copy = text::empty_page(page);

    widgets::render_body(buf, rows.line(), page.label(), widgets::title_style());
    rows.skip(1);

    widgets::render_section(buf, rows.line(), text::WHY_EMPTY);
    let reason_height = widgets::wrapped_height(copy.reason, width);
    widgets::render_body(
        buf,
        rows.take(reason_height),
        copy.reason,
        widgets::muted_style(),
    );

    rows.skip(1);
    widgets::render_section(buf, rows.line(), text::WHAT_GATEWAY_SUPPORTS);
    let supported_height = widgets::wrapped_height(copy.supported, width);
    widgets::render_body(
        buf,
        rows.take(supported_height),
        copy.supported,
        widgets::muted_style(),
    );

    rows.skip(1);
    let notice_height = widgets::wrapped_height(text::EMPTY_NOTICE, width);
    widgets::render_body(
        buf,
        rows.take(notice_height),
        text::EMPTY_NOTICE,
        widgets::dim_style(),
    );
}
