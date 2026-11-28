//! Shared single-line input state and caret-to-cell mapping.
//!
//! Editing (motion, selection, undo/redo, yank) is delegated to
//! [`TextAreaState`]; this type only drops multiline behaviour and adds the
//! masking / horizontal-window mapping a single-line field needs. Callers keep
//! owning geometry, hit testing and focus.
//!
//! Masking is applied at render time: the value itself stays in clear text.
//! `Debug` prints the length only, because a field may hold an API key.

use crate::textarea::{TextAreaState, display_width_before};

/// Mask character used for secret fields.
pub const MASK: char = '•';

/// One rendered window of a field plus the caret column inside it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FieldDisplay {
    /// Visible text (already masked when requested), clipped to the field width.
    pub text: String,
    /// Caret column relative to the first visible cell.
    pub caret_col: u16,
    /// Selected cell range `[start, end)` relative to the first visible cell.
    pub selection: Option<(u16, u16)>,
}

/// Editable single-line field.
///
/// The caret is stored as a character index internally; [`Self::caret`] still
/// returns a byte offset for existing callers.
#[derive(Clone, Default)]
pub struct TextField {
    state: TextAreaState,
}

impl std::fmt::Debug for TextField {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TextField")
            .field("len", &self.state.len())
            .finish_non_exhaustive()
    }
}

impl PartialEq for TextField {
    fn eq(&self, other: &Self) -> bool {
        self.state.text == other.state.text && self.state.cursor == other.state.cursor
    }
}

impl Eq for TextField {}

