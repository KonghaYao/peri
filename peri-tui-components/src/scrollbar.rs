//! Scrollbar appearance is supplied by the caller; this module never reads theme atoms.
//! Horizontal geometry and painting live together so drag hit-testing matches drawing.
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::{Scrollbar, ScrollbarOrientation};

pub fn vertical_scrollbar(thumb: Style, arrow: Style) -> Scrollbar<'static> {
    Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .thumb_symbol(" ")
        .thumb_style(thumb)
        .track_symbol(None)
        .begin_symbol(Some("▲"))
        .begin_style(arrow)
        .end_symbol(Some("▼"))
        .end_style(arrow)
}

/// Horizontal thumb geometry. `width` is the whole bar row, arrow cells included;
/// the track excludes one cell at each end.
///
/// Rendering and hit testing share this type so a dragged thumb cannot drift from
/// where it was painted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HorizontalBar {
    /// Track cells between the two arrows.
    pub track: usize,
    /// Thumb start, as a cell offset inside the track.
    pub thumb_start: usize,
    /// Thumb length in cells; never zero and never wider than the track.
    pub thumb_len: usize,
    viewport: usize,
    content: usize,
}

impl HorizontalBar {
    /// `None` when nothing overflows, the row is too narrow for arrows plus a
    /// track, or the viewport is empty.
    #[must_use]
    pub fn new(width: u16, viewport: usize, content: usize, offset: usize) -> Option<Self> {
        let track = usize::from(width).checked_sub(2)?;
        if track == 0 || viewport == 0 || content <= viewport {
            return None;
        }
        let thumb_len = round_divide(viewport.saturating_mul(track), content).clamp(1, track);
        let span = track - thumb_len;
        let max_offset = content - viewport;
        let thumb_start = round_divide(offset.min(max_offset).saturating_mul(span), max_offset);
        Some(Self {
            track,
            thumb_start,
            thumb_len,
            viewport,
            content,
        })
    }

    /// Offset inside the thumb when `cell` hits it; `None` for the empty track.
    #[must_use]
    pub fn grab(&self, cell: usize) -> Option<usize> {
        (self.thumb_start..self.thumb_start + self.thumb_len)
            .contains(&cell)
            .then(|| cell - self.thumb_start)
    }

    /// Scroll offset for a thumb start; values outside the legal span are clamped,
    /// so drags past either end simply stop at the boundary.
    #[must_use]
    pub fn offset_for(&self, thumb_start: usize) -> usize {
        let span = self.track - self.thumb_len;
        if span == 0 {
            return 0;
        }
        round_divide(thumb_start.min(span) * (self.content - self.viewport), span)
    }
}

/// Paint `◄`/`►` ends plus the thumb block. The track itself is left untouched,
/// matching [`vertical_scrollbar`].
pub fn horizontal_scrollbar(
    area: Rect,
    buf: &mut Buffer,
    thumb: Style,
    arrow: Style,
    bar: &HorizontalBar,
) {
    if area.width < 2 || area.height == 0 {
        return;
    }
    buf.set_string(area.x, area.y, "◄", arrow);
    buf.set_string(area.right() - 1, area.y, "►", arrow);
    let x = area.x + 1;
    for cell in bar.thumb_start..bar.thumb_start + bar.thumb_len {
        buf.set_string(x + cell as u16, area.y, " ", thumb);
    }
}

/// Round to nearest, like `ratatui`'s scrollbar internals.
fn round_divide(numerator: usize, denominator: usize) -> usize {
    (numerator + denominator / 2)
        .checked_div(denominator)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::{Color, Style};

    use super::{HorizontalBar, horizontal_scrollbar};

    fn scroll_bar(offset: usize) -> HorizontalBar {
        // 20 visible columns out of 100 content columns on a 32-cell row:
        // 30 track cells, thumb 6 wide (round(20 * 30 / 100)).
        HorizontalBar::new(32, 20, 100, offset).expect("overflowing bar")
    }

    #[test]
    fn no_bar_without_overflow_or_room() {
        assert_eq!(HorizontalBar::new(32, 20, 20, 0), None);
        assert_eq!(HorizontalBar::new(2, 20, 100, 0), None);
        assert_eq!(HorizontalBar::new(32, 0, 100, 0), None);
    }

    #[test]
    fn thumb_rests_at_both_ends() {
        assert_eq!(scroll_bar(0).thumb_start, 0);
        let last = scroll_bar(80);
        assert_eq!(last.thumb_start + last.thumb_len, last.track);
        assert_eq!(last.track, 30);
        assert_eq!(last.thumb_len, 6);
    }

    #[test]
    fn grab_only_inside_thumb() {
        let bar = scroll_bar(80);
        assert_eq!(bar.thumb_start, 24);
        assert_eq!(bar.grab(23), None);
        assert_eq!(bar.grab(24), Some(0));
        assert_eq!(bar.grab(29), Some(5));
        assert_eq!(bar.grab(30), None);
    }

    #[test]
    fn offset_for_round_trips_through_thumb_start() {
        let max = 80;
        for offset in [0, 7, 33, 40, 61, max] {
            let bar = scroll_bar(offset);
            // Track cells quantise the offset; the inverse keeps the thumb in the
            // same cell, stays in bounds and is stable when applied again.
            let back = bar.offset_for(bar.thumb_start);
            assert!(back <= max, "offset {offset} produced {back}");
            assert_eq!(scroll_bar(back).thumb_start, bar.thumb_start);
            assert_eq!(
                scroll_bar(back).offset_for(scroll_bar(back).thumb_start),
                back
            );
        }
        assert_eq!(scroll_bar(max).offset_for(scroll_bar(max).thumb_start), max);
        assert_eq!(scroll_bar(0).offset_for(0), 0);
        assert_eq!(
            scroll_bar(40).offset_for(usize::MAX),
            max,
            "drag past the end clamps"
        );
    }

    #[test]
    fn paint_draws_arrows_and_thumb_block() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 32, 1));
        let thumb = Style::new().fg(Color::DarkGray).bg(Color::DarkGray);
        let arrow = Style::new().fg(Color::Red);
        horizontal_scrollbar(
            Rect::new(0, 0, 32, 1),
            &mut buf,
            thumb,
            arrow,
            &scroll_bar(0),
        );

        assert_eq!(buf[(0, 0)].symbol(), "◄");
        assert_eq!(buf[(31, 0)].symbol(), "►");
        assert_eq!(buf[(0, 0)].style().fg, Some(Color::Red));
        // Thumb occupies track cells 0..6 → buffer columns 1..7.
        assert_eq!(buf[(1, 0)].style().bg, Some(Color::DarkGray));
        assert_eq!(buf[(6, 0)].style().bg, Some(Color::DarkGray));
        assert_ne!(buf[(7, 0)].style().bg, Some(Color::DarkGray));
    }
}
