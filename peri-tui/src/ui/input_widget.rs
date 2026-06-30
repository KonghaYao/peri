//! v2 InputWidget — renders [`InputState`] as a ratatui [`Widget`].
//!
//! Replaces `tui_textarea::TextArea` for rendering while keeping
//! [`InputState`] as the canonical data source. This is the foundation
//! for B3 MigrateInput: once rendering goes through InputWidget,
//! `UiState.textarea` can be incrementally removed.
//!
//! Features:
//! - Multi-line text with CJK-safe cursor positioning
//! - Selection highlighting (reverse video)
//! - Prediction text (dimmed suffix after cursor)
//! - Border + padding via ratatui [`Block`]
//! - Vertical scroll when content exceeds visible area
//! - Placeholder text when buffer is empty and unfocused

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line as TuiLine, Span},
    widgets::{Block, Widget},
};

use unicode_width::UnicodeWidthChar;

use crate::state_machine::input::{CursorPos, InputState};
use crate::ui::theme;

/// A ratatui Widget that renders an [`InputState`].
///
/// Usage:
/// ```ignore
/// let widget = InputWidget::new(&input_state)
///     .block(Block::default().borders(Borders::TOP | Borders::BOTTOM))
///     .show_cursor(true);
/// f.render_widget(widget, area);
/// ```
#[derive(Debug, Clone)]
pub struct InputWidget<'a> {
    /// Reference to the input state to render.
    pub input: &'a InputState,

    /// Visual block (border + padding).
    pub block: Block<'a>,

    /// Text style for normal content.
    pub text_style: Style,

    /// Cursor style (applied to the character at the cursor position).
    pub cursor_style: Style,

    /// Selection highlight style.
    pub selection_style: Style,

    /// Prediction text style (greyed-out suggestion).
    pub prediction_style: Style,

    /// Placeholder text shown when buffer is empty.
    pub placeholder: Option<String>,

    /// Placeholder text style.
    pub placeholder_style: Style,

    /// Whether to render the cursor.
    pub show_cursor: bool,
}

impl<'a> InputWidget<'a> {
    /// Create a new InputWidget from an InputState reference.
    pub fn new(input: &'a InputState) -> Self {
        Self {
            input,
            block: Block::default(),
            text_style: Style::default().fg(theme::TEXT),
            cursor_style: Style::default()
                .fg(theme::TEXT)
                .bg(theme::SELECTION_BG)
                .add_modifier(Modifier::BOLD),
            selection_style: Style::default().fg(theme::TEXT).bg(theme::SELECTION_BG),
            prediction_style: Style::default().fg(theme::MUTED),
            placeholder: None,
            placeholder_style: Style::default().fg(theme::MUTED),
            show_cursor: false,
        }
    }

