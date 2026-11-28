//! One hit map per rendered frame.
//!
//! Rendering registers every clickable rectangle in the coordinates of the
//! frame that produced it. Mouse routing uses this map and never recomputes
//! geometry, so a click can never land on something the user did not see.
//! Modal regions live on [`Layer::Overlay`]; while an overlay is open the base
//! layer is ignored, which makes click-through impossible.

use peri_tui_components::scrollbar::VerticalBar;
use ratatui::layout::{Position, Rect};

use crate::app::Intent;

/// Which visual layer a hit belongs to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Layer {
    /// Shell, page and scrollbar regions.
    Base,
    /// Modal overlays; these capture every click while open.
    Overlay,
}

/// One clickable rectangle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HitRegion {
    /// Absolute frame rectangle.
    pub rect: Rect,
    /// What a click means.
    pub intent: Intent,
    /// Which layer owns the region.
    pub layer: Layer,
}

/// A wheel-scrollable region.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScrollZone {
    /// Sidebar navigation rows.
    Sidebar,
    /// Page content.
    Content,
}

/// A scrollbar rendered in the last frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScrollbarHit {
    /// Whole scrollbar column, arrows included.
    pub area: Rect,
    /// Which content the bar controls.
    pub zone: ScrollZone,
    /// Geometry captured at render time.
    pub bar: VerticalBar,
}

/// All clickable and scrollable regions of one frame.
#[derive(Clone, Debug, Default)]
pub struct HitMap {
    regions: Vec<HitRegion>,
    scroll_zones: Vec<(Rect, ScrollZone)>,
    scrollbars: Vec<ScrollbarHit>,
    overlay_area: Option<Rect>,
    /// The frame was too small for the shell.
    pub too_small: bool,
}

impl HitMap {
    /// Register a clickable region.
    pub fn region(&mut self, layer: Layer, rect: Rect, intent: Intent) {
        if rect.is_empty() {
            return;
        }
        self.regions.push(HitRegion {
            rect,
            intent,
            layer,
        });
    }

    /// Register a wheel zone.
    pub fn scroll_zone(&mut self, rect: Rect, zone: ScrollZone) {
        if rect.is_empty() {
            return;
        }
        self.scroll_zones.push((rect, zone));
    }

    /// Register a scrollbar column.
    pub fn scrollbar(&mut self, area: Rect, zone: ScrollZone, bar: VerticalBar) {
        if area.is_empty() {
            return;
        }
        self.scrollbars.push(ScrollbarHit { area, zone, bar });
    }

    /// Every clickable region, in registration order.
    #[must_use]
    pub fn regions(&self) -> &[HitRegion] {
        &self.regions
    }

    /// Every registered scrollbar.
    #[must_use]
    pub fn scrollbars(&self) -> &[ScrollbarHit] {
        &self.scrollbars
    }

    /// Rect of the first region carrying `intent`, regardless of layer.
    #[must_use]
    pub fn region_rect(&self, intent: Intent) -> Option<Rect> {
        self.regions
            .iter()
            .find(|region| region.intent == intent)
            .map(|region| region.rect)
    }

    /// Resolve a click to its region.
    ///
    /// Later registrations win, so overlay panels drawn after the shell take
    /// precedence. When `overlay_active` is true only [`Layer::Overlay`]
    /// regions are eligible; a miss means the click must be swallowed or
    /// handled as a backdrop click by the caller.
    #[must_use]
    pub fn click_region(&self, overlay_active: bool, column: u16, row: u16) -> Option<HitRegion> {
        let position = Position::new(column, row);
        self.regions
            .iter()
            .rev()
            .find(|region| {
                region.rect.contains(position)
                    && match region.layer {
                        Layer::Overlay => overlay_active,
                        Layer::Base => !overlay_active,
                    }
            })
            .cloned()
            .map(|mut region| {
                if let Intent::FocusField { field, click: None } = region.intent {
                    region.intent = Intent::FocusField {
                        field,
                        click: Some(crate::ui::mouse::field_click(region.rect, column)),
                    };
                }
                if let Intent::FocusTextAreaAt { field, .. } = region.intent {
                    region.intent = Intent::FocusTextAreaAt {
                        field,
                        click: crate::ui::mouse::text_area_click(region.rect, column, row),
                    };
                }
                region
            })
    }