impl TextField {
    /// A field with `value` and the caret at the end.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        let mut field = Self::default();
        field.set_value(value);
        field
    }

    /// Current value. Secret callers must not display the result.
    #[must_use]
    pub fn value(&self) -> &str {
        &self.state.text
    }

    /// Whether the trimmed value is empty.
    #[must_use]
    pub fn is_blank(&self) -> bool {
        self.state.text.trim().is_empty()
    }

    /// Caret byte offset.
    #[must_use]
    pub fn caret(&self) -> usize {
        self.state.cursor_byte()
    }

    /// Replace the value and place the caret at the end.
    pub fn set_value(&mut self, value: impl Into<String>) {
        self.state.replace_all_no_undo(value.into());
        self.state.clear_undo_history();
    }

    /// Remove every character.
    pub fn clear(&mut self) {
        self.set_value("");
    }

    /// Insert text, dropping control characters (a paste may include newlines).
    pub fn insert_str(&mut self, text: &str) {
        let clean: String = text.chars().filter(|ch| !ch.is_control()).collect();
        if clean.is_empty() {
            return;
        }
        self.state.insert_str(&clean);
    }

    /// Insert one character at the caret.
    pub fn insert_char(&mut self, character: char) {
        if !character.is_control() {
            self.state.insert_char(character);
        }
    }

    /// Delete the word or selection before the caret.
    ///
    /// The shared [`TextAreaState::delete_word_backward`] is tuned for the
    /// multiline editor and can step over two whitespace-separated words; a
    /// single-line field wants the conventional "delete the previous word"
    /// behaviour, so the boundary is computed here and removed as a selection.
    pub fn delete_word_left(&mut self) {
        if self.state.has_selection() {
            self.state.delete_selection();
            return;
        }
        let chars: Vec<char> = self.state.text.chars().collect();
        let end = self.caret_char();
        let mut start = end;
        while start > 0 && chars[start - 1].is_whitespace() {
            start -= 1;
        }
        while start > 0 && !chars[start - 1].is_whitespace() {
            start -= 1;
        }
        if start < end {
            self.state.selection_start = Some(start);
            self.state.delete_selection();
        }
    }

    /// Delete the word or selection after the caret.
    pub fn delete_word_right(&mut self) {
        if self.state.has_selection() {
            self.state.delete_selection();
            return;
        }
        let chars: Vec<char> = self.state.text.chars().collect();
        let start = self.caret_char();
        let mut end = start;
        while end < chars.len() && chars[end].is_whitespace() {
            end += 1;
        }
        while end < chars.len() && !chars[end].is_whitespace() {
            end += 1;
        }
        while end < chars.len() && chars[end].is_whitespace() {
            end += 1;
        }
        if end > start {
            self.state.selection_start = Some(start);
            self.state.cursor = end;
            self.state.delete_selection();
        }
    }

    /// Delete the character or selection before the caret.
    pub fn backspace(&mut self) {
        self.state.backspace();
    }

    /// Delete the character or selection under the caret.
    pub fn delete_forward(&mut self) {
        self.state.delete_forward();
    }

    /// Move one character left.
    pub fn move_left(&mut self) {
        self.state.cursor_left();
    }

    /// Move one character right.
    pub fn move_right(&mut self) {
        self.state.cursor_right();
    }

    /// Move to the start.
    pub fn move_home(&mut self) {
        self.state.cursor_home();
    }

    /// Move to the end.
    pub fn move_end(&mut self) {
        self.state.cursor_end();
    }

    /// Move one word left.
    pub fn move_word_left(&mut self) {
        self.state.cursor_word_left();
    }

    /// Move one word right.
    pub fn move_word_right(&mut self) {
        self.state.cursor_word_right();
    }

    /// Extend the selection one character left.
    pub fn extend_left(&mut self) {
        self.state.cursor_left_extend();
    }

    /// Extend the selection one character right.
    pub fn extend_right(&mut self) {
        self.state.cursor_right_extend();
    }

    /// Extend the selection one word left.
    pub fn extend_word_left(&mut self) {
        self.state.cursor_word_left_extend();
    }

    /// Extend the selection one word right.
    pub fn extend_word_right(&mut self) {
        self.state.cursor_word_right_extend();
    }

    /// Extend the selection to the start.
    pub fn extend_home(&mut self) {
        self.state.cursor_home_extend();
    }

    /// Extend the selection to the end.
    pub fn extend_end(&mut self) {
        self.state.cursor_end_extend();
    }

    /// Select the whole value.
    pub fn select_all(&mut self) {
        self.state.select_all();
    }

    /// Undo the last edit. Returns whether anything was undone.
    pub fn undo(&mut self) -> bool {
        self.state.undo()
    }

    /// Redo the last undone edit. Returns whether anything was redone.
    pub fn redo(&mut self) -> bool {
        self.state.redo()
    }

    /// Whether a non-empty selection is active.
    #[must_use]
    pub fn has_selection(&self) -> bool {
        self.state.has_selection()
    }

    /// Normalized character range `[start, end)`.
    #[must_use]
    pub fn selection_range(&self) -> Option<(usize, usize)> {
        self.state.selection_range()
    }

    /// Delete the selection, returning the removed text.
    pub fn delete_selection(&mut self) -> Option<String> {
        self.state.delete_selection()
    }

    /// Visible window for the current caret.
    #[must_use]
    pub fn display(&self, width: u16, masked: bool) -> FieldDisplay {
        let width = usize::from(width.max(1));
        let caret_char = self.caret_char();
        let start = self.window_start(width, masked, caret_char);
        let chars: Vec<char> = self.state.text.chars().collect();
        let mut text = String::new();
        let mut used = 0usize;
        let mut end = start;
        while end < chars.len() {
            let character_width = if masked {
                1
            } else {
                display_width_before(&self.state.text, end + 1)
                    - display_width_before(&self.state.text, end)
            };
            if used + character_width > width {
                break;
            }
            used += character_width;
            text.push(if masked { MASK } else { chars[end] });
            end += 1;
        }
        let caret_width = self.width_between(start, caret_char, masked);
        let caret_col = caret_width.min(width.saturating_sub(1)) as u16;
        let selection = self
            .state
            .selection_range()
            .and_then(|(sel_start, sel_end)| {
                let visible_start = sel_start.max(start);
                let visible_end = sel_end.min(end);
                if visible_start >= visible_end {
                    return None;
                }
                let start_cell = self.width_between(start, visible_start, masked);
                let end_cell = self.width_between(start, visible_end, masked);
                (start_cell < end_cell).then_some((start_cell as u16, end_cell as u16))
            });
        FieldDisplay {
            text,
            caret_col,
            selection,
        }
    }

    /// Place the caret from a click `column` inside a field of `width` cells.
    ///
    /// The visible window is recomputed with the same rule as [`Self::display`],
    /// so a click maps to the cell the user actually saw.
    pub fn caret_from_column(&mut self, column: u16, width: u16, masked: bool) {
        let width = usize::from(width.max(1));
        let caret_char = self.caret_char();
        let start = self.window_start(width, masked, caret_char);
        let chars: Vec<char> = self.state.text.chars().collect();
        let mut consumed = 0usize;
        let mut target = start;
        for offset in start..chars.len() {
            let character_width = if masked {
                1
            } else {
                display_width_before(&self.state.text, offset + 1)
                    - display_width_before(&self.state.text, offset)
            };
            if usize::from(column) < consumed + character_width {
                target = offset;
                break;
            }
            consumed += character_width;
            target = offset + 1;
        }
        self.state.cursor = target.min(chars.len());
        self.state.cancel_selection();
    }

    /// Caret character index, clamped to the text length.
    fn caret_char(&self) -> usize {
        self.state.cursor.min(self.state.len())
    }

    /// First visible character so the caret stays inside the last cell.
    fn window_start(&self, width: usize, masked: bool, caret_char: usize) -> usize {
        let limit = width.saturating_sub(1);
        let mut start = caret_char;
        while start > 0 {
            if self.width_between(start - 1, caret_char, masked) > limit {
                break;
            }
            start -= 1;
        }
        start
    }

    /// Cell width between two character indexes.
    fn width_between(&self, start: usize, end: usize, masked: bool) -> usize {
        if masked {
            end.saturating_sub(start)
        } else {
            display_width_before(&self.state.text, end)
                .saturating_sub(display_width_before(&self.state.text, start))
        }
    }
}

#[cfg(test)]
#[path = "input_test.rs"]
mod tests;