    /// Set the block (border + padding).
    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = block;
        self
    }

    /// Set whether to show the cursor.
    pub fn show_cursor(mut self, show: bool) -> Self {
        self.show_cursor = show;
        self
    }

    /// Set placeholder text (shown when buffer is empty and unfocused).
    pub fn placeholder(mut self, text: impl Into<String>) -> Self {
        self.placeholder = Some(text.into());
        self
    }

    /// Get the inner area after applying block borders and padding.
    pub fn inner_area(&self, area: Rect) -> Rect {
        self.block.inner(area)
    }

    /// Calculate the visual column for a byte position in a line.
    /// Uses unicode-width for CJK safety.
    #[allow(dead_code)]
    fn visual_column(line: &str, byte_pos: usize) -> u16 {
        let pos = byte_pos.min(line.len());
        line[..pos]
            .chars()
            .map(|c| c.width().unwrap_or(0) as u16)
            .sum()
    }

    /// Render the widget.
    fn render_content(&self, area: Rect, buf: &mut Buffer) {
        let lines = &self.input.lines;
        let cursor = &self.input.cursor;
        // Determine the normalized selection range.
        let selection_range: Option<(CursorPos, CursorPos)> =
            self.input.selection.as_ref().map(|sel| {
                let r = sel.range();
                (
                    CursorPos::new(r.start_row, r.start_col),
                    CursorPos::new(r.end_row, r.end_col),
                )
            });

        // Compute vertical scroll: ensure cursor row is visible.
        let visible_rows = area.height as usize;
        let scroll = if cursor.row >= self.first_visible_row(visible_rows) {
            cursor.row.saturating_sub(visible_rows.saturating_sub(1))
        } else if cursor.row < self.first_visible_row(0) {
            0 // reset logic in first_visible_row handles this
        } else {
            0 // FIXME: track persistent scroll offset via InputState field
        };

        for (i, line_text) in lines.iter().enumerate().skip(scroll) {
            let row = area.y + (i - scroll) as u16;
            if row >= area.bottom() {
                break;
            }

            // Build spans for this line: [text before selection] [selection] [text after] [prediction]
            let spans = self.build_line_spans(line_text, i, selection_range, cursor);
            let line_width = spans.iter().map(|s| s.width() as u16).sum::<u16>();

            // Pad to fill the line area width to prevent visual artifacts.
            let padding = area.width.saturating_sub(line_width);
            let mut final_spans = spans;
            if padding > 0 {
                final_spans.push(Span::styled(" ".repeat(padding as usize), self.text_style));
            }

            let tui_line = TuiLine::from(final_spans);
            buf.set_line(area.x, row, &tui_line, area.width);
        }

        // Placeholder: if buffer is empty and no cursor shown
        if lines.len() == 1 && lines[0].is_empty() && !self.show_cursor {
            if let Some(ref placeholder) = self.placeholder {
                let span = Span::styled(placeholder.as_str(), self.placeholder_style);
                buf.set_span(area.x, area.y, &span, area.width);
            }
        }
    }

    /// Build styled spans for a single line, applying selection and cursor highlighting.
    fn build_line_spans(
        &self,
        line: &str,
        row: usize,
        selection: Option<(CursorPos, CursorPos)>,
        cursor: &CursorPos,
    ) -> Vec<Span<'static>> {
        let mut spans: Vec<Span<'static>> = Vec::new();

        // Determine the selection range for this row.
        let (sel_start_byte, sel_end_byte) = match selection {
            Some((start, end)) if row >= start.row && row <= end.row => {
                let s = if row == start.row { start.col_byte } else { 0 };
                let e = if row == end.row {
                    end.col_byte
                } else {
                    line.len()
                };
                // Ensure byte positions are on char boundaries.
                let s = CursorPos::snap_col_to_char_boundary(line, s.min(line.len()));
                let e = CursorPos::snap_col_to_char_boundary(line, e.min(line.len()));
                if s < e {
                    (s, e)
                } else {
                    (e, s)
                }
            }
            _ => (line.len(), line.len()), // no selection on this row
        };

        // Cursor position on this row.
        let cursor_byte = if cursor.row == row {
            cursor.col_byte
        } else {
            line.len() + 1
        };
        let cursor_byte = CursorPos::snap_col_to_char_boundary(line, cursor_byte.min(line.len()));

        // Build segments: [before_sel] [sel] [after_sel]
        // Cursor may fall in any segment.
        let segments = [
            (0usize, sel_start_byte.min(line.len())),
            (sel_start_byte.min(line.len()), sel_end_byte.min(line.len())),
            (sel_end_byte.min(line.len()), line.len()),
        ];
        for &(start, end) in &segments {
            if start >= end {
                continue;
            }
            let slice = &line[start..end];
            let mut char_offset = 0usize;
            for (ci, c) in slice.char_indices() {
                let abs_byte = start + ci;
                let style = if self.show_cursor && abs_byte == cursor_byte {
                    // Cursor highlight: only highlight the char under cursor.
                    self.cursor_style
                } else {
                    // Use selection style if within selection range.
                    if abs_byte >= sel_start_byte && abs_byte < sel_end_byte {
                        self.selection_style
                    } else {
                        self.text_style
                    }
                };

                let mut ch_str = c.to_string();
                // Handle newline characters (shouldn't appear in single line, but be safe)
                if c == '\n' {
                    ch_str = " ".to_string();
                }
                spans.push(Span::styled(ch_str, style));
                char_offset = ci + c.len_utf8();
            }
            // If cursor is at end of line (cursor_byte == line.len()), add a cursor-width space.
            // But not if it's within a segment.
            // Actually handle this after the loop.
            let _ = char_offset;
        }

        // Cursor at end of line: append a highlighted space.
        if self.show_cursor && cursor.row == row && cursor_byte >= line.len() {
            spans.push(Span::styled(" ", self.cursor_style));
        }

        // Prediction text: only on the cursor row, after the line content.
        if cursor.row == row && cursor_byte >= line.len() {
            if let Some(ref pred) = self.input.prediction {
                if !pred.is_empty() {
                    spans.push(Span::styled(pred.clone(), self.prediction_style));
                }
            }
        }

        spans
    }

    /// Compute the first visible row based on scroll state.
    /// For now, cursor-driven scroll: keep cursor in view.
    fn first_visible_row(&self, _visible_rows: usize) -> usize {
        // Simple: always start from row 0 for now.
        // Full scroll tracking will be added when InputState gains a scroll_offset field.
        0
    }
}

