//! Single-line text field state and caret mapping.
//!
//! The dashboard's forms only need single-line inputs. The shared
//! `peri-tui-components` text area is multi-line and has no masking, so the
//! state and the caret mapping live here; rendering is in [`crate::ui::widgets`].
//! Masking is applied at render time: the value itself is never masked.

use peri_tui_components::textarea::display_width_before;

/// Mask character used for secret fields.
pub const MASK: char = '•';

/// One rendered window of a field plus the caret column inside it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FieldDisplay {
    /// Visible text (already masked when requested), clipped to the field width.
    pub text: String,
    /// Caret column relative to the first visible cell.
    pub caret_col: u16,
}

/// Editable single-line field with a byte-indexed caret.
///
/// `caret` is always on a UTF-8 character boundary. `Debug` is safe: the value
/// may hold an API key and must not be printed by callers.
#[derive(Clone, Default, Eq, PartialEq)]
pub struct TextField {
    value: String,
    caret: usize,
}

impl std::fmt::Debug for TextField {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TextField")
            .field("len", &self.value.chars().count())
            .finish_non_exhaustive()
    }
}

impl TextField {
    /// A field with `value` and the caret at the end.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        let caret = value.len();
        Self { value, caret }
    }

    /// Current value. Secret callers must not display the result.
    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }

    /// Whether the trimmed value is empty.
    #[must_use]
    pub fn is_blank(&self) -> bool {
        self.value.trim().is_empty()
    }

    /// Caret byte offset.
    #[must_use]
    pub fn caret(&self) -> usize {
        self.caret
    }

    /// Replace the value and place the caret at the end.
    pub fn set_value(&mut self, value: impl Into<String>) {
        self.value = value.into();
        self.caret = self.value.len();
    }

    /// Remove every character.
    pub fn clear(&mut self) {
        self.value.clear();
        self.caret = 0;
    }

    /// Insert text, dropping control characters (a paste may include newlines).
    pub fn insert_str(&mut self, text: &str) {
        let clean: String = text.chars().filter(|ch| !ch.is_control()).collect();
        if clean.is_empty() {
            return;
        }
        self.value.insert_str(self.caret, &clean);
        self.caret += clean.len();
    }

    /// Delete the character before the caret.
    pub fn backspace(&mut self) {
        if self.caret == 0 {
            return;
        }
        let previous = self.value[..self.caret]
            .char_indices()
            .next_back()
            .map_or(0, |(index, _)| index);
        self.value.replace_range(previous..self.caret, "");
        self.caret = previous;
    }

    /// Delete the character under the caret.
    pub fn delete_forward(&mut self) {
        let Some(character) = self.value[self.caret..].chars().next() else {
            return;
        };
        let end = self.caret + character.len_utf8();
        self.value.replace_range(self.caret..end, "");
    }

    /// Move one character left.
    pub fn move_left(&mut self) {
        if let Some((index, _)) = self.value[..self.caret].char_indices().next_back() {
            self.caret = index;
        }
    }

    /// Move one character right.
    pub fn move_right(&mut self) {
        if let Some(character) = self.value[self.caret..].chars().next() {
            self.caret += character.len_utf8();
        }
    }

    /// Move to the start.
    pub fn move_home(&mut self) {
        self.caret = 0;
    }

    /// Move to the end.
    pub fn move_end(&mut self) {
        self.caret = self.value.len();
    }

    /// Place the caret from a click `column` inside a field of `width` cells.
    ///
    /// The visible window is recomputed with the same rule as [`Self::display`],
    /// so a click maps to the cell the user actually saw.
    pub fn caret_from_column(&mut self, column: u16, width: u16, masked: bool) {
        let width = usize::from(width.max(1));
        let caret_char = self.char_index(self.caret);
        let start = self.window_start(width, masked, caret_char);
        let chars: Vec<char> = self.value.chars().collect();
        let mut consumed = 0usize;
        let mut target = start;
        for (offset, character) in chars.iter().enumerate().skip(start) {
            let character_width = if masked {
                1
            } else {
                display_width_before(&self.value, offset + 1)
                    - display_width_before(&self.value, offset)
            };
            if usize::from(column) < consumed + character_width {
                target = offset;
                break;
            }
            consumed += character_width;
            target = offset + 1;
            let _ = character;
        }
        self.caret = self.byte_at_char(target);
    }

    /// Visible window for the current caret.
    #[must_use]
    pub fn display(&self, width: u16, masked: bool) -> FieldDisplay {
        let width = usize::from(width.max(1));
        let caret_char = self.char_index(self.caret);
        let start = self.window_start(width, masked, caret_char);
        let chars: Vec<char> = self.value.chars().collect();
        let mut text = String::new();
        let mut used = 0usize;
        let mut end = start;
        while end < chars.len() {
            let character_width = if masked {
                1
            } else {
                display_width_before(&self.value, end + 1) - display_width_before(&self.value, end)
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
        FieldDisplay { text, caret_col }
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
            display_width_before(&self.value, end)
                .saturating_sub(display_width_before(&self.value, start))
        }
    }

    /// Character index for a byte offset.
    fn char_index(&self, byte: usize) -> usize {
        self.value[..byte.min(self.value.len())].chars().count()
    }

    /// Byte offset after `characters` characters.
    fn byte_at_char(&self, characters: usize) -> usize {
        self.value
            .char_indices()
            .nth(characters)
            .map_or(self.value.len(), |(index, _)| index)
    }
}

#[cfg(test)]
#[path = "text_field_test.rs"]
mod tests;
