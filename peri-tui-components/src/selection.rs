//! Display-column selection helpers for terminal text.
//!
//! A selection is expressed as two visual `(row, column)` points. This module
//! owns the point type, the column→byte conversion and the grapheme-safe slices
//! used by both highlighting and copying. Message identity, re-anchoring after
//! history prepend and the clipboard stay in the application.

use std::ops::Range;

use ratatui::text::Line;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// A point in wrapped content space.
///
/// The row is unbounded (`usize`: content may exceed `u16::MAX` visual rows);
/// the column is a terminal display column (`u16`: never wider than the screen).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SelectionPoint {
    pub row: usize,
    pub column: u16,
}

impl SelectionPoint {
    #[must_use]
    pub const fn new(row: usize, column: u16) -> Self {
        Self { row, column }
    }

    /// Order two endpoints so rendering and extraction agree on direction.
    #[must_use]
    pub fn ordered(a: Self, b: Self) -> (Self, Self) {
        if a <= b { (a, b) } else { (b, a) }
    }
}

impl From<(usize, u16)> for SelectionPoint {
    fn from((row, column): (usize, u16)) -> Self {
        Self::new(row, column)
    }
}

impl From<SelectionPoint> for (usize, u16) {
    fn from(point: SelectionPoint) -> Self {
        (point.row, point.column)
    }
}

/// Concatenate all span text of a line, ignoring styles.
#[must_use]
pub fn line_plain_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<Vec<_>>()
        .concat()
}

/// Byte offset for a display column inside `text`.
///
/// Columns are measured with grapheme clusters, matching ratatui's rendering.
/// A pointer on the left half of a wide grapheme stays before it; the right
/// half has passed it, so the returned offset includes the whole grapheme.
#[must_use]
pub fn visual_col_to_byte_offset(text: &str, target_col: u16) -> usize {
    let mut col = 0u16;
    for (byte, grapheme) in text.grapheme_indices(true) {
        let width = UnicodeWidthStr::width(grapheme) as u16;
        if width > 0 && target_col < col + width {
            let half = col + width.div_ceil(2);
            return if target_col >= half {
                byte + grapheme.len()
            } else {
                byte
            };
        }
        col += width;
    }
    text.len()
}

/// Start byte for a selection endpoint: any column inside a grapheme returns
/// that grapheme's start, so ZWJ sequences and combining marks are never split.
#[must_use]
pub fn row_start_byte(text: &str, row_start_byte: usize, col_in_row: u16) -> usize {
    let head = text.split_at(row_start_byte).1;
    let mut col = 0u16;
    for (offset, grapheme) in head.grapheme_indices(true) {
        let width = UnicodeWidthStr::width(grapheme) as u16;
        if width > 0 && col_in_row < col + width {
            return row_start_byte + offset;
        }
        col += width;
    }
    row_start_byte + head.len()
}

/// End byte for a selection endpoint. See [`visual_col_to_byte_offset`] for the
/// half-character rule.
#[must_use]
pub fn row_end_byte(text: &str, row_start_byte: usize, col_in_row: u16) -> usize {
    row_start_byte + visual_col_to_byte_offset(text.split_at(row_start_byte).1, col_in_row)
}