impl<'a> Widget for InputWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // Render the block (borders + background).
        self.block.clone().render(area, buf);

        let inner = self.block.inner(area);
        if inner.width == 0 || inner.height == 0 {
            return;
        }

        self.render_content(inner, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::widgets::{Borders, Padding};

    fn make_input(text: &str) -> InputState {
        let mut s = InputState::default();
        if !text.is_empty() {
            s.clear_buffer();
            s.insert_str(text);
        }
        s
    }

    #[test]
    fn test_widget_creation() {
        let input = make_input("hello");
        let widget = InputWidget::new(&input)
            .block(Block::default().borders(Borders::ALL))
            .show_cursor(true);
        assert!(widget.show_cursor);
        assert_eq!(widget.input.text(), "hello");
    }

    #[test]
    fn test_visual_column_ascii() {
        assert_eq!(InputWidget::visual_column("hello", 3), 3);
    }

    #[test]
    fn test_visual_column_cjk() {
        // "你好" — each char is 2 columns wide
        assert_eq!(InputWidget::visual_column("你好", 3), 2); // after first char
        assert_eq!(InputWidget::visual_column("你好", 6), 4); // after both chars
    }

    #[test]
    fn test_inner_area_respects_block() {
        let input = make_input("");
        let block = Block::default()
            .borders(Borders::ALL)
            .padding(Padding::new(2, 0, 1, 1));
        let widget = InputWidget::new(&input).block(block);
        let outer = Rect::new(0, 0, 80, 24);
        let inner = widget.inner_area(outer);
        // borders = 1 each side → width: 80-2=78, height: 24-2=22
        // padding = left 2, right 0, top 1, bottom 1
        // inner.width = 78-2=76, inner.height = 22-2=20
        assert_eq!(inner.width, 76);
        assert_eq!(inner.height, 20);
    }

    #[test]
    fn test_placeholder_when_empty() {
        let input = make_input("");
        let widget = InputWidget::new(&input)
            .placeholder("Type a message...")
            .show_cursor(false);
        assert!(widget.placeholder.is_some());
    }

    #[test]
    fn test_build_line_spans_no_cursor() {
        let input = make_input("hello");
        let widget = InputWidget::new(&input).show_cursor(false);
        let spans = widget.build_line_spans("hello", 0, None, &CursorPos::default());
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "hello");
    }

    #[test]
    fn test_build_line_spans_with_cursor() {
        let input = make_input("hello");
        let widget = InputWidget::new(&input).show_cursor(true);
        // cursor at (0, 1) — byte 1 is 'e'
        let spans = widget.build_line_spans("hello", 0, None, &CursorPos::new(0, 1));
        // Should have: 'h' (normal), 'e' (cursor style), "llo" (normal)
        assert!(spans.len() >= 3);
    }

    #[test]
    fn test_build_line_spans_cursor_at_end() {
        let input = make_input("hi");
        let widget = InputWidget::new(&input).show_cursor(true);
        // cursor at end of line
        let spans = widget.build_line_spans("hi", 0, None, &CursorPos::new(0, 2));
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        // "hi" + cursor space
        assert!(text.contains("hi"));
        assert!(spans.iter().any(|s| s.style == widget.cursor_style));
    }
}
