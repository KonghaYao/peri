//! Getting-started overlay.

use peri_tui_components::button::ButtonTone;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Block, Clear, Widget};

use crate::app::{ActionId, Intent};
use crate::ui::hit::{HitMap, Layer};
use crate::ui::text;
use crate::ui::widgets::{self, Rows};

/// Render the help overlay over the already-drawn shell.
pub fn render(buf: &mut Buffer, area: Rect, hits: &mut HitMap) {
    if area.width < 12 || area.height < 6 {
        return;
    }
    let panel = Rect::new(
        area.x.saturating_add(1),
        area.y.saturating_add(1),
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    );
    hits.set_overlay_area(panel);
    Clear.render(panel, buf);
    let block = Block::bordered()
        .title(Line::from(text::HELP_TITLE).style(widgets::title_style()))
        .border_style(widgets::border_style(true));
    let inner = block.inner(panel);
    block.render(panel, buf);
    if inner.is_empty() {
        return;
    }

    let mut rows = Rows::new(inner);
    let reserve = 1.min(inner.height);
    let body_height = inner.height.saturating_sub(reserve);
    'sections: for (index, section) in text::HELP_SECTIONS.iter().enumerate() {
        let blank = u16::from(index > 0);
        let lines: u16 = section
            .lines
            .iter()
            .map(|line| widgets::wrapped_height(line, inner.width))
            .sum();
        let needed = blank.saturating_add(1).saturating_add(lines);
        if rows.used().saturating_add(needed) > body_height {
            break 'sections;
        }
        rows.skip(blank);
        widgets::render_section(buf, rows.line(), section.title);
        for line in section.lines {
            let height = widgets::wrapped_height(line, inner.width);
            widgets::render_body(buf, rows.take(height), line, widgets::muted_style());
        }
    }

    if reserve == 1 {
        let hint_row = Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1);
        let close_label = text::action_label(ActionId::CloseHelp);
        let close_width = widgets::button_width(close_label);
        let hint_width = hint_row.width.saturating_sub(close_width.saturating_add(1));
        buf.set_string(
            hint_row.x,
            hint_row.y,
            widgets::fit(text::HELP_HINT, hint_width),
            widgets::dim_style(),
        );
        if hint_row.width >= close_width {
            let close = widgets::button_rect(
                hint_row,
                hint_row.right().saturating_sub(close_width),
                close_label,
            );
            widgets::render_button(buf, close, close_label, false, ButtonTone::Secondary);
            hits.region(Layer::Overlay, close, Intent::Activate(ActionId::CloseHelp));
        }
    }
}