/// Byte starts of every visual row produced by `Paragraph`'s word wrap.
///
/// The mapping is derived from the real rendered buffer instead of assuming
/// `column = row * width`: ratatui wraps at word boundaries, CJK text has no
/// spaces, and rows differ in used width. Result length is rows + 1; the last
/// entry is `plain.len()`. Content beyond `u16::MAX` rows falls back to a
/// width-based walk because a `Buffer` cannot address those rows.
#[must_use]
pub fn wrap_row_byte_starts(line: &Line<'_>, plain: &str, width: u16) -> Vec<usize> {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::text::Text;
    use ratatui::widgets::{Paragraph, Widget, Wrap};

    let width = width.max(1);
    if plain.is_empty() {
        return vec![0];
    }
    let line_count = Paragraph::new(Text::from(line.clone()))
        .wrap(Wrap { trim: false })
        .line_count(width);
    if line_count <= 1 {
        return vec![0, plain.len()];
    }
    if line_count > u16::MAX as usize {
        return wrap_row_byte_starts_large(plain, usize::from(width));
    }

    let height = u16::try_from(line_count).expect("line count checked against u16::MAX");
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    Paragraph::new(Text::from(line.clone()))
        .wrap(Wrap { trim: false })
        .render(area, &mut buf);

    // Grapheme byte offsets share ratatui's grapheme boundary semantics.
    let graphemes: Vec<&str> = plain.graphemes(true).collect();
    let mut grapheme_bytes: Vec<usize> = Vec::with_capacity(graphemes.len() + 1);
    grapheme_bytes.push(0);
    let mut byte_off = 0usize;
    for grapheme in &graphemes {
        byte_off += grapheme.len();
        grapheme_bytes.push(byte_off);
    }

    let mut starts: Vec<usize> = Vec::with_capacity(line_count + 1);
    starts.push(0);
    let mut consumed = 0usize;
    let total = graphemes.len();

    for row in 0..height.saturating_sub(1) {
        for col in 0..width {
            if consumed >= total {
                break;
            }
            let Some(cell) = buf.cell((col, row)) else {
                continue;
            };
            let symbol = cell.symbol();
            if symbol.is_empty() {
                continue; // continuation cell of a wide grapheme
            }
            let next = graphemes[consumed];
            if symbol == next {
                consumed += 1;
            } else if !symbol.chars().all(char::is_whitespace) && next.starts_with(symbol) {
                // Some paths write only the grapheme's first scalar; width still
                // consumes the whole cluster, so do not split inside it.
                consumed += 1;
            }
            // Else: trailing padding space, not part of the content.
        }
        // Ratatui 在单词换行边界会丢弃分隔空白（即使 `trim: false`），例如
        // "hello world" 在宽度 5 下第二行从 `world` 开始。只按 cell 匹配会让
        // 下一行起点停在未渲染的空格上——对齐到下一行实际渲染的首个字符。
        if consumed < total {
            let next_symbol = (0..width).find_map(|col| {
                buf.cell((col, row + 1))
                    .map(|cell| cell.symbol())
                    .filter(|symbol| !symbol.is_empty())
            });
            if let Some(symbol) = next_symbol {
                while consumed < total
                    && graphemes[consumed] != symbol
                    && graphemes[consumed].chars().all(char::is_whitespace)
                {
                    consumed += 1;
                }
            }
        }
        starts.push(*grapheme_bytes.get(consumed).unwrap_or(&plain.len()));
    }
    starts.push(plain.len());
    starts
}

fn wrap_row_byte_starts_large(plain: &str, width: usize) -> Vec<usize> {
    let mut starts = vec![0];
    let mut row_width = 0usize;
    for (byte, grapheme) in plain.grapheme_indices(true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if row_width > 0 && row_width.saturating_add(grapheme_width) > width {
            starts.push(byte);
            row_width = 0;
        }
        row_width = row_width.saturating_add(grapheme_width);
    }
    starts.push(plain.len());
    starts
}

/// Byte range selected on one logical line.
///
/// `line_rows` is the line's visual row span; `start`/`end` are ordered points.
/// Returns `None` when the selection does not touch this line or the visible
/// range is empty, so callers keep the line unchanged.
#[must_use]
pub fn line_byte_range(
    plain: &str,
    row_starts: &[usize],
    line_rows: Range<usize>,
    start: SelectionPoint,
    end: SelectionPoint,
) -> Option<Range<usize>> {
    if row_starts.is_empty() || line_rows.is_empty() {
        return None;
    }
    let start_in_line = line_rows.contains(&start.row);
    let end_in_line = line_rows.contains(&end.row);
    if !start_in_line && !end_in_line {
        // A line strictly between the two endpoints is selected in full. A line
        // outside the ordered range on either side is untouched.
        let strictly_between = start.row < line_rows.start && end.row >= line_rows.end;
        return (strictly_between && !plain.is_empty()).then_some(0..plain.len());
    }
    let last_row = row_starts.len() - 1;
    let start_row = start.row.saturating_sub(line_rows.start).min(last_row);
    let end_row = end.row.saturating_sub(line_rows.start).min(last_row);

    let (begin, finish) = match (start_in_line, end_in_line) {
        (true, true) => (
            row_start_byte(plain, row_starts[start_row], start.column),
            row_end_byte(plain, row_starts[end_row], end.column),
        ),
        (true, false) => (
            row_start_byte(plain, row_starts[start_row], start.column),
            plain.len(),
        ),
        (false, true) => (0, row_end_byte(plain, row_starts[end_row], end.column)),
        (false, false) => unreachable!("at least one endpoint is inside the line"),
    };
    (begin < finish).then_some(begin..finish)
}

