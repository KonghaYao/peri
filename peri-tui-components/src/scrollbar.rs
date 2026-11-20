//! Scrollbar appearance is supplied by the caller; this module never reads theme atoms.
//! Vertical and horizontal geometry and painting live together so drag hit-testing
//! matches drawing.
use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::{Scrollbar, ScrollbarOrientation};

/// Rows a single wheel notch scrolls. Hosts keep their own scroll configuration
/// (fps, follow state, paging); only the shared step lives here.
pub const SCROLL_LINES: u16 = 3;

/// Apply one scroll delta in rows to `offset`, bounded to `[0, max_offset]`.
///
/// A stale offset above `max_offset` is pulled back into bounds instead of being
/// preserved, so callers may pass values written before a resize.
#[must_use]
pub fn bounded_offset(offset: usize, delta: i32, max_offset: usize) -> usize {
    let next = if delta >= 0 {
        offset.saturating_add(delta as usize)
    } else {
        offset.saturating_sub(delta.unsigned_abs() as usize)
    };
    next.min(max_offset)
}

/// Caller-time drag throttle.
///
/// The component never reads a clock: the host passes `now` (its own monotonic
/// clock) and its configured interval, so tests and the app share one rule.
#[derive(Debug, Clone, Copy, Default)]
pub struct Throttle {
    last: Option<Instant>,
}

impl Throttle {
    /// `true` when at least `interval` elapsed since the last accepted call;
    /// accepted calls record `now`.
    pub fn allow(&mut self, now: Instant, interval: Duration) -> bool {
        if self.would_allow(now, interval) {
            self.last = Some(now);
            true
        } else {
            false
        }
    }

    /// Same decision as [`Self::allow`] without recording `now`.
    pub fn would_allow(&self, now: Instant, interval: Duration) -> bool {
        !self
            .last
            .is_some_and(|last| now.saturating_duration_since(last) < interval)
    }

    /// Forget the previous call; the next [`Self::allow`] passes immediately.
    pub fn reset(&mut self) {
        self.last = None;
    }
}

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

/// Vertical thumb geometry in one whole bar column, arrows included.
///
/// Input is the raw content offset (`0..=content - viewport`), not ratatui's
/// item position: the thumb and the inverse mapping are derived once from the
/// same numbers, so painting and hit testing cannot disagree. The movable track
/// maps its first and last cell to `0` and `max_offset` explicitly; the middle
/// keeps the proportional mapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VerticalBar {
    /// Track rows between the two arrows.
    pub track: usize,
    /// Thumb start, as a row offset inside the track.
    pub thumb_start: usize,
    /// Thumb height in rows; never zero and never taller than the track.
    pub thumb_len: usize,
    viewport: usize,
    content: usize,
    /// 原始内容偏移。`thumb_start` 是量化后的格子，同格拖拽要靠它保持不跳动。
    offset: usize,
}

impl VerticalBar {
    /// `None` when nothing overflows, the bar cannot hold a track row, or the
    /// viewport is empty.
    #[must_use]
    pub fn new(height: u16, viewport: usize, content: usize, offset: usize) -> Option<Self> {
        let track = usize::from(height).checked_sub(2)?;
        if track == 0 || viewport == 0 || content <= viewport {
            return None;
        }
        let max_offset = content - viewport;
        let thumb_len = round_divide(viewport.saturating_mul(track), content).clamp(1, track);
        let span = track - thumb_len;
        let thumb_start = if span == 0 {
            0
        } else {
            round_divide(offset.min(max_offset).saturating_mul(span), max_offset)
        };
        Some(Self {
            track,
            thumb_start,
            thumb_len,
            viewport,
            content,
            offset: offset.min(max_offset),
        })
    }

    /// Largest legal content offset (`content - viewport`).
    #[must_use]
    pub fn max_offset(&self) -> usize {
        self.content - self.viewport
    }

    /// Content offset for a thumb starting at `thumb_start` track cells.
    ///
    /// Both ends of the movable span map to `0` / `max_offset`; values outside
    /// clamp, so dragging past an end stops at the boundary. A full-track thumb
    /// has no movable span and reports `0`.
    #[must_use]
    pub fn offset_for_thumb_start(&self, thumb_start: usize) -> usize {
        let span = self.track - self.thumb_len;
        if span == 0 {
            return 0;
        }
        round_divide(
            thumb_start.min(span).saturating_mul(self.max_offset()),
            span,
        )
    }

