//! Page dispatcher: routes the active screen and page to its view module.
//!
//! Pages are laid out in **content coordinates**: the cursor keeps advancing
//! past the viewport so the full content height is known, while only the
//! visible slice is drawn into the scratch buffer. Scrolling therefore never
//! truncates the page: every row up to the 500-row list cap and a trailing
//! footer stays reachable.

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};

use crate::app::{App, Page, Screen};
use crate::ui::hit::HitMap;
use crate::ui::widgets::Rows;

use super::{
    empty, metrics, onboarding, query, request_log, session_detail, session_form, session_log,
    settings,
};

/// Extra scratch rows above the viewport used to draw a block that starts just
/// above the visible window. Every multi-line block drawn by a page is smaller
/// than this, so the visible tail stays correct while scrolling.
pub const TOP_MARGIN: u16 = 64;
/// Horizontal page padding on wide terminals.
const WIDE_PADDING: u16 = 2;
/// Horizontal page padding on narrow terminals.
const NARROW_PADDING: u16 = 1;

/// Result of rendering one page body.
#[derive(Clone, Copy, Debug, Default)]
pub struct PageRender {
    /// Measured content height, including padding.
    pub height: u16,
    /// Caret position in content coordinates.
    pub cursor: Option<Position>,
}

/// Scratch buffer height needed for `viewport_height` visible rows.
#[must_use]
pub fn scratch_height(viewport_height: u16) -> u16 {
    TOP_MARGIN.saturating_add(viewport_height)
}

/// Render the active screen body for the visible window into `buf` (local
/// coordinates) and register its click regions.
///
/// `scroll` is the first content row shown; regions are registered in local
/// buffer coordinates, so the caller only has to subtract [`TOP_MARGIN`] when
/// lifting them into the frame.
pub fn render(
    buf: &mut Buffer,
    width: u16,
    viewport_height: u16,
    scroll: u16,
    app: &App,
    hits: &mut HitMap,
) -> PageRender {
    if width == 0 || viewport_height == 0 {
        return PageRender::default();
    }
    let padding = if width >= 60 {
        WIDE_PADDING
    } else {
        NARROW_PADDING
    };
    let inner_width = width.saturating_sub(padding.saturating_mul(2));
    let area = Rect::new(padding, padding, inner_width, viewport_height);
    let mut rows = Rows::window(area, scroll, TOP_MARGIN, viewport_height);
    let cursor = match app.screen() {
        Screen::Onboarding => onboarding::render(buf, &mut rows, app, hits),
        Screen::SettingsCredentials => settings::render_credentials(buf, &mut rows, app, hits),
        Screen::SettingsObservability => settings::render_observability(buf, &mut rows, app, hits),
        Screen::Main => match app.page() {
            Page::Settings => {
                settings::render(buf, &mut rows, app, hits);
                None
            }
            Page::SessionLog => session_log::render(buf, &mut rows, app, hits),
            Page::CoreMetrics => metrics::render_core(buf, &mut rows, app, hits),
            Page::AgentMetrics => metrics::render_agent(buf, &mut rows, app, hits),
            Page::RequestLog => request_log::render(buf, &mut rows, app, hits),
            Page::Query => query::render(buf, &mut rows, app, hits),
            // Placeholder pages are reachable but have no data source yet; each
            // renders an honest empty state until a later stage implements it.
            Page::SandboxMetrics
            | Page::Agents
            | Page::Artifacts
            | Page::Vaults
            | Page::Mcp
            | Page::Automations
            | Page::Nodes => {
                empty::render(app.page(), buf, &mut rows);
                None
            }
        },
        Screen::SessionCreate => session_form::render(buf, &mut rows, app, hits),
        Screen::SessionDetail => session_detail::render(buf, &mut rows, app, hits),
        Screen::SessionEdit => session_form::render_edit(buf, &mut rows, app, hits),
    };
    let height = padding
        .saturating_add(rows.used())
        .saturating_add(padding)
        .max(1);
    let cursor = cursor.map(|cursor| {
        Position::new(
            cursor.x,
            cursor.y.saturating_sub(TOP_MARGIN).saturating_add(scroll),
        )
    });
    PageRender { height, cursor }
}
