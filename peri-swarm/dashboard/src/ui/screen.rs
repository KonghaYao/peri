//! Top-level frame renderer.
//!
//! The whole frame is produced here: shell chrome first, then the page body
//! through an off-screen window so the wheel offset can be applied without a
//! second layout implementation, then any modal overlay. Clickable rectangles
//! are registered in the same pass and returned as a [`HitMap`].
//!
//! The page is laid out in content coordinates; only the visible slice is
//! drawn. When the detail timeline follows the newest items the window is
//! re-anchored to the bottom after the content height is known, so a page
//! update can never leave the viewport stranded above the new entries.

use peri_tui_components::button::ButtonTone;
use peri_tui_components::scrollbar::VerticalBar;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;
use ratatui::Frame;

use crate::app::{ActionId, App, DetailTab, Focus, Intent};
use crate::ui::hit::{HitMap, Layer, ScrollZone};
use crate::ui::pages::{self, TOP_MARGIN};
use crate::ui::{dialogs, shell, text, widgets};

/// Render one frame and return its hit map.
pub fn render(frame: &mut Frame<'_>, app: &mut App) -> HitMap {
    let area = frame.area();
    let mut hits = HitMap::default();
    if shell::too_small(area) {
        shell::render_too_small(frame.buffer_mut(), area);
        hits.too_small = true;
        return hits;
    }

    let layout = shell::ShellLayout::compute(area, app);
    app.set_viewport_width(area.width);
    app.set_sidebar_viewport(layout.sidebar_list.map_or(0, |list| list.height));

    shell::render_header(frame.buffer_mut(), &layout, app);
    shell::register_menu_button(&mut hits, &layout, app);
    if let Some(banner) = app.active_banner() {
        shell::render_banner(frame.buffer_mut(), &layout, &banner, &mut hits);
    }
    shell::render_sidebar(frame.buffer_mut(), &layout, app, &mut hits);
    shell::render_footer(frame.buffer_mut(), &layout, app);

    render_body(frame, app, &layout, &mut hits);
    dialogs::render_active(frame.buffer_mut(), area, app, &mut hits);
    hits
}

fn render_body(
    frame: &mut Frame<'_>,
    app: &mut App,
    layout: &shell::ShellLayout,
    hits: &mut HitMap,
) {
    let body = layout.body;
    let content_width = body.width.saturating_sub(1);
    if content_width == 0 || body.height == 0 {
        return;
    }
    let mut scratch = Buffer::empty(Rect::new(
        0,
        0,
        content_width,
        pages::scratch_height(body.height),
    ));
    let page_start = hits.len();
    let requested = u16::try_from(app.content_scroll()).unwrap_or(u16::MAX);
    let mut rendered = pages::render(
        &mut scratch,
        content_width,
        body.height,
        requested,
        app,
        hits,
    );
    let max_scroll = rendered.height.saturating_sub(body.height);
    // Following means the viewport was already anchored to the bottom of the
    // previous frame. Re-anchor to the new bottom after a page update; a view
    // that was scrolled up keeps its offset and shows the jump control instead.
    let follow = app.detail_timeline_following() && app.content_scroll() >= app.content_max();
    let scroll = if follow {
        max_scroll
    } else {
        requested.min(max_scroll)
    };
    if scroll != requested {
        scratch.reset();
        hits.truncate_since(page_start);
        rendered = pages::render(&mut scratch, content_width, body.height, scroll, app, hits);
    }
    app.set_content_layout(body.height, rendered.height, usize::from(scroll));

    blit(&scratch, frame.buffer_mut(), body, TOP_MARGIN);
    hits.translate_since(
        page_start,
        body.x,
        i32::from(body.y) - i32::from(TOP_MARGIN),
        body,
    );

    if let Some(cursor) = rendered.cursor {
        if cursor.y >= scroll
            && cursor.y < scroll.saturating_add(body.height)
            && cursor.x < content_width
        {
            frame.set_cursor_position(Position::new(
                body.x.saturating_add(cursor.x),
                body.y.saturating_add(cursor.y.saturating_sub(scroll)),
            ));
        }
    }

    hits.scroll_zone(body, ScrollZone::Content);
    // A modal owns its own scrolling; the page scrollbar behind it would look
    // like part of the overlay, so it is hidden while one is open.
    let bar_area = Rect::new(body.right().saturating_sub(1), body.y, 1, body.height);
    if app.overlay().is_none() {
        if let Some(bar) = VerticalBar::new(
            body.height,
            usize::from(body.height),
            usize::from(rendered.height),
            usize::from(scroll),
        ) {
            bar.paint(
                bar_area,
                frame.buffer_mut(),
                widgets::selected_style(),
                widgets::dim_style(),
            );
            hits.scrollbar(bar_area, ScrollZone::Content, bar);
        }
    }

    let jump_visible =
        app.timeline_unseen() || !app.detail_timeline_following() || scroll < max_scroll;
    render_jump_control(frame.buffer_mut(), body, app, hits, jump_visible);
}

/// Fixed bottom-right control that returns the timeline to the newest items.
///
/// It is drawn over the page body after the scroll window, so it stays visible
/// and clickable no matter how far the timeline is scrolled.
fn render_jump_control(buf: &mut Buffer, body: Rect, app: &App, hits: &mut HitMap, visible: bool) {
    if !visible
        || app.screen() != crate::app::Screen::SessionDetail
        || app.detail_tab() != DetailTab::Timeline
        || body.height == 0
    {
        return;
    }
    let label = text::action_label(ActionId::JumpToLatest);
    let width = widgets::button_width(label, ButtonTone::Primary);
    let right = body.right().saturating_sub(1);
    if width == 0 || width > right.saturating_sub(body.x) {
        return;
    }
    let rect = Rect::new(right - width, body.bottom().saturating_sub(1), width, 1);
    if rect.is_empty() {
        return;
    }
    let background = Style::default().bg(surface_raised());
    buf.set_string(
        rect.x,
        rect.y,
        " ".repeat(usize::from(rect.width)),
        background,
    );
    widgets::render_button(
        buf,
        rect,
        label,
        app.focus() == Focus::Action(ActionId::JumpToLatest),
        ButtonTone::Primary,
    );
    hits.region(Layer::Base, rect, Intent::Activate(ActionId::JumpToLatest));
}

fn surface_raised() -> ratatui::style::Color {
    widgets::theme().semantic.surface.raised
}

fn blit(src: &Buffer, dst: &mut Buffer, area: Rect, source_start: u16) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    for row in 0..area.height {
        let source_y = source_start.saturating_add(row);
        if source_y >= src.area.height {
            break;
        }
        for column in 0..area.width {
            let Some(cell) = src.cell((column, source_y)) else {
                break;
            };
            let x = area.x.saturating_add(column);
            let y = area.y.saturating_add(row);
            if let Some(target) = dst.cell_mut((x, y)) {
                *target = cell.clone();
            }
        }
    }
}