    /// Hit test a row inside the whole bar area (arrows included).
    #[must_use]
    pub fn press(&self, area: Rect, row: u16) -> VerticalPress {
        let bottom = area.y.saturating_add(area.height).saturating_sub(1);
        if row == area.y {
            return VerticalPress::Top;
        }
        if row == bottom {
            return VerticalPress::Bottom;
        }
        let thumb_top = area
            .y
            .saturating_add(1)
            .saturating_add(self.thumb_start as u16);
        let thumb_end = thumb_top.saturating_add(self.thumb_len as u16);
        if row >= thumb_top && row < thumb_end {
            return VerticalPress::Thumb {
                grab: row.saturating_sub(thumb_top),
            };
        }
        let track_cell = row.saturating_sub(area.y).saturating_sub(1) as usize;
        // Centre the thumb under the pointer, matching a grab at its middle.
        let target_thumb_start = track_cell.saturating_sub(self.thumb_len / 2);
        VerticalPress::Track {
            offset: self.offset_for_thumb_start(target_thumb_start),
        }
    }

    /// Offset while dragging: `grab` is the row captured inside the thumb at
    /// press time, so the thumb keeps its relative position under the pointer.
    /// Moving off the scrollbar column does not change the row mapping.
    #[must_use]
    pub fn drag_offset(&self, area: Rect, row: u16, grab: u16) -> usize {
        let new_thumb_top = row.saturating_sub(grab);
        let track_cell = new_thumb_top.saturating_sub(area.y).saturating_sub(1) as usize;
        let span = self.track - self.thumb_len;
        if span == 0 {
            // 满轨 thumb 没有可移动空间——保持既有语义返回 0。
            return 0;
        }
        let target = track_cell.min(span);
        // 偏移→格子是有损映射：格子未变时保留原始偏移，避免同一格内按下即跳动。
        if target == self.thumb_start {
            self.offset
        } else {
            self.offset_for_thumb_start(target)
        }
    }

    /// Paint `▲`/`▼` ends plus the thumb block in the area's rightmost column,
    /// matching `ScrollbarOrientation::VerticalRight`. Track cells stay
    /// untouched, matching [`vertical_scrollbar`].
    pub fn paint(&self, area: Rect, buf: &mut Buffer, thumb: Style, arrow: Style) {
        if area.width == 0 || area.height < 2 {
            return;
        }
        let x = area.x.saturating_add(area.width).saturating_sub(1);
        buf.set_string(x, area.y, "▲", arrow);
        buf.set_string(x, area.y + area.height - 1, "▼", arrow);
        let last_track_row = area.y + area.height - 1;
        for cell in self.thumb_start..self.thumb_start + self.thumb_len {
            let row = area.y + 1 + cell as u16;
            if row >= last_track_row {
                break;
            }
            buf.set_string(x, row, " ", thumb);
        }
    }
}

/// What a press on the vertical bar means. The host applies `Top`/`Bottom`
/// itself (it owns follow-bottom state and paging).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerticalPress {
    /// ▲ (the top row): jump to the top.
    Top,
    /// ▼ (the last row): jump to the maximum offset.
    Bottom,
    /// The thumb: capture `grab` rows inside it; no jump on press.
    Thumb { grab: u16 },
    /// Empty track: offset with the thumb centred on the pointer.
    Track { offset: usize },
}

