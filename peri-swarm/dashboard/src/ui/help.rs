//! Getting-started overlay.
//!
//! The content is taller than an 80x24 terminal, so the overlay scrolls
//! independently of the page behind it: wheel, Up/Down, PgUp/PgDn, Home/End.
//! The shell's page scrollbar is suppressed while an overlay is open, so the
//! only bar on screen is this overlay's own.

use peri_tui_components::button::ButtonTone;
use peri_tui_components::scrollbar::VerticalBar;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Block, Clear, Widget};

use crate::app::{ActionId, App, Intent};
use crate::ui::hit::{HitMap, Layer};
use crate::ui::text;
use crate::ui::widgets;

/// Render the help overlay over the already-drawn shell.
pub fn render(buf: &mut Buffer, area: Rect, app: &mut App, hits: &mut HitMap) {
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

    let reserve = 1.min(inner.height);
    let body_height = inner.height.saturating_sub(reserve);
    // Reserve the last column for the overlay's own scrollbar.
    let text_width = inner.width.saturating_sub(1);
    let body = Rect::new(inner.x, inner.y, text_width, body_height);

    let mut display: Vec<(bool, String)> = Vec::new();
    for line in help_lines() {
        for text in widgets::wrap_words(&line.text, text_width) {
            display.push((line.heading, text));
        }
    }
    let total = u16::try_from(display.len()).unwrap_or(u16::MAX);
    let scroll = app
        .help_scroll()
        .min(usize::from(total.saturating_sub(body_height)));
    let offset = u16::try_from(scroll).unwrap_or(u16::MAX);
    app.set_help_layout(usize::from(total), body_height);

    for (visible_row, (heading, text)) in display
        .iter()
        .skip(usize::from(offset))
        .take(usize::from(body_height))
        .enumerate()
    {
        if text.is_empty() {
            continue;
        }
        let y = body
            .y
            .saturating_add(u16::try_from(visible_row).unwrap_or(u16::MAX));
        let style = if *heading {
            widgets::section_style()
        } else {
            widgets::muted_style()
        };
        buf.set_string(body.x, y, widgets::fit(text, text_width), style);
    }

    if body_height > 0 && inner.width > 1 {
        if let Some(bar) = VerticalBar::new(
            body_height,
            usize::from(body_height),
            usize::from(total),
            scroll,
        ) {
            let bar_area = Rect::new(inner.right().saturating_sub(1), inner.y, 1, body_height);
            bar.paint(
                bar_area,
                buf,
                widgets::selected_style(),
                widgets::dim_style(),
            );
        }
    }

    if reserve == 1 {
        let hint_row = Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1);
        let close_label = text::action_label(ActionId::CloseHelp);
        let close_width = widgets::button_width(close_label, ButtonTone::Secondary);
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
                ButtonTone::Secondary,
            );
            widgets::render_button(buf, close, close_label, false, ButtonTone::Secondary);
            hits.region(Layer::Overlay, close, Intent::Activate(ActionId::CloseHelp));
        }
    }
}

struct HelpLine {
    heading: bool,
    text: String,
}

fn help_lines() -> Vec<HelpLine> {
    let mut lines = Vec::new();
    for (index, section) in text::HELP_SECTIONS.iter().enumerate() {
        if index > 0 {
            lines.push(HelpLine {
                heading: false,
                text: String::new(),
            });
        }
        lines.push(HelpLine {
            heading: true,
            text: section.title.to_owned(),
        });
        for line in section.lines {
            lines.push(HelpLine {
                heading: false,
                text: (*line).to_owned(),
            });
        }
    }
    lines
}

#[cfg(test)]
#[path = "help_test.rs"]
mod tests;
