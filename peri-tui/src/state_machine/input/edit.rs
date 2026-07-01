//! High-level editing operations for InputState.
//!
//! All cursor movements are CJK-aware — they use char-level traversal rather than
//! byte indexing. Indices (row, col) use byte offset for the column, matching
//! `CursorPos` contract.

use super::{CursorPos, InputState};
use crate::runtime::effect::Effect;

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
            let col =
                CursorPos::snap_col_to_char_boundary(&self.lines[new_row], self.cursor.col_byte);
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
            let col =
                CursorPos::snap_col_to_char_boundary(&self.lines[new_row], self.cursor.col_byte);
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

// ── Phase 1: v2 InputState methods with Effect return ────────────────────────

impl InputState {
    /// Insert a single character at cursor position.
    /// If a selection is active, it is deleted first.
    pub fn type_char(&mut self, ch: char) -> Vec<Effect> {
        if self.selection.is_some() {
            self.delete_selection();
        }
        self.insert_str(&ch.to_string());
        vec![Effect::Render]
    }

    /// Delete char before cursor (Backspace).
    /// If a selection is active, it is deleted instead.
    pub fn delete_prev_char(&mut self) -> Vec<Effect> {
        if self.selection.is_some() {
            self.delete_selection();
            return vec![Effect::Render];
        }
        self.backspace();
        vec![Effect::Render]
    }

    /// Delete char at cursor (Delete key).
    /// If a selection is active, it is deleted instead.
    pub fn delete_next_char(&mut self) -> Vec<Effect> {
        if self.selection.is_some() {
            self.delete_selection();
            return vec![Effect::Render];
        }
        let CursorPos { row, col_byte } = self.cursor;
        let line_len = self.lines[row].len();
        if col_byte < line_len {
            // CJK-safe: advance by one char and delete the range.
            let next_byte = self.lines[row][col_byte..]
                .chars()
                .next()
                .map(|c| col_byte + c.len_utf8())
                .unwrap_or(line_len);
            self.lines[row].replace_range(col_byte..next_byte, "");
        } else if row + 1 < self.lines.len() {
            // 在行尾：合并下一行到当前行
            let next_line = self.lines.remove(row + 1);
            self.lines[row].push_str(&next_line);
        }
        // cursor 位置不变（行尾合并后 cursor 自然在连接点）
        vec![Effect::Render]
    }

    /// Delete word before cursor (Ctrl+W / Option+Backspace).
    /// If a selection is active, it is deleted instead.
    pub fn delete_prev_word(&mut self) -> Vec<Effect> {
        if self.selection.is_some() {
            self.delete_selection();
            return vec![Effect::Render];
        }
        self.delete_word();
        vec![Effect::Render]
    }

    /// Delete from cursor to start of line (Ctrl+U).
    /// If a selection is active, it is deleted instead.
    pub fn delete_to_line_start(&mut self) -> Vec<Effect> {
        if self.selection.is_some() {
            self.delete_selection();
            return vec![Effect::Render];
        }
        self.delete_line_by_head();
        vec![Effect::Render]
    }

    /// Select all text (Ctrl+A).
    pub fn select_all(&mut self) -> Vec<Effect> {
        InputEdit::select_all(self);
        vec![Effect::Render]
    }

    /// Insert newline at cursor (Shift+Enter / Alt+Enter).
    pub fn insert_newline(&mut self) -> Vec<Effect> {
        self.insert_str("\n");
        vec![Effect::Render]
    }

    /// Move cursor left by one char.
    pub fn cursor_left(&mut self) -> Vec<Effect> {
        self.move_cursor_left(false);
        vec![Effect::Render]
    }

    /// Move cursor right by one char.
    pub fn cursor_right(&mut self) -> Vec<Effect> {
        self.move_cursor_right(false);
        vec![Effect::Render]
    }

    /// Move cursor to start of line (Home).
    pub fn cursor_line_start(&mut self) -> Vec<Effect> {
        self.move_cursor_home(false);
        vec![Effect::Render]
    }

    /// Move cursor to end of line (End).
    pub fn cursor_line_end(&mut self) -> Vec<Effect> {
        self.move_cursor_end(false);
        vec![Effect::Render]
    }

    /// Replace textarea content (for @mention / slash completion injection).
    pub fn replace_text(&mut self, text: String) -> Vec<Effect> {
        self.clear_buffer();
        self.insert_str(&text);
        vec![Effect::Render]
    }

    /// Move cursor up one line (for v2 InputState-based cursor navigation).
    pub fn cursor_up(&mut self) -> Vec<Effect> {
        self.move_cursor_up(false);
        vec![Effect::Render]
    }

