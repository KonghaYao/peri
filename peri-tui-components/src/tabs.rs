//! Tab hit geometry.
//!
//! The dashboard draws tabs with `ratatui::widgets::Tabs` and registers one hit
//! region per drawn label. This module reproduces the default `Tabs` layout so
//! the regions mirror what the user sees: one column of left padding, the
//! title, one column of right padding, then the `" | "` divider. Adjacent
//! labels therefore start five columns apart.
//!
//! The selected index never affects geometry; the host resolves it for styling.

use ratatui::layout::Rect;
use ratatui::text::Span;

/// One rectangle per tab title, clipped to the drawn area.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TabLayout {
    /// Label rectangles in title order. Empty when the area is empty.
    pub rects: Vec<Rect>,
}

/// Column width of the divider drawn between titles (`" | "`).
const DIVIDER_WIDTH: u16 = 3;

/// Label geometry for the default `Tabs` layout.
///
/// Titles that do not fit are clipped; nothing beyond the area is reported.
#[must_use]
pub fn tab_layout(area: Rect, titles: &[&str], selected: usize) -> TabLayout {
    let _ = selected;
    let mut rects = Vec::new();
    if area.is_empty() {
        return TabLayout { rects };
    }
    let right = area.right();
    let mut x = area.x;
    for (index, title) in titles.iter().enumerate() {
        let last = index + 1 == titles.len();
        if x >= right {
            break;
        }
        // Left padding.
        x = x.saturating_add(1);
        if x >= right {
            break;
        }
        let width = Span::raw(*title).width();
        let width = u16::try_from(width).unwrap_or(u16::MAX);
        let visible = width.min(right.saturating_sub(x));
        rects.push(Rect::new(x, area.y, visible, 1));
        x = x.saturating_add(width);
        if x >= right {
            break;
        }
        // Right padding.
        x = x.saturating_add(1);
        if x >= right || last {
            break;
        }
        // Divider.
        x = x.saturating_add(DIVIDER_WIDTH);
    }
    TabLayout { rects }
}

#[cfg(test)]
#[path = "tabs_test.rs"]
mod tests;