/// Round to nearest, like `ratatui`'s scrollbar internals.
fn round_divide(numerator: usize, denominator: usize) -> usize {
    numerator
        .saturating_add(denominator / 2)
        .checked_div(denominator)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::{Color, Style};

    use super::{
        HorizontalBar, Throttle, VerticalBar, VerticalPress, bounded_offset, horizontal_scrollbar,
    };

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

    // ── VerticalBar：原始偏移 + 绘制/命中同源 ───────────────────────────

    const LONG_CONTENT: usize = 1000;
    const LONG_VIEWPORT: usize = 10;

    fn vertical_area(height: u16) -> Rect {
        Rect::new(79, 0, 1, height)
    }

    fn vertical_bar(height: u16, content: usize, viewport: usize, offset: usize) -> VerticalBar {
        VerticalBar::new(height, viewport, content, offset).expect("overflowing bar")
    }

    fn painted_thumb_rows(buf: &Buffer, area: Rect) -> Vec<u16> {
        let thumb = Color::DarkGray;
        (area.y + 1..area.y + area.height - 1)
            .filter(|&row| buf[(area.x, row)].bg == thumb)
            .collect()
    }

    fn paint(bar: &VerticalBar, area: Rect) -> Buffer {
        let mut buf = Buffer::empty(area);
        bar.paint(
            area,
            &mut buf,
            Style::new().bg(Color::DarkGray),
            Style::new().fg(Color::Red),
        );
        buf
    }

    #[test]
    fn vertical_bar_needs_overflow_room_and_viewport() {
        assert_eq!(VerticalBar::new(10, 20, 20, 0), None, "no overflow");
        assert_eq!(VerticalBar::new(2, 10, 1000, 0), None, "no track rows");
        assert_eq!(VerticalBar::new(10, 0, 1000, 0), None, "empty viewport");
    }

    #[test]
    fn vertical_bar_maps_movable_track_ends_to_zero_and_max() {
        // 1000 rows, 10 visible, bar height 10 → track 8, thumb 1, span 7.
        let top = vertical_bar(10, LONG_CONTENT, LONG_VIEWPORT, 0);
        assert_eq!(top.track, 8);
        assert_eq!(top.thumb_len, 1);
        assert_eq!(top.thumb_start, 0);
        assert_eq!(top.max_offset(), 990);

        let bottom = vertical_bar(10, LONG_CONTENT, LONG_VIEWPORT, 990);
        assert_eq!(bottom.thumb_start, 7);
        assert_eq!(bottom.offset_for_thumb_start(0), 0);
        assert_eq!(bottom.offset_for_thumb_start(7), 990);
        assert_eq!(
            bottom.offset_for_thumb_start(usize::MAX),
            990,
            "drags past the last cell clamp to the bottom"
        );
    }

    #[test]
    fn vertical_bar_1000_rows_drag_to_last_cell_reaches_990() {
        let area = vertical_area(10);
        assert_eq!(LONG_CONTENT - LONG_VIEWPORT, 990, "acceptance raw offset");

        // Thumb at the top: the last track cell is empty track → jump to 990.
        let top = vertical_bar(area.height, LONG_CONTENT, LONG_VIEWPORT, 0);
        assert_eq!(
            top.press(area, area.y + area.height - 2),
            VerticalPress::Track { offset: 990 }
        );
        assert_eq!(
            top.press(area, area.y + area.height - 1),
            VerticalPress::Bottom
        );

        // Thumb already at the bottom: grabbing and dragging it stays at 990.
        let bottom = vertical_bar(area.height, LONG_CONTENT, LONG_VIEWPORT, 990);
        assert_eq!(
            bottom.press(area, area.y + area.height - 2),
            VerticalPress::Thumb { grab: 0 }
        );
        assert_eq!(bottom.drag_offset(area, area.y + area.height - 2, 0), 990);
    }

    #[test]
    fn vertical_bar_thumb_middle_press_does_not_jump() {
        // 100 rows, 20 visible, height 12 → track 10, thumb 2, span 8.
        let area = vertical_area(12);
        let offset = 40;
        let bar = vertical_bar(area.height, 100, 20, offset);
        assert_eq!((bar.thumb_start, bar.thumb_len), (4, 2));

        let rows = painted_thumb_rows(&paint(&bar, area), area);
        assert_eq!(rows, vec![area.y + 5, area.y + 6]);
        let pressed = rows[1];
        let VerticalPress::Thumb { grab } = bar.press(area, pressed) else {
            panic!("middle of the thumb must grab, not jump");
        };
        assert_eq!(grab, 1);
        assert_eq!(bar.offset_for_thumb_start(bar.thumb_start), offset);
        assert_eq!(
            bar.drag_offset(area, pressed, grab),
            offset,
            "a drag without movement keeps the offset"
        );

        // Drag along the same grab to the last track cell → real bottom.
        assert_eq!(bar.drag_offset(area, area.y + area.height - 2, grab), 80);

        // Moving the thumb up by two cells quantises downward, never out of bounds.
        let up = bar.drag_offset(area, pressed - 2, grab);
        assert!(up < offset && up <= 80);
    }

    #[test]
    fn vertical_bar_same_cell_drag_keeps_original_offset() {
        // 1000 行内容 / 10 行视口：offset 495 落在 thumb 格 4，但 4 反推是 566。
        // 同一格内按下并拖拽不得把偏移改成 566。
        let area = vertical_area(10);
        let bar = vertical_bar(area.height, LONG_CONTENT, LONG_VIEWPORT, 495);
        assert_eq!(bar.thumb_start, 4);
        let row = area.y + 1 + bar.thumb_start as u16;
        assert_eq!(bar.press(area, row), VerticalPress::Thumb { grab: 0 });
        assert_eq!(bar.drag_offset(area, row, 0), 495, "同格拖拽保持原偏移");
        // 移到下一格仍按比例映射。
        assert_eq!(
            bar.drag_offset(area, row + 1, 0),
            bar.offset_for_thumb_start(5)
        );
    }

    #[test]
    fn vertical_bar_tiny_and_full_track_heights_are_stable() {
        for height in 0..=2 {
            assert_eq!(VerticalBar::new(height, 10, LONG_CONTENT, 0), None);
        }
        // height 3 → one track cell, full thumb, no movable span.
        let area = vertical_area(3);
        let bar = vertical_bar(3, LONG_CONTENT, LONG_VIEWPORT, 500);
        assert_eq!((bar.track, bar.thumb_len, bar.thumb_start), (1, 1, 0));
        assert_eq!(bar.press(area, area.y), VerticalPress::Top);
        assert_eq!(
            bar.press(area, area.y + 1),
            VerticalPress::Thumb { grab: 0 }
        );
        assert_eq!(bar.press(area, area.y + 2), VerticalPress::Bottom);
        assert_eq!(bar.drag_offset(area, area.y + 1, 0), 0);

        // 11 rows in 10-row viewport, height 6 → track 4, thumb fills it.
        let bar = vertical_bar(6, 11, 10, 1);
        assert_eq!((bar.track, bar.thumb_len), (4, 4));
        assert_eq!(bar.offset_for_thumb_start(0), 0);
        assert_eq!(bar.offset_for_thumb_start(usize::MAX), 0);
    }

    #[test]
    fn vertical_bar_long_content_and_resize_keep_both_ends() {
        let max_offset = 1_000_000 - 10;
        for height in [10u16, 12, 24, 60] {
            let area = vertical_area(height);
            let top = vertical_bar(height, 1_000_000, 10, 0);
            assert_eq!(top.thumb_start, 0);
            let bottom = vertical_bar(height, 1_000_000, 10, max_offset);
            assert_eq!(bottom.thumb_start, bottom.track - bottom.thumb_len);
            assert_eq!(
                bottom.drag_offset(area, area.y + area.height - 2, 0),
                max_offset
            );
            // A mid-range offset survives a thumb round-trip at this height.
            let mid = vertical_bar(height, 1_000_000, 10, 500_000);
            let back = mid.offset_for_thumb_start(mid.thumb_start);
            assert!(back <= max_offset);
            assert_eq!(
                vertical_bar(height, 1_000_000, 10, back).thumb_start,
                mid.thumb_start
            );
        }
    }

    #[test]
    fn vertical_bar_paint_draws_arrows_and_painted_thumb_matches_hit() {
        let area = vertical_area(12);
        let bar = vertical_bar(area.height, 100, 20, 40);
        let buf = paint(&bar, area);
        assert_eq!(buf[(area.x, area.y)].symbol(), "▲");
        assert_eq!(buf[(area.x, area.y)].fg, Color::Red);
        assert_eq!(buf[(area.x, area.y + area.height - 1)].symbol(), "▼");
        assert_eq!(painted_thumb_rows(&buf, area), vec![area.y + 5, area.y + 6]);
        // Track cells outside the thumb stay untouched.
        assert_eq!(buf[(area.x, area.y + 1)].symbol(), " ");
        assert_ne!(buf[(area.x, area.y + 1)].bg, Color::DarkGray);
        for row in area.y + 1..area.y + area.height - 1 {
            let painted = buf[(area.x, row)].bg == Color::DarkGray;
            let hit = matches!(bar.press(area, row), VerticalPress::Thumb { .. });
            assert_eq!(painted, hit, "row {row} painted but not hittable");
        }
    }

    #[test]
    fn vertical_bar_paints_the_rightmost_column() {
        let area = Rect::new(0, 0, 3, 6);
        let bar = vertical_bar(6, 20, 10, 0);
        let buf = paint(&bar, area);
        assert_eq!(buf[(2, 0)].symbol(), "▲");
        assert_eq!(buf[(2, 5)].symbol(), "▼");
        for x in 0..2 {
            assert_eq!(buf[(x, 0)].symbol(), " ", "left columns stay untouched");
        }
    }

    #[test]
    fn bounded_offset_clamps_both_ends() {
        assert_eq!(bounded_offset(10, 3, 12), 12);
        assert_eq!(bounded_offset(10, 3, 100), 13);
        assert_eq!(bounded_offset(1, -3, 100), 0);
        assert_eq!(bounded_offset(100, -3, 100), 97);
        assert_eq!(bounded_offset(10, 0, 100), 10);
        assert_eq!(bounded_offset(120, 3, 100), 100, "stale offset is bounded");
        assert_eq!(
            bounded_offset(120, -3, 100),
            100,
            "stale offset stays bounded"
        );
    }

    #[test]
    fn throttle_only_reads_caller_time() {
        let start = Instant::now();
        let interval = Duration::from_millis(50);
        let mut throttle = Throttle::default();
        assert!(throttle.allow(start, interval), "first event passes");
        assert!(!throttle.allow(start + Duration::from_millis(10), interval));
        assert!(!throttle.allow(start + Duration::from_millis(49), interval));
        assert!(throttle.allow(start + Duration::from_millis(50), interval));
        assert!(!throttle.allow(start + Duration::from_millis(51), interval));
        assert!(
            !throttle.would_allow(start + Duration::from_millis(60), interval),
            "would_allow never records time"
        );
        assert!(throttle.would_allow(start + Duration::from_millis(100), interval));
        throttle.reset();
        assert!(throttle.allow(start + Duration::from_millis(52), interval));
    }
}