    /// Move cursor down one line (for v2 InputState-based cursor navigation).
    pub fn cursor_down(&mut self) -> Vec<Effect> {
        self.move_cursor_down(false);
        vec![Effect::Render]
    }
}

// ── Phase 1 tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod phase1_tests {
    use super::*;
    use crate::state_machine::input::{AtMentionState, Selection};

    // ── type_char ─────────────────────────────────────────────────────────

    #[test]
    fn test_type_char_basic() {
        let mut state = InputState::default();
        let effects = state.type_char('H');
        assert_eq!(state.text(), "H");
        assert_eq!(state.cursor, CursorPos::new(0, 1));
        assert!(matches!(effects.as_slice(), [Effect::Render]));
    }

    #[test]
    fn test_type_char_cjk() {
        let mut state = InputState::default();
        state.type_char('你');
        state.type_char('好');
        assert_eq!(state.text(), "你好");
        assert_eq!(state.cursor.col_byte, 6); // 每个 3 字节
    }

    #[test]
    fn test_type_char_replaces_selection() {
        let mut state = InputState {
            lines: vec!["hello world".to_string()],
            cursor: CursorPos::new(0, 5),
            selection: Some(Selection::normal(0, 0, 0, 5)),
            ..Default::default()
        };
        state.type_char('X');
        assert_eq!(state.text(), "X world");
        assert_eq!(state.cursor, CursorPos::new(0, 1));
    }

    // ── delete_prev_char ──────────────────────────────────────────────────

    #[test]
    fn test_delete_prev_char_basic() {
        let mut state = InputState {
            lines: vec!["hello".to_string()],
            cursor: CursorPos::new(0, 3),
            ..Default::default()
        };
        state.delete_prev_char();
        assert_eq!(state.text(), "helo");
        assert_eq!(state.cursor, CursorPos::new(0, 2));
    }

    #[test]
    fn test_delete_prev_char_at_line_start_merges() {
        let mut state = InputState {
            lines: vec!["hello".to_string(), "world".to_string()],
            cursor: CursorPos::new(1, 0),
            ..Default::default()
        };
        state.delete_prev_char();
        assert_eq!(state.lines, vec!["helloworld".to_string()]);
        assert_eq!(state.cursor, CursorPos::new(0, 5));
    }

    #[test]
    fn test_delete_prev_char_deletes_selection_first() {
        let mut state = InputState {
            lines: vec!["hello world".to_string()],
            cursor: CursorPos::new(0, 11),
            selection: Some(Selection::normal(0, 0, 0, 6)),
            ..Default::default()
        };
        state.delete_prev_char();
        assert_eq!(state.text(), "world");
        assert_eq!(state.cursor, CursorPos::new(0, 0));
    }

    // ── delete_next_char ──────────────────────────────────────────────────

    #[test]
    fn test_delete_next_char_basic() {
        let mut state = InputState {
            lines: vec!["hello".to_string()],
            cursor: CursorPos::new(0, 2),
            ..Default::default()
        };
        state.delete_next_char();
        assert_eq!(state.text(), "helo");
        // cursor 不变
        assert_eq!(state.cursor, CursorPos::new(0, 2));
    }

    #[test]
    fn test_delete_next_char_at_line_end_merges() {
        let mut state = InputState {
            lines: vec!["hello".to_string(), "world".to_string()],
            cursor: CursorPos::new(0, 5),
            ..Default::default()
        };
        state.delete_next_char();
        assert_eq!(state.lines, vec!["helloworld".to_string()]);
        assert_eq!(state.cursor, CursorPos::new(0, 5));
    }

    #[test]
    fn test_delete_next_char_deletes_selection_first() {
        let mut state = InputState {
            lines: vec!["hello world".to_string()],
            cursor: CursorPos::new(0, 0),
            selection: Some(Selection::normal(0, 0, 0, 6)),
            ..Default::default()
        };
        state.delete_next_char();
        assert_eq!(state.text(), "world");
    }

    // ── delete_prev_word ──────────────────────────────────────────────────

    #[test]
    fn test_delete_prev_word_ascii() {
        let mut state = InputState {
            lines: vec!["hello world".to_string()],
            cursor: CursorPos::new(0, 11),
            ..Default::default()
        };
        state.delete_prev_word();
        assert_eq!(state.text(), "hello ");
        assert_eq!(state.cursor, CursorPos::new(0, 6));
    }

    #[test]
    fn test_delete_prev_word_cjk() {
        let mut state = InputState {
            lines: vec!["你好世界".to_string()],
            cursor: CursorPos::new(0, 9), // 第三个字 "世" 之后
            ..Default::default()
        };
        state.delete_prev_word();
        // "世" 是 CJK，应删除一个字符
        assert_eq!(state.text(), "你好界");
    }

    #[test]
    fn test_delete_prev_word_deletes_selection_first() {
        let mut state = InputState {
            lines: vec!["hello world".to_string()],
            cursor: CursorPos::new(0, 11),
            selection: Some(Selection::normal(0, 0, 0, 5)),
            ..Default::default()
        };
        state.delete_prev_word();
        assert_eq!(state.text(), " world");
    }

    // ── delete_to_line_start ──────────────────────────────────────────────

    #[test]
    fn test_delete_to_line_start_basic() {
        let mut state = InputState {
            lines: vec!["hello world".to_string()],
            cursor: CursorPos::new(0, 6),
            ..Default::default()
        };
        state.delete_to_line_start();
        assert_eq!(state.text(), "world");
        assert_eq!(state.cursor, CursorPos::new(0, 0));
    }

    #[test]
    fn test_delete_to_line_start_deletes_selection_first() {
        let mut state = InputState {
            lines: vec!["hello world".to_string()],
            cursor: CursorPos::new(0, 0),
            selection: Some(Selection::normal(0, 0, 0, 5)),
            ..Default::default()
        };
        state.delete_to_line_start();
        assert_eq!(state.text(), " world");
    }

    // ── select_all ────────────────────────────────────────────────────────

    #[test]
    fn test_select_all() {
        let mut state = InputState {
            lines: vec!["hello".to_string()],
            cursor: CursorPos::new(0, 0),
            ..Default::default()
        };
        state.select_all();
        assert_eq!(state.cursor, CursorPos::new(0, 5));
        let sel = state.selection.unwrap();
        let range = sel.range();
        assert_eq!(range.start_row, 0);
        assert_eq!(range.start_col, 0);
        assert_eq!(range.end_row, 0);
        assert_eq!(range.end_col, 5);
    }

    // ── insert_newline ────────────────────────────────────────────────────

    #[test]
    fn test_insert_newline_basic() {
        let mut state = InputState {
            lines: vec!["hello".to_string()],
            cursor: CursorPos::new(0, 2),
            ..Default::default()
        };
        state.insert_newline();
        assert_eq!(state.lines, vec!["he".to_string(), "llo".to_string()]);
        // cursor 落在新行，col_byte=0（split 后光标在新行开头）
        assert_eq!(state.cursor, CursorPos::new(1, 0));
    }

    // ── cursor_left / cursor_right ────────────────────────────────────────

    #[test]
    fn test_cursor_left() {
        let mut state = InputState {
            lines: vec!["hello".to_string()],
            cursor: CursorPos::new(0, 3),
            ..Default::default()
        };
        state.cursor_left();
        assert_eq!(state.cursor, CursorPos::new(0, 2));
        assert!(state.selection.is_none());
    }

    #[test]
    fn test_cursor_right() {
        let mut state = InputState {
            lines: vec!["hello".to_string()],
            cursor: CursorPos::new(0, 2),
            ..Default::default()
        };
        state.cursor_right();
        assert_eq!(state.cursor, CursorPos::new(0, 3));
        assert!(state.selection.is_none());
    }

    // ── cursor_line_start / cursor_line_end ───────────────────────────────

    #[test]
    fn test_cursor_line_start() {
        let mut state = InputState {
            lines: vec!["hello".to_string()],
            cursor: CursorPos::new(0, 3),
            ..Default::default()
        };
        state.cursor_line_start();
        assert_eq!(state.cursor, CursorPos::new(0, 0));
    }

    #[test]
    fn test_cursor_line_end() {
        let mut state = InputState {
            lines: vec!["hello".to_string()],
            cursor: CursorPos::new(0, 0),
            ..Default::default()
        };
        state.cursor_line_end();
        assert_eq!(state.cursor, CursorPos::new(0, 5));
    }

    // ── replace_text ──────────────────────────────────────────────────────

    #[test]
    fn test_replace_text() {
        let mut state = InputState {
            lines: vec!["hello".to_string()],
            cursor: CursorPos::new(0, 2),
            ..Default::default()
        };
        state.replace_text("world".to_string());
        assert_eq!(state.text(), "world");
        assert_eq!(state.cursor, CursorPos::new(0, 5));
        assert!(state.selection.is_none());
    }

    #[test]
    fn test_replace_text_clears_at_mention() {
        let mut state = InputState {
            lines: vec!["@file".to_string()],
            at_mention: Some(AtMentionState {
                candidates: vec!["a".into()],
                selected: 0,
            }),
            ..Default::default()
        };
        state.replace_text("done".to_string());
        assert_eq!(state.text(), "done");
        assert!(state.at_mention.is_none());
    }
}
