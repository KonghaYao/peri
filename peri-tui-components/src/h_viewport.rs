//! Horizontal viewport for content that is wider than its column budget.
//!
//! Storybook documents and inline chat Mermaid share this geometry: overflow,
//! clamping, wheel step and the bottom bar all derive from one pair of numbers
//! (`viewport`, `content`) so two hosts cannot disagree about where the right
//! edge is.

use unicode_width::UnicodeWidthStr;

use crate::scrollbar::{HorizontalBar, bounded_offset};

/// Columns a single horizontal wheel notch or arrow key pans.
pub const WHEEL_COLS: usize = 8;

/// Widest line in display cells; CJK and emoji count their terminal width.
#[must_use]
pub fn content_width(lines: &[String]) -> usize {
    lines
        .iter()
        .map(|line| line.as_str().width())
        .max()
        .unwrap_or(0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HorizontalViewport {
    viewport: usize,
    content: usize,
}

impl HorizontalViewport {
    #[must_use]
    pub fn new(viewport: usize, content: usize) -> Self {
        Self { viewport, content }
    }

    #[must_use]
    pub fn viewport(&self) -> usize {
        self.viewport
    }

    #[must_use]
    pub fn content(&self) -> usize {
        self.content
    }

    /// `true` when some content column is unreachable without panning; an empty
    /// viewport never overflows (there is nothing to show).
    #[must_use]
    pub fn overflows(&self) -> bool {
        self.viewport > 0 && self.content > self.viewport
    }

    /// Largest legal offset. Zero when the content fits.
    #[must_use]
    pub fn max_offset(&self) -> usize {
        if self.overflows() {
            self.content - self.viewport
        } else {
            0
        }
    }

    #[must_use]
    pub fn clamp(&self, offset: usize) -> usize {
        offset.min(self.max_offset())
    }

    /// Apply one wheel/key step to `offset`, bounded by [`Self::max_offset`].
    #[must_use]
    pub fn pan(&self, offset: usize, delta: i32) -> usize {
        bounded_offset(self.clamp(offset), delta, self.max_offset())
    }

    /// Bottom bar for a `width`-cell row; `None` when nothing overflows or the
    /// row cannot hold two arrows plus a track.
    #[must_use]
    pub fn bar(&self, width: u16, offset: usize) -> Option<HorizontalBar> {
        if !self.overflows() {
            return None;
        }
        HorizontalBar::new(width, self.viewport, self.content, self.clamp(offset))
    }
}

#[cfg(test)]
#[path = "h_viewport_test.rs"]
mod tests;
