//! High-level editing operations for InputState.
//!
//! All cursor movements are CJK-aware — they use char-level traversal rather than
//! byte indexing. Indices (row, col) use byte offset for the column, matching
//! `CursorPos` contract.

use super::{CursorPos, InputState};

/// Editing operations on the input buffer.
pub trait InputEdit {
    /// Start tracking a selection at the current cursor position.
    fn start_selection(&mut self);

    /// Select all text.
    fn select_all(&mut self);

    /// Move cursor left by one character. `extend_selection` extends (Shift+Left).
    fn move_cursor_left(&mut self, extend_selection: bool);

    /// Move cursor right by one character. `extend_selection` extends (Shift+Right).
    fn move_cursor_right(&mut self, extend_selection: bool);

    /// Move cursor to the start of the current line.
    fn move_cursor_home(&mut self, extend_selection: bool);

    /// Move cursor to the end of the current line.
    fn move_cursor_end(&mut self, extend_selection: bool);

    /// Move cursor up one line (if possible).
    fn move_cursor_up(&mut self, extend_selection: bool);

    /// Move cursor down one line (if possible).
    fn move_cursor_down(&mut self, extend_selection: bool);

    /// Delete the current selection if any, otherwise delete one word backward.
    fn delete_word(&mut self);

    /// Delete from cursor to the start of the line.
    fn delete_line_by_head(&mut self);
}

impl InputEdit for InputState {
    fn start_selection(&mut self) {
        self.selection = Some(super::Selection::normal(
            self.cursor.row,
            self.cursor.col_byte,
            self.cursor.row,
            self.cursor.col_byte,
        ));
    }

    fn select_all(&mut self) {
        self.cursor = CursorPos::new(
            self.lines.len().saturating_sub(1),
            self.lines.last().map(|s| s.len()).unwrap_or(0),
        );
        self.selection = Some(super::Selection::normal(
            0,
            0,
            self.cursor.row,
            self.cursor.col_byte,
        ));
    }

    fn move_cursor_left(&mut self, extend_selection: bool) {
        let CursorPos { row, col_byte } = self.cursor;
        let (new_row, new_col) = if col_byte > 0 {
            // Move back one char within the current line.
            let line = &self.lines[row];
            let prev_byte = line[..col_byte]
                .char_indices()
                .last()
                .map(|(b, _)| b)
                .unwrap_or(0);
            (row, prev_byte)
        } else if row > 0 {
            // Move to end of previous line.
            let prev_len = self.lines[row - 1].len();
            (row - 1, prev_len)
        } else {
            (row, col_byte)
        };

        self.cursor = CursorPos::new(new_row, new_col);
        if extend_selection {
            self.extend_selection_to_cursor();
        } else {
            self.selection = None;
        }
    }

    fn move_cursor_right(&mut self, extend_selection: bool) {
        let CursorPos { row, col_byte } = self.cursor;
        let line = &self.lines[row];
        let (new_row, new_col) = if col_byte < line.len() {
            // Advance by one char within the line.
            let next_byte = line[col_byte..]
                .chars()
                .next()
                .map(|c| col_byte + c.len_utf8())
                .unwrap_or(line.len());
            (row, next_byte)
        } else if row + 1 < self.lines.len() {
            // Move to start of next line.
            (row + 1, 0)
        } else {
            (row, col_byte)
        };

        self.cursor = CursorPos::new(new_row, new_col);
        if extend_selection {
            self.extend_selection_to_cursor();
        } else {
            self.selection = None;
        }
    }

    fn move_cursor_home(&mut self, extend_selection: bool) {
        self.cursor = CursorPos::new(self.cursor.row, 0);
        if extend_selection {
            self.extend_selection_to_cursor();
        } else {
            self.selection = None;
        }
    }

    fn move_cursor_end(&mut self, extend_selection: bool) {
        let row = self.cursor.row;
        let len = self.lines[row].len();
        self.cursor = CursorPos::new(row, len);
        if extend_selection {
            self.extend_selection_to_cursor();
        } else {
            self.selection = None;
        }
    }

    fn move_cursor_up(&mut self, extend_selection: bool) {
        if self.cursor.row > 0 {
            let new_row = self.cursor.row - 1;
            let col = self.cursor.col_byte.min(self.lines[new_row].len());
            self.cursor = CursorPos::new(new_row, col);
        }
        if extend_selection {
            self.extend_selection_to_cursor();
        } else {
            self.selection = None;
        }
    }

    fn move_cursor_down(&mut self, extend_selection: bool) {
        if self.cursor.row + 1 < self.lines.len() {
            let new_row = self.cursor.row + 1;
            let col = self.cursor.col_byte.min(self.lines[new_row].len());
            self.cursor = CursorPos::new(new_row, col);
        }
        if extend_selection {
            self.extend_selection_to_cursor();
        } else {
            self.selection = None;
        }
    }

    fn delete_word(&mut self) {
        // CJK-safe: treat each CJK character as its own word boundary.
        // ASCII words are separated by spaces/punctuation.
        let CursorPos { row, col_byte } = self.cursor;
        let line = &self.lines[row];
        if col_byte == 0 {
            // Already at start, nothing to delete.
            return;
        }
        let before = &line[..col_byte];
        let chars: Vec<(usize, char)> = before.char_indices().collect();
        if chars.is_empty() {
            return;
        }
        let last_char = chars.last().unwrap().1;
        // CJK char: delete one char; ASCII: delete one word.
        let delete_start = if last_char > '\u{7f}' || is_punct(last_char) {
            chars.last().map(|(b, _)| *b).unwrap_or(0)
        } else {
            // Scan backward through non-space ASCII chars.
            let mut i = chars.len();
            while i > 0 {
                i -= 1;
                let (b, ch) = chars[i];
                if ch.is_ascii_whitespace() {
                    // Delete the word after the space.
                    return self.delete_from_to(b + ch.len_utf8(), col_byte, row);
                }
            }
            0
        };
        self.delete_from_to(delete_start, col_byte, row);
    }

    fn delete_line_by_head(&mut self) {
        let row = self.cursor.row;
        self.delete_from_to(0, self.cursor.col_byte, row);
    }
}

impl InputState {
    /// Internal helper: delete text from `from_byte` to `to_byte` on line `row`.
    fn delete_from_to(&mut self, from_byte: usize, to_byte: usize, row: usize) {
        let line = &mut self.lines[row];
        line.replace_range(from_byte..to_byte, "");
        self.cursor = CursorPos::new(row, from_byte);
    }

    /// Extend current selection to include the current cursor position.
    fn extend_selection_to_cursor(&mut self) {
        if let Some(ref mut sel) = self.selection {
            sel.cursor_row = self.cursor.row;
            sel.cursor_col = self.cursor.col_byte;
        } else {
            self.start_selection();
        }
    }
}

fn is_punct(c: char) -> bool {
    c.is_ascii_punctuation() || c.is_ascii_whitespace()
}
