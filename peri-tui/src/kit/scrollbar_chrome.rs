//! Shared scrollbar chrome for the message transcript and nested detail view.

use peri_theme::atoms::THEME_ATOM;
use peri_tui_components::scrollbar::VerticalBar;
use ratatui_kit::prelude::ComponentDrawer;
use ratatui_kit::ratatui::layout::Rect;
use ratatui_kit::ratatui::style::Modifier;
use ratatui_kit::ratatui::style::Style;

/// Draw the transcript/detail scrollbar from the raw content offset.
///
/// The bar derives thumb geometry once; the message area's hit testing builds the
/// same [`VerticalBar`] for the same frame.
pub(crate) fn draw_scrollbar(
    drawer: &mut ComponentDrawer,
    area: Rect,
    content_length: usize,
    offset: usize,
    viewport_length: usize,
) {
    if content_length <= viewport_length || area.is_empty() {
        return;
    }
    let Some(bar) = VerticalBar::new(area.height, viewport_length, content_length, offset) else {
        return;
    };
    let theme_state = THEME_ATOM.state();
    let semantic = theme_state.read().semantic;
    let thumb = semantic.text.dim;
    let arrow = Style::default()
        .fg(semantic.text.muted)
        .add_modifier(Modifier::BOLD);
    bar.paint(
        area,
        drawer.buffer_mut(),
        Style::default().fg(thumb).bg(thumb),
        arrow,
    );
}