#[cfg(test)]
mod tests {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::text::{Line, Text};
    use ratatui::widgets::{Paragraph, Widget, Wrap};

    use super::{
        SelectionPoint, line_byte_range, line_plain_text, row_end_byte, row_start_byte,
        visual_col_to_byte_offset, wrap_row_byte_starts,
    };

    fn line(text: &str) -> Line<'static> {
        Line::from(text.to_string())
    }

    fn rendered_rows(line: &Line<'_>, width: u16, height: u16) -> Vec<String> {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        Paragraph::new(Text::from(line.clone()))
            .wrap(Wrap { trim: false })
            .render(area, &mut buf);
        (0..height)
            .map(|y| {
                (0..width)
                    .filter_map(|x| buf.cell((x, y)))
                    .filter(|cell| !cell.symbol().is_empty() && cell.symbol() != " ")
                    .map(|cell| cell.symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn points_order_by_row_then_column() {
        let a = SelectionPoint::new(2, 5);
        let b = SelectionPoint::new(1, 9);
        assert_eq!(SelectionPoint::ordered(a, b), (b, a));
        assert_eq!(SelectionPoint::ordered(a, a), (a, a));
        let point: SelectionPoint = (usize::MAX, 7).into();
        assert_eq!(point, SelectionPoint::new(usize::MAX, 7));
        let tuple: (usize, u16) = point.into();
        assert_eq!(tuple, (usize::MAX, 7));
    }

    #[test]
    fn wide_graphemes_map_by_half_character() {
        let text = "a你b";
        assert_eq!(visual_col_to_byte_offset(text, 0), 0, "left of 'a'");
        assert_eq!(visual_col_to_byte_offset(text, 1), 1, "left half of '你'");
        assert_eq!(
            visual_col_to_byte_offset(text, 2),
            4,
            "right half includes it"
        );
        assert_eq!(visual_col_to_byte_offset(text, 3), 4, "'b' passed");
        assert_eq!(
            visual_col_to_byte_offset(text, 8),
            text.len(),
            "past the end"
        );
    }

    #[test]
    fn zwj_sequence_is_one_grapheme() {
        let family = "👨‍👩‍👧‍👦";
        let text = format!("{family}abcdef");
        let bytes = family.len();
        assert_eq!(visual_col_to_byte_offset(&text, 0), 0);
        assert_eq!(visual_col_to_byte_offset(&text, 1), bytes);
        assert_eq!(visual_col_to_byte_offset(&text, 2), bytes);
        assert_eq!(visual_col_to_byte_offset(&text, 5), bytes + 3);
        assert_eq!(
            row_start_byte(&text, 0, 1),
            0,
            "start never splits the cluster"
        );
        assert_eq!(row_end_byte(&text, 0, 1), bytes, "end includes the cluster");
    }

    #[test]
    fn wrap_starts_match_real_paragraph_word_wrap() {
        let line = line("alpha beta gamma");
        let plain = line_plain_text(&line);
        let width = 8;
        let rows = rendered_rows(&line, width, 3);
        assert_eq!(rows, vec!["alpha", "beta", "gamma"], "fixture sanity");
        let starts = wrap_row_byte_starts(&line, &plain, width);
        assert_eq!(starts.len(), 4, "one start per visual row plus the end");
        for pair in starts.windows(2) {
            assert!(pair[0] <= pair[1], "starts are monotonic");
        }
        assert_eq!(plain[starts[0]..starts[1]].trim(), "alpha");
        assert_eq!(plain[starts[1]..starts[2]].trim(), "beta");
        assert_eq!(plain[starts[2]..starts[3]].trim(), "gamma");
        assert_eq!(starts[3], plain.len());

        // Selecting from the second visual row to the third yields "beta\ngamma".
        let start = SelectionPoint::new(0, 2);
        let end = SelectionPoint::new(2, 3);
        let first = line_byte_range(&plain, &starts, 0..3, start, end).expect("range");
        assert_eq!(&plain[first], "pha beta gam");
    }

    #[test]
    fn wrap_start_skips_space_dropped_at_word_boundary() {
        // Ratatui 的 word wrap 去掉行尾分隔空格：宽度 5 下 "hello world"
        // 的第二行只渲染 "world"，起点不能停在那个空格上。
        let line = line("hello world");
        let plain = line_plain_text(&line);
        let width = 5;
        assert_eq!(rendered_rows(&line, width, 2), vec!["hello", "world"]);
        let starts = wrap_row_byte_starts(&line, &plain, width);
        assert_eq!(&plain[starts[1]..starts[2]], "world", "第二行从首字符开始");
        let range = line_byte_range(
            &plain,
            &starts,
            0..2,
            SelectionPoint::new(1, 0),
            SelectionPoint::new(1, 5),
        )
        .expect("row 1 selection");
        assert_eq!(&plain[range], "world", "整行选择不带上丢弃的空格");
    }

    #[test]
    fn wrap_starts_match_real_paragraph_for_wide_text() {
        let line = line("你好世界");
        let plain = line_plain_text(&line);
        let width = 4;
        let rows = rendered_rows(&line, width, 2);
        assert_eq!(rows, vec!["你好", "世界"]);
        let starts = wrap_row_byte_starts(&line, &plain, width);
        assert_eq!(starts.len(), 3);
        assert_eq!(&plain[starts[0]..starts[1]], "你好");
        assert_eq!(&plain[starts[1]..starts[2]], "世界");

        // Cross-row selection: end of the first row through the second row.
        let range = line_byte_range(
            &plain,
            &starts,
            0..2,
            SelectionPoint::new(0, 2),
            SelectionPoint::new(1, 2),
        )
        .expect("range");
        assert_eq!(&plain[range], "好世");
    }

    #[test]
    fn line_byte_range_covers_whole_line_and_rejects_empty() {
        let line = line("abcdef");
        let plain = line_plain_text(&line);
        let starts = wrap_row_byte_starts(&line, &plain, 20);
        // Line spans rows 10..11; the selection covers it completely.
        let range = line_byte_range(
            &plain,
            &starts,
            10..11,
            SelectionPoint::new(9, 0),
            SelectionPoint::new(12, 0),
        )
        .expect("whole line");
        assert_eq!(range, 0..6);
        // Selection elsewhere does not touch this line.
        assert_eq!(
            line_byte_range(
                &plain,
                &starts,
                10..11,
                SelectionPoint::new(0, 0),
                SelectionPoint::new(3, 0),
            ),
            None
        );
        // Zero-width selection stays empty.
        assert_eq!(
            line_byte_range(
                &plain,
                &starts,
                10..11,
                SelectionPoint::new(10, 2),
                SelectionPoint::new(10, 2),
            ),
            None
        );
    }

    #[test]
    fn over_u16_max_visual_rows_fall_back_to_width_wrapping() {
        let plain = "a".repeat(70_000);
        let line = line(&plain);
        let starts = wrap_row_byte_starts(&line, &plain, 1);
        assert_eq!(starts.len(), 70_001, "one row per ASCII column");
        assert_eq!(starts[1], 1);
        assert_eq!(*starts.last().expect("end"), plain.len());
    }
}
