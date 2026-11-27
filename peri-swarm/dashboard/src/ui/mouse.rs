//! Mouse routing against the most recently rendered [`HitMap`].
//!
//! No geometry is recomputed here: clicks, wheel zones, and scrollbar drags all
//! come from the frame the user actually saw.

use peri_tui_components::scrollbar::{VerticalBar, VerticalPress};
use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::app::{App, Effect, FieldClick, Intent, Overlay, TextAreaClick};
use crate::ui::hit::{HitMap, ScrollZone};

/// One in-progress scrollbar drag.
#[derive(Clone, Copy, Debug)]
pub struct ScrollDrag {
    zone: ScrollZone,
    area: Rect,
    bar: VerticalBar,
    grab: u16,
}

/// Route one mouse event and return effects for the runtime.
pub fn handle_mouse(
    app: &mut App,
    hits: &HitMap,
    drag: &mut Option<ScrollDrag>,
    mouse: MouseEvent,
) -> Vec<Effect> {
    let overlay_active = app.overlay().is_some();
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if hits.too_small {
                return Vec::new();
            }
            if let Some(hit) = hits.scrollbar_at(overlay_active, mouse.column, mouse.row) {
                match hit.bar.press(hit.area, mouse.row) {
                    VerticalPress::Thumb { grab } => {
                        *drag = Some(ScrollDrag {
                            zone: hit.zone,
                            area: hit.area,
                            bar: hit.bar,
                            grab,
                        });
                    }
                    VerticalPress::Track { offset } => {
                        apply_scroll(app, hit.zone, offset);
                    }
                    VerticalPress::Top => apply_scroll(app, hit.zone, 0),
                    VerticalPress::Bottom => {
                        apply_scroll(app, hit.zone, hit.bar.max_offset());
                    }
                }
                return Vec::new();
            }
            if let Some(region) = hits.click_region(overlay_active, mouse.column, mouse.row) {
                return app.apply(region.intent);
            }
            if overlay_active
                && hits.outside_overlay(mouse.column, mouse.row)
                && matches!(
                    app.overlay(),
                    Some(Overlay::Help | Overlay::NarrowMenu | Overlay::ErrorActions)
                )
            {
                return app.apply(Intent::CloseOverlay);
            }
            Vec::new()
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if let Some(active) = drag {
                let offset = active.bar.drag_offset(active.area, mouse.row, active.grab);
                apply_scroll(app, active.zone, offset);
            }
            Vec::new()
        }
        MouseEventKind::Up(MouseButton::Left) => {
            *drag = None;
            Vec::new()
        }
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            if let Some(zone) = hits.wheel_zone(overlay_active, mouse.column, mouse.row) {
                let delta = if mouse.kind == MouseEventKind::ScrollDown {
                    1
                } else {
                    -1
                };
                let intent = match zone {
                    ScrollZone::Sidebar => Intent::ScrollSidebar(delta),
                    ScrollZone::Content => Intent::ScrollContent(delta),
                };
                return app.apply(intent);
            }
            Vec::new()
        }
        _ => Vec::new(),
    }
}

/// Field caret mapping for a click inside a field rectangle.
#[must_use]
pub fn field_click(rect: Rect, column: u16) -> FieldClick {
    let column = column.saturating_sub(rect.x.saturating_add(1));
    let width = rect.width.saturating_sub(2);
    FieldClick { column, width }
}

/// Caret mapping for a click inside a bordered multiline area.
#[must_use]
pub fn text_area_click(rect: Rect, column: u16, row: u16) -> TextAreaClick {
    TextAreaClick {
        row: row.saturating_sub(rect.y.saturating_add(1)),
        column: column.saturating_sub(rect.x.saturating_add(1)),
        width: rect.width.saturating_sub(2),
        height: rect.height.saturating_sub(2),
    }
}

fn apply_scroll(app: &mut App, zone: ScrollZone, offset: usize) {
    match zone {
        ScrollZone::Sidebar => app.set_sidebar_scroll(offset),
        ScrollZone::Content => app.set_content_scroll(offset),
    }
}

#[cfg(test)]
#[path = "mouse_test.rs"]
mod tests;
