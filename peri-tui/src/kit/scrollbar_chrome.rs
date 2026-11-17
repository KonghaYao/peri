//! Shared scrollbar chrome for the message transcript and nested detail view.

use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::prelude::ComponentDrawer;
use ratatui_kit::ratatui::layout::Rect;
use ratatui_kit::ratatui::style::Modifier;
use ratatui_kit::ratatui::style::Style;
use ratatui_kit::ratatui::widgets::ScrollbarState;

pub(crate) fn draw_scrollbar(
    drawer: &mut ComponentDrawer,
    area: Rect,
    content_length: usize,
    position: usize,
    viewport_length: usize,
) {
    if content_length <= viewport_length || area.is_empty() {
        return;
    }
    let theme_state = THEME_ATOM.state();
    let semantic = theme_state.read().semantic;
    let thumb = semantic.text.dim;
    let arrow = Style::default()
        .fg(semantic.text.muted)
        .add_modifier(Modifier::BOLD);
    let scrollbar = peri_tui_components::scrollbar::vertical_scrollbar(
        Style::default().fg(thumb).bg(thumb),
        arrow,
    );
    let mut state = ScrollbarState::new(content_length)
        .position(position)
        .viewport_content_length(viewport_length);
    drawer.render_stateful_widget(scrollbar, area, &mut state);
}