    /// Resolve a click to its intent.
    #[must_use]
    pub fn click(&self, overlay_active: bool, column: u16, row: u16) -> Option<Intent> {
        self.click_region(overlay_active, column, row)
            .map(|region| region.intent)
    }

    /// Wheel zone under the pointer. Modals block background scrolling.
    #[must_use]
    pub fn wheel_zone(&self, overlay_active: bool, column: u16, row: u16) -> Option<ScrollZone> {
        if overlay_active {
            return None;
        }
        let position = Position::new(column, row);
        self.scroll_zones
            .iter()
            .rev()
            .find(|(rect, _)| rect.contains(position))
            .map(|(_, zone)| *zone)
    }

    /// Scrollbar under the pointer. Modals block background scrollbars.
    #[must_use]
    pub fn scrollbar_at(
        &self,
        overlay_active: bool,
        column: u16,
        row: u16,
    ) -> Option<ScrollbarHit> {
        if overlay_active {
            return None;
        }
        let position = Position::new(column, row);
        self.scrollbars
            .iter()
            .rev()
            .find(|hit| hit.area.contains(position))
            .copied()
    }

    /// Register the modal panel bounds. Clicks outside `area` are backdrop
    /// clicks; clicks inside it are swallowed even when they miss a button.
    pub fn set_overlay_area(&mut self, area: Rect) {
        self.overlay_area = Some(area);
    }

    /// Whether the last frame drew a modal panel.
    #[must_use]
    pub fn overlay_area(&self) -> Option<Rect> {
        self.overlay_area
    }

    /// Whether a click landed outside the modal panel.
    #[must_use]
    pub fn outside_overlay(&self, column: u16, row: u16) -> bool {
        let position = Position::new(column, row);
        self.overlay_area
            .is_none_or(|area| !area.contains(position))
    }

    /// Translate the regions registered since `start` by `(dx, dy)` and clip
    /// them to `clip`. Used to lift a page rendered in local coordinates into
    /// the shell body through the scroll offset.
    pub fn translate_since(&mut self, start: usize, dx: u16, dy: i32, clip: Rect) {
        for region in self.regions.iter_mut().skip(start) {
            if let Some(rect) = translated(region.rect, dx, dy, clip) {
                region.rect = rect;
            } else {
                region.rect = Rect::default();
            }
        }
        self.regions.retain(|region| !region.rect.is_empty());
    }

    /// Number of regions registered so far.
    #[must_use]
    pub fn len(&self) -> usize {
        self.regions.len()
    }

    /// Drop the regions registered after `start`.
    ///
    /// Used when a page is measured first and then re-rendered at a different
    /// scroll offset: the measured regions would describe the old window.
    pub fn truncate_since(&mut self, start: usize) {
        self.regions.truncate(start);
    }

    /// Whether no regions were registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }
}

fn translated(rect: Rect, dx: u16, dy: i32, clip: Rect) -> Option<Rect> {
    if rect.is_empty() || clip.is_empty() {
        return None;
    }
    let x = i32::from(rect.x).saturating_add(i32::from(dx));
    let y = i32::from(rect.y).saturating_add(dy);
    let right = x.saturating_add(i32::from(rect.width));
    let bottom = y.saturating_add(i32::from(rect.height));
    let clip_left = i32::from(clip.x);
    let clip_top = i32::from(clip.y);
    let clip_right = clip_left.saturating_add(i32::from(clip.width));
    let clip_bottom = clip_top.saturating_add(i32::from(clip.height));
    let left = x.max(clip_left);
    let top = y.max(clip_top);
    let right = right.min(clip_right);
    let bottom = bottom.min(clip_bottom);
    if left >= right || top >= bottom {
        return None;
    }
    Some(Rect::new(
        u16::try_from(left).ok()?,
        u16::try_from(top).ok()?,
        u16::try_from(right - left).ok()?,
        u16::try_from(bottom - top).ok()?,
    ))
}

#[cfg(test)]
#[path = "hit_test.rs"]
mod tests;
